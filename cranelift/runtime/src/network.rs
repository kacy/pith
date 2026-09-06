use crate::bytes::{pith_bytes_from_vec, pith_bytes_ref};
use crate::concurrency::scheduler::{backend, Backend};
use crate::fd_handle::{self, FdKind, Guard};
use crate::fdio;
use crate::ffi_util::{cstr_str, cstr_str_or_empty};
use crate::netpoll;
use std::os::unix::io::RawFd;

// ---------------------------------------------------------------------------
// handles
//
// what the language holds for a socket is a handle from `fd_handle`, not the
// descriptor: the fd number stamped with a generation, so a connection that
// was closed — and whose number the kernel has since handed to another — is a
// stale handle that every entry point below refuses with an ordinary failure,
// where the raw number would have reached the other connection's syscalls.
// `socket()` takes the hold for one call; `open_socket()` registers a
// descriptor pith just created and returns the handle the caller gets.
// ---------------------------------------------------------------------------

/// the hold on `handle` for one socket call, or `None` for a handle that is
/// not a live socket.
fn socket(handle: i64) -> Option<Guard> {
    fd_handle::acquire(handle, FdKind::Socket)
}

/// register a socket pith just created or accepted and return its handle.
/// a descriptor the table cannot hold is closed and reported as a failure.
fn open_socket(fd: RawFd) -> i64 {
    let handle = fd_handle::open(fd, FdKind::Socket);
    if handle == 0 {
        close_raw(fd);
    }
    handle
}

// ---------------------------------------------------------------------------
// green-backend helpers
//
// under `PITH_GREEN` socket fds are non-blocking and a would-block syscall
// yields the green task to the epoll reactor (see `netpoll`) instead of parking
// the worker OS thread. every helper below is a no-op path when the flag is off:
// `is_green()` is false, so `pith_tcp_*` take their original blocking branches
// byte-for-byte, and a blocking fd never returns `WouldBlock` for the retry
// loops to react to.
// ---------------------------------------------------------------------------

/// true when the green backend is selected. gates non-blocking socket setup and
/// the yield-instead-of-block retry loops.
fn is_green() -> bool {
    backend() == Backend::Green
}

