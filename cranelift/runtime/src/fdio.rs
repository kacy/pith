//! reading and writing a pollable fd without parking a green worker.
//!
//! this is the shape sockets have used since the epoll reactor landed: put the
//! fd in non-blocking mode, run the syscall, and when it would block hand the
//! green task to the reactor (`netpoll`) instead of sleeping the worker OS
//! thread on it. the task resumes when the fd is ready and retries; the worker
//! ran everything else in the meantime.
//!
//! sockets and child-process pipes share the readiness machinery and the retry
//! loops; they differ only in which syscall pair moves the bytes, which is what
//! `Channel` below picks. a regular file cannot use any of it: epoll always
//! reports a file ready no matter how slow its device is, which is why file i/o
//! goes to a thread pool (`blocking`) instead.
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

/// what kind of thing an fd refers to, which is to say which syscall pair moves
/// its bytes.
///
/// on a connected stream socket `recv`/`send` with no flags do exactly what
/// `read`/`write` do: the same bytes, the same short counts, the same errnos,
/// and the same SIGPIPE on a peer that hung up. the one difference is the whole
/// reason this distinction exists — they refuse a descriptor that is not a
/// socket, with `ENOTSOCK`, where `read`/`write` cheerfully talk to whatever the
/// number now refers to.
#[derive(Clone, Copy)]
enum Channel {
    /// a child process's pipe. `recv`/`send` reject a pipe outright, so pipes
    /// keep the general syscalls and give up the recycled-fd check with them.
    Pipe,
    /// a connected socket. takes `recv`/`send` so that a descriptor closed while
    /// a task was still using it — and then handed straight to the next `open`
    /// by the kernel, which reuses the lowest free number — fails loudly instead
    /// of reading someone else's file or writing over it.
    ///
    /// this catches recycling onto a *non-socket* only. a freed number that the
    /// next `accept` or `connect` takes is still a socket, so a stale task reads
    /// and writes the new connection with nothing to object — the descriptor is
    /// the right kind, just the wrong one. telling those apart needs a
    /// generation stamped on the fd, which this is not.
    Socket,
}

impl Channel {
    /// the flags argument for `recv`/`send`. always none: `MSG_NOSIGNAL` would
    /// suppress the SIGPIPE a `write` to a dead peer raises today, and
    /// `signals::ignore_sigpipe` documents that disposition as replaceable — a
    /// program that arms SIGPIPE through `pith_signal_notify` is entitled to
    /// keep seeing it. no flags keeps this change to the non-socket check.
    const NO_FLAGS: libc::c_int = 0;

    /// one read into `buf`, returning the raw syscall result.
    ///
    /// # Safety
    /// `buf` must be writable for `size` bytes and `fd` owned by pith for the
    /// length of the call.
    unsafe fn read(self, fd: i32, buf: *mut u8, size: usize) -> isize {
        let buf = buf as *mut libc::c_void;
        match self {
            Channel::Pipe => libc::read(fd, buf, size),
            Channel::Socket => libc::recv(fd, buf, size, Self::NO_FLAGS),
        }
    }

    /// one write from `data`, returning the raw syscall result.
    ///
    /// # Safety
    /// `data` must be readable for `size` bytes and `fd` owned by pith for the
    /// length of the call.
    unsafe fn write(self, fd: i32, data: *const u8, size: usize) -> isize {
        let data = data as *const libc::c_void;
        match self {
            Channel::Pipe => libc::write(fd, data, size),
            Channel::Socket => libc::send(fd, data, size, Self::NO_FLAGS),
        }
    }

    /// stop the process if `err` says this fd is not a socket. a no-op for a
    /// pipe, which never asks the question.
    fn check(self, fd: i64, err: i32, operation: &str) {
        if matches!(self, Channel::Socket) && err == libc::ENOTSOCK {
            report_not_a_socket(fd, operation);
        }
    }
}

