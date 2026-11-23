//! integer overflow reported from a trap.
//!
//! the code generator checks Int and UInt `+`, `-` and `*` with a trap
//! instruction on the overflow path (`ud2` on x86-64, `udf` on aarch64)
//! rather than a call into the runtime: a call there, even one never taken,
//! makes the values live across the check survive it, which costs spills in
//! the loops the checks sit in. the trap raises SIGILL, and the handler here
//! turns it into the same `pith runtime error: integer overflow` a call
//! would have printed, through `pith_runtime_error`.
//!
//! generated code raises no other trap, but a SIGILL can still come from
//! elsewhere (an instruction the CPU lacks), so the handler reads the
//! faulting instruction and passes anything that is not a trap instruction
//! to the default action.

use std::sync::Once;

static INSTALL: Once = Once::new();

/// install the SIGILL handler once per process.
pub(crate) fn install() {
    INSTALL.call_once(|| {
        // SAFETY: `libc::sigaction` is a C POD struct valid to zero-initialize.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = on_sigill as *const () as usize;
        // SA_NODEFER: the error path may leave the handler without
        // returning, and SIGILL must not stay blocked when it does.
        action.sa_flags = libc::SA_SIGINFO | libc::SA_NODEFER;
        // SAFETY: `sigemptyset` and `sigaction` are plain libc calls on stack-local storage.
        unsafe {
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(libc::SIGILL, &action, std::ptr::null_mut());
        }
    });
}

/// whether the instruction at `pc` is the trap the code generator emits.
///
/// # Safety
/// `pc` must be null or the faulting address the kernel reported for SIGILL,
/// where the instruction bytes are mapped and readable.
unsafe fn is_trap_instruction(pc: *const u8) -> bool {
    if pc.is_null() {
        return false;
    }
    #[cfg(target_arch = "x86_64")]
    {
        // ud2
        // SAFETY: Caller guarantees `pc` points to a mapped faulting instruction.
        unsafe { *pc == 0x0f && *pc.add(1) == 0x0b }
    }
    #[cfg(target_arch = "aarch64")]
    {
        // udf #imm16: the top sixteen bits of the word are zero
        // SAFETY: Caller guarantees `pc` points to a mapped 4-byte faulting instruction.
        unsafe { (pc as *const u32).read_unaligned() >> 16 == 0 }
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        false
    }
}

extern "C" fn on_sigill(sig: libc::c_int, info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
    // SAFETY: the kernel hands the handler a valid siginfo for SIGILL. a
    // positive si_code is a fault the kernel raised, the only kind whose
    // si_addr is an instruction address (external kill(pid, SIGILL) puts
    // sender pid/uid in that union).
    let (code, pc) = unsafe { ((*info).si_code, (*info).si_addr() as *const u8) };
    // SAFETY: see is_trap_instruction; guarded by code > 0 above.
    if code > 0 && unsafe { is_trap_instruction(pc) } {
        // the error path leaves the handler without a sigreturn, so the mask
        // the kernel or a sanitizer set for the handler would stay in force.
        // SA_NODEFER keeps SIGILL out of it in a plain build, but tsan runs
        // every handler with all signals blocked, and the next overflow on
        // the same thread (a green worker running another task) was then
        // a blocked SIGILL, which the kernel turns into a kill. put back the
        // mask the faulting code ran with, as sigreturn would.
        let uc = ctx as *const libc::ucontext_t;
        if !uc.is_null() {
            // SAFETY: an SA_SIGINFO handler's third argument is the ucontext of
            // the interrupted code, and `pthread_sigmask` is async-signal-safe.
            unsafe {
                libc::pthread_sigmask(libc::SIG_SETMASK, &(*uc).uc_sigmask, std::ptr::null_mut());
            }
        }
        // SAFETY: the runtime's error trap; it does not return.
        unsafe {
            crate::runtime_core::pith_runtime_error(7);
        }
        return;
    }
    // not ours: restore the default action and let the process die of SIGILL
    // as it would have. a fault re-raises itself when the instruction runs
    // again; a signal someone sent (code <= 0) does not, so send it again.
    // SAFETY: `libc::sigaction` is a C POD struct valid to zero-initialize.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = libc::SIG_DFL;
    // SAFETY: `sigemptyset` and `sigaction` are async-signal-safe on stack-local storage.
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(sig, &action, std::ptr::null_mut());
    }
    if code <= 0 {
        // SAFETY: `raise` is async-signal-safe.
        unsafe { libc::raise(sig) };
    }
}
