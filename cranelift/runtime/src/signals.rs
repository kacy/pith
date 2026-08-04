//! posix signal delivery, turned into an ordinary readable fd.
//!
//! a server that cannot hear SIGTERM cannot be deployed: every rolling update
//! kills it mid-request. this module is the plumbing that lets pith code wait
//! for a signal the same way it waits for a socket — which, under the green
//! backend, means parking the task rather than its worker.
//!
//! ## why a self-pipe
//!
//! a signal handler runs on whatever thread the kernel picks, at whatever
//! instruction it was executing. only async-signal-safe functions may be called
//! from one: no allocation, no locks, nothing that touches the pith runtime
//! (which allocates and locks nearly everywhere). so the handler here does
//! exactly one thing — `write()` a single byte, the signal number, into a pipe —
//! and every ounce of real work happens on the reading side, in normal code.
//!
//! `signalfd` would also work on linux, but it reads signals pending for the
//! *calling thread* plus the process, which only behaves under an inherited
//! `pthread_sigmask` in every thread. the green worker pool and the netpoll
//! reactor thread start lazily, so there is no single point at which we could
//! set that mask for all of them. a `sigaction` handler is process-wide by
//! construction and has no such ordering requirement, and its read end is a
//! plain pipe fd that works on every unix — so the fallback platforms get the
//! same semantics rather than a second implementation.
//!
//! ## how a waiting task parks
//!
//! the read end is non-blocking. `pith_signal_wait` tries a one-byte read; on
//! `EWOULDBLOCK` it calls `fdio::wait_ready`, which is the same seam every
//! socket read goes through: inside a green task it registers the fd with the
//! epoll reactor and suspends the coroutine, so the worker OS thread runs other
//! tasks; outside one it blocks on `poll`. either way there is no spin.
//!
//! ## what the handler must not do
//!
//! besides the write, it saves and restores `errno`. a handler that returns
//! with `errno` clobbered corrupts the interrupted code's error check — the
//! classic version of this bug turns an unrelated `read` into a spurious
//! failure. `SA_RESTART` is set so the runtime's in-flight blocking syscalls
//! resume instead of failing with `EINTR`.

use crate::fdio;
use crate::{ensure_perf_stats_registered, perf_count, PERF_SIGNAL_DELIVERIES, PERF_SIGNAL_WAITS};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Once;

/// the write end of the self-pipe, published for the signal handler to read.
/// an atomic (not a `OnceLock`) because the handler may only touch things that
/// are async-signal-safe, and a relaxed atomic load is; a lock is not. `-1`
/// means the pipe is not built yet, in which case the handler does nothing.
static PIPE_WRITE_FD: AtomicI32 = AtomicI32::new(-1);

/// the read end, used only by `pith_signal_wait` on ordinary threads.
static PIPE_READ_FD: AtomicI32 = AtomicI32::new(-1);

/// the highest signal number this module will arm. signal numbers are encoded
/// as bit positions in the `mask` argument, and the mask is an i64, so bit 63 is
/// the ceiling. every standard signal (SIGTERM is 15, the realtime range starts
/// at 34) that a shutdown path cares about is far below it.
const MAX_SIGNAL: i64 = 63;

/// the signal handler. runs on an arbitrary thread at an arbitrary instruction,
/// so its entire body must be async-signal-safe: one relaxed atomic load, one
/// `write`, and the `errno` save/restore around it. nothing here allocates,
/// locks, or calls into the pith runtime.
///
/// a full pipe (`EAGAIN`) drops the byte, which is correct: the pipe holds
/// thousands of pending bytes, and a reader that is that far behind has already
/// been told to shut down. losing the 4096th duplicate SIGTERM changes nothing.
extern "C" fn handle_signal(sig: libc::c_int) {
    let fd = PIPE_WRITE_FD.load(Ordering::Relaxed);
    if fd < 0 {
        return;
    }
    // SAFETY: `__errno_location` is async-signal-safe and the pointer it
    // returns is valid for this thread for the length of the handler.
    let saved_errno = unsafe { *libc::__errno_location() };
    let byte = sig as u8;
    // SAFETY: a one-byte write to our own non-blocking pipe write end. `write`
    // is async-signal-safe; the return value is deliberately ignored (see the
    // doc comment on a full pipe).
    unsafe {
        libc::write(fd, &byte as *const u8 as *const libc::c_void, 1);
        *libc::__errno_location() = saved_errno;
    }
}

