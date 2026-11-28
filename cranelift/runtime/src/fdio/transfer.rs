//! Pipe and socket read/write loops (green yielding and OS-thread blocking).

use super::wait::{socket_timeout_ms, WaitBudget};
use super::{errno, is_would_block, MAX_CHUNK};
use crate::fd_handle::Guard;

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
pub(super) enum Channel {
    /// a child process's pipe. `recv`/`send` reject a pipe outright, so pipes
    /// keep the general syscalls.
    Pipe,
    /// a connected socket, read and written with `recv`/`send`.
    Socket,
    /// a socket left in blocking mode (the os-thread backend) that has a
    /// deadline set. each call passes `MSG_DONTWAIT`, so a would-block comes
    /// back to the retry loop, which waits in `poll` for no longer than the
    /// deadline allows, where a plain blocking call would sit in the kernel
    /// for as long as `SO_RCVTIMEO` lets it.
    PolledSocket,
}

impl Channel {
    /// the flags argument for `recv`/`send`. always none: `MSG_NOSIGNAL` would
    /// suppress the SIGPIPE a `write` to a dead peer raises, and
    /// `signals::ignore_sigpipe` documents that disposition as replaceable: a
    /// program that arms SIGPIPE through `pith_signal_notify` is entitled to
    /// keep seeing it. with no flags, the only difference from `read`/`write`
    /// is the non-socket check.
    const NO_FLAGS: libc::c_int = 0;

    pub(super) fn is_socket(self) -> bool {
        !matches!(self, Channel::Pipe)
    }

    /// one read into `buf`, returning the raw syscall result.
    ///
    /// # Safety
    /// `fd` must be owned by pith for the length of the call.
    pub(super) unsafe fn read(self, fd: i32, buf: &mut [u8]) -> isize {
        let ptr = buf.as_mut_ptr() as *mut libc::c_void;
        let size = buf.len();
        match self {
            // SAFETY: `buf` is writable for `size` bytes; caller guarantees `fd` is owned.
            Channel::Pipe => unsafe { libc::read(fd, ptr, size) },
            // SAFETY: `buf` is writable for `size` bytes; caller guarantees `fd` is owned.
            Channel::Socket => unsafe { libc::recv(fd, ptr, size, Self::NO_FLAGS) },
            // SAFETY: `buf` is writable for `size` bytes; caller guarantees `fd` is owned.
            Channel::PolledSocket => unsafe { libc::recv(fd, ptr, size, libc::MSG_DONTWAIT) },
        }
    }