/// abort with a diagnostic: an fd pith is using as a socket is not one.
///
/// this is not a runtime condition a caller could handle — it is the process
/// having lost track of one of its own descriptors — and handing it back as an
/// ordinary i/o error would put it straight back in the dark. every caller above
/// treats a failed read or write as "the connection is gone", closes, and moves
/// on, which is exactly the silence this check exists to break. so it stops
/// here, naming the fd and the operation, in the manner of the other runtime
/// invariant checks (`pith_cstring_substring` on a split character).
fn report_not_a_socket(fd: i64, operation: &str) -> ! {
    eprintln!(
        "pith runtime error: {} on fd {}, which is not a socket (ENOTSOCK)",
        operation, fd
    );
    eprintln!(
        "  either this descriptor was never a socket, or — the case worth \
         hunting — it was closed while a task was still using it and the kernel \
         handed the number to the next file the process opened. that read \
         returns another file's bytes and that write corrupts it, silently, \
         which is why this stops instead of reporting an i/o error."
    );
    eprintln!(
        "  close a connection only after awaiting every task that reads or \
         writes it."
    );
    // panic-guard: reading or writing a closed fd would corrupt whatever reused it; see the diagnostic above.
    std::process::exit(1);
}

/// read up to `size` bytes from a child's pipe. see `read_channel` for the
/// contract; a pipe gets no recycled-fd check, because the syscall that would
/// give us one rejects pipes.
pub(crate) fn read_yielding(fd: i64, size: usize) -> Option<Vec<u8>> {
    read_channel(fd, size, Channel::Pipe)
}

/// read up to `size` bytes from a socket, failing loudly if `fd` is no longer
/// one. the socket half of `read_yielding`.
pub(crate) fn socket_read_yielding(fd: i64, size: usize) -> Option<Vec<u8>> {
    read_channel(fd, size, Channel::Socket)
}