/// build the self-pipe exactly once. both ends are `O_CLOEXEC` (a child process
/// has no business inheriting our signal queue) and `O_NONBLOCK` (the handler
/// must never block, and the reader drives its own waiting through
/// `fdio::wait_ready`). returns false if the pipe could not be created.
fn ensure_pipe() -> bool {
    static PIPE_ONCE: Once = Once::new();
    PIPE_ONCE.call_once(|| {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `pipe2` fills the two-element array we own with the read and
        // write ends; the return value is checked before either is published.
        let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) };
        if rc != 0 {
            return;
        }
        PIPE_READ_FD.store(fds[0], Ordering::Release);
        // publish the write end last: the handler reads only this one, so until
        // it is set no signal can reach a half-built pipe.
        PIPE_WRITE_FD.store(fds[1], Ordering::Release);
    });
    PIPE_READ_FD.load(Ordering::Acquire) >= 0
}

/// install `handle_signal` as the disposition for `sig`. returns false if the
/// kernel rejects it — SIGKILL and SIGSTOP cannot be caught, and a signal
/// number outside the platform's range is invalid.
fn install_handler(sig: libc::c_int) -> bool {
    // SAFETY: `sigaction` is zero-initialized then filled with a valid handler,
    // an empty block mask, and SA_RESTART. `sigemptyset` and `sigaction` are
    // plain libc calls on stack-local storage we own.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = handle_signal as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        // SA_RESTART so a signal does not turn the runtime's in-flight reads,
        // writes, and epoll waits into spurious EINTR failures.
        action.sa_flags = libc::SA_RESTART;
        libc::sigaction(sig, &action, std::ptr::null_mut()) == 0
    }
}

/// start queueing the signals named by `mask` — a bitmask where bit N selects
/// signal N, so SIGTERM (15) is `1 << 15`. safe to call more than once and from
/// more than one place; installing a handler twice is idempotent.
///
/// returns the number of signals armed, or `-1` if the mask names nothing, names
/// a signal that cannot be caught, or the self-pipe could not be built. a
/// partial arm is reported as failure rather than silently delivering a subset:
/// a server that believes it is listening for SIGTERM and is not would drop
/// every request of its final deploy.
///
/// arming a signal replaces its default disposition. after this call SIGTERM no
/// longer terminates the process on its own — something must wait for it and
/// act. that is the point, and it is also the footgun; `std.signal` documents it.
#[no_mangle]
pub extern "C" fn pith_signal_notify(mask: i64) -> i64 {
    if mask <= 0 {
        return -1;
    }
    if !ensure_pipe() {
        return -1;
    }
    let mut armed = 0;
    for sig in 1..=MAX_SIGNAL {
        if mask & (1 << sig) == 0 {
            continue;
        }
        if !install_handler(sig as libc::c_int) {
            return -1;
        }
        armed += 1;
    }
    if armed == 0 {
        return -1;
    }
    armed
}

/// return the next queued signal number, `0` if `timeout_ms` elapsed first, or
/// `-1` on error (including a wait with nothing armed — waiting for a signal
/// that can never arrive is a bug worth reporting, not a silent forever-block).
///
/// `timeout_ms < 0` waits indefinitely. inside a green task the wait parks the
/// task on the epoll reactor and leaves its worker free; outside one it blocks
/// the calling thread on `poll`.
#[no_mangle]
pub extern "C" fn pith_signal_wait(timeout_ms: i64) -> i64 {
    let fd = PIPE_READ_FD.load(Ordering::Acquire);
    if fd < 0 {
        return -1;
    }
    ensure_perf_stats_registered();
    perf_count(&PERF_SIGNAL_WAITS, 1);
    loop {
        let mut byte = 0u8;
        // SAFETY: reading one byte from our own non-blocking pipe read end into
        // a local. a short read is impossible for a one-byte request.
        let n = unsafe { libc::read(fd, &mut byte as *mut u8 as *mut libc::c_void, 1) };
        if n == 1 {
            perf_count(&PERF_SIGNAL_DELIVERIES, 1);
            return byte as i64;
        }
        if n == 0 {
            // the write end is process-global and never closed, so EOF here
            // cannot happen; report it rather than spinning on it.
            return -1;
        }
        let err = fdio::errno();
        if err == libc::EINTR {
            continue;
        }
        if !fdio::is_would_block(err) {
            return -1;
        }
        // nothing queued: park until the pipe is readable or the deadline
        // passes. this is the same seam socket reads use — green tasks suspend
        // their coroutine here, os threads block on poll, neither spins.
        match fdio::wait_ready(fd as i64, true, timeout_ms) {
            1 => continue,
            0 => return 0,
            _ => return -1,
        }
    }
}

