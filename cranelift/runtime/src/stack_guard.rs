//! stack overflow reports.
//!
//! a green task runs on a fixed-size coroutine stack with a guard page at its
//! low end, and the main thread and every os thread have a guard region below
//! theirs. running into one raises SIGSEGV (SIGBUS on macos) on a stack with
//! no room left, so without help the process died by a bare signal and said
//! nothing. a server runs every request in a task, so one deeply recursive
//! request took the whole process down that way.
//!
//! the handler here runs on an alternate signal stack, which every thread
//! that runs pith code installs once (`enter_thread`). it checks the fault
//! address against the guard of the green task running on the thread, then
//! against the thread's own guard. a hit writes one line to stderr and exits
//! with `STACK_OVERFLOW_EXIT`. anything else is a real crash: the handler puts
//! back the disposition it replaced and returns, the faulting instruction runs
//! again, and the process dies by the original signal (with its core dump) or
//! reaches the handler that was there before.
//!
//! the handler reads only thread-local cells, atomics and the task arena, and
//! calls only `write`, `sigaction`, `raise` and `_exit`, which are all
//! async-signal-safe.
//!
//! sanitizer builds install nothing. asan and tsan handle SIGSEGV themselves
//! and report a stack overflow with a full trace, and a handler of ours in
//! front of theirs would hide it.

#[cfg(not(any(pith_asan, pith_tsan)))]
use std::cell::{Cell, RefCell};
#[cfg(not(any(pith_asan, pith_tsan)))]
use std::sync::atomic::{AtomicUsize, Ordering};

/// the exit status of a process stopped by a stack overflow. it differs from
/// the 1 of every other runtime error so a supervisor can tell the two apart,
/// and it is the status go uses for the same failure.
pub const STACK_OVERFLOW_EXIT: i32 = 2;

/// how far below a guard page a fault still counts as running into it. a
/// frame larger than a page moves the stack pointer past the guard in one
/// step, and its first store lands below the guard rather than in it.
#[cfg(not(any(pith_asan, pith_tsan)))]
const GUARD_SLACK: usize = 64 * 1024;

/// the size of each thread's alternate signal stack. the handler itself needs
/// little; the kernel's signal frame with the full vector register state is
/// what takes room, a few KiB on a machine with avx-512.
#[cfg(not(any(pith_asan, pith_tsan)))]
const ALT_STACK_SIZE: usize = 64 * 1024;

/// the page size, read once at install so the handler need not call
/// `sysconf`.
#[cfg(not(any(pith_asan, pith_tsan)))]
static PAGE_SIZE: AtomicUsize = AtomicUsize::new(4096);

/// the dispositions the handler replaced, for SIGSEGV and SIGBUS. written once
/// by `install_handlers`, then only read.
#[cfg(not(any(pith_asan, pith_tsan)))]
static mut PREVIOUS: [Option<libc::sigaction>; 2] = [None, None];

#[cfg(not(any(pith_asan, pith_tsan)))]
const FAULT_SIGNALS: [libc::c_int; 2] = [libc::SIGSEGV, libc::SIGBUS];

#[cfg(not(any(pith_asan, pith_tsan)))]
thread_local! {
    /// the address range below this thread's stack that a fault in means an
    /// overflow, `(0, 0)` until `enter_thread` has run. a const cell with no
    /// destructor, so the handler can read it.
    static THREAD_GUARD: Cell<(usize, usize)> = const { Cell::new((0, 0)) };

    /// whether this thread is the one that called `pith_runtime_start`, which
    /// is the process's main thread.
    static IS_MAIN: Cell<bool> = const { Cell::new(false) };

    /// the alternate signal stack this thread installed, if it installed one.
    /// dropping it at thread exit disables the stack before freeing it.
    static ALT_STACK: RefCell<Option<AltStack>> = const { RefCell::new(None) };
}

#[cfg(not(any(pith_asan, pith_tsan)))]
struct AltStack {
    buffer: Vec<u8>,
}

/// buffers of threads that have exited, for the next thread to take. an
/// os-thread task is a thread of its own, so without this every spawn paid
/// an allocation and a free for its alternate stack.
#[cfg(not(any(pith_asan, pith_tsan)))]
static SPARE_ALT_STACKS: std::sync::Mutex<Vec<Vec<u8>>> = std::sync::Mutex::new(Vec::new());

