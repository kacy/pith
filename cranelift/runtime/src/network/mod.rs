//! TCP sockets for the language: listen, connect, accept, read, write, wait,
//! deadlines, close and shutdown, plus the `pith_dns_resolve` builtin.
//!
//! every entry point works on either task backend. under green, sockets are
//! non-blocking and a would-block parks the task on the reactor (`netpoll`)
//! through `fdio`; under os threads, sockets block in the kernel.

use crate::concurrency::scheduler::{backend, Backend};
use crate::fd_handle::{self, FdKind, Guard};
use crate::ffi_util::with_cstr_str;
use crate::netpoll;
use std::os::unix::io::RawFd;

// ---------------------------------------------------------------------------
// handles
//
// what the language holds for a socket is a handle from `fd_handle`, not the
// descriptor: the fd number stamped with a generation, so a connection that
// was closed (and whose number the kernel has since handed to another) is a
// stale handle that every entry point below refuses with an ordinary failure,
// where the raw number would have reached the other connection's syscalls.
// `socket()` takes the hold for one call; `open_socket()` registers a
// descriptor pith just created and returns the handle the caller gets.
// ---------------------------------------------------------------------------

/// the hold on `handle` for one socket call, or `None` for a handle that is
/// not a live socket.
pub(super) fn socket(handle: i64) -> Option<Guard> {
    fd_handle::acquire(handle, FdKind::Socket)
}

/// register a socket pith just created or accepted and return its handle.
/// a descriptor the table cannot hold is closed and reported as a failure.
pub(super) fn open_socket(fd: RawFd) -> i64 {
    let handle = fd_handle::open(fd, FdKind::Socket);
    if handle == 0 {
        close_raw(fd);
    }
    handle
}

// ---------------------------------------------------------------------------
// green-backend helpers
//
// under the green backend socket fds are non-blocking and a would-block
// syscall yields the green task to the epoll reactor (see `netpoll`) instead
// of parking the worker OS thread. on the os-thread backend `is_green()` is
// false, so `pith_tcp_*` take their blocking branches, and a blocking fd never
// returns `WouldBlock` for the retry loops to react to.
// ---------------------------------------------------------------------------

/// true when the green backend is selected. gates non-blocking socket setup and
/// the yield-instead-of-block retry loops.
pub(super) fn is_green() -> bool {
    backend() == Backend::Green
}

/// disable nagle on a raw fd. see the note in the blocking connect path for why
/// request/response protocols want this.
pub(super) fn set_nodelay_raw(fd: RawFd) {
    let one: libc::c_int = 1;
    // SAFETY: setsockopt with a valid fd and an int-sized option value.
    unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_NODELAY,
            &one as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
}

/// read the pending socket error (SO_ERROR), used after a non-blocking connect
/// reports writable to tell a completed connection from a refused one.
pub(super) fn socket_error(fd: RawFd) -> i32 {
    let mut err: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: getsockopt SO_ERROR into an int of the matching size on a valid fd.
    unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            &mut err as *mut libc::c_int as *mut libc::c_void,
            &mut len,
        );
    }
    err
}
// the green paths create their sockets with raw libc calls, so they set
// close-on-exec themselves (std does it for the os-thread paths): a child
// process spawned later must not inherit a server's connections. linux sets
// it, with non-blocking, in the call that creates the descriptor; macos has no
// `accept4` or `SOCK_CLOEXEC`, so both flags are set right after, which leaves
// a short window where a concurrent spawn can still inherit the descriptor.

