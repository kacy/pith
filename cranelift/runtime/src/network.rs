use crate::bytes::{forge_bytes_from_vec, forge_bytes_ref};

/// TCP listen — bind and listen on addr:port, return server fd
#[no_mangle]
pub unsafe extern "C" fn forge_tcp_listen(addr: *const i8, port: i64) -> i64 {
    use std::net::TcpListener;

    let host = if addr.is_null() {
        "0.0.0.0"
    } else {
        std::ffi::CStr::from_ptr(addr).to_str().unwrap_or("0.0.0.0")
    };
    let bind_addr = format!("{}:{}", host, port);
    match TcpListener::bind(&bind_addr) {
        Ok(listener) => {
            use std::os::unix::io::IntoRawFd;
            listener.into_raw_fd() as i64
        }
        Err(_) => 0,
    }
}

/// TCP connect — connect to addr:port, return connection fd
#[no_mangle]
pub unsafe extern "C" fn forge_tcp_connect(addr: *const i8, port: i64) -> i64 {
    use std::net::TcpStream;

    let host = if addr.is_null() {
        "127.0.0.1"
    } else {
        std::ffi::CStr::from_ptr(addr).to_str().unwrap_or("127.0.0.1")
    };
    let connect_addr = format!("{}:{}", host, port);
    match TcpStream::connect(&connect_addr) {
        Ok(stream) => {
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
            use std::os::unix::io::IntoRawFd;
            stream.into_raw_fd() as i64
        }
        Err(_) => 0,
    }
}

/// TCP accept — accept a connection on a server fd, return client fd
#[no_mangle]
pub extern "C" fn forge_tcp_accept(server_fd: i64) -> i64 {
    if server_fd <= 0 {
        return 0;
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

/// TCP read — read up to 4096 bytes from connection fd, return as C string
#[no_mangle]
pub extern "C" fn forge_tcp_read(conn_fd: i64) -> *mut i8 {
    use std::io::Read;
    use std::net::TcpStream;
    use std::os::unix::io::FromRawFd;

    if conn_fd <= 0 {
        return std::ptr::null_mut();
    }
    let mut stream = unsafe { TcpStream::from_raw_fd(conn_fd as i32) };
    let mut buf = vec![0u8; 4096];
    let result = match stream.read(&mut buf) {
        Ok(n) => {
            buf.truncate(n);
            let s = String::from_utf8_lossy(&buf).to_string();
            crate::forge_strdup_string(&s)
        }
        Err(_) => std::ptr::null_mut(),
    };
    use std::os::unix::io::IntoRawFd;
    let _ = stream.into_raw_fd();
    result
}

/// TCP read with max bytes limit
#[no_mangle]
pub extern "C" fn forge_tcp_read2(conn_fd: i64, max_bytes: i64) -> *mut i8 {
    use std::io::Read;
    use std::net::TcpStream;
    use std::os::unix::io::FromRawFd;

    if conn_fd <= 0 {
        return std::ptr::null_mut();
    }
    let mut stream = unsafe { TcpStream::from_raw_fd(conn_fd as i32) };
    let size = if max_bytes > 0 { max_bytes as usize } else { 4096 };
    let mut buf = vec![0u8; size];
    let result = match stream.read(&mut buf) {
        Ok(n) => {
            buf.truncate(n);
            let s = String::from_utf8_lossy(&buf).to_string();
            crate::forge_strdup_string(&s)
        }
        Err(_) => std::ptr::null_mut(),
    };
    use std::os::unix::io::IntoRawFd;
    let _ = stream.into_raw_fd();
    result
}

#[no_mangle]
pub extern "C" fn forge_tcp_read_bytes(conn_fd: i64, max_bytes: i64) -> i64 {
    use std::io::Read;
    use std::net::TcpStream;
    use std::os::unix::io::FromRawFd;

    if conn_fd <= 0 {
        return 0;
    }
    let mut stream = unsafe { TcpStream::from_raw_fd(conn_fd as i32) };
    let size = if max_bytes > 0 { max_bytes as usize } else { 4096 };
    let mut buf = vec![0u8; size];
    let result = match stream.read(&mut buf) {
        Ok(n) => {
            buf.truncate(n);
            forge_bytes_from_vec(buf)
        }
        Err(_) => 0,
    };
    use std::os::unix::io::IntoRawFd;
    let _ = stream.into_raw_fd();
    result
}

fn forge_tcp_wait(fd: i64, events: i16, timeout_ms: i64) -> i64 {
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

#[no_mangle]
pub extern "C" fn forge_tcp_wait_readable(fd: i64, timeout_ms: i64) -> i64 {
    forge_tcp_wait(fd, libc::POLLIN, timeout_ms)
}

#[no_mangle]
pub extern "C" fn forge_tcp_wait_writable(fd: i64, timeout_ms: i64) -> i64 {
    forge_tcp_wait(fd, libc::POLLOUT, timeout_ms)
}

/// TCP write — write data to connection fd, return bytes written
#[no_mangle]
pub unsafe extern "C" fn forge_tcp_write(conn_fd: i64, data: *const i8) -> i64 {
    use std::io::Write;
    use std::net::TcpStream;
    use std::os::unix::io::FromRawFd;

    if conn_fd <= 0 {
        return 0;
    }
    let mut stream = TcpStream::from_raw_fd(conn_fd as i32);
    let s = std::ffi::CStr::from_ptr(data).to_str().unwrap_or("");
    let result = match stream.write(s.as_bytes()) {
        Ok(n) => n as i64,
        Err(_) => 0,
    };
    let _ = stream.flush();
    use std::os::unix::io::IntoRawFd;
    let _ = stream.into_raw_fd();
    result
}

#[no_mangle]
pub unsafe extern "C" fn forge_tcp_write_bytes(conn_fd: i64, data: i64) -> i64 {
    use std::io::Write;
    use std::net::TcpStream;
    use std::os::unix::io::FromRawFd;

    let Some(bytes) = forge_bytes_ref(data) else {
        return 0;
    };
    if conn_fd <= 0 {
        return 0;
    }
    let mut stream = TcpStream::from_raw_fd(conn_fd as i32);
    let result = match stream.write(&bytes.data) {
        Ok(n) => n as i64,
        Err(_) => 0,
    };
    use std::os::unix::io::IntoRawFd;
    let _ = stream.into_raw_fd();
    result
}

/// TCP set read timeout in milliseconds (0 = no timeout)
#[no_mangle]
pub extern "C" fn forge_tcp_set_timeout(fd: i64, ms: i64) {
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
pub extern "C" fn forge_tcp_close(fd: i64) {
    if fd <= 0 {
        return;
    }

    use std::net::TcpStream;
    use std::os::unix::io::FromRawFd;

    drop(unsafe { TcpStream::from_raw_fd(fd as i32) });
}

/// DNS resolve — resolve hostname to IP address string
#[no_mangle]
pub unsafe extern "C" fn forge_dns_resolve(hostname: *const i8) -> *mut i8 {
    use std::net::ToSocketAddrs;

    if hostname.is_null() {
        return std::ptr::null_mut();
    }
    let host = std::ffi::CStr::from_ptr(hostname).to_str().unwrap_or("");
    let addr_str = format!("{}:0", host);
    match addr_str.to_socket_addrs() {
        Ok(mut addrs) => {
            if let Some(addr) = addrs.next() {
                crate::forge_strdup_string(&addr.ip().to_string())
            } else {
                std::ptr::null_mut()
            }
        }
        Err(_) => std::ptr::null_mut(),
    }
}
