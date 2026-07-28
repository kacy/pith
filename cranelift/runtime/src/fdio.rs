//! reading and writing a pollable fd without parking a green worker.
//!
//! this is the shape sockets have used since the epoll reactor landed: put the
//! fd in non-blocking mode, run the syscall, and when it would block hand the
//! green task to the reactor (`netpoll`) instead of sleeping the worker OS
//! thread on it. the task resumes when the fd is ready and retries; the worker
//! ran everything else in the meantime.
//!
//! nothing here is socket-specific — it is all `read`, `write`, and readiness —
//! so sockets and child-process pipes share it. a regular file cannot use any
//! of it: epoll always reports a file ready no matter how slow its device is,
//! which is why file i/o goes to a thread pool (`blocking`) instead.
//!
//! every function works whichever backend is running. off a green task there is
//! no coroutine to suspend, so a would-block waits in `poll` — which is what
//! the blocking fd these calls replaced did anyway.

use crate::concurrency::green;
use crate::netpoll;
use std::os::unix::io::RawFd;

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
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
}

/// poll a single fd until it is ready, times out, or errors. the blocking wait
/// every non-green caller uses, and the whole story on platforms without the
/// epoll reactor (see `netpoll_fallback`).
pub(crate) fn poll_wait(fd: i64, events: i16, timeout_ms: i64) -> i64 {
    if fd <= 0 {
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
/// are not (the main thread, a non-green spawn, or the flag-off backend). returns
/// the tri-state readiness contract: `1` ready, `0` timed out, `-1` error.
///
/// this is the seam: the green branch never parks the worker OS thread; the
/// os-thread branch is an ordinary blocking `poll` and must not spin.
pub(crate) fn wait_ready(fd: i64, read: bool, timeout_ms: i64) -> i64 {
    if fd <= 0 {
        return -1;
    }
    match green::current_task() {
        Some(task) => netpoll::wait_io(fd as RawFd, read, timeout_ms, task),
        None => {
            let events = if read { libc::POLLIN } else { libc::POLLOUT };
            poll_wait(fd, events, timeout_ms)
        }
    }
}

/// read up to `size` bytes, yielding on would-block until data arrives, the
/// writer closes (returns an empty vec — real EOF), or a hard error (returns
/// `None`). mirrors the blocking path's contract: 0 bytes is EOF, not an error.
pub(crate) fn read_yielding(fd: i64, size: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; size];
    loop {
        // SAFETY: reading up to `size` bytes into a buffer we own; `fd` is owned
        // by pith for the duration of this call.
        let n = unsafe { libc::read(fd as i32, buf.as_mut_ptr() as *mut libc::c_void, size) };
        if n >= 0 {
            buf.truncate(n as usize);
            return Some(buf);
        }
        let err = errno();
        if is_would_block(err) {
            // nothing ready — yield to the reactor and retry when readable.
            if wait_ready(fd, true, -1) != 1 {
                return None;
            }
            continue;
        }
        if err == libc::EINTR {
            continue;
        }
        return None;
    }
}

/// write one buffer: `write()` once, yielding only if it would block with
/// nothing written yet. returns the bytes written (a partial count is returned
/// as-is — callers loop), or `0` for a real error or closed reader. it never
/// returns `0` for a would-block: `0` must mean closed/EOF only, so a
/// would-block waits and retries until at least one byte goes out.
pub(crate) fn write_yielding(fd: i64, data: &[u8]) -> i64 {
    if data.is_empty() {
        return 0;
    }
    loop {
        // SAFETY: writing `data.len()` bytes from a buffer we borrow; `fd` is
        // owned by pith for the duration of this call.
        let n = unsafe { libc::write(fd as i32, data.as_ptr() as *const libc::c_void, data.len()) };
        if n > 0 {
            return n as i64;
        }
        if n == 0 {
            // a zero-length write of non-empty data means the reader is gone.
            return 0;
        }
        let err = errno();
        if is_would_block(err) {
            if wait_ready(fd, false, -1) != 1 {
                return 0;
            }
            continue;
        }
        if err == libc::EINTR {
            continue;
        }
        return 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a pipe pair, non-blocking on both ends so the would-block arms run.
    fn pipe() -> (RawFd, RawFd) {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: pipe(2) fills the two-element array we pass it.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        set_nonblocking(fds[0]);
        set_nonblocking(fds[1]);
        (fds[0], fds[1])
    }

    fn close(fd: RawFd) {
        // SAFETY: closing an fd this test owns.
        unsafe { libc::close(fd) };
    }

    #[test]
    fn a_write_round_trips_to_the_read_end() {
        let (read_end, write_end) = pipe();
        assert_eq!(write_yielding(write_end as i64, b"pith"), 4);
        assert_eq!(read_yielding(read_end as i64, 16).unwrap(), b"pith");
        close(read_end);
        close(write_end);
    }

    #[test]
    fn a_closed_writer_reads_as_eof_not_an_error() {
        let (read_end, write_end) = pipe();
        close(write_end);
        // Some(empty) is EOF; None would be an error, which this is not.
        assert!(read_yielding(read_end as i64, 16).unwrap().is_empty());
        close(read_end);
    }

    #[test]
    fn a_bad_fd_reports_an_error_rather_than_eof() {
        assert!(read_yielding(-1, 16).is_none());
        assert_eq!(write_yielding(-1, b"x"), 0);
        assert_eq!(wait_ready(-1, true, 0), -1);
    }

    #[test]
    fn an_idle_fd_times_out_instead_of_reporting_ready() {
        let (read_end, write_end) = pipe();
        assert_eq!(wait_ready(read_end as i64, true, 0), 0);
        // once there is something to read it reports ready.
        assert_eq!(write_yielding(write_end as i64, b"x"), 1);
        assert_eq!(wait_ready(read_end as i64, true, 0), 1);
        close(read_end);
        close(write_end);
    }
}