/// send `sig` to this process. process-directed (`kill`, not `raise`) so it
/// behaves exactly like the signal an orchestrator sends, rather than being
/// pinned to the calling thread. returns 1 on success, 0 on failure.
///
/// this exists so a signal path can be tested honestly: a test that signals
/// itself exercises the real handler, the real pipe, and the real wait.
#[no_mangle]
pub extern "C" fn pith_signal_raise(sig: i64) -> i64 {
    if sig <= 0 || sig > MAX_SIGNAL {
        return 0;
    }
    // SAFETY: `getpid` and `kill` are plain libc calls; the signal number was
    // just range-checked.
    let rc = unsafe { libc::kill(libc::getpid(), sig as libc::c_int) };
    if rc == 0 {
        1
    } else {
        0
    }
}

/// this process's id.
#[no_mangle]
pub extern "C" fn pith_getpid() -> i64 {
    // SAFETY: `getpid` takes no arguments and cannot fail.
    unsafe { libc::getpid() as i64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    // the signal numbers std.signal hardcodes must match this platform's. they
    // are identical across every posix system in practice, but a pith constant
    // that silently named the wrong signal would arm the wrong handler and let
    // SIGTERM keep killing the process mid-request.
    #[test]
    fn std_signal_constants_match_libc() {
        assert_eq!(libc::SIGHUP, 1);
        assert_eq!(libc::SIGINT, 2);
        assert_eq!(libc::SIGQUIT, 3);
        assert_eq!(libc::SIGTERM, 15);
    }

    // an empty or negative mask names no signal to wait for. returning a handle
    // anyway would hand back a queue nothing can ever arrive on.
    #[test]
    fn notify_rejects_an_empty_mask() {
        assert_eq!(pith_signal_notify(0), -1);
        assert_eq!(pith_signal_notify(-1), -1);
    }

    // a signal that cannot be caught fails the whole arm rather than arming the
    // catchable half and reporting success.
    #[test]
    fn notify_rejects_an_uncatchable_signal() {
        assert_eq!(pith_signal_notify(1 << libc::SIGKILL), -1);
    }

    // out-of-range signal numbers are rejected before reaching `kill`.
    #[test]
    fn raise_rejects_out_of_range_signals() {
        assert_eq!(pith_signal_raise(0), 0);
        assert_eq!(pith_signal_raise(-3), 0);
        assert_eq!(pith_signal_raise(MAX_SIGNAL + 1), 0);
    }

    #[test]
    fn getpid_matches_libc() {
        assert_eq!(pith_getpid(), unsafe { libc::getpid() } as i64);
    }

    // the timeout case and the delivery case share one queue, so they are one
    // test rather than two. the self-pipe and the process's signal dispositions
    // are process-global by construction, while cargo runs each `#[test]` on its
    // own thread of a single process — as two tests, the delivery test's SIGUSR1
    // was read by the timeout test's wait, and both failed. sequencing them here
    // is the only ordering the shared queue actually permits.
    #[test]
    fn a_wait_times_out_when_idle_and_reports_a_raised_signal() {
        // nothing queued yet: the wait reports a timeout — distinct from a
        // delivery (a positive signal number) and from an error (-1), so a drain
        // loop can tell "no signal yet" from "this wait will never work".
        assert_eq!(pith_signal_notify(1 << libc::SIGUSR2), 1);
        assert_eq!(pith_signal_wait(10), 0);

        // the round trip, end to end on a real thread: arm SIGUSR1, send it to
        // this process, and read it back off the pipe. SIGUSR1 is used (not
        // SIGTERM) so a stray delivery cannot kill the test process, and the
        // wait carries a generous timeout so a slow ci box reports a real
        // failure rather than hanging the suite.
        assert_eq!(pith_signal_notify(1 << libc::SIGUSR1), 1);
        assert_eq!(pith_signal_raise(libc::SIGUSR1 as i64), 1);
        assert_eq!(pith_signal_wait(5000), libc::SIGUSR1 as i64);
    }
}