    /// one write from `data`, returning the raw syscall result.
    ///
    /// # Safety
    /// `fd` must be owned by pith for the length of the call.
    pub(super) unsafe fn write(self, fd: i32, data: &[u8]) -> isize {
        let ptr = data.as_ptr() as *const libc::c_void;
        let size = data.len();
        match self {
            // SAFETY: `data` is readable for `size` bytes; caller guarantees `fd` is owned.
            Channel::Pipe => unsafe { libc::write(fd, ptr, size) },
            // SAFETY: `data` is readable for `size` bytes; caller guarantees `fd` is owned.
            Channel::Socket => unsafe { libc::send(fd, ptr, size, Self::NO_FLAGS) },
            // SAFETY: `data` is readable for `size` bytes; caller guarantees `fd` is owned.
            Channel::PolledSocket => unsafe { libc::send(fd, ptr, size, libc::MSG_DONTWAIT) },
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
    pub(super) fn timeout_ms(self, fd: i64, read: bool) -> i64 {
        match self {
            // `SO_RCVTIMEO` is a socket option and a pipe is not a socket:
            // there is nothing to read, and nothing sets a deadline on a child
            // process's stdout in the first place. a pipe read waits for its
            // writer.
            Channel::Pipe => -1,
            Channel::Socket | Channel::PolledSocket => socket_timeout_ms(
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
    pub(super) fn check(self, fd: i64, err: i32, operation: &str) {
        if self.is_socket() && err == libc::ENOTSOCK {
            report_not_a_socket(fd, operation);
        }
    }
}
/// abort with a diagnostic: an fd pith is using as a socket is not one.
///
/// this is not a runtime condition a caller could handle (it is the process
/// having lost track of one of its own descriptors), and handing it back as an
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
/// socket's read deadline expires, the writer closes (returns an empty vec,
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
    let size = size.min(MAX_CHUNK);
    let mut buf = vec![0u8; size];
    let mut budget = WaitBudget::Unasked;
    loop {
        // SAFETY: reading into a buffer we own; `fd` is held
        // open by `guard` for the duration of this call.
        let n = unsafe { channel.read(fd as i32, &mut buf) };
        if n >= 0 {
            if n == 0 && !guard.is_live() {
                return None;
            }
            if channel.is_socket() {
                crate::perf_stats!(PERF_SOCK_READS += 1, PERF_SOCK_READ_BYTES += n as usize);
            }
            buf.truncate(n as usize);
            return Some(buf);
        }
        let err = errno();
        channel.check(fd, err, "read");
        if is_would_block(err) {
            // nothing ready: yield to the reactor and retry when readable, or
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

/// why a write to a pipe or socket failed, as the text a pith error carries.
/// the runtime entry points prefix it with the builtin's name.
pub(crate) type WriteError = String;

/// the reason text for a failed write syscall: the os's own description of
/// the errno (`Broken pipe (os error 32)`), except for the two outcomes that
/// are not the peer's or the descriptor's doing. a handle closed by another
/// task while the write was inside is reported as that, since the errno the
/// closer's shutdown produces (`EPIPE`) would blame the peer, and the
/// `EAGAIN` of an expired send deadline is reported as a timeout.
fn write_error(guard: &Guard, err: i32) -> WriteError {
    if !guard.is_live() {
        return "the handle was closed".to_string();
    }
    if is_would_block(err) {
        return "write timed out".to_string();
    }
    std::io::Error::from_raw_os_error(err).to_string()
}

/// write one buffer to a child's pipe. see `write_channel` for the contract.
pub(crate) fn write_yielding(guard: &Guard, data: &[u8]) -> Result<usize, WriteError> {
    write_channel(guard, data, Channel::Pipe)
}

/// write one buffer to a socket. the socket half of `write_yielding`.
pub(crate) fn socket_write_yielding(guard: &Guard, data: &[u8]) -> Result<usize, WriteError> {
    write_channel(guard, data, Channel::Socket)
}

/// write one buffer: one write syscall, yielding only if it would block with
/// nothing written yet. returns the bytes written (a partial count is returned
/// as-is; callers loop), or the reason for a real error, a closed reader, or a
/// send deadline that expired, which is again what the blocking path reports
/// for the `EAGAIN` an expired `SO_SNDTIMEO` gives it. an empty buffer is an
/// immediate `Ok(0)`: nothing was asked for and nothing failed. a non-empty
/// one never comes back as `Ok(0)`: a would-block waits and retries until at
/// least one byte goes out.
///
/// nothing in pith sets `SO_SNDTIMEO`, so this budget resolves to "wait
/// forever" on every socket the runtime has. it is read anyway so that the two
/// directions stay the same shape: a send deadline, if one is ever set, must
/// bound a green write exactly as it already bounds an os-thread one.
fn write_channel(guard: &Guard, data: &[u8], channel: Channel) -> Result<usize, WriteError> {
    if data.is_empty() {
        return Ok(0);
    }
    let fd = guard.fd() as i64;
    let mut budget = WaitBudget::Unasked;
    loop {
        // SAFETY: writing from a buffer we borrow; `fd` is
        // held open by `guard` for the duration of this call.
        let n = unsafe { channel.write(fd as i32, data) };
        if n > 0 {
            if channel.is_socket() {
                crate::perf_stats!(PERF_SOCK_WRITES += 1, PERF_SOCK_WRITE_BYTES += n as usize);
            }
            return Ok(n as usize);
        }
        if n == 0 {
            // a zero-length write of non-empty data means the reader is gone.
            return Err("the reader is gone".to_string());
        }
        let err = errno();
        channel.check(fd, err, "write");
        if is_would_block(err) {
            match budget.wait(guard, false, channel) {
                1 => continue,
                0 => return Err("write timed out".to_string()),
                _ => return Err("the handle was closed".to_string()),
            }
        }
        if err == libc::EINTR {
            continue;
        }
        return Err(write_error(guard, err));
    }
}

/// one read from a pipe that blocks in the kernel (OS-thread backend).
pub(crate) fn pipe_read_blocking(guard: &Guard, size: usize) -> Option<Vec<u8>> {
    read_blocking_channel(guard, size, Channel::Pipe)
}

/// one write to a pipe that blocks in the kernel (OS-thread backend).
pub(crate) fn pipe_write_blocking(guard: &Guard, data: &[u8]) -> Result<usize, WriteError> {
    if data.is_empty() {
        return Ok(0);
    }
    write_blocking_channel(guard, data, Channel::Pipe)
}

/// one read from a socket that blocks in the kernel: the os-thread backend,
/// where there is no green task to suspend and no reactor to hand the fd to.
///
/// a would-block is not a retry cue here. a blocking socket reports one only
/// when `SO_RCVTIMEO` expired (the timeout `pith_tcp_set_timeout` installs), so
/// it is a failure, the same as `TcpStream::read` would report. `None` covers
/// every error alike.
///
/// a close from another task returns this call by shutting the socket down,
/// which reads as end of stream; the handle being dead by then is what tells
/// that apart from the peer finishing, and it is reported as an error.
///
/// a socket with a read deadline goes through the polled retry loop instead,
/// so the wait ends at the deadline rather than at the next `SO_RCVTIMEO`.
pub(crate) fn socket_read_blocking(guard: &Guard, size: usize) -> Option<Vec<u8>> {
    if crate::fd_handle::deadline(guard.fd(), false).is_some() {
        return read_channel(guard, size, Channel::PolledSocket);
    }
    read_blocking_channel(guard, size, Channel::Socket)
}

fn read_blocking_channel(guard: &Guard, size: usize, channel: Channel) -> Option<Vec<u8>> {
    let fd = guard.fd() as i64;
    let size = size.min(MAX_CHUNK);
    let mut buf = vec![0u8; size];
    loop {
        // the syscall can sit in the kernel indefinitely touching only this
        // owned buffer, so it is a native window for the cycle collector. the
        // bracket is kept to the syscall itself.
        let n = {
            let _native = crate::cycle::native_bracket();
            // SAFETY: reading into a buffer we own; `fd` is
            // held open by `guard` for the duration of this call.
            unsafe { channel.read(fd as i32, &mut buf) }
        };
        if n >= 0 {
            if n == 0 && !guard.is_live() {
                return None;
            }
            buf.truncate(n as usize);
            return Some(buf);
        }
        let err = errno();
        if err == libc::EINTR {
            continue;
        }
        channel.check(fd, err, "read");
        return None;
    }
}

/// one write to a socket that blocks in the kernel, the counterpart of
/// `socket_read_blocking`. returns the bytes accepted, or the reason for any
/// error. an empty buffer is a zero-byte `send`, which succeeds with `Ok(0)`.
/// a socket with a write deadline goes through the polled retry loop, as a
/// read with a read deadline does.
pub(crate) fn socket_write_blocking(guard: &Guard, data: &[u8]) -> Result<usize, WriteError> {
    if crate::fd_handle::deadline(guard.fd(), true).is_some() {
        return write_channel(guard, data, Channel::PolledSocket);
    }
    write_blocking_channel(guard, data, Channel::Socket)
}

fn write_blocking_channel(
    guard: &Guard,
    data: &[u8],
    channel: Channel,
) -> Result<usize, WriteError> {
    let fd = guard.fd() as i64;
    loop {
        // a full send/pipe buffer can hold this in the kernel indefinitely;
        // like the blocking read it touches only the borrowed buffer, so it is
        // a native window for the cycle collector, kept to the syscall itself.
        let n = {
            let _native = crate::cycle::native_bracket();
            // SAFETY: writing from a buffer we borrow; `fd` is
            // held open by `guard` for the duration of this call.
            unsafe { channel.write(fd as i32, data) }
        };
        if n >= 0 {
            return Ok(n as usize);
        }
        let err = errno();
        if err == libc::EINTR {
            continue;
        }
        channel.check(fd, err, "write");
        return Err(write_error(guard, err));
    }
}

