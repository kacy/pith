//! Readiness waiting, deadline budgeting, and socket timeout caching.

use super::transfer::Channel;
use super::{deadline_after_ms, MAX_WAIT_MS};
use crate::concurrency::green;
use crate::fd_handle::Guard;
use crate::netpoll;
use std::os::unix::io::RawFd;
use std::time::Instant;

/// poll a single fd until it is ready, times out, or errors: the blocking wait
/// every non-green caller uses, and the whole story on platforms without the
/// epoll reactor (see `netpoll_fallback`). accepts any non-negative fd,
/// including 0; see `wait_ready_unguarded` for why the guarded and unguarded
/// waits are split.
pub(crate) fn poll_wait_any_fd(fd: i64, events: i16, timeout_ms: i64) -> i64 {
    if fd < 0 {
        return -1;
    }
    let mut poll_fd = libc::pollfd {
        fd: fd as i32,
        events,
        revents: 0,
    };
    let timeout = if timeout_ms < 0 {
        -1
    } else if timeout_ms > i32::MAX as i64 {
        i32::MAX
    } else {
        timeout_ms as i32
    };
    // the wait loop is a native window for the cycle collector: from here to
    // every return the thread reads only this stack frame and errno, never a
    // heap handle, so a stop-the-world counts it as stopped. the bracket
    // drops on return, where its exit side re-checks the stop request.
    let _native = crate::cycle::native_bracket();
    loop {
        // SAFETY: polling one `pollfd` we own for the length of the call.
        let status = unsafe { libc::poll(&mut poll_fd, 1, timeout) };
        if status > 0 {
            let revents = poll_fd.revents;
            if (revents & libc::POLLNVAL) != 0 || (revents & libc::POLLERR) != 0 {
                return -1;
            }
            if (revents & events) != 0 || (revents & libc::POLLHUP) != 0 {
                return 1;
            }
            return -1;
        }
        if status == 0 {
            return 0;
        }
        let kind = std::io::Error::last_os_error().kind();
        if kind == std::io::ErrorKind::Interrupted {
            continue;
        }
        return -1;
    }
}

/// wait for `fd` to become readable (`read`) or writable, yielding to the epoll
/// reactor when we are running inside a green task, or blocking on `poll` when we
/// are not (the main thread, or the os-thread backend). returns
/// the tri-state readiness contract: `1` ready, `0` timed out, `-1` error.
///
/// this is the seam: the green branch never parks the worker OS thread; the
/// os-thread branch is an ordinary blocking `poll` and must not spin.
///
/// this is the raw-fd wait, for descriptors that are not handles: a connect in
/// progress, a pidfd, the terminal. a call on a handle waits through
/// `wait_guarded`, which also notices the handle being closed under it.
pub(crate) fn wait_ready(fd: i64, read: bool, timeout_ms: i64) -> i64 {
    if fd <= 0 {
        return -1;
    }
    crate::perf_stats!(PERF_REACTOR_WAITS += 1);
    wait_ready_unguarded(fd, read, timeout_ms)
}

/// `wait_ready` on a handle's fd. a close that lands while the caller is parked
/// ends the wait with an error rather than a readiness: the reactor resolves a
/// parked waiter that way itself, and the `poll` branch (which the closer
/// wakes by shutting the socket down) asks the handle on the way out.
pub(crate) fn wait_guarded(guard: &Guard, read: bool, timeout_ms: i64) -> i64 {
    crate::perf_stats!(PERF_REACTOR_WAITS += 1);
    wait_with(guard.fd() as i64, read, timeout_ms, &|| guard.is_live())
}

/// `wait_guarded` for a caller that has not just seen its syscall would block,
/// such as `tcp_wait_readable`. the reactor wakes a task when the fd's readiness
/// changes, so a fd that is already ready would never wake it. a green caller
/// therefore asks the fd first and waits only when it is not ready now.
pub(crate) fn wait_guarded_level(guard: &Guard, read: bool, timeout_ms: i64) -> i64 {
    match probe(guard.fd() as i64, read, timeout_ms) {
        Some(1) if !guard.is_live() => -1,
        Some(ready) => ready,
        None => wait_guarded(guard, read, timeout_ms),
    }
}

/// `wait_ready_unguarded` for a caller that has not just seen its syscall
/// would block; see `wait_guarded_level`.
pub(crate) fn wait_ready_level(fd: i64, read: bool, timeout_ms: i64) -> i64 {
    probe(fd, read, timeout_ms).unwrap_or_else(|| wait_ready_unguarded(fd, read, timeout_ms))
}

/// a green task's answer without parking, when `poll` with no timeout gives
/// one: the fd is ready or failed, or it is not ready and the caller asked not
/// to wait. `None` means the caller must wait, which off a green task is
/// always the case, since that wait is a `poll` already.
pub(super) fn probe(fd: i64, read: bool, timeout_ms: i64) -> Option<i64> {
    green::current_task()?;
    let events = if read { libc::POLLIN } else { libc::POLLOUT };
    match poll_wait_any_fd(fd, events, 0) {
        0 if timeout_ms != 0 => None,
        ready => Some(ready),
    }
}

/// `wait_ready` without the `fd <= 0` guard, for the one caller that
/// legitimately waits on fd 0: the terminal builtins reading stdin. `0` is a
/// "no handle" sentinel throughout the socket code, so the guard stays on
/// `wait_ready` itself; this seam exists precisely so it never weakens.
pub(crate) fn wait_ready_unguarded(fd: i64, read: bool, timeout_ms: i64) -> i64 {
    wait_with(fd, read, timeout_ms, &|| true)
}

