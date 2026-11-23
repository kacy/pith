//! failure isolation: a runtime error inside `isolate_call` fails that call
//! instead of the process.
//!
//! `pith_isolate_call` runs a pith closure behind a catch frame. the frame is
//! a few words on the caller's own stack, linked into a chain whose head
//! belongs to the running task (a field of its threadlocal map) or, off the
//! green scheduler, to the os thread. a runtime error that finds a frame
//! records its message there and jumps back into `isolate_enter`, which
//! restores the callee-saved registers it pushed on the way in and returns to
//! `pith_isolate_call` as if the closure had returned. a runtime error that
//! finds no frame prints its message and exits with status 1, as before.
//!
//! the jump skips every frame between the error and the catch frame without
//! running anything in them. those are compiled pith frames, which have no
//! destructors to run, and the runtime's own error paths, which reach `fail`
//! holding nothing that needs a drop. what the pith frames held is lost: the
//! counts they owned leak, and their `defer` blocks do not run. the one thing
//! the runtime recovers is a `Mutex` the call locked and still holds, which
//! is unlocked, since a lock left held would park every later caller for
//! good. docs/failure_isolation.md has the design and the measurements.
//!
//! a frame lives on the stack of the task that set it, and the jump restores
//! a stack pointer into that same stack, so a task that parked and resumed on
//! another worker inside the call is still caught correctly. the chain head
//! is read through the task's map each time, never cached across pith code.

use std::cell::Cell;

/// one catch frame. `sp` is the stack pointer `isolate_enter` saved after
/// pushing the callee-saved registers, and `isolate_resume` restores it.
struct Frame {
    sp: usize,
    prev: *mut Frame,
    /// the runtime error that unwound the call, once there is one.
    message: Option<String>,
    /// the mutexes the call locked and has not unlocked, oldest first.
    locks: Vec<usize>,
}

thread_local! {
    /// the chain head of an os thread: the main thread, and every thread of
    /// the os-thread backend. a green task keeps its head in its own map.
    static THREAD_FRAME: Cell<usize> = const { Cell::new(0) };
}

/// the innermost catch frame of the running task or thread, or null.
#[inline]
fn head() -> *mut Frame {
    match crate::concurrency::green::current_task_tls() {
        // SAFETY: the running task's own map, which only it touches.
        Some(map) => unsafe { (*map).isolation as *mut Frame },
        None => thread_frame() as *mut Frame,
    }
}

#[inline]
fn set_head(frame: *mut Frame) {
    match crate::concurrency::green::current_task_tls() {
        // SAFETY: as in `head`.
        Some(map) => unsafe { (*map).isolation = frame as usize },
        None => set_thread_frame(frame as usize),
    }
}

// the touches of `THREAD_FRAME`, each behind a call the optimizer cannot
// inline, so no frame that runs pith code holds the cell's address (see
// tooling/check_tls_barriers.sh).
#[inline(never)]
fn thread_frame() -> usize {
    THREAD_FRAME.with(|c| c.get())
}

#[inline(never)]
fn set_thread_frame(frame: usize) {
    THREAD_FRAME.with(|c| c.set(frame))
}

/// Run the closure `closure` (a pith `fn() -> Int`) behind a catch frame.
/// Returns the empty string when it returned, or the message of the runtime
/// error that stopped it, as an owned string.
///
/// # Safety
/// `closure` must be a pith closure handle the caller keeps alive for the
/// call.
#[no_mangle]
pub unsafe extern "C" fn pith_isolate_call(closure: i64) -> *mut i8 {
    // SAFETY: Caller guarantees `closure` is a live pith closure handle.
    let func = unsafe { crate::pith_closure_get_fn(closure) };
    if func == 0 {
        return crate::runtime_core::pith_cstring_empty();
    }
    let mut frame = Frame {
        sp: 0,
        prev: head(),
        message: None,
        locks: Vec::new(),
    };
    // every access below goes through `fp`, so the compiler reloads the frame
    // after the call: a failure writes it from another function.
    let fp: *mut Frame = &mut frame;
    set_head(fp);
    // SAFETY: `fp` points to our live stack frame and `func`/`closure` form a valid pith closure.
    unsafe { run(fp, func, closure) };
    // read the head's slot afresh: the task may be on another thread by now.
    // SAFETY: `fp` points to our stack-local `frame`.
    let prev = unsafe { (*fp).prev };
    set_head(prev);
    // SAFETY: `fp` is unlinked from the catch chain and uniquely owned by this stack frame.
    let (locks, message) = unsafe { (std::mem::take(&mut (*fp).locks), (*fp).message.take()) };
    match message {
        None => {
            // a call that returned holding a lock leaves it held, as any
            // function may. an enclosing frame takes the entries over, so a
            // failure further out still finds them.
            if !prev.is_null() && !locks.is_empty() {
                // SAFETY: `prev` is an enclosing catch frame still live on this same stack.
                unsafe { (*prev).locks.extend(locks) };
            }
            crate::runtime_core::pith_cstring_empty()
        }
        Some(message) => {
            for handle in locks.into_iter().rev() {
                // SAFETY: Each recorded handle came from `note_lock` for a mutex locked by this call.
                unsafe { crate::concurrency::mutex::pith_mutex_unlock(handle as *mut _) };
            }
            crate::runtime_core::pith_strdup_string(&message)
        }
    }
}

