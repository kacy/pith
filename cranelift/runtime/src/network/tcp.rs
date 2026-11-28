//! TCP listen, connect, accept, peer address, timeout, and deadline builtins.

use super::{
    accept_raw, close_raw, is_green, open_socket, set_nodelay_raw, socket, socket_error,
    stream_socket, widen_backlog,
};
use crate::fd_handle;
use crate::fdio;
use crate::ffi_util::with_cstr_str_or_empty;

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
/// through to `0`.
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

    let fd = stream_socket(family);
    if fd < 0 {
        return 0;
    }
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
fn tcp_listen_impl(host: &str, port: i64) -> i64 {
    use std::net::TcpListener;
    use std::os::unix::io::IntoRawFd;

    let host = if host.is_empty() { "0.0.0.0" } else { host };
    let bind_addr = format!("{}:{}", host, port);
    match TcpListener::bind(&bind_addr) {
        Ok(listener) => {
            let fd = listener.into_raw_fd();
            widen_backlog(fd);
            // green mode: a non-blocking listener lets accept yield instead of
            // parking the worker.
            if is_green() {
                fdio::set_nonblocking(fd);
            }
            open_socket(fd)
        }
        Err(_) => 0,
    }
}

/// TCP listen: bind and listen on addr:port, return the listener's handle
#[no_mangle]
pub unsafe extern "C" fn pith_tcp_listen(addr: *const i8, port: i64) -> i64 {
    // the first socket of the process is where SIGPIPE stops being fatal: a peer
    // that hangs up mid-response must give this server an EPIPE to handle, not a
    // signal that kills it. see signals::ignore_sigpipe.
    crate::signals::ignore_sigpipe();
    // SAFETY: Caller guarantees `addr` is null or a valid null-terminated C string.
    unsafe { with_cstr_str_or_empty(addr, |host| tcp_listen_impl(host, port)) }
}

fn tcp_connect_impl(host: &str, port: i64) -> i64 {
    use std::net::TcpStream;
    use std::os::unix::io::IntoRawFd;

    let host = if host.is_empty() { "127.0.0.1" } else { host };

    // green mode: a non-blocking connect that yields the task on EINPROGRESS.
    if is_green() {
        return green_connect(host, port);
    }

    let connect_addr = format!("{}:{}", host, port);
    let connected = {
        let _native = crate::cycle::native_bracket();
        TcpStream::connect(&connect_addr)
    };
    match connected {
        Ok(stream) => {
            // no read deadline: a fresh connection waits indefinitely, exactly as
            // the green path leaves it. a client that wants bounded reads sets
            // one explicitly via tcp.set_timeout.
            // disable nagle's algorithm: request/response protocols (http, grpc,
            // the db drivers) write small frames and then wait for a reply, which
            // otherwise collides with the peer's delayed acks for a ~40ms stall
            // per round-trip.
            let _ = stream.set_nodelay(true);
            open_socket(stream.into_raw_fd())
        }
        Err(_) => 0,
    }
}

/// TCP connect: connect to addr:port, return the connection's handle
#[no_mangle]
pub unsafe extern "C" fn pith_tcp_connect(addr: *const i8, port: i64) -> i64 {
    // as in pith_tcp_listen: a client writing to a server that closed first must
    // see EPIPE rather than be killed.
    crate::signals::ignore_sigpipe();
    // SAFETY: Caller guarantees `addr` is null or a valid null-terminated C string.
    unsafe { with_cstr_str_or_empty(addr, |host| tcp_connect_impl(host, port)) }
}

/// TCP accept: accept a connection on a listener handle, return the client's
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
            let fd = accept_raw(server_fd);
            if fd >= 0 {
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
    use std::os::unix::io::{FromRawFd, IntoRawFd};

    // SAFETY: `listener` guard keeps `server_fd` open; `into_raw_fd` below prevents closing it.
    let listener_ref = unsafe { TcpListener::from_raw_fd(server_fd) };
    let accepted = {
        let _native = crate::cycle::native_bracket();
        listener_ref.accept()
    };
    let result = match accepted {
        Ok((stream, _addr)) => open_socket(stream.into_raw_fd()),
        Err(_) => 0,
    };
    let _ = listener_ref.into_raw_fd();
    result
}
/// The address of the peer of a connected socket, as `ip:port` (`[ip]:port` for
/// ipv6), or an empty string for a stale handle or a socket with no peer.
#[no_mangle]
pub extern "C" fn pith_tcp_peer_address(conn: i64) -> *mut i8 {
    use std::mem::ManuallyDrop;
    use std::net::TcpStream;
    use std::os::unix::io::FromRawFd;

    let address = socket(conn).and_then(|guard| {
        // the guard keeps the descriptor open, and ManuallyDrop leaves it open
        let stream = ManuallyDrop::new(unsafe { TcpStream::from_raw_fd(guard.fd()) });
        stream.peer_addr().ok()
    });
    crate::pith_strdup_string(&address.map(|a| a.to_string()).unwrap_or_default())
}
/// TCP set read timeout in milliseconds (0 = no timeout). a stale handle is
/// left alone.
#[no_mangle]
pub extern "C" fn pith_tcp_set_timeout(conn: i64, ms: i64) {
    let Some(guard) = socket(conn) else {
        return;
    };

    use std::net::TcpStream;
    use std::os::unix::io::{FromRawFd, IntoRawFd};

    // SAFETY: `guard` keeps `guard.fd()` open; `into_raw_fd` below prevents closing it.
    let stream = unsafe { TcpStream::from_raw_fd(guard.fd()) };
    if ms <= 0 {
        let _ = stream.set_read_timeout(None);
    } else {
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(ms as u64)));
    }
    let _ = stream.into_raw_fd();
    // the next wait reads the value back, as the kernel rounded it.
    fd_handle::cache_timeout(&guard, false, None);
}

/// set or clear a connection's read or write deadline: `ms` milliseconds from
/// now, or none for `ms <= 0`. a call that would wait past it fails as an
/// expired socket timeout does. a stale handle is left alone.
fn set_deadline(conn: i64, write: bool, ms: i64) {
    let Some(guard) = socket(conn) else {
        return;
    };
    let at = crate::fdio::deadline_after_ms(ms);
    fd_handle::set_deadline(&guard, write, at);
}

/// the read deadline: every read on the connection, however many, must finish
/// waiting by then. it composes with `pith_tcp_set_timeout`, which bounds each
/// read on its own; a wait ends at whichever comes first.
#[no_mangle]
pub extern "C" fn pith_tcp_set_read_deadline(conn: i64, ms: i64) {
    set_deadline(conn, false, ms);
}

/// the write deadline, the counterpart of `pith_tcp_set_read_deadline`.
#[no_mangle]
pub extern "C" fn pith_tcp_set_write_deadline(conn: i64, ms: i64) {
    set_deadline(conn, true, ms);
}
