use crate::bytes::{pith_bytes_from_vec, pith_bytes_ref};
use crate::concurrency::scheduler::{backend, Backend};
use crate::fdio;
use crate::ffi_util::{cstr_str, cstr_str_or_empty};
use crate::netpoll;
use std::os::unix::io::RawFd;

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
    fd as i64
}

/// close a raw fd, cleaning up any reactor state first. used by the green connect
/// path on its error branches.
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

/// TCP listen — bind and listen on addr:port, return server fd
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
            fd
        }
        Err(_) => 0,
    }
}

/// TCP connect — connect to addr:port, return connection fd
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
            stream.into_raw_fd() as i64
        }
        Err(_) => 0,
    }
}

/// TCP accept — accept a connection on a server fd, return client fd
#[no_mangle]
pub extern "C" fn pith_tcp_accept(server_fd: i64) -> i64 {
    if server_fd <= 0 {
        return 0;
    }

    // green mode: accept on the non-blocking listener, yielding on would-block.
    if is_green() {
        loop {
            // SAFETY: accept on a valid listener fd; a null addr/len asks the
            // kernel not to report the peer address, which we don't need.
            let fd = unsafe {
                libc::accept(server_fd as i32, std::ptr::null_mut(), std::ptr::null_mut())
            };
            if fd >= 0 {
                fdio::set_nonblocking(fd);
                set_nodelay_raw(fd);
                return fd as i64;
            }
            let err = fdio::errno();
            if fdio::is_would_block(err) {
                if fdio::wait_ready(server_fd, true, -1) != 1 {
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

    let listener = unsafe { TcpListener::from_raw_fd(server_fd as i32) };
    let result = match listener.accept() {
        Ok((stream, _addr)) => {
            use std::os::unix::io::IntoRawFd;
            stream.into_raw_fd() as i64
        }
        Err(_) => 0,
    };
    use std::os::unix::io::IntoRawFd;
    let _ = listener.into_raw_fd();
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

/// one read from a connection fd, whichever backend is running: the green path
/// yields the task to the reactor on would-block, the os-thread path blocks in
/// the kernel. `None` is a failure, `Some(empty)` is end of stream.
///
/// both go through `fdio`'s socket calls, which use `recv` rather than `read` so
/// that an fd the process closed and the kernel handed to some later `open`
/// fails with `ENOTSOCK` instead of quietly returning that file's bytes.
fn socket_read(conn_fd: i64, size: usize) -> Option<Vec<u8>> {
    if conn_fd <= 0 {
        return None;
    }
    if is_green() {
        fdio::socket_read_yielding(conn_fd, size)
    } else {
        fdio::socket_read_blocking(conn_fd, size)
    }
}

/// one write to a connection fd, the counterpart of `socket_read`. returns the
/// bytes accepted, `0` for a failure or a closed peer.
fn socket_write(conn_fd: i64, data: &[u8]) -> i64 {
    if conn_fd <= 0 {
        return 0;
    }
    if is_green() {
        fdio::socket_write_yielding(conn_fd, data)
    } else {
        fdio::socket_write_blocking(conn_fd, data)
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

#[no_mangle]
pub extern "C" fn pith_tcp_wait_readable(fd: i64, timeout_ms: i64) -> i64 {
    fdio::wait_ready(fd, true, timeout_ms)
}

#[no_mangle]
pub extern "C" fn pith_tcp_wait_writable(fd: i64, timeout_ms: i64) -> i64 {
    fdio::wait_ready(fd, false, timeout_ms)
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

/// TCP set read timeout in milliseconds (0 = no timeout)
#[no_mangle]
pub extern "C" fn pith_tcp_set_timeout(fd: i64, ms: i64) {
    if fd < 0 {
        return;
    }

    use std::net::TcpStream;
    use std::os::unix::io::FromRawFd;

    let stream = unsafe { TcpStream::from_raw_fd(fd as i32) };
    if ms <= 0 {
        let _ = stream.set_read_timeout(None);
    } else {
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(ms as u64)));
    }
    use std::os::unix::io::IntoRawFd;
    let _ = stream.into_raw_fd();
}

/// TCP close — close the file descriptor
#[no_mangle]
pub extern "C" fn pith_tcp_close(fd: i64) {
    if fd <= 0 {
        return;
    }
    // drop any reactor registration for this fd before closing it, so a parked
    // waiter's stale (fd, interest) entry can't linger or be reused for a fresh
    // fd with the same number. a no-op when the reactor was never built.
    netpoll::on_close(fd as RawFd);
    unsafe {
        libc::close(fd as i32);
    }
}

/// shut a socket down for both directions without closing it — the graceful
/// shutdown primitive for a *listening* socket. returns 1 on success, 0 if the
/// kernel refuses (an fd that is not a socket, or already shut down).
///
/// this exists because closing a listener is not enough to stop an accept loop.
/// a green task parked on `accept` is woken by the close (the reactor's
/// `on_close` resolves its wait), but an accept loop running on a thread with no
/// green task behind it — `listen()` called straight from `main`, the shape
/// every server example uses — is blocked inside `accept`/`poll`, and closing an
/// fd does not wake a blocking call already waiting on it. `shutdown` does: the
/// blocked `accept` returns `EINVAL` and a poll/epoll wait reports `HUP`.
///
/// it also removes the close race. the fd stays valid, so the accept loop that
/// owns the listener closes it itself, once, on its own way out — rather than
/// having it closed underneath by another task, which risks closing whatever the
/// kernel has since handed that number to.
#[no_mangle]
pub extern "C" fn pith_tcp_shutdown(fd: i64) -> i64 {
    if fd <= 0 {
        return 0;
    }
    // SAFETY: `shutdown` on an fd pith owns. it does not free the descriptor, so
    // it is safe to call while another task is blocked on the same fd — which is
    // the entire point.
    let rc = unsafe { libc::shutdown(fd as i32, libc::SHUT_RDWR) };
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