/// how many spare buffers the pool keeps.
#[cfg(not(any(pith_asan, pith_tsan)))]
const SPARE_ALT_STACKS_MAX: usize = 64;

#[cfg(not(any(pith_asan, pith_tsan)))]
impl Drop for AltStack {
    fn drop(&mut self) {
        // SAFETY: disables this thread's alternate stack, which is this
        // buffer, before the buffer is handed on or freed.
        unsafe {
            let mut off: libc::stack_t = std::mem::zeroed();
            off.ss_flags = libc::SS_DISABLE;
            libc::sigaltstack(&off, std::ptr::null_mut());
        }
        let mut spare = SPARE_ALT_STACKS.lock().unwrap_or_else(|p| p.into_inner());
        if spare.len() < SPARE_ALT_STACKS_MAX {
            spare.push(std::mem::take(&mut self.buffer));
        }
    }
}

/// the program's first call: arm the stack overflow report for the main
/// thread. the code generator emits it at the top of `main`, since the
/// runtime is a static library with no startup hook of its own.
#[no_mangle]
pub extern "C" fn pith_runtime_start() {
    #[cfg(not(any(pith_asan, pith_tsan)))]
    IS_MAIN.with(|m| m.set(true));
    enter_thread();
}

/// arm the stack overflow report on the calling thread: install the process
/// handler on first use, give the thread an alternate signal stack, and
/// record its guard region. called once per thread that runs pith code: the
/// main thread, each green worker and each os-thread task.
pub fn enter_thread() {
    #[cfg(not(any(pith_asan, pith_tsan)))]
    {
        static INSTALL: std::sync::Once = std::sync::Once::new();
        INSTALL.call_once(install_handlers);
        ALT_STACK.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_some() {
                return;
            }
            THREAD_GUARD.with(|g| g.set(thread_guard()));
            *slot = install_alt_stack();
        });
    }
}

/// point SIGSEGV and SIGBUS at `on_fault`, on the alternate stack, keeping
/// what was there for faults that are not overflows.
#[cfg(not(any(pith_asan, pith_tsan)))]
fn install_handlers() {
    // SAFETY: plain libc calls on locals. `PREVIOUS` is written here, once,
    // as each handler goes in. in a program this runs from
    // `pith_runtime_start`, before main has started any other thread, so no
    // handler on another thread can read it mid-write.
    unsafe {
        let page = libc::sysconf(libc::_SC_PAGESIZE);
        if page > 0 {
            PAGE_SIZE.store(page as usize, Ordering::Relaxed);
        }
        for (i, &sig) in FAULT_SIGNALS.iter().enumerate() {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_fault as *const () as usize;
            libc::sigemptyset(&mut action.sa_mask);
            action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
            let mut previous: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(sig, &action, &mut previous) == 0 {
                (*std::ptr::addr_of_mut!(PREVIOUS))[i] = Some(previous);
            }
        }
    }
}

/// give this thread an alternate signal stack, unless it has one already (a
/// test harness thread, say), in which case that one serves. one syscall in
/// the common case: the call that installs the stack also returns the old one.
#[cfg(not(any(pith_asan, pith_tsan)))]
fn install_alt_stack() -> Option<AltStack> {
    // SAFETY: sets this thread's alternate stack, or puts back the one it
    // had. the buffer is kept in the thread-local slot until thread exit, and
    // `AltStack::drop` disables it first.
    unsafe {
        let spare = SPARE_ALT_STACKS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pop();
        let mut buffer = spare.unwrap_or_else(|| Vec::with_capacity(ALT_STACK_SIZE));
        let stack = libc::stack_t {
            ss_sp: buffer.as_mut_ptr().cast(),
            ss_flags: 0,
            ss_size: ALT_STACK_SIZE,
        };
        let mut previous: libc::stack_t = std::mem::zeroed();
        if libc::sigaltstack(&stack, &mut previous) != 0 {
            return None;
        }
        if previous.ss_flags & libc::SS_DISABLE == 0 {
            libc::sigaltstack(&previous, std::ptr::null_mut());
            return None;
        }
        Some(AltStack { buffer })
    }
}