/// call `func(closure)` behind the frame `fp`.
#[inline(always)]
unsafe fn run(fp: *mut Frame, func: i64, closure: i64) {
    // SAFETY: Caller guarantees `fp` is a live catch frame and `func`/`closure` are callable.
    unsafe { isolate_enter(std::ptr::addr_of_mut!((*fp).sp), func, closure) };
}

/// whether this target has a catch frame. without one, `isolate_enter` is a
/// plain call and every runtime error exits.
const CATCHES: bool = cfg!(any(target_arch = "x86_64", target_arch = "aarch64"));

/// Stop at a runtime error: unwind to the innermost catch frame with
/// `message`, or print it and exit with status 1 when there is none.
///
/// The caller must hold nothing that needs a drop: the jump skips its frame.
#[cold]
#[inline(never)]
pub(crate) fn fail(message: String) -> ! {
    let frame = head();
    if frame.is_null() || !CATCHES {
        eprintln!("pith runtime error: {message}");
        // panic-guard: a runtime error outside an isolated call stops the program.
        std::process::exit(1);
    }
    // SAFETY: a frame on the chain belongs to a `pith_isolate_call` that is
    // still running below us on this same stack.
    unsafe {
        (*frame).message = Some(message);
        #[cfg(pith_asan)]
        __asan_handle_no_return();
        isolate_resume((*frame).sp)
    }
}

#[cfg(pith_asan)]
extern "C" {
    fn __asan_handle_no_return();
}

#[cold]
#[inline(never)]
unsafe fn note_lock_slow(frame: *mut Frame, handle: usize) {
    // SAFETY: Caller guarantees `frame` is the running task's live catch frame.
    unsafe { (*frame).locks.push(handle) };
}

#[cold]
#[inline(never)]
unsafe fn note_unlock_slow(mut frame: *mut Frame, handle: usize) {
    while !frame.is_null() {
        // SAFETY: frames on the chain are live, as in `fail`.
        let locks = unsafe { &mut (*frame).locks };
        if let Some(i) = locks.iter().rposition(|&h| h == handle) {
            locks.remove(i);
            return;
        }
        // SAFETY: frames on the chain are live, as in `fail`.
        frame = unsafe { (*frame).prev };
    }
}

/// A mutex the running task just locked. Inside an isolated call it is
/// recorded, so a failure can unlock it.
#[inline(always)]
pub(crate) fn note_lock(handle: usize) {
    let frame = head();
    if !frame.is_null() {
        // SAFETY: the running task's own frame.
        unsafe { note_lock_slow(frame, handle) };
    }
}

/// A mutex the running task just unlocked: forget the newest record of it on
/// the chain, if any.
#[inline(always)]
pub(crate) fn note_unlock(handle: usize) {
    let frame = head();
    if !frame.is_null() {
        // SAFETY: frames on the chain are live, as in `fail`.
        unsafe { note_unlock_slow(frame, handle) };
    }
}

// `isolate_enter(sp_slot, func, closure)` pushes the callee-saved registers,
// stores the stack pointer in `*sp_slot` and calls `func(closure)`. it returns
// normally when the call does. `isolate_resume(sp)` is the other way out: it
// sets the stack pointer back to a saved `sp` and runs the same epilogue, so
// `isolate_enter` returns to its caller with every callee-saved register as
// it was on entry, whatever the skipped frames did with them.

#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn isolate_enter(sp_slot: *mut usize, func: i64, closure: i64) {
    core::arch::naked_asm!(
        "push rbp",
        "mov rbp, rsp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        // six pushes after the return address: one more word aligns the call
        "sub rsp, 8",
        "mov [rdi], rsp",
        "mov rdi, rdx",
        "call rsi",
        "add rsp, 8",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "ret",
    )
}

#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn isolate_resume(sp: usize) -> ! {
    core::arch::naked_asm!(
        "mov rsp, rdi",
        "add rsp, 8",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "ret",
    )
}