/// a non-blocking, close-on-exec stream socket, or a negative value on failure.
#[cfg(target_os = "linux")]
pub(super) fn stream_socket(family: libc::c_int) -> RawFd {
    // SAFETY: creating a socket; the caller checks the result before use.
    unsafe {
        libc::socket(
            family,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    }
}

/// a non-blocking, close-on-exec stream socket, or a negative value on failure.
#[cfg(not(target_os = "linux"))]
pub(super) fn stream_socket(family: libc::c_int) -> RawFd {
    // SAFETY: creating a socket; the caller checks the result before use.
    cloexec_nonblocking(unsafe { libc::socket(family, libc::SOCK_STREAM, 0) })
}

/// accept one connection on `listener` as a non-blocking, close-on-exec
/// descriptor, or a negative value with errno set.
#[cfg(target_os = "linux")]
pub(super) fn accept_raw(listener: RawFd) -> RawFd {
    // SAFETY: accept on a listener fd the caller holds open; a null addr/len
    // asks the kernel not to report the peer address, which we don't need.
    unsafe {
        libc::accept4(
            listener,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
        )
    }
}

/// accept one connection on `listener` as a non-blocking, close-on-exec
/// descriptor, or a negative value with errno set.
#[cfg(not(target_os = "linux"))]
pub(super) fn accept_raw(listener: RawFd) -> RawFd {
    // SAFETY: as in the linux version.
    cloexec_nonblocking(unsafe {
        libc::accept(listener, std::ptr::null_mut(), std::ptr::null_mut())
    })
}

/// set close-on-exec and non-blocking on `fd` if it is a descriptor, and
/// return it unchanged either way.
#[cfg(not(target_os = "linux"))]
pub(super) fn cloexec_nonblocking(fd: RawFd) -> RawFd {
    if fd >= 0 {
        // SAFETY: plain fcntl on a descriptor this process just created.
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        fdio::set_nonblocking(fd);
    }
    fd
}

/// close a raw fd that never became a handle, cleaning up any reactor state
/// first. used by the green connect path on its error branches.
pub(super) fn close_raw(fd: RawFd) {
    netpoll::on_close(fd);
    // SAFETY: closing an fd we own.
    unsafe {
        libc::close(fd);
    }
}

// ---------------------------------------------------------------------------
// FFI surface
// ---------------------------------------------------------------------------

/// the accept queue length asked of the kernel. linux and the bsds clamp a
/// larger value to the system maximum (`net.core.somaxconn`, 4096 on current
/// linux), so this means "as long as the system allows", which is what go asks
/// for too.
const LISTEN_BACKLOG: libc::c_int = 65535;

/// raise a listening socket's accept queue to `LISTEN_BACKLOG`. std's
/// `TcpListener::bind` listens with a backlog of 128. when more connections
/// than that arrive in one burst, linux drops the overflow: a client whose
/// final handshake ack was dropped believes it is connected and sends its
/// request, which is dropped too, so its first request waits out a 200 ms or
/// longer retransmit timeout. a load test that opens 1024 connections at once
/// hit this on every run. calling listen again on a listening socket only
/// updates its backlog.
pub(super) fn widen_backlog(fd: RawFd) {
    // SAFETY: `fd` is a listening socket this process just created. a failure
    // leaves the original backlog in place, which still works.
    unsafe {
        libc::listen(fd, LISTEN_BACKLOG);
    }
}
/// TCP close: retire the handle and close its descriptor.
///
/// the close is exactly once per handle: a second call, or a call on a handle
/// that was never open, does nothing, where a raw `close(2)` would have landed
/// on whatever now holds the number. a call still inside on this handle
/// (parked in the reactor, or blocked in the kernel on the os-thread backend)
/// is returned with an error, and the descriptor itself is closed once the last
/// such call is out (see `fd_handle::close`).
#[no_mangle]
pub extern "C" fn pith_tcp_close(conn: i64) {
    fd_handle::close(conn);
}

/// shut a socket down for both directions without closing it: the graceful
/// shutdown primitive for a *listening* socket. returns 1 on success, 0 if the
/// kernel refuses (a stale handle, or a socket already shut down).
///
/// this is how a loop is stopped without taking its socket away from it. a
/// blocked `accept` returns `EINVAL`, a poll/epoll wait reports `HUP`, and a
/// blocked read returns end of stream, whichever backend and whichever thread
/// the loop runs on; the handle stays open, so the loop that owns it closes it
/// itself, once, on its own way out. closing from another task is safe as
/// well (`pith_tcp_close` wakes a call still inside the same way and the
/// handle refuses every call after), but a shutdown leaves the close where the
/// ownership is, and a reader told to stop can tell that apart from a
/// connection that is gone.
#[no_mangle]
pub extern "C" fn pith_tcp_shutdown(conn: i64) -> i64 {
    let Some(guard) = socket(conn) else {
        return 0;
    };
    // SAFETY: `shutdown` on an fd the guard holds open. it does not free the
    // descriptor, so it is safe to call while another task is blocked on the
    // same fd, which is the entire point.
    let rc = unsafe { libc::shutdown(guard.fd(), libc::SHUT_RDWR) };
    if rc == 0 {
        1
    } else {
        0
    }
}

fn dns_resolve_impl(host: &str) -> *mut i8 {
    match crate::dns::resolve(host, 0).first() {
        Some(addr) => crate::pith_strdup_string(&addr.ip().to_string()),
        None => std::ptr::null_mut(),
    }
}

/// DNS resolve: resolve hostname to IP address string
///
/// under the green backend the lookup runs on the resolver pool and this task
/// parks, so the worker stays free; under os threads it blocks here.
/// either way a failed lookup and one that returns no address both come back
/// null, which is what the caller already treats as an error.
#[no_mangle]
pub unsafe extern "C" fn pith_dns_resolve(hostname: *const i8) -> *mut i8 {
    // SAFETY: Caller guarantees `hostname` is null or a valid null-terminated C string.
    unsafe { with_cstr_str(hostname, dns_resolve_impl) }.unwrap_or(std::ptr::null_mut())
}

mod io;
mod tcp;
mod unix;
#[cfg(test)]
mod tests;

pub use io::*;
pub use tcp::*;
pub use unix::*;