/// the range of addresses below this thread's stack whose fault means an
/// overflow: the guard (or, on the main thread, the kernel's guard gap) and
/// `GUARD_SLACK` beyond it, plus the lowest usable page. `(0, 0)` when the
/// platform cannot say where the stack is.
#[cfg(not(any(pith_asan, pith_tsan)))]
fn thread_guard() -> (usize, usize) {
    let page = PAGE_SIZE.load(Ordering::Relaxed);
    #[cfg(target_os = "linux")]
    if IS_MAIN.with(|m| m.get()) {
        return main_thread_guard(page);
    }
    match thread_stack_low() {
        Some((low, guard)) if low > 0 => (low.saturating_sub(guard.max(GUARD_SLACK)), low + page),
        _ => (0, 0),
    }
}

/// the main thread's guard range on linux. `pthread_getattr_np` finds the
/// main stack by reading /proc/self/maps, which added about 40us, a tenth,
/// to the start of a small program, so this estimates from the stack limit
/// instead. the stack can grow down to its top less RLIMIT_STACK, and its top
/// sits above this frame by at most the arguments and environment, which
/// linux caps at a quarter of the limit. the range covers that window and
/// `GUARD_SLACK` below it. a fault in the window is past the end of the
/// stack, since above the real limit the kernel grows the stack rather than
/// fault. an unlimited stack has no end to report.
#[cfg(all(target_os = "linux", not(any(pith_asan, pith_tsan))))]
fn main_thread_guard(page: usize) -> (usize, usize) {
    // SAFETY: `getrlimit` fills a local.
    let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrlimit(libc::RLIMIT_STACK, &mut limit) } != 0
        || limit.rlim_cur == libc::RLIM_INFINITY
    {
        return (0, 0);
    }
    let size = limit.rlim_cur as usize;
    let here = &limit as *const libc::rlimit as usize;
    let low = here.saturating_sub(size);
    (low.saturating_sub(GUARD_SLACK), low + size / 4 + page)
}

/// the lowest usable address of this thread's stack and the size of the guard
/// below it.
#[cfg(all(target_os = "linux", not(any(pith_asan, pith_tsan))))]
fn thread_stack_low() -> Option<(usize, usize)> {
    // SAFETY: `pthread_getattr_np` fills an attribute object we own, which is
    // read and then destroyed.
    unsafe {
        let mut attr: libc::pthread_attr_t = std::mem::zeroed();
        if libc::pthread_getattr_np(libc::pthread_self(), &mut attr) != 0 {
            return None;
        }
        let mut addr: *mut libc::c_void = std::ptr::null_mut();
        let mut size = 0usize;
        let mut guard = 0usize;
        let ok = libc::pthread_attr_getstack(&attr, &mut addr, &mut size) == 0
            && libc::pthread_attr_getguardsize(&attr, &mut guard) == 0;
        libc::pthread_attr_destroy(&mut attr);
        ok.then_some((addr as usize, guard))
    }
}

