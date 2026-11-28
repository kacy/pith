//! integer overflow reported from a trap.
//!
//! the code generator checks Int and UInt `+`, `-` and `*`, the conversions
//! to the narrow and unsigned types (which also check narrow arithmetic) and
//! shift counts with a trap instruction on the overflow path (`ud2` on
//! x86-64, `udf` on aarch64) rather than a call into the runtime: a call
//! there, even one never taken,
//! makes the values live across the check survive it, which costs spills in
//! the loops the checks sit in. the trap raises SIGILL, and the handler here
//! turns it into the same `pith runtime error: integer overflow` a call
//! would have printed, through `pith_runtime_error`.
//!
//! generated code raises no other trap, but a SIGILL can still come from
//! elsewhere (an instruction the CPU lacks), so the handler reads the
//! faulting instruction and passes anything that is not a trap instruction
//! to the default action.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Once;

/// Kernel `si_code` values raised by trap instructions (`ud2` / `udf`):
/// - `1` (`ILL_ILLOPC` on Linux, macOS, BSDs)
/// - `2` (`ILL_ILLOPN` on Linux x86-64 for `#UD`/`ud2`; `ILL_ILLTRP` on macOS)
fn is_trap_si_code(code: libc::c_int) -> bool {
    code == 1 || code == 2
}

static INSTALL: Once = Once::new();

#[cfg(target_arch = "x86_64")]
const TRAP_INSN_LEN: usize = 2;
#[cfg(target_arch = "aarch64")]
const TRAP_INSN_LEN: usize = 4;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const TRAP_INSN_LEN: usize = 1;

/// Executable `PT_LOAD` ranges of the main executable, collected once at
/// install time so `on_sigill` can reject `SIGILL` faults raised outside our
/// compiled executable (e.g. from C shared libraries or jumps to heap/stack).
#[cfg(target_os = "linux")]
const MAIN_EXEC_RANGES_CAP: usize = 16;
#[cfg(target_os = "linux")]
static MAIN_EXEC_STARTS: [AtomicUsize; MAIN_EXEC_RANGES_CAP] =
    [const { AtomicUsize::new(0) }; MAIN_EXEC_RANGES_CAP];
#[cfg(target_os = "linux")]
static MAIN_EXEC_ENDS: [AtomicUsize; MAIN_EXEC_RANGES_CAP] =
    [const { AtomicUsize::new(0) }; MAIN_EXEC_RANGES_CAP];
#[cfg(target_os = "linux")]
static MAIN_EXEC_RANGES_LEN: AtomicUsize = AtomicUsize::new(0);

#[cfg(target_os = "linux")]
extern "C" fn collect_main_exec_cb(
    info: *mut libc::dl_phdr_info,
    _size: libc::size_t,
    _data: *mut libc::c_void,
) -> libc::c_int {
    if info.is_null() {
        return 0;
    }
    // SAFETY: `dl_iterate_phdr` passes a valid `dl_phdr_info` pointer.
    let info = unsafe { &*info };
    // On Linux (glibc/musl), the main executable has an empty `dlpi_name`.
    let is_main = info.dlpi_name.is_null() || unsafe { *info.dlpi_name == 0 };
    if !is_main || info.dlpi_phdr.is_null() {
        return 0;
    }
    let base = info.dlpi_addr as usize;
    let count = info.dlpi_phnum as usize;
    for i in 0..count {
        // SAFETY: `dlpi_phdr` points to `dlpi_phnum` entries.
        let phdr = unsafe { &*info.dlpi_phdr.add(i) };
        if phdr.p_type == libc::PT_LOAD && (phdr.p_flags & libc::PF_X) != 0 {
            let start = base.wrapping_add(phdr.p_vaddr as usize);
            let end = start.saturating_add(phdr.p_memsz as usize);
            if start < end {
                let idx = MAIN_EXEC_RANGES_LEN.load(Ordering::Relaxed);
                if idx < MAIN_EXEC_RANGES_CAP {
                    MAIN_EXEC_STARTS[idx].store(start, Ordering::Relaxed);
                    MAIN_EXEC_ENDS[idx].store(end, Ordering::Relaxed);
                    MAIN_EXEC_RANGES_LEN.store(idx + 1, Ordering::Release);
                }
            }
        }
    }
    1
}

#[cfg(target_os = "linux")]
fn collect_main_exec_ranges() {
    // SAFETY: `collect_main_exec_cb` only reads the kernel/loader `dl_phdr_info`
    // and writes atomics; called once inside `INSTALL.call_once`.
    unsafe {
        libc::dl_iterate_phdr(Some(collect_main_exec_cb), std::ptr::null_mut());
    }
}

