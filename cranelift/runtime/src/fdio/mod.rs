//! reading and writing a pollable fd without parking a green worker.
//!
//! put the fd in non-blocking mode, run the syscall, and when it would block
//! hand the green task to the reactor (`netpoll`) instead of sleeping the
//! worker OS thread on it. the task resumes when the fd is ready and retries;
//! the worker ran everything else in the meantime.
//!
//! sockets and child-process pipes share the readiness machinery and the retry
//! loops; they differ only in which syscall pair moves the bytes, which is what
//! `Channel` below picks. a regular file cannot use any of it: epoll always
//! reports a file ready no matter how slow its device is, which is why file i/o
//! goes to a thread pool (`blocking`) instead.
//!
//! every function works whichever backend is running. off a green task there is
//! no coroutine to suspend, so a would-block waits in `poll`, which behaves
//! like a blocking syscall would.
//!
//! a wait is bounded by the socket's own timeout option. the os-thread backend
//! gets that for free (its fd is blocking, so `SO_RCVTIMEO` bounds the syscall
//! in the kernel), and the retry loops here read the same option back and hand
//! it to the wait, so a read deadline means the same thing on either backend
//! instead of being inert on the one that is the default.
//!
//! a socket can also carry a fixed deadline (`pith_tcp_set_read_deadline`,
//! `pith_tcp_set_write_deadline`, kept in `fd_handle`). the socket option
//! bounds each call on its own, so a peer that sends one byte just inside it
//! keeps a read going forever; a deadline is an instant, so the time left
//! shrinks across calls. a wait takes whichever of the two ends first. on the
//! os-thread backend a socket with a deadline is driven by the same retry
//! loops, one `MSG_DONTWAIT` call at a time, since a blocking call in the
//! kernel knows only the socket option.

use std::os::unix::io::RawFd;
use std::time::{Duration, Instant};

/// the current errno as a plain int.
pub(crate) fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// would this syscall have blocked? (EAGAIN and EWOULDBLOCK are the same value on
/// linux, but check both for portability of intent.)
pub(crate) fn is_would_block(err: i32) -> bool {
    err == libc::EAGAIN || err == libc::EWOULDBLOCK
}

/// set `O_NONBLOCK` on a raw fd. a failure leaves the fd blocking, which is
/// still safe: the caller then blocks in the syscall rather than yielding.
pub(crate) fn set_nonblocking(fd: RawFd) {
    // SAFETY: plain fcntl on a valid fd we just created or accepted.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags >= 0 {
        // SAFETY: setting O_NONBLOCK on the same valid fd.
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    }
}

/// how many bytes a read asks for when the caller gives no limit.
pub(crate) const DEFAULT_CHUNK: usize = 4096;

/// upper bound for a single read chunk allocation (4 MiB), so a caller passing
/// an untrusted or huge `max_bytes` cannot trigger a single-shot OOM abort
/// before the kernel even answers. callers reading exact/all bytes loop.
pub(crate) const MAX_CHUNK: usize = 4 * 1024 * 1024;

/// the byte count one read asks for: `max_bytes` clamped to `MAX_CHUNK`, or
/// `DEFAULT_CHUNK` when it is not positive. shared by socket, pipe and file reads.
pub(crate) fn chunk_size(max_bytes: i64) -> usize {
    if max_bytes > 0 {
        (max_bytes as usize).min(MAX_CHUNK)
    } else {
        DEFAULT_CHUNK
    }
}

/// the longest wait anything here asks for, in milliseconds: a shade under 25
/// days.
///
/// `poll` takes its timeout as an `int`, and the reactor turns its timeout
/// into an `Instant` deadline, where a duration past what the clock can
/// represent overflows the addition rather than expressing anything a caller
/// meant. clamping is the right reading either way: at this length a wait and
/// a wait that never ends are the same wait.
pub(crate) const MAX_WAIT_MS: i64 = i32::MAX as i64;

/// compute `Instant::now() + ms` without panicking when `ms` is huge.
/// returns `None` for `ms <= 0` or if `Instant` overflows even after clamping.
pub(crate) fn deadline_after_ms(ms: i64) -> Option<Instant> {
    if ms <= 0 {
        return None;
    }
    let clamped = ms.min(MAX_WAIT_MS) as u64;
    Instant::now().checked_add(Duration::from_millis(clamped))
}

mod transfer;
mod wait;
#[cfg(test)]
mod tests;

pub(crate) use transfer::{
    pipe_read_blocking, pipe_write_blocking, read_yielding, socket_read_blocking,
    socket_read_yielding, socket_write_blocking, socket_write_yielding, write_yielding, WriteError,
};
pub(crate) use wait::{
    wait_guarded, wait_guarded_level, wait_ready, wait_ready_level, wait_ready_unguarded,
};
