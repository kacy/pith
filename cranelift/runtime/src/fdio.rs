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
//!
//! a wait is bounded by the socket's own timeout option. the os-thread backend
//! gets that for free — its fd is blocking, so `SO_RCVTIMEO` bounds the syscall
//! in the kernel — and the retry loops here read the same option back and hand
//! it to the wait, so a read deadline means the same thing on either backend
//! instead of being inert on the one that is the default.

use crate::concurrency::green;
use crate::fd_handle::Guard;
use crate::netpoll;
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
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
}

/// the longest wait anything here asks for, in milliseconds — a shade under 25
/// days.
///
/// `poll` takes its timeout as an `int` and has always been clamped to this, and
/// the reactor turns its timeout into an `Instant` deadline, where a duration
/// past what the clock can represent overflows the addition rather than
/// expressing anything a caller meant. clamping is the honest reading either
/// way: at this length a wait and a wait that never ends are the same wait.
const MAX_WAIT_MS: i64 = i32::MAX as i64;

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
/// are not (the main thread, a non-green spawn, or the flag-off backend). returns
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
/// parked waiter that way itself, and the `poll` branch — which the closer
/// wakes by shutting the socket down — asks the handle on the way out.
pub(crate) fn wait_guarded(guard: &Guard, read: bool, timeout_ms: i64) -> i64 {
    crate::perf_stats!(PERF_REACTOR_WAITS += 1);
    wait_with(guard.fd() as i64, read, timeout_ms, &|| guard.is_live())
}

/// `wait_ready` without the `fd <= 0` guard, for the one caller that
/// legitimately waits on fd 0: the terminal builtins reading stdin. `0` is a
/// "no handle" sentinel throughout the socket code, so the guard stays on
/// `wait_ready` itself — this seam exists precisely so it never weakens.
pub(crate) fn wait_ready_unguarded(fd: i64, read: bool, timeout_ms: i64) -> i64 {
    wait_with(fd, read, timeout_ms, &|| true)
}