/// disable nagle on a raw fd — see the note in the blocking connect path for why
/// request/response protocols want this.
fn set_nodelay_raw(fd: RawFd) {
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
fn socket_error(fd: RawFd) -> i32 {
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

/// fill a zeroed `sockaddr_storage` from a resolved `SocketAddr`, returning the
/// address length for `connect`. handles both v4 and v6; ports and addresses go
/// out in network byte order.
fn fill_sockaddr(
    addr: &std::net::SocketAddr,
    storage: &mut libc::sockaddr_storage,
) -> libc::socklen_t {
    match addr {
        std::net::SocketAddr::V4(a) => {
            // SAFETY: `storage` is zeroed and larger than `sockaddr_in`.
            let sin = storage as *mut libc::sockaddr_storage as *mut libc::sockaddr_in;
            unsafe {
                (*sin).sin_family = libc::AF_INET as libc::sa_family_t;
                (*sin).sin_port = a.port().to_be();
                // octets() is already in network order; laying them out via
                // `from_ne_bytes` puts the same bytes into s_addr's memory.
                (*sin).sin_addr.s_addr = u32::from_ne_bytes(a.ip().octets());
            }
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t
        }
        std::net::SocketAddr::V6(a) => {
            // SAFETY: `storage` is zeroed and larger than `sockaddr_in6`.
            let sin6 = storage as *mut libc::sockaddr_storage as *mut libc::sockaddr_in6;
            unsafe {
                (*sin6).sin6_family = libc::AF_INET6 as libc::sa_family_t;
                (*sin6).sin6_port = a.port().to_be();
                (*sin6).sin6_addr.s6_addr = a.ip().octets();
                (*sin6).sin6_flowinfo = a.flowinfo();
                (*sin6).sin6_scope_id = a.scope_id();
            }
            std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t
        }
    }
}

/// green-mode connect: a non-blocking socket whose `connect` returns `EINPROGRESS`
/// and completes asynchronously. we yield the task until the socket is writable,
/// then check `SO_ERROR` to distinguish a completed connection from a refusal.
/// returns the fd on success or `0` on failure, matching the blocking path.
///
/// the name lookup runs on the resolver pool (see `crate::dns`) so getaddrinfo
/// blocks one of its threads rather than this worker. we try every resolved
/// address in order and keep the first that connects, matching
/// `TcpStream::connect`: a name like `localhost` often resolves to `::1` before
/// `127.0.0.1`, and a server bound only to IPv4 refuses the first before the
/// second succeeds. no addresses (a resolver failure or an empty answer) falls
/// through to `0`, as it always has.
fn green_connect(host: &str, port: i64) -> i64 {
    for addr in crate::dns::resolve(host, port) {
        let fd = green_connect_addr(&addr);
        if fd != 0 {
            return fd;
        }
    }
    0
}

/// green-mode connect to a single resolved address. returns the fd on success or
/// `0` on failure so the caller can fall through to the next resolved address.
fn green_connect_addr(addr: &std::net::SocketAddr) -> i64 {
    let family = match addr {
        std::net::SocketAddr::V4(_) => libc::AF_INET,
        std::net::SocketAddr::V6(_) => libc::AF_INET6,
    };

    // SAFETY: creating a stream socket; the fd is validated before any use.
    let fd = unsafe { libc::socket(family, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return 0;
    }
    fdio::set_nonblocking(fd);
    set_nodelay_raw(fd);

    // SAFETY: sockaddr_storage is plain-old-data; zeroing it is a valid start.
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let len = fill_sockaddr(addr, &mut storage);
    // SAFETY: connecting a non-blocking socket to a valid sockaddr of `len` bytes.
    let rc = unsafe {
        libc::connect(
            fd,
            &storage as *const libc::sockaddr_storage as *const libc::sockaddr,
            len,
        )
    };
    if rc != 0 {
        let err = fdio::errno();
        if err != libc::EINPROGRESS {
            close_raw(fd);
            return 0;
        }
        // in-progress: wait for writable (handshake done or failed).
        if fdio::wait_ready(fd as i64, false, -1) != 1 {
            close_raw(fd);
            return 0;
        }
        if socket_error(fd) != 0 {
            close_raw(fd);
            return 0;
        }
    }
    open_socket(fd)
}

/// close a raw fd that never became a handle, cleaning up any reactor state
/// first. used by the green connect path on its error branches.
fn close_raw(fd: RawFd) {
    netpoll::on_close(fd);
    // SAFETY: closing an fd we own.
    unsafe {
        libc::close(fd);
    }
}

// ---------------------------------------------------------------------------
// FFI surface
// ---------------------------------------------------------------------------

/// TCP listen — bind and listen on addr:port, return the listener's handle
#[no_mangle]
pub unsafe extern "C" fn pith_tcp_listen(addr: *const i8, port: i64) -> i64 {
    use std::net::TcpListener;

    // the first socket of the process is where SIGPIPE stops being fatal: a peer
    // that hangs up mid-response must give this server an EPIPE to handle, not a
    // signal that kills it. see signals::ignore_sigpipe.
    crate::signals::ignore_sigpipe();

    let host = cstr_str_or_empty(addr);
    let host = if host.is_empty() { "0.0.0.0" } else { host };
    let bind_addr = format!("{}:{}", host, port);
    match TcpListener::bind(&bind_addr) {
        Ok(listener) => {
            use std::os::unix::io::IntoRawFd;
            let fd = listener.into_raw_fd() as i64;
            // green mode: a non-blocking listener lets accept yield instead of
            // parking the worker.
            if is_green() {
                fdio::set_nonblocking(fd as RawFd);
            }
            open_socket(fd as RawFd)
        }
        Err(_) => 0,
    }
}

/// TCP connect — connect to addr:port, return the connection's handle
#[no_mangle]
pub unsafe extern "C" fn pith_tcp_connect(addr: *const i8, port: i64) -> i64 {
    use std::net::TcpStream;

    // as in pith_tcp_listen: a client writing to a server that closed first must
    // see EPIPE rather than be killed.
    crate::signals::ignore_sigpipe();

    let host = cstr_str_or_empty(addr);
    let host = if host.is_empty() { "127.0.0.1" } else { host };

    // green mode: a non-blocking connect that yields the task on EINPROGRESS.
    if is_green() {
        return green_connect(host, port);
    }

    let connect_addr = format!("{}:{}", host, port);
    match TcpStream::connect(&connect_addr) {
        Ok(stream) => {
            // no read deadline: a fresh connection waits indefinitely, exactly as
            // the green path leaves it. a client that wants bounded reads sets
            // one explicitly via tcp.set_timeout.
            // disable nagle's algorithm: request/response protocols (http, grpc,
            // the db drivers) write small frames and then wait for a reply, which
            // otherwise collides with the peer's delayed acks for a ~40ms stall
            // per round-trip.
            let _ = stream.set_nodelay(true);
            use std::os::unix::io::IntoRawFd;
            open_socket(stream.into_raw_fd())
        }
        Err(_) => 0,
    }
}

/// TCP accept — accept a connection on a listener handle, return the client's
/// handle. a listener closed while this is parked on it returns a failure,
/// which is how an accept loop learns the listener is gone.
#[no_mangle]
pub extern "C" fn pith_tcp_accept(server: i64) -> i64 {
    let Some(listener) = socket(server) else {
        return 0;
    };
    let server_fd = listener.fd();

    // green mode: accept on the non-blocking listener, yielding on would-block.
    if is_green() {
        loop {
            // SAFETY: accept on a listener fd the guard holds open; a null
            // addr/len asks the kernel not to report the peer address, which we
            // don't need.
            let fd = unsafe { libc::accept(server_fd, std::ptr::null_mut(), std::ptr::null_mut()) };
            if fd >= 0 {
                fdio::set_nonblocking(fd);
                set_nodelay_raw(fd);
                return open_socket(fd);
            }
            let err = fdio::errno();
            if fdio::is_would_block(err) {
                if fdio::wait_guarded(&listener, true, -1) != 1 {
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

    use std::net::TcpListener;
    use std::os::unix::io::FromRawFd;

    let listener_ref = unsafe { TcpListener::from_raw_fd(server_fd) };
    let result = match listener_ref.accept() {
        Ok((stream, _addr)) => {
            use std::os::unix::io::IntoRawFd;
            open_socket(stream.into_raw_fd())
        }
        Err(_) => 0,
    };
    use std::os::unix::io::IntoRawFd;
    let _ = listener_ref.into_raw_fd();
    result
}

/// the size one read asks for. a non-positive `max_bytes` means the default
/// chunk, as it always has.
fn read_size(max_bytes: i64) -> usize {
    if max_bytes > 0 {
        max_bytes as usize
    } else {
        4096
    }
}

/// one read from a connection handle, whichever backend is running: the green
/// path yields the task to the reactor on would-block, the os-thread path blocks
/// in the kernel. `None` is a failure — a stale handle, a closed connection, or
/// a handle closed by another task while this read was inside — and
/// `Some(empty)` is end of stream.
fn socket_read(conn: i64, size: usize) -> Option<Vec<u8>> {
    let guard = socket(conn)?;
    if is_green() {
        fdio::socket_read_yielding(&guard, size)
    } else {
        fdio::socket_read_blocking(&guard, size)
    }
}

/// one write to a connection handle, the counterpart of `socket_read`. returns
/// the bytes accepted, `0` for a failure or a closed peer.
fn socket_write(conn: i64, data: &[u8]) -> i64 {
    let Some(guard) = socket(conn) else {
        return 0;
    };
    if is_green() {
        fdio::socket_write_yielding(&guard, data)
    } else {
        fdio::socket_write_blocking(&guard, data)
    }
}

/// TCP read — read up to 4096 bytes from connection fd, return as C string
#[no_mangle]
pub extern "C" fn pith_tcp_read(conn_fd: i64) -> *mut i8 {
    match socket_read(conn_fd, 4096) {
        Some(buf) => crate::pith_strdup_string(&String::from_utf8_lossy(&buf)),
        None => std::ptr::null_mut(),
    }
}

/// TCP read with max bytes limit
#[no_mangle]
pub extern "C" fn pith_tcp_read2(conn_fd: i64, max_bytes: i64) -> *mut i8 {
    match socket_read(conn_fd, read_size(max_bytes)) {
        Some(buf) => crate::pith_strdup_string(&String::from_utf8_lossy(&buf)),
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn pith_tcp_read_bytes(conn_fd: i64, max_bytes: i64) -> i64 {
    match socket_read(conn_fd, read_size(max_bytes)) {
        Some(buf) => pith_bytes_from_vec(buf),
        None => 0,
    }
}

/// wait for a handle to become readable or writable, the tri-state contract of
/// `fdio::wait_ready`: `1` ready, `0` timed out, `-1` an error — which covers
/// a stale handle and a handle closed while the wait was inside.
fn wait_handle(conn: i64, read: bool, timeout_ms: i64) -> i64 {
    let Some(guard) = socket(conn) else {
        return -1;
    };
    fdio::wait_guarded(&guard, read, timeout_ms)
}

#[no_mangle]
pub extern "C" fn pith_tcp_wait_readable(conn: i64, timeout_ms: i64) -> i64 {
    wait_handle(conn, true, timeout_ms)
}

#[no_mangle]
pub extern "C" fn pith_tcp_wait_writable(conn: i64, timeout_ms: i64) -> i64 {
    wait_handle(conn, false, timeout_ms)
}

/// TCP write — write data to connection fd, return bytes written
#[no_mangle]
pub unsafe extern "C" fn pith_tcp_write(conn_fd: i64, data: *const i8) -> i64 {
    socket_write(conn_fd, cstr_str_or_empty(data).as_bytes())
}

#[no_mangle]
pub unsafe extern "C" fn pith_tcp_write_bytes(conn_fd: i64, data: i64) -> i64 {
    let Some(bytes) = pith_bytes_ref(data) else {
        return 0;
    };
    socket_write(conn_fd, &bytes.data)
}

/// TCP set read timeout in milliseconds (0 = no timeout). a stale handle is
/// left alone.
#[no_mangle]
pub extern "C" fn pith_tcp_set_timeout(conn: i64, ms: i64) {
    let Some(guard) = socket(conn) else {
        return;
    };

    use std::net::TcpStream;
    use std::os::unix::io::FromRawFd;

    let stream = unsafe { TcpStream::from_raw_fd(guard.fd()) };
    if ms <= 0 {
        let _ = stream.set_read_timeout(None);
    } else {
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(ms as u64)));
    }
    use std::os::unix::io::IntoRawFd;
    let _ = stream.into_raw_fd();
}

/// TCP close — retire the handle and close its descriptor.
///
/// the close is exactly once per handle: a second call, or a call on a handle
/// that was never open, does nothing, where a raw `close(2)` would have landed
/// on whatever now holds the number. a call still inside on this handle —
/// parked in the reactor, or blocked in the kernel on the os-thread backend —
/// is returned with an error, and the descriptor itself is closed once the last
/// such call is out (see `fd_handle::close`).
#[no_mangle]
pub extern "C" fn pith_tcp_close(conn: i64) {
    fd_handle::close(conn);
}

/// shut a socket down for both directions without closing it — the graceful
/// shutdown primitive for a *listening* socket. returns 1 on success, 0 if the
/// kernel refuses (a stale handle, or a socket already shut down).
///
/// this is how a loop is stopped without taking its socket away from it. a
/// blocked `accept` returns `EINVAL`, a poll/epoll wait reports `HUP`, and a
/// blocked read returns end of stream, whichever backend and whichever thread
/// the loop runs on; the handle stays open, so the loop that owns it closes it
/// itself, once, on its own way out. closing from another task is safe as
/// well — `pith_tcp_close` wakes a call still inside the same way and the
/// handle refuses every call after — but a shutdown leaves the close where the
/// ownership is, and a reader told to stop can tell that apart from a
/// connection that is gone.
#[no_mangle]
pub extern "C" fn pith_tcp_shutdown(conn: i64) -> i64 {
    let Some(guard) = socket(conn) else {
        return 0;
    };
    // SAFETY: `shutdown` on an fd the guard holds open. it does not free the
    // descriptor, so it is safe to call while another task is blocked on the
    // same fd — which is the entire point.
    let rc = unsafe { libc::shutdown(guard.fd(), libc::SHUT_RDWR) };
    if rc == 0 {
        1
    } else {
        0
    }
}

/// DNS resolve — resolve hostname to IP address string
///
/// under the green backend the lookup runs on the resolver pool and this task
/// parks, so the worker stays free; under os threads it blocks here as before.
/// either way a failed lookup and one that returns no address both come back
/// null, which is what the caller already treats as an error.
#[no_mangle]
pub unsafe extern "C" fn pith_dns_resolve(hostname: *const i8) -> *mut i8 {
    let Some(host) = cstr_str(hostname) else {
        return std::ptr::null_mut();
    };
    match crate::dns::resolve(host, 0).first() {
        Some(addr) => crate::pith_strdup_string(&addr.ip().to_string()),
        None => std::ptr::null_mut(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_resolve_rejects_null_and_invalid_utf8() {
        let invalid = [0xffu8, 0x00];

        unsafe {
            assert!(pith_dns_resolve(std::ptr::null()).is_null());
            assert!(pith_dns_resolve(invalid.as_ptr() as *const i8).is_null());
        }
    }
}