/// read up to `size` bytes, yielding on would-block until data arrives, the
/// writer closes (returns an empty vec — real EOF), or a hard error (returns
/// `None`). mirrors the blocking path's contract: 0 bytes is EOF, not an error.
fn read_channel(fd: i64, size: usize, channel: Channel) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; size];
    loop {
        // SAFETY: reading up to `size` bytes into a buffer we own; `fd` is owned
        // by pith for the duration of this call.
        let n = unsafe { channel.read(fd as i32, buf.as_mut_ptr(), size) };
        if n >= 0 {
            buf.truncate(n as usize);
            return Some(buf);
        }
        let err = errno();
        channel.check(fd, err, "read");
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

/// write one buffer to a child's pipe. see `write_channel` for the contract.
pub(crate) fn write_yielding(fd: i64, data: &[u8]) -> i64 {
    write_channel(fd, data, Channel::Pipe)
}

/// write one buffer to a socket, failing loudly if `fd` is no longer one. the
/// socket half of `write_yielding`, and the more valuable one: a write that
/// lands on a recycled descriptor overwrites whatever file inherited the number
/// with no error anywhere.
pub(crate) fn socket_write_yielding(fd: i64, data: &[u8]) -> i64 {
    write_channel(fd, data, Channel::Socket)
}

/// write one buffer: one write syscall, yielding only if it would block with
/// nothing written yet. returns the bytes written (a partial count is returned
/// as-is — callers loop), or `0` for a real error or closed reader. it never
/// returns `0` for a would-block: `0` must mean closed/EOF only, so a
/// would-block waits and retries until at least one byte goes out.
fn write_channel(fd: i64, data: &[u8], channel: Channel) -> i64 {
    if data.is_empty() {
        return 0;
    }
    loop {
        // SAFETY: writing `data.len()` bytes from a buffer we borrow; `fd` is
        // owned by pith for the duration of this call.
        let n = unsafe { channel.write(fd as i32, data.as_ptr(), data.len()) };
        if n > 0 {
            return n as i64;
        }
        if n == 0 {
            // a zero-length write of non-empty data means the reader is gone.
            return 0;
        }
        let err = errno();
        channel.check(fd, err, "write");
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

/// one read from a socket that blocks in the kernel: the os-thread backend,
/// where there is no green task to suspend and no reactor to hand the fd to.
///
/// a would-block is not a retry cue here. a blocking socket reports one only
/// when `SO_RCVTIMEO` expired — the timeout `pith_tcp_set_timeout` installs — so
/// it is a failure, which is what the `TcpStream::read` this replaced made of
/// it. `None` covers every error alike, as that did.
pub(crate) fn socket_read_blocking(fd: i64, size: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; size];
    // SAFETY: reading up to `size` bytes into a buffer we own; `fd` is owned by
    // pith for the duration of this call.
    let n = unsafe { Channel::Socket.read(fd as i32, buf.as_mut_ptr(), size) };
    if n >= 0 {
        buf.truncate(n as usize);
        return Some(buf);
    }
    Channel::Socket.check(fd, errno(), "read");
    None
}

/// one write to a socket that blocks in the kernel, the counterpart of
/// `socket_read_blocking`. returns the bytes accepted, or `0` for any error.
pub(crate) fn socket_write_blocking(fd: i64, data: &[u8]) -> i64 {
    // SAFETY: writing `data.len()` bytes from a buffer we borrow; `fd` is owned
    // by pith for the duration of this call.
    let n = unsafe { Channel::Socket.write(fd as i32, data.as_ptr(), data.len()) };
    if n >= 0 {
        return n as i64;
    }
    Channel::Socket.check(fd, errno(), "write");
    0
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

    /// a connected stream socket pair — the shape every `Channel::Socket` call
    /// site has. left blocking: these tests only read when something is there,
    /// and a would-block off a green task waits in `poll` forever.
    fn socket_pair() -> (RawFd, RawFd) {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: socketpair(2) fills the two-element array we pass it.
        let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(rc, 0);
        (fds[0], fds[1])
    }

    /// open a read-only fd on a regular file — a stand-in for the descriptor the
    /// kernel hands to the next `open` after a socket number is freed.
    fn open_a_file() -> RawFd {
        let path = c"/etc/hostname";
        // SAFETY: opening a path that exists on every linux host, read-only.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY) };
        assert!(fd >= 0);
        fd
    }

    fn close(fd: RawFd) {
        // SAFETY: closing an fd this test owns.
        unsafe { libc::close(fd) };
    }

    /// the raw errno from one socket read of `fd`, which must fail.
    fn socket_read_errno(fd: RawFd) -> i32 {
        let mut buf = [0u8; 16];
        // SAFETY: reading into a stack buffer of the length we pass.
        let n = unsafe { Channel::Socket.read(fd, buf.as_mut_ptr(), buf.len()) };
        assert_eq!(n, -1);
        errno()
    }

    /// the raw errno from one socket write to `fd`, which must fail.
    fn socket_write_errno(fd: RawFd) -> i32 {
        let data = b"x";
        // SAFETY: writing one byte from a buffer that outlives the call.
        let n = unsafe { Channel::Socket.write(fd, data.as_ptr(), data.len()) };
        assert_eq!(n, -1);
        errno()
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

    #[test]
    fn a_socket_moves_the_same_bytes_recv_and_send_as_read_and_write_did() {
        let (a, b) = socket_pair();
        assert_eq!(socket_write_yielding(a as i64, b"pith"), 4);
        assert_eq!(socket_read_yielding(b as i64, 16).unwrap(), b"pith");
        // and the blocking pair, which is the os-thread backend's path.
        assert_eq!(socket_write_blocking(b as i64, b"back"), 4);
        assert_eq!(socket_read_blocking(a as i64, 16).unwrap(), b"back");
        close(a);
        close(b);
    }

    #[test]
    fn a_socket_whose_peer_closed_reads_as_eof_not_an_error() {
        let (a, b) = socket_pair();
        close(a);
        // Some(empty) is EOF, matching what `read` reported here before.
        assert!(socket_read_yielding(b as i64, 16).unwrap().is_empty());
        assert!(socket_read_blocking(b as i64, 16).unwrap().is_empty());
        close(b);
    }

    /// the fact the whole change rests on: the socket syscalls refuse a
    /// descriptor that is not a socket, where `read`/`write` would have used it.
    #[test]
    fn the_socket_syscalls_reject_a_file_and_a_pipe_with_enotsock() {
        let file = open_a_file();
        assert_eq!(socket_read_errno(file), libc::ENOTSOCK);
        assert_eq!(socket_write_errno(file), libc::ENOTSOCK);
        close(file);

        let (read_end, write_end) = pipe();
        assert_eq!(socket_read_errno(read_end), libc::ENOTSOCK);
        assert_eq!(socket_write_errno(write_end), libc::ENOTSOCK);
        // which is exactly why a child process's pipe keeps `Channel::Pipe`:
        // the same two fds work fine through it.
        assert_eq!(write_yielding(write_end as i64, b"x"), 1);
        assert_eq!(read_yielding(read_end as i64, 16).unwrap(), b"x");
        close(read_end);
        close(write_end);
    }

    #[test]
    fn a_genuinely_closed_fd_is_ebadf_not_enotsock() {
        // a number nothing is open on: the socket calls report it the same way
        // `read`/`write` do, so only a *recycled* fd trips the new check.
        let (a, b) = socket_pair();
        close(a);
        close(b);
        assert_eq!(socket_read_errno(a), libc::EBADF);
        assert_eq!(socket_write_errno(a), libc::EBADF);
    }

    // -----------------------------------------------------------------------
    // the abort path.
    //
    // `report_not_a_socket` exits the process, so each case runs this same test
    // binary as a child with `PITH_FDIO_NOT_A_SOCKET` naming the call to make,
    // and the parent asserts on how the child died. re-running the binary rather
    // than forking keeps the child clear of the test harness's threads.
    // -----------------------------------------------------------------------

    /// which call the child should make on a non-socket fd.
    const ABORT_CHILD: &str = "PITH_FDIO_NOT_A_SOCKET";

    /// run this test binary again as a child, asking it for `call`, and return
    /// its exit code and stderr. `test_name` is the full `module::test` path the
    /// harness matches with `--exact`.
    fn abort_child(test_name: &str, call: &str) -> (Option<i32>, String) {
        let exe = std::env::current_exe().expect("test binary path");
        let output = std::process::Command::new(exe)
            .args([test_name, "--exact", "--nocapture"])
            .env(ABORT_CHILD, call)
            .output()
            .expect("re-run the test binary");
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        (output.status.code(), stderr)
    }

    /// in the child, make the requested call on a file descriptor that is not a
    /// socket. never returns — every arm is expected to abort.
    fn make_the_bad_call(call: &str) -> ! {
        let file = open_a_file() as i64;
        match call {
            "read_yielding" => {
                socket_read_yielding(file, 16);
            }
            "write_yielding" => {
                socket_write_yielding(file, b"x");
            }
            "read_blocking" => {
                socket_read_blocking(file, 16);
            }
            "write_blocking" => {
                socket_write_blocking(file, b"x");
            }
            other => panic!("unknown call {other}"),
        }
        panic!("{call} on a non-socket returned instead of stopping the process");
    }

    /// the child half of every abort case: `Some` when we are the child.
    fn requested_call() -> Option<String> {
        std::env::var(ABORT_CHILD).ok()
    }

    #[test]
    fn a_socket_read_on_a_recycled_fd_stops_the_process() {
        if let Some(call) = requested_call() {
            make_the_bad_call(&call);
        }
        for call in ["read_yielding", "read_blocking"] {
            let (code, stderr) = abort_child(
                "fdio::tests::a_socket_read_on_a_recycled_fd_stops_the_process",
                call,
            );
            assert_eq!(code, Some(1), "{call} stderr: {stderr}");
            assert!(
                stderr.contains("read on fd") && stderr.contains("not a socket (ENOTSOCK)"),
                "{call} stderr: {stderr}"
            );
        }
    }

    #[test]
    fn a_socket_write_on_a_recycled_fd_stops_the_process() {
        if let Some(call) = requested_call() {
            make_the_bad_call(&call);
        }
        for call in ["write_yielding", "write_blocking"] {
            let (code, stderr) = abort_child(
                "fdio::tests::a_socket_write_on_a_recycled_fd_stops_the_process",
                call,
            );
            assert_eq!(code, Some(1), "{call} stderr: {stderr}");
            assert!(
                stderr.contains("write on fd") && stderr.contains("not a socket (ENOTSOCK)"),
                "{call} stderr: {stderr}"
            );
        }
    }
}