/// the wait both entry points share. `live` answers whether the thing being
/// waited on still exists: the reactor asks it once the waiter is registered
/// and before the task parks, and the `poll` branch asks it after `poll`
/// returns, so a close between the caller's own check and the wait cannot be
/// missed by either. a raw fd is always live.
fn wait_with(fd: i64, read: bool, timeout_ms: i64, live: &dyn Fn() -> bool) -> i64 {
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
/// `-1` for "no deadline" — which covers the option being unset, the fd not
/// being a socket, and the call failing for any other reason. `-1` is the safe
/// answer to all three: it is what every caller that never set a timeout wants,
/// and it is what this path did before it asked at all.
///
/// a sub-millisecond timeout rounds up to 1 ms rather than down to 0, because
/// `0` means "do not wait" to `wait_ready` and would turn a 500 µs deadline into
/// an instant failure.
fn socket_timeout_ms(fd: i64, option: libc::c_int) -> i64 {
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

/// what kind of thing an fd refers to, which is to say which syscall pair moves
/// its bytes.
///
/// on a connected stream socket `recv`/`send` with no flags do exactly what
/// `read`/`write` do: the same bytes, the same short counts, the same errnos,
/// and the same SIGPIPE on a peer that hung up. the one difference is that
/// they refuse a descriptor that is not a socket, with `ENOTSOCK`, where
/// `read`/`write` cheerfully talk to whatever the number refers to.
///
/// that refusal used to be the only defence against a recycled descriptor.
/// the handle every call now holds (`fd_handle`) keeps the number reserved for
/// the length of the call and refuses a stale handle before any syscall, so an
/// `ENOTSOCK` here can no longer come from recycling; it is kept as an
/// invariant check, and it stops the process because reaching it would mean
/// the bookkeeping had lost a descriptor.
#[derive(Clone, Copy)]
enum Channel {
    /// a child process's pipe. `recv`/`send` reject a pipe outright, so pipes
    /// keep the general syscalls.
    Pipe,
    /// a connected socket, read and written with `recv`/`send`.
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

    /// how long a would-block wait on this fd may last, in milliseconds, or
    /// `-1` to wait forever.
    ///
    /// the kernel is the source of truth. `pith_tcp_set_timeout` writes
    /// `SO_RCVTIMEO`, and the os-thread backend's blocking read has always got
    /// its deadline by handing the syscall over to the kernel that holds it, so
    /// reading it back here is the same number by construction rather than a
    /// second copy of it that has to be kept in step.
    ///
    /// it also keeps a deadline with its socket. the kernel gives a closed
    /// fd's number straight to the next `open`, and clears the socket options
    /// with it; a `fd -> deadline` table in the runtime would instead hand a
    /// fresh connection the timeout of the one that used to hold its number,
    /// silently, and only under load.
    fn timeout_ms(self, fd: i64, read: bool) -> i64 {
        match self {
            // `SO_RCVTIMEO` is a socket option and a pipe is not a socket:
            // there is nothing to read, and nothing sets a deadline on a child
            // process's stdout in the first place. a pipe read waits for its
            // writer, as it always has.
            Channel::Pipe => -1,
            Channel::Socket => socket_timeout_ms(
                fd,
                if read {
                    libc::SO_RCVTIMEO
                } else {
                    libc::SO_SNDTIMEO
                },
            ),
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

/// the time one retry loop may spend waiting for its fd: asked for the first
/// time the loop would block, then spent down across the retries after it.
///
/// two properties, both of which the shape exists for.
///
/// the timeout is only ever asked for on the path that is about to park, so a
/// read that finds its bytes already there pays nothing for it. the extra
/// `getsockopt` lands exactly where the caller was going to sleep anyway.
///
/// and a wake with nothing behind it cannot restart the clock. the caller asked
/// for one bounded read, not an unbounded series of bounded waits, so a retry
/// gets what is left of the original budget rather than a fresh copy of it —
/// which is also how `SO_RCVTIMEO` bounds the blocking read this mirrors.
#[derive(Clone, Copy)]
enum WaitBudget {
    /// the loop has not blocked yet, so nothing has been asked for.
    Unasked,
    /// no deadline. a pipe, or a socket with no timeout set — which is most of
    /// them, and everything that never calls `set_timeout`.
    Forever,
    /// give up once this instant passes.
    Until(Instant),
}

impl WaitBudget {
    /// wait for `fd` to become ready within what is left of the budget,
    /// resolving the budget from the fd on the first call. returns
    /// `wait_ready`'s tri-state, with `0` also covering a budget already spent.
    fn wait(&mut self, guard: &Guard, read: bool, channel: Channel) -> i64 {
        let resolved = match *self {
            WaitBudget::Unasked => {
                let budget = match channel.timeout_ms(guard.fd() as i64, read) {
                    ms if ms > 0 => {
                        WaitBudget::Until(Instant::now() + Duration::from_millis(ms as u64))
                    }
                    _ => WaitBudget::Forever,
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
                // round any sub-millisecond remainder up: `0` would mean "do not
                // wait", which is a timeout the deadline has not reached yet.
                let left = at.duration_since(now).as_millis().min(i64::MAX as u128) as i64;
                std::cmp::max(1, left)
            }
            _ => -1,
        };
        wait_guarded(guard, read, remaining)
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
        "  the runtime has lost track of one of its own descriptors: a socket \
         handle resolved to a number that is not a socket. a read here would \
         return another file's bytes and a write would corrupt it, silently, \
         which is why this stops instead of reporting an i/o error."
    );
    // panic-guard: reading or writing a closed fd would corrupt whatever reused it; see the diagnostic above.
    std::process::exit(1);
}

/// read up to `size` bytes from a child's pipe. see `read_channel` for the
/// contract.
pub(crate) fn read_yielding(guard: &Guard, size: usize) -> Option<Vec<u8>> {
    read_channel(guard, size, Channel::Pipe)
}

/// read up to `size` bytes from a socket. the socket half of `read_yielding`.
pub(crate) fn socket_read_yielding(guard: &Guard, size: usize) -> Option<Vec<u8>> {
    read_channel(guard, size, Channel::Socket)
}

/// read up to `size` bytes, yielding on would-block until data arrives, the
/// socket's read deadline expires, the writer closes (returns an empty vec —
/// real EOF), or a hard error (returns `None`). mirrors the blocking path's
/// contract exactly: 0 bytes is EOF, not an error, and an expired deadline is
/// `None`, which is what `socket_read_blocking` makes of the `EAGAIN` its
/// `SO_RCVTIMEO` produces.
///
/// the guard is what makes `fd` the right descriptor for the whole call, parks
/// included. an end of stream is reported as an error when the handle was
/// closed while the call was inside: that is the closer shutting the socket
/// down to return this call, not the peer finishing.
fn read_channel(guard: &Guard, size: usize, channel: Channel) -> Option<Vec<u8>> {
    let fd = guard.fd() as i64;
    let mut buf = vec![0u8; size];
    let mut budget = WaitBudget::Unasked;
    loop {
        // SAFETY: reading up to `size` bytes into a buffer we own; `fd` is held
        // open by `guard` for the duration of this call.
        let n = unsafe { channel.read(fd as i32, buf.as_mut_ptr(), size) };
        if n >= 0 {
            if n == 0 && !guard.is_live() {
                return None;
            }
            if matches!(channel, Channel::Socket) {
                crate::perf_stats!(PERF_SOCK_READS += 1, PERF_SOCK_READ_BYTES += n as usize);
            }
            buf.truncate(n as usize);
            return Some(buf);
        }
        let err = errno();
        channel.check(fd, err, "read");
        if is_would_block(err) {
            // nothing ready — yield to the reactor and retry when readable, or
            // give up once the socket's read deadline runs out.
            if budget.wait(guard, true, channel) != 1 {
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
pub(crate) fn write_yielding(guard: &Guard, data: &[u8]) -> i64 {
    write_channel(guard, data, Channel::Pipe)
}

/// write one buffer to a socket. the socket half of `write_yielding`.
pub(crate) fn socket_write_yielding(guard: &Guard, data: &[u8]) -> i64 {
    write_channel(guard, data, Channel::Socket)
}

/// write one buffer: one write syscall, yielding only if it would block with
/// nothing written yet. returns the bytes written (a partial count is returned
/// as-is — callers loop), or `0` for a real error, a closed reader, or a send
/// deadline that expired — which is again what the blocking path reports for
/// the `EAGAIN` an expired `SO_SNDTIMEO` gives it. it never returns `0` for an
/// ordinary would-block: `0` must mean closed/EOF only, so a would-block waits
/// and retries until at least one byte goes out.
///
/// nothing in pith sets `SO_SNDTIMEO` today, so this budget resolves to "wait
/// forever" on every socket the runtime has. it is read anyway so that the two
/// directions stay the same shape: a send deadline, if one is ever set, must
/// bound a green write exactly as it already bounds an os-thread one.
fn write_channel(guard: &Guard, data: &[u8], channel: Channel) -> i64 {
    if data.is_empty() {
        return 0;
    }
    let fd = guard.fd() as i64;
    let mut budget = WaitBudget::Unasked;
    loop {
        // SAFETY: writing `data.len()` bytes from a buffer we borrow; `fd` is
        // held open by `guard` for the duration of this call.
        let n = unsafe { channel.write(fd as i32, data.as_ptr(), data.len()) };
        if n > 0 {
            if matches!(channel, Channel::Socket) {
                crate::perf_stats!(PERF_SOCK_WRITES += 1, PERF_SOCK_WRITE_BYTES += n as usize);
            }
            return n as i64;
        }
        if n == 0 {
            // a zero-length write of non-empty data means the reader is gone.
            return 0;
        }
        let err = errno();
        channel.check(fd, err, "write");
        if is_would_block(err) {
            if budget.wait(guard, false, channel) != 1 {
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
///
/// a close from another task returns this call by shutting the socket down,
/// which reads as end of stream; the handle being dead by then is what tells
/// that apart from the peer finishing, and it is reported as an error.
pub(crate) fn socket_read_blocking(guard: &Guard, size: usize) -> Option<Vec<u8>> {
    let fd = guard.fd() as i64;
    let mut buf = vec![0u8; size];
    // the syscall can sit in the kernel up to SO_RCVTIMEO — forever, with no
    // timeout set — touching only this owned buffer, so it is a native window
    // for the cycle collector. the bracket is kept to the syscall itself.
    let n = {
        let _native = crate::cycle::native_bracket();
        // SAFETY: reading up to `size` bytes into a buffer we own; `fd` is
        // held open by `guard` for the duration of this call.
        unsafe { Channel::Socket.read(fd as i32, buf.as_mut_ptr(), size) }
    };
    if n >= 0 {
        if n == 0 && !guard.is_live() {
            return None;
        }
        buf.truncate(n as usize);
        return Some(buf);
    }
    Channel::Socket.check(fd, errno(), "read");
    None
}

/// one write to a socket that blocks in the kernel, the counterpart of
/// `socket_read_blocking`. returns the bytes accepted, or `0` for any error.
pub(crate) fn socket_write_blocking(guard: &Guard, data: &[u8]) -> i64 {
    let fd = guard.fd() as i64;
    // a full send buffer can hold this in the kernel indefinitely; like the
    // blocking read it touches only the borrowed buffer, so it is a native
    // window for the cycle collector, kept to the syscall itself.
    let n = {
        let _native = crate::cycle::native_bracket();
        // SAFETY: writing `data.len()` bytes from a buffer we borrow; `fd` is
        // held open by `guard` for the duration of this call.
        unsafe { Channel::Socket.write(fd as i32, data.as_ptr(), data.len()) }
    };
    if n >= 0 {
        return n as i64;
    }
    Channel::Socket.check(fd, errno(), "write");
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fd_handle::{self, FdKind};

    /// a hold on `fd` as a socket handle, the shape every socket call site
    /// passes in. each call registers the number afresh, which is what a
    /// test wants: the hold is over this fd, whatever an earlier test did
    /// with the number.
    fn sock(fd: RawFd) -> Guard {
        let handle = fd_handle::open(fd, FdKind::Socket);
        fd_handle::acquire(handle, FdKind::Socket).expect("a fresh socket handle is live")
    }

    /// a hold on `fd` as a pipe handle.
    fn pipe_hold(fd: RawFd) -> Guard {
        let handle = fd_handle::open(fd, FdKind::Pipe);
        fd_handle::acquire(handle, FdKind::Pipe).expect("a fresh pipe handle is live")
    }

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
        assert_eq!(write_yielding(&pipe_hold(write_end), b"pith"), 4);
        assert_eq!(read_yielding(&pipe_hold(read_end), 16).unwrap(), b"pith");
        close(read_end);
        close(write_end);
    }

    #[test]
    fn a_closed_writer_reads_as_eof_not_an_error() {
        let (read_end, write_end) = pipe();
        close(write_end);
        // Some(empty) is EOF; None would be an error, which this is not.
        assert!(read_yielding(&pipe_hold(read_end), 16).unwrap().is_empty());
        close(read_end);
    }

    #[test]
    fn a_bad_fd_reports_an_error_rather_than_eof() {
        assert_eq!(wait_ready(-1, true, 0), -1);
        assert_eq!(wait_ready_unguarded(-1, true, 0), -1);
    }

    /// a handle closed by another task while a read is parked on it — the
    /// green case the reactor handles — has an os-thread twin: the closer
    /// shuts the socket down to return the blocked read, which reads as end
    /// of stream, and the dead handle turns that into an error. a peer that
    /// really finished still reads as eof, because its handle is live.
    #[test]
    fn a_read_returned_by_a_close_is_an_error_where_a_peer_eof_is_not() {
        let (a, b) = socket_pair();
        let handle = fd_handle::open(a, FdKind::Socket);
        let guard = fd_handle::acquire(handle, FdKind::Socket).expect("live");
        // the close finds a user inside: it shuts the socket down and defers.
        assert!(fd_handle::close(handle));
        assert!(socket_read_blocking(&guard, 16).is_none());
        assert!(socket_read_yielding(&guard, 16).is_none());
        // and the write side of the same close.
        assert_eq!(socket_write_blocking(&guard, b"x"), 0);
        assert_eq!(socket_write_yielding(&guard, b"x"), 0);
        drop(guard);
        close(b);
    }

    /// a wait on a handle closed underneath it ends with an error, not a
    /// readiness, on the `poll` branch: the shutdown the closer runs makes
    /// the socket report ready, and the dead handle turns that into `-1`.
    #[test]
    fn a_wait_on_a_handle_closed_underneath_it_reports_an_error() {
        let (a, b) = socket_pair();
        let handle = fd_handle::open(a, FdKind::Socket);
        let guard = fd_handle::acquire(handle, FdKind::Socket).expect("live");
        assert!(fd_handle::close(handle));
        assert_eq!(wait_guarded(&guard, true, 1000), -1);
        drop(guard);
        close(b);
    }

    #[test]
    fn an_idle_fd_times_out_instead_of_reporting_ready() {
        let (read_end, write_end) = pipe();
        assert_eq!(wait_ready(read_end as i64, true, 0), 0);
        // once there is something to read it reports ready.
        assert_eq!(write_yielding(&pipe_hold(write_end), b"x"), 1);
        assert_eq!(wait_ready(read_end as i64, true, 0), 1);
        close(read_end);
        close(write_end);
    }

    #[test]
    fn a_socket_moves_the_same_bytes_recv_and_send_as_read_and_write_did() {
        let (a, b) = socket_pair();
        assert_eq!(socket_write_yielding(&sock(a), b"pith"), 4);
        assert_eq!(socket_read_yielding(&sock(b), 16).unwrap(), b"pith");
        // and the blocking pair, which is the os-thread backend's path.
        assert_eq!(socket_write_blocking(&sock(b), b"back"), 4);
        assert_eq!(socket_read_blocking(&sock(a), 16).unwrap(), b"back");
        close(a);
        close(b);
    }

    #[test]
    fn a_socket_whose_peer_closed_reads_as_eof_not_an_error() {
        let (a, b) = socket_pair();
        close(a);
        // Some(empty) is EOF, matching what `read` reported here before.
        assert!(socket_read_yielding(&sock(b), 16).unwrap().is_empty());
        assert!(socket_read_blocking(&sock(b), 16).unwrap().is_empty());
        close(b);
    }

    /// the socket syscalls refuse a descriptor that is not a socket, where
    /// `read`/`write` would have used it — the invariant check behind
    /// `report_not_a_socket`.
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
        assert_eq!(write_yielding(&pipe_hold(write_end), b"x"), 1);
        assert_eq!(read_yielding(&pipe_hold(read_end), 16).unwrap(), b"x");
        close(read_end);
        close(write_end);
    }

    #[test]
    fn a_genuinely_closed_fd_is_ebadf_not_enotsock() {
        // a number nothing is open on: the socket calls report it the same way
        // `read`/`write` do, so only a wrong-kind fd trips the check.
        //
        // the probed number must stay closed across the assertions, and cargo
        // runs tests on parallel threads whose own sockets and pipes recycle
        // low fd numbers immediately. parking the probe on a high slot keeps
        // it out of lowest-available allocation's reach.
        let (a, b) = socket_pair();
        let high: RawFd = 941;
        assert_eq!(unsafe { libc::dup2(a, high) }, high);
        close(a);
        close(b);
        close(high);
        assert_eq!(socket_read_errno(high), libc::EBADF);
        assert_eq!(socket_write_errno(high), libc::EBADF);
    }

    // -----------------------------------------------------------------------
    // read deadlines.
    //
    // these run off a green task, so `wait_ready` takes its `poll` branch — the
    // deadline arithmetic is the same either way, and this keeps the tests free
    // of a reactor. the end-to-end green case lives in
    // tests/cases/test_socket_read_timeout.pith, which runs on both backends.
    // -----------------------------------------------------------------------

    /// write `SO_RCVTIMEO` directly, in microseconds, for the cases
    /// `pith_tcp_set_timeout`'s millisecond argument cannot express.
    fn set_read_timeout_micros(fd: RawFd, usec: i64) {
        let tv = libc::timeval {
            tv_sec: 0,
            tv_usec: usec as libc::suseconds_t,
        };
        // SAFETY: setsockopt with a `timeval` of the length we declare.
        let rc = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                &tv as *const libc::timeval as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        };
        assert_eq!(rc, 0);
    }

    /// the fact the whole fix rests on: `SO_RCVTIMEO` is still recorded on a
    /// non-blocking socket, and reads back. the option has no *effect* on a
    /// non-blocking syscall — which is the bug — but the kernel keeps it, so it
    /// is a usable place to have put the deadline.
    ///
    /// it does not read back byte-identical to what was written: linux stores
    /// the timeout in jiffies, so it comes back rounded up to the tick (250 ms
    /// reads as 252 on a 250 Hz kernel). that is the deadline the blocking
    /// backend actually waits, so taking the kernel's number rather than a
    /// remembered one is the more faithful answer, not a lossy one. the
    /// assertion is therefore a band: never shorter than asked, never
    /// meaningfully longer.
    #[test]
    fn a_read_deadline_is_readable_back_off_a_non_blocking_socket() {
        let (a, b) = socket_pair();
        set_nonblocking(a);
        crate::network::pith_tcp_set_timeout(fd_handle::open(a, FdKind::Socket), 250);
        let deadline = Channel::Socket.timeout_ms(a as i64, true);
        assert!((250..300).contains(&deadline), "read back {deadline} ms");
        // one direction only, and only the socket it was set on.
        assert_eq!(Channel::Socket.timeout_ms(a as i64, false), -1);
        assert_eq!(Channel::Socket.timeout_ms(b as i64, true), -1);
        // and clearing it puts the socket back to waiting forever.
        crate::network::pith_tcp_set_timeout(fd_handle::open(a, FdKind::Socket), 0);
        assert_eq!(Channel::Socket.timeout_ms(a as i64, true), -1);
        close(a);
        close(b);
    }

    /// a sub-millisecond deadline must not round down to `0`, which `wait_ready`
    /// reads as "do not wait" and would answer with an instant timeout. the
    /// kernel's own jiffy rounding usually settles this first — 500 µs comes
    /// back as a whole tick — but the guard is what makes that a property
    /// rather than a coincidence of the host's HZ.
    #[test]
    fn a_sub_millisecond_deadline_rounds_up_rather_than_to_no_wait_at_all() {
        let (a, b) = socket_pair();
        set_read_timeout_micros(a, 500);
        let deadline = Channel::Socket.timeout_ms(a as i64, true);
        assert!(deadline >= 1, "a 500 us deadline read back as {deadline} ms");
        close(a);
        close(b);
    }

    /// a pipe has no deadline to read — `SO_RCVTIMEO` is a socket option — so a
    /// child process's stdout waits for its writer exactly as it always has.
    #[test]
    fn a_pipe_has_no_deadline_and_keeps_waiting_forever() {
        let (read_end, write_end) = pipe();
        assert_eq!(Channel::Pipe.timeout_ms(read_end as i64, true), -1);
        assert_eq!(Channel::Pipe.timeout_ms(write_end as i64, false), -1);
        // even on a descriptor that *is* a socket with a deadline set, the pipe
        // channel does not go looking for one.
        let (a, b) = socket_pair();
        crate::network::pith_tcp_set_timeout(fd_handle::open(a, FdKind::Socket), 250);
        assert_eq!(Channel::Pipe.timeout_ms(a as i64, true), -1);
        close(a);
        close(b);
        close(read_end);
        close(write_end);
    }

    /// an idle socket with a deadline gives up on it, and reports the giving up
    /// exactly as the blocking backend does: `None`.
    #[test]
    fn an_idle_socket_read_gives_up_at_its_deadline_on_both_paths() {
        let (a, b) = socket_pair();
        set_nonblocking(a);
        crate::network::pith_tcp_set_timeout(fd_handle::open(a, FdKind::Socket), 150);
        crate::network::pith_tcp_set_timeout(fd_handle::open(b, FdKind::Socket), 150);

        let start = Instant::now();
        // the yielding path: the one that used to wait forever here.
        assert!(socket_read_yielding(&sock(a), 16).is_none());
        let waited = start.elapsed();
        assert!(waited >= Duration::from_millis(120), "gave up after {waited:?}");
        assert!(waited < Duration::from_secs(5), "waited {waited:?}");

        // and the blocking path, whose answer it has to match. `b` is left
        // blocking, so this is `SO_RCVTIMEO` doing the work in the kernel.
        assert!(socket_read_blocking(&sock(b), 16).is_none());

        close(a);
        close(b);
    }

    /// the budget is spent down across retries rather than reset by each one: a
    /// second wait on an exhausted budget gives up at once instead of buying
    /// another full deadline. without this a socket that keeps waking with
    /// nothing behind it would never reach its deadline at all.
    #[test]
    fn a_retry_gets_what_is_left_of_the_deadline_not_a_fresh_one() {
        let (a, b) = socket_pair();
        set_nonblocking(a);
        crate::network::pith_tcp_set_timeout(fd_handle::open(a, FdKind::Socket), 150);
        let mut budget = WaitBudget::Unasked;

        let start = Instant::now();
        assert_eq!(budget.wait(&sock(a), true, Channel::Socket), 0);
        assert!(start.elapsed() >= Duration::from_millis(120));

        let retry = Instant::now();
        assert_eq!(budget.wait(&sock(a), true, Channel::Socket), 0);
        let spent = retry.elapsed();
        assert!(spent < Duration::from_millis(50), "retry waited {spent:?}");

        close(a);
        close(b);
    }

    /// a wait longer than the clock can express is clamped rather than carried
    /// into arithmetic that cannot hold it: the reactor turns a timeout into
    /// `Instant::now() + d`, and that addition panics on a duration past the
    /// clock's range. `pith_tcp_wait_readable` passes its argument straight
    /// through, so the value is a caller's to choose.
    ///
    /// off a green task this takes the `poll` branch, so what it pins is the
    /// argument surviving the clamp — the reactor branch needs a green task and
    /// is covered by the clamp being in `wait_ready` rather than in either arm.
    #[test]
    fn an_absurd_wait_is_clamped_rather_than_overflowing_a_deadline() {
        let (a, b) = socket_pair();
        assert_eq!(socket_write_yielding(&sock(b), b"x"), 1);
        assert_eq!(wait_ready(a as i64, true, i64::MAX), 1);
        close(a);
        close(b);
    }

    /// no deadline set still means wait forever, which is what nearly every
    /// socket in the stdlib is: the budget resolves to `Forever` and the wait it
    /// asks for is the unbounded one.
    #[test]
    fn a_socket_with_no_deadline_still_waits_forever() {
        let (a, b) = socket_pair();
        set_nonblocking(a);
        let mut budget = WaitBudget::Unasked;
        // ready immediately, so this does not actually block — what is being
        // checked is which deadline it resolved before waiting.
        assert_eq!(socket_write_yielding(&sock(b), b"x"), 1);
        assert_eq!(budget.wait(&sock(a), true, Channel::Socket), 1);
        assert!(matches!(budget, WaitBudget::Forever));
        close(a);
        close(b);
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
        // a socket handle over a regular file: the bookkeeping mistake the
        // check exists for, made deliberately.
        let file = sock(open_a_file());
        match call {
            "read_yielding" => {
                socket_read_yielding(&file, 16);
            }
            "write_yielding" => {
                socket_write_yielding(&file, b"x");
            }
            "read_blocking" => {
                socket_read_blocking(&file, 16);
            }
            "write_blocking" => {
                socket_write_blocking(&file, b"x");
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
    fn a_socket_read_on_a_non_socket_stops_the_process() {
        if let Some(call) = requested_call() {
            make_the_bad_call(&call);
        }
        for call in ["read_yielding", "read_blocking"] {
            let (code, stderr) = abort_child(
                "fdio::tests::a_socket_read_on_a_non_socket_stops_the_process",
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
    fn a_socket_write_on_a_non_socket_stops_the_process() {
        if let Some(call) = requested_call() {
            make_the_bad_call(&call);
        }
        for call in ["write_yielding", "write_blocking"] {
            let (code, stderr) = abort_child(
                "fdio::tests::a_socket_write_on_a_non_socket_stops_the_process",
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