/// the wait both entry points share. `live` answers whether the thing being
/// waited on still exists: the reactor asks it once the waiter is registered
/// and before the task parks, and the `poll` branch asks it after `poll`
/// returns, so a close between the caller's own check and the wait cannot be
/// missed by either. a raw fd is always live.
pub(super) fn wait_with(fd: i64, read: bool, timeout_ms: i64, live: &dyn Fn() -> bool) -> i64 {
    if fd < 0 {
        return -1;
    }
    // a negative timeout is "wait forever" and passes through; a positive one is
    // clamped here rather than in each branch, so neither the reactor's deadline
    // arithmetic nor `poll`'s `int` sees a value it cannot hold.
    let timeout_ms = std::cmp::min(timeout_ms, MAX_WAIT_MS);
    match green::current_task() {
        Some(task) => netpoll::wait_io(fd as RawFd, read, timeout_ms, task, live),
        None => {
            let events = if read { libc::POLLIN } else { libc::POLLOUT };
            let ready = poll_wait_any_fd(fd, events, timeout_ms);
            if ready == 1 && !live() {
                return -1;
            }
            ready
        }
    }
}

/// read one of the socket timeout options off `fd` as milliseconds, returning
/// `-1` for "no deadline". that covers the option being unset, the fd not
/// being a socket, and the call failing for any other reason. `-1` is the safe
/// answer to all three: it is what every caller that never set a timeout wants.
///
/// a sub-millisecond timeout rounds up to 1 ms rather than down to 0, because
/// `0` means "do not wait" to `wait_ready` and would turn a 500 µs deadline into
/// an instant failure.
pub(super) fn socket_timeout_ms(fd: i64, option: libc::c_int) -> i64 {
    let mut tv = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut len = std::mem::size_of::<libc::timeval>() as libc::socklen_t;
    // SAFETY: getsockopt writing a `timeval` of exactly the length we declare,
    // on an fd pith owns for the duration of the call.
    let rc = unsafe {
        libc::getsockopt(
            fd as i32,
            libc::SOL_SOCKET,
            option,
            &mut tv as *mut libc::timeval as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 {
        return -1;
    }
    if tv.tv_sec == 0 && tv.tv_usec == 0 {
        return -1;
    }
    let ms = (tv.tv_sec as i64).saturating_mul(1000) + (tv.tv_usec as i64) / 1000;
    std::cmp::max(1, ms).min(MAX_WAIT_MS)
}
/// `channel.timeout_ms` for the fd `guard` holds, read from the kernel once
/// per handle and cached after that. `pith_tcp_set_timeout` forgets the cached
/// value, so the next wait reads the new one back.
pub(super) fn cached_timeout_ms(guard: &Guard, read: bool, channel: Channel) -> i64 {
    if !channel.is_socket() {
        return channel.timeout_ms(guard.fd() as i64, read);
    }
    let write = !read;
    if let Some(ms) = crate::fd_handle::cached_timeout(guard, write) {
        return ms;
    }
    let ms = channel.timeout_ms(guard.fd() as i64, read);
    crate::fd_handle::cache_timeout(guard, write, Some(ms));
    ms
}

/// the time one retry loop may spend waiting for its fd: asked for the first
/// time the loop would block, then spent down across the retries after it.
///
/// two properties, both of which the shape exists for.
///
/// the timeout is only ever asked for on the path that is about to park, so a
/// read that finds its bytes already there pays nothing for it. the extra
/// `getsockopt`, made once per handle (see `cached_timeout_ms`), lands exactly
/// where the caller was going to sleep anyway.
///
/// and a wake with nothing behind it cannot restart the clock. the caller asked
/// for one bounded read, not an unbounded series of bounded waits, so a retry
/// gets what is left of the original budget rather than a fresh copy of it,
/// which is also how `SO_RCVTIMEO` bounds the blocking read this mirrors.
#[derive(Clone, Copy)]
pub(super) enum WaitBudget {
    /// the loop has not blocked yet, so nothing has been asked for.
    Unasked,
    /// no deadline. a pipe, or a socket with no timeout set (most of them, and
    /// everything that never calls `set_timeout`).
    Forever,
    /// give up once this instant passes.
    Until(Instant),
}

impl WaitBudget {
    /// wait for `fd` to become ready within what is left of the budget,
    /// resolving the budget from the fd on the first call. returns
    /// `wait_ready`'s tri-state, with `0` also covering a budget already spent.
    pub(super) fn wait(&mut self, guard: &Guard, read: bool, channel: Channel) -> i64 {
        let resolved = match *self {
            WaitBudget::Unasked => {
                let per_call = match cached_timeout_ms(guard, read, channel) {
                    ms if ms > 0 => deadline_after_ms(ms),
                    _ => None,
                };
                // the socket's deadline, when it has one, is a fixed instant,
                // so whichever of it and the per-call timeout comes first wins.
                let deadline = crate::fd_handle::deadline(guard.fd(), !read);
                let budget = match (per_call, deadline) {
                    (Some(a), Some(b)) => WaitBudget::Until(a.min(b)),
                    (Some(at), None) | (None, Some(at)) => WaitBudget::Until(at),
                    (None, None) => WaitBudget::Forever,
                };
                *self = budget;
                budget
            }
            settled => settled,
        };
        let remaining = match resolved {
            WaitBudget::Until(at) => {
                let now = Instant::now();
                if at <= now {
                    return 0;
                }
                // round the remainder up to whole milliseconds: rounding down
                // would give up just before the deadline, and `0` would mean
                // "do not wait", which is a timeout the deadline has not reached.
                let nanos = at.duration_since(now).as_nanos();
                nanos.div_ceil(1_000_000).min(i64::MAX as u128) as i64
            }
            _ => -1,
        };
        wait_guarded(guard, read, remaining)
    }
}
