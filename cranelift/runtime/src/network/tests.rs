use super::io::*;
use super::unix::*;
use super::*;

/// whether `fd` has close-on-exec and non-blocking set.
fn cloexec_and_nonblocking(fd: RawFd) -> (bool, bool) {
    // SAFETY: reading the flags of a descriptor the test owns.
    let fd_flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    // SAFETY: as above.
    let fl_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    (
        fd_flags & libc::FD_CLOEXEC != 0,
        fl_flags & libc::O_NONBLOCK != 0,
    )
}

#[test]
fn green_accept_and_connect_sockets_are_cloexec() {
    use std::os::unix::io::AsRawFd;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let _client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let accepted = accept_raw(listener.as_raw_fd());
    assert!(accepted >= 0, "accept: {}", std::io::Error::last_os_error());
    assert_eq!(cloexec_and_nonblocking(accepted), (true, true));

    let created = stream_socket(libc::AF_INET);
    assert!(created >= 0);
    assert_eq!(cloexec_and_nonblocking(created), (true, true));

    // SAFETY: closing the descriptors this test created.
    unsafe {
        libc::close(accepted);
        libc::close(created);
    }
}

#[test]
fn dns_resolve_rejects_null_and_invalid_utf8() {
    let invalid = [0xffu8, 0x00];

    unsafe {
        assert!(pith_dns_resolve(std::ptr::null()).is_null());
        assert!(pith_dns_resolve(invalid.as_ptr() as *const i8).is_null());
    }
}

// a burst of connections larger than std's default backlog of 128 must
// all complete their handshakes while nothing is accepting yet. with the
// default backlog the kernel drops the SYNs past the queue limit and the
// connects below time out.
#[test]
fn listener_queues_a_burst_past_the_default_backlog() {
    use std::os::unix::io::AsRawFd;
    const BURST: usize = 300;
    let max = std::fs::read_to_string("/proc/sys/net/core/somaxconn")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(0);
    if max <= BURST {
        return;
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    widen_backlog(listener.as_raw_fd());
    let addr = listener.local_addr().unwrap();
    let clients: Vec<_> = (0..BURST)
        .map(|i| {
            std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(500))
                .unwrap_or_else(|e| panic!("connect {i} of {BURST} failed: {e}"))
        })
        .collect();
    assert_eq!(clients.len(), BURST);
}

#[cfg(unix)]
#[test]
fn unix_socket_listen_connect_accept_roundtrip() {
    let dir = std::env::temp_dir().join(format!(
        "pith-unix-rt-test-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let sock_path = dir.join("test.sock");
    let _ = std::fs::remove_file(&sock_path);
    let cpath = std::ffi::CString::new(sock_path.to_str().unwrap()).unwrap();

    let listener = unsafe { pith_unix_listen(cpath.as_ptr()) };
    assert!(listener != 0, "pith_unix_listen failed");
    let client = unsafe { pith_unix_connect(cpath.as_ptr()) };
    assert!(client != 0, "pith_unix_connect failed");
    let server = pith_unix_accept(listener);
    assert!(server != 0, "pith_unix_accept failed");

    let payload = b"hello unix";
    let written = socket_write(client, payload).unwrap();
    assert_eq!(written, payload.len());
    let received = socket_read(server, 64).unwrap();
    assert_eq!(received, payload);

    pith_tcp_close(server);
    pith_tcp_close(client);
    pith_tcp_close(listener);
    let _ = std::fs::remove_file(&sock_path);
    let _ = std::fs::remove_dir(&dir);
}