#[cfg(all(target_os = "macos", not(any(pith_asan, pith_tsan))))]
fn thread_stack_low() -> Option<(usize, usize)> {
    // SAFETY: both calls only read the calling thread's own attributes.
    unsafe {
        let this = libc::pthread_self();
        let top = libc::pthread_get_stackaddr_np(this) as usize;
        let size = libc::pthread_get_stacksize_np(this);
        Some((top - size, PAGE_SIZE.load(Ordering::Relaxed)))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", pith_asan, pith_tsan)))]
fn thread_stack_low() -> Option<(usize, usize)> {
    None
}

/// the SIGSEGV and SIGBUS handler. see the module comment for what it
/// decides and why every call in it is async-signal-safe.
#[cfg(not(any(pith_asan, pith_tsan)))]
extern "C" fn on_fault(sig: libc::c_int, info: *mut libc::siginfo_t, _context: *mut libc::c_void) {
    // SAFETY: the kernel hands a valid siginfo to an SA_SIGINFO handler. a
    // positive si_code is a fault the kernel raised, the only kind whose
    // si_addr is an address.
    let (code, addr) = unsafe { ((*info).si_code, (*info).si_addr() as usize) };
    if code > 0 {
        let page = PAGE_SIZE.load(Ordering::Relaxed);
        if let Some((id, bottom)) = crate::concurrency::green::running_task_stack() {
            if bottom != 0 && addr >= bottom.saturating_sub(GUARD_SLACK) && addr < bottom + page {
                report(&[
                    b"stack overflow in task ",
                    digits(id as u64).as_bytes(),
                    b" (green task stacks are ",
                    digits((crate::concurrency::green::task_stack_size() / 1024) as u64).as_bytes(),
                    b" KiB; PITH_GREEN_STACK_KB sets the size)",
                ]);
            }
        }
        let (low, high) = THREAD_GUARD.with(|g| g.get());
        if addr >= low && addr < high {
            if IS_MAIN.with(|m| m.get()) {
                report(&[b"stack overflow in the main thread"]);
            }
            report(&[b"stack overflow in a task thread"]);
        }
    }
    // not an overflow: put back what was there and let the fault happen again.
    let index = if sig == libc::SIGSEGV { 0 } else { 1 };
    // SAFETY: `PREVIOUS` was written as this handler went in and is
    // never written again. `sigaction` and `raise` are async-signal-safe.
    unsafe {
        match (*std::ptr::addr_of!(PREVIOUS))[index] {
            Some(previous) => libc::sigaction(sig, &previous, std::ptr::null_mut()),
            None => libc::signal(sig, libc::SIG_DFL) as libc::c_int,
        };
        // a fault re-raises itself when the instruction runs again; a signal
        // someone sent does not, so send it again.
        if code <= 0 {
            libc::raise(sig);
        }
    }
}

/// write `pith runtime error: <parts>` and a newline to stderr, then exit
/// with `STACK_OVERFLOW_EXIT` without running atexit handlers or destructors,
/// none of which is safe from a signal.
#[cfg(not(any(pith_asan, pith_tsan)))]
fn report(parts: &[&[u8]]) -> ! {
    let write = |bytes: &[u8]| {
        // SAFETY: `write` is async-signal-safe and the slice is valid. a short
        // or failed write loses part of the message, which nothing can fix
        // from here.
        unsafe { libc::write(2, bytes.as_ptr().cast(), bytes.len()) };
    };
    write(b"pith runtime error: ");
    for part in parts {
        write(part);
    }
    write(b"\n");
    // SAFETY: `_exit` is async-signal-safe and ends the process.
    unsafe { libc::_exit(STACK_OVERFLOW_EXIT) }
}

/// a number's decimal digits in a fixed buffer, formatted without allocating.
#[cfg(not(any(pith_asan, pith_tsan)))]
struct Digits {
    buf: [u8; 20],
    start: usize,
}

#[cfg(not(any(pith_asan, pith_tsan)))]
impl Digits {
    fn as_bytes(&self) -> &[u8] {
        &self.buf[self.start..]
    }
}

#[cfg(not(any(pith_asan, pith_tsan)))]
fn digits(mut n: u64) -> Digits {
    let mut out = Digits {
        buf: [0; 20],
        start: 20,
    };
    loop {
        out.start -= 1;
        out.buf[out.start] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            return out;
        }
    }
}

#[cfg(test)]
mod tests {
    #![cfg(not(any(pith_asan, pith_tsan)))]
    use super::*;

    #[test]
    fn digits_format_without_allocating() {
        assert_eq!(digits(0).as_bytes(), b"0");
        assert_eq!(digits(7).as_bytes(), b"7");
        assert_eq!(digits(1024).as_bytes(), b"1024");
        assert_eq!(digits(u64::MAX).as_bytes(), b"18446744073709551615");
    }

    // a thread's guard sits just below the stack it runs on: a local's
    // address is above the range, and the range is not empty.
    #[test]
    fn a_thread_guard_lies_below_its_stack() {
        std::thread::spawn(|| {
            enter_thread();
            let (low, high) = THREAD_GUARD.with(|g| g.get());
            let local = 0u8;
            let here = &local as *const u8 as usize;
            assert!(low < high, "empty guard range");
            assert!(
                high <= here,
                "guard {low:#x}..{high:#x} not below {here:#x}"
            );
        })
        .join()
        .unwrap();
    }
}