#[cfg(not(target_os = "linux"))]
fn collect_main_exec_ranges() {}

/// Async-signal-safe check that `[pc, pc + TRAP_INSN_LEN)` falls inside an
/// executable segment of the main binary (when ranges were collected).
fn pc_in_main_executable(pc: usize) -> bool {
    #[cfg(target_os = "linux")]
    {
        let len = MAIN_EXEC_RANGES_LEN.load(Ordering::Acquire);
        if len == 0 {
            return true;
        }
        let Some(pc_end) = pc.checked_add(TRAP_INSN_LEN) else {
            return false;
        };
        for i in 0..len.min(MAIN_EXEC_RANGES_CAP) {
            let start = MAIN_EXEC_STARTS[i].load(Ordering::Relaxed);
            let end = MAIN_EXEC_ENDS[i].load(Ordering::Relaxed);
            if pc >= start && pc_end <= end {
                return true;
            }
        }
        false
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pc;
        true
    }
}

/// install the SIGILL handler once per process.
pub(crate) fn install() {
    INSTALL.call_once(|| {
        collect_main_exec_ranges();
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

fn is_trap_bytes(bytes: &[u8]) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        // ud2
        bytes.len() >= 2 && bytes[0] == 0x0f && bytes[1] == 0x0b
    }
    #[cfg(target_arch = "aarch64")]
    {
        // Cranelift emits `udf #<trap_code>` with a non-zero imm16 for our
        // user overflow trap code (`OVERFLOW_TRAP_CODE = 7`), so reject
        // all-zero `0x0000_0000` words from jumping into zeroed memory.
        if bytes.len() < 4 {
            return false;
        }
        let insn = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        insn != 0 && (insn >> 16) == 0
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = bytes;
        false
    }
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
    // SAFETY: Caller guarantees `pc` points to a mapped faulting instruction of
    // at least `TRAP_INSN_LEN` bytes (and on Linux, `pc_in_main_executable`
    // verified `[pc, pc + TRAP_INSN_LEN)` lies inside a main `PT_LOAD|PF_X` segment).
    let bytes = unsafe { std::slice::from_raw_parts(pc, TRAP_INSN_LEN) };
    is_trap_bytes(bytes)
}

extern "C" fn on_sigill(sig: libc::c_int, info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
    // SAFETY: the kernel hands the handler a valid siginfo for SIGILL.
    // Trap instructions (`ud2` / `udf`) always raise `ILL_ILLOPC`; external
    // `kill(pid, SIGILL)` has `code <= 0` and other SIGILL faults carry
    // distinct `ILL_*` codes.
    let (code, pc) = unsafe { ((*info).si_code, (*info).si_addr() as *const u8) };
    // SAFETY: see is_trap_instruction; guarded by trap si_code and executable-segment check.
    if is_trap_si_code(code)
        && !pc.is_null()
        && pc_in_main_executable(pc as usize)
        && unsafe { is_trap_instruction(pc) }
    {
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
        // Zero-allocation static failure path: avoids heap allocation and
        // stdio/atexit locks inside the signal handler.
        crate::failure::fail_static("integer overflow");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trap_bytes_match_only_expected_trap_encoding() {
        #[cfg(target_arch = "x86_64")]
        {
            assert!(is_trap_bytes(&[0x0f, 0x0b]));
            assert!(!is_trap_bytes(&[0x00, 0x00]));
            assert!(!is_trap_bytes(&[0x90, 0x90]));
        }
        #[cfg(target_arch = "aarch64")]
        {
            // udf #7 is accepted; zeroed memory 0x0000_0000 is rejected
            assert!(is_trap_bytes(&7u32.to_le_bytes()));
            assert!(!is_trap_bytes(&0u32.to_le_bytes()));
            assert!(!is_trap_bytes(&0xd503_201fu32.to_le_bytes())); // nop
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn main_executable_ranges_include_own_code_and_reject_stack_or_null() {
        install();
        assert!(
            MAIN_EXEC_RANGES_LEN.load(Ordering::Acquire) > 0,
            "expected at least one PT_LOAD|PF_X range for main executable"
        );
        let code_addr = install as *const () as usize;
        assert!(
            pc_in_main_executable(code_addr),
            "runtime code address {code_addr:#x} should be in main executable"
        );
        let stack_var = 0u64;
        let stack_addr = &stack_var as *const u64 as usize;
        assert!(!pc_in_main_executable(stack_addr));
        assert!(!pc_in_main_executable(0));
    }
}
