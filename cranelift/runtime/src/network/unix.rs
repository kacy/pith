//! Unix domain socket listen, connect, and accept builtins.

use super::{
    accept_raw, close_raw, is_green, open_socket, socket, socket_error, stream_socket,
    widen_backlog,
};
use crate::fdio;
use crate::ffi_util::with_cstr_str;

/// green-mode connect to an AF_UNIX stream socket at `path`. returns the
/// registered socket handle on success or `0` on failure.
#[cfg(unix)]
fn green_unix_connect(path: &str) -> i64 {
    let bytes = path.as_bytes();
    if bytes.is_empty() || bytes.contains(&0) {
        return 0;
    }
    let fd = stream_socket(libc::AF_UNIX);
    if fd < 0 {
        return 0;
    }
    // SAFETY: sockaddr_un is plain-old-data; zeroing initializes sun_family,
    // sun_path (including the trailing NUL), and any platform-specific fields.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.len() >= addr.sun_path.len() {
        close_raw(fd);
        return 0;
    }
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (i, &b) in bytes.iter().enumerate() {
        addr.sun_path[i] = b as libc::c_char;
    }
    let len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1)
        as libc::socklen_t;
    // SAFETY: connecting a non-blocking AF_UNIX socket to a valid sockaddr_un.
    let rc = unsafe {
        libc::connect(
            fd,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            len,
        )
    };
    if rc != 0 {
        let err = fdio::errno();
        if err != libc::EINPROGRESS && err != libc::EAGAIN {
            close_raw(fd);
            return 0;
        }
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

#[cfg(unix)]
fn unix_listen_impl(path_str: &str) -> i64 {
    use std::os::unix::io::IntoRawFd;
    use std::os::unix::net::UnixListener;

    if path_str.is_empty() {
        return 0;
    }
    match UnixListener::bind(path_str) {
        Ok(listener) => {
            let fd = listener.into_raw_fd();
            widen_backlog(fd);
            if is_green() {
                fdio::set_nonblocking(fd);
            }
            open_socket(fd)
        }
        Err(_) => 0,
    }
}

/// Unix domain socket listen: bind and listen on `path`, returning a socket
/// handle compatible with `tcp_read*`, `tcp_write*`, `tcp_close`, and deadlines.
#[no_mangle]
pub unsafe extern "C" fn pith_unix_listen(path: *const i8) -> i64 {
    #[cfg(unix)]
    {
        crate::signals::ignore_sigpipe();
        // SAFETY: Caller guarantees `path` is null or a valid null-terminated C string.
        unsafe { with_cstr_str(path, unix_listen_impl) }.unwrap_or(0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        0
    }
}

#[cfg(unix)]
fn unix_connect_impl(path_str: &str) -> i64 {
    use std::os::unix::io::IntoRawFd;
    use std::os::unix::net::UnixStream;

    if path_str.is_empty() {
        return 0;
    }
    if is_green() {
        return green_unix_connect(path_str);
    }
    let connected = {
        let _native = crate::cycle::native_bracket();
        UnixStream::connect(path_str)
    };
    match connected {
        Ok(stream) => open_socket(stream.into_raw_fd()),
        Err(_) => 0,
    }
}

/// Unix domain socket connect: connect to an AF_UNIX stream socket at `path`,
/// returning a socket handle compatible with `tcp_read*`, `tcp_write*`, and `tcp_close`.
#[no_mangle]
pub unsafe extern "C" fn pith_unix_connect(path: *const i8) -> i64 {
    #[cfg(unix)]
    {
        crate::signals::ignore_sigpipe();
        // SAFETY: Caller guarantees `path` is null or a valid null-terminated C string.
        unsafe { with_cstr_str(path, unix_connect_impl) }.unwrap_or(0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        0
    }
}

/// Unix domain socket accept: accept a connection on a Unix listener handle,
/// returning a client socket handle.
#[no_mangle]
pub extern "C" fn pith_unix_accept(server: i64) -> i64 {
    #[cfg(unix)]
    {
        let Some(listener) = socket(server) else {
            return 0;
        };
        let server_fd = listener.fd();
        if is_green() {
            loop {
                let fd = accept_raw(server_fd);
                if fd >= 0 {
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
        use std::os::unix::io::{FromRawFd, IntoRawFd};
        use std::os::unix::net::UnixListener;

        // SAFETY: `listener` guard keeps `server_fd` open; `into_raw_fd` below prevents closing it.
        let listener_ref = unsafe { UnixListener::from_raw_fd(server_fd) };
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
    #[cfg(not(unix))]
    {
        let _ = server;
        0
    }
}