#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn isolate_enter(sp_slot: *mut usize, func: i64, closure: i64) {
    core::arch::naked_asm!(
        "stp x29, x30, [sp, #-160]!",
        "mov x29, sp",
        "stp x19, x20, [sp, #16]",
        "stp x21, x22, [sp, #32]",
        "stp x23, x24, [sp, #48]",
        "stp x25, x26, [sp, #64]",
        "stp x27, x28, [sp, #80]",
        "stp d8, d9, [sp, #96]",
        "stp d10, d11, [sp, #112]",
        "stp d12, d13, [sp, #128]",
        "stp d14, d15, [sp, #144]",
        "mov x9, sp",
        "str x9, [x0]",
        "mov x0, x2",
        "blr x1",
        "ldp d14, d15, [sp, #144]",
        "ldp d12, d13, [sp, #128]",
        "ldp d10, d11, [sp, #112]",
        "ldp d8, d9, [sp, #96]",
        "ldp x27, x28, [sp, #80]",
        "ldp x25, x26, [sp, #64]",
        "ldp x23, x24, [sp, #48]",
        "ldp x21, x22, [sp, #32]",
        "ldp x19, x20, [sp, #16]",
        "ldp x29, x30, [sp], #160",
        "ret",
    )
}

#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn isolate_resume(sp: usize) -> ! {
    core::arch::naked_asm!(
        "mov sp, x0",
        "ldp d14, d15, [sp, #144]",
        "ldp d12, d13, [sp, #128]",
        "ldp d10, d11, [sp, #112]",
        "ldp d8, d9, [sp, #96]",
        "ldp x27, x28, [sp, #80]",
        "ldp x25, x26, [sp, #64]",
        "ldp x23, x24, [sp, #48]",
        "ldp x21, x22, [sp, #32]",
        "ldp x19, x20, [sp, #16]",
        "ldp x29, x30, [sp], #160",
        "ret",
    )
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
unsafe extern "C" fn isolate_enter(_sp_slot: *mut usize, func: i64, closure: i64) {
    // SAFETY: Caller guarantees `func` is a valid closure entry point for the pith closure ABI.
    // panic-guard: calling a compiler-emitted closure body through the pith closure abi.
    let f: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(func as usize) };
    f(closure);
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
unsafe extern "C" fn isolate_resume(_sp: usize) -> ! {
    // unreachable: `fail` exits instead on a target without a catch frame.
    std::process::abort()
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn returns(_closure: i64) -> i64 {
        7
    }

    extern "C" fn fails(_closure: i64) -> i64 {
        fail(String::from("boom"))
    }

    extern "C" fn nested(_closure: i64) -> i64 {
        let fp = head();
        assert!(!fp.is_null());
        let mut inner = Frame {
            sp: 0,
            prev: fp,
            message: None,
            locks: Vec::new(),
        };
        let ip: *mut Frame = &mut inner;
        set_head(ip);
        unsafe { run(ip, fails as usize as i64, 0) };
        set_head(fp);
        assert_eq!(unsafe { (*ip).message.take() }.as_deref(), Some("boom"));
        fail(String::from("outer"))
    }

    fn call(func: extern "C" fn(i64) -> i64) -> Option<String> {
        let mut frame = Frame {
            sp: 0,
            prev: head(),
            message: None,
            locks: Vec::new(),
        };
        let fp: *mut Frame = &mut frame;
        set_head(fp);
        unsafe { run(fp, func as usize as i64, 0) };
        let prev = unsafe { (*fp).prev };
        set_head(prev);
        unsafe { (*fp).message.take() }
    }

    #[test]
    fn a_returning_call_reports_nothing() {
        assert_eq!(call(returns), None);
        assert!(head().is_null());
    }

    #[test]
    fn a_failure_returns_to_the_innermost_frame() {
        assert_eq!(call(fails).as_deref(), Some("boom"));
        assert_eq!(call(nested).as_deref(), Some("outer"));
        assert!(head().is_null());
    }

    #[test]
    fn a_failure_keeps_the_callers_registers() {
        // values live across the call sit in callee-saved registers; a jump
        // that lost them would change the sum.
        let mut sum = 0u64;
        for i in 0..1000u64 {
            let a = i.wrapping_mul(31);
            let b = i ^ 0x55;
            assert!(call(fails).is_some());
            sum = sum.wrapping_add(a + b);
        }
        let expected: u64 = (0..1000u64).map(|i| i.wrapping_mul(31) + (i ^ 0x55)).sum();
        assert_eq!(sum, expected);
    }
}
