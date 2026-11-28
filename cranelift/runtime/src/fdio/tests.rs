use super::transfer::*;
use super::wait::*;
use super::*;
use crate::fd_handle::{self, FdKind, Guard};

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

/// a connected stream socket pair, the shape every `Channel::Socket` call
/// site has. left blocking: these tests only read when something is there,
/// and a would-block off a green task waits in `poll` forever.
fn socket_pair() -> (RawFd, RawFd) {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: socketpair(2) fills the two-element array we pass it.
    let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
    assert_eq!(rc, 0);
    (fds[0], fds[1])
}

/// open a read-only fd on a regular file, a stand-in for the descriptor the
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
    // SAFETY: reading into a stack buffer; test expects failure on `fd`.
    let n = unsafe { Channel::Socket.read(fd, &mut buf) };
    assert_eq!(n, -1);
    errno()
}

/// the raw errno from one socket write to `fd`, which must fail.
fn socket_write_errno(fd: RawFd) -> i32 {
    let data = b"x";
    // SAFETY: writing one byte from a static buffer; test expects failure on `fd`.
    let n = unsafe { Channel::Socket.write(fd, data) };
    assert_eq!(n, -1);
    errno()
}

#[test]
fn a_write_round_trips_to_the_read_end() {
    let (read_end, write_end) = pipe();
    assert_eq!(write_yielding(&pipe_hold(write_end), b"pith"), Ok(4));
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

/// a handle closed by another task while a read is parked on it (the
/// green case the reactor handles) has an os-thread twin: the closer
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
    assert_eq!(socket_write_blocking(&guard, b"x"), Err("the handle was closed".to_string()));
    assert_eq!(socket_write_yielding(&guard, b"x"), Err("the handle was closed".to_string()));
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
    assert_eq!(write_yielding(&pipe_hold(write_end), b"x"), Ok(1));
    assert_eq!(wait_ready(read_end as i64, true, 0), 1);
    close(read_end);
    close(write_end);
}

#[test]
fn a_socket_moves_the_same_bytes_recv_and_send_as_read_and_write_did() {
    let (a, b) = socket_pair();
    assert_eq!(socket_write_yielding(&sock(a), b"pith"), Ok(4));
    assert_eq!(socket_read_yielding(&sock(b), 16).unwrap(), b"pith");
    // and the blocking pair, which is the os-thread backend's path.
    assert_eq!(socket_write_blocking(&sock(b), b"back"), Ok(4));
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

/// an empty write is a success that wrote nothing, on every path, where a
/// write to a gone reader is an error naming the errno. the two used to
/// share the return value 0 (#1153).
#[test]
fn an_empty_write_succeeds_and_a_gone_reader_is_an_error() {
    let broken = Err("Broken pipe (os error 32)".to_string());

    let (read_end, write_end) = pipe();
    assert_eq!(write_yielding(&pipe_hold(write_end), b""), Ok(0));
    close(read_end);
    assert_eq!(write_yielding(&pipe_hold(write_end), b"x"), broken);
    // with nothing asked for, the gone reader goes unnoticed.
    assert_eq!(write_yielding(&pipe_hold(write_end), b""), Ok(0));
    close(write_end);

    let (a, b) = socket_pair();
    assert_eq!(socket_write_yielding(&sock(a), b""), Ok(0));
    assert_eq!(socket_write_blocking(&sock(a), b""), Ok(0));
    close(b);
    assert_eq!(socket_write_yielding(&sock(a), b"x"), broken);
    assert_eq!(socket_write_blocking(&sock(a), b"x"), broken);
    close(a);
}

/// the socket syscalls refuse a descriptor that is not a socket, where
/// `read`/`write` would have used it: the invariant check behind
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
    assert_eq!(write_yielding(&pipe_hold(write_end), b"x"), Ok(1));
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
// these run off a green task, so `wait_ready` takes its `poll` branch; the
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
/// non-blocking syscall (which is the bug), but the kernel keeps it, so it
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
/// kernel's own jiffy rounding usually settles this first (500 µs comes
/// back as a whole tick), but the guard is what makes that a property
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

/// a pipe has no deadline to read (`SO_RCVTIMEO` is a socket option), so a
/// child process's stdout waits for its writer.
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

/// a handle caches its timeout on the first wait, and setting a new one
/// makes the next wait read it back rather than keep the old answer.
#[test]
fn a_timeout_set_after_a_wait_bounds_the_next_wait() {
    let (a, b) = socket_pair();
    set_nonblocking(a);
    let handle = fd_handle::open(a, FdKind::Socket);
    let guard = fd_handle::acquire(handle, FdKind::Socket).expect("live handle");
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        // SAFETY: one byte from a static buffer to a socket this test owns.
        unsafe { libc::send(b, b"x".as_ptr() as *const libc::c_void, 1, libc::MSG_NOSIGNAL) };
        b
    });
    // this wait finds no timeout and caches that.
    assert_eq!(socket_read_yielding(&guard, 16).map(|bytes| bytes.len()), Some(1));
    let b = writer.join().expect("writer panicked");
    crate::network::pith_tcp_set_timeout(handle, 150);
    let start = Instant::now();
    assert!(socket_read_yielding(&guard, 16).is_none());
    let waited = start.elapsed();
    assert!(waited >= Duration::from_millis(120), "gave up after {waited:?}");
    assert!(waited < Duration::from_secs(5), "waited {waited:?}");
    drop(guard);
    close(a);
    close(b);
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

/// a peer that sends a byte just inside the per-read timeout never trips
/// it, but a deadline is an instant, so a series of reads still ends there.
/// both paths: `a` is non-blocking (the yielding loop), `b` blocking (the
/// polled loop the os-thread backend takes once a deadline is set).
#[test]
fn a_drip_fed_read_gives_up_at_the_deadline_on_both_paths() {
    for blocking in [false, true] {
        let (a, b) = socket_pair();
        if !blocking {
            set_nonblocking(a);
        }
        let handle = fd_handle::open(a, FdKind::Socket);
        crate::network::pith_tcp_set_timeout(handle, 200);
        crate::network::pith_tcp_set_read_deadline(handle, 300);
        let writer = std::thread::spawn(move || {
            for _ in 0..40 {
                std::thread::sleep(Duration::from_millis(50));
                // SAFETY: one byte from a static buffer to a socket this test owns.
                if unsafe { libc::send(b, b"x".as_ptr() as *const libc::c_void, 1, libc::MSG_NOSIGNAL) } != 1 {
                    break;
                }
            }
            close(b);
        });
        let start = Instant::now();
        let mut got = 0;
        loop {
            // `sock` would register the number afresh, which drops its
            // deadline, so the reads hold the handle the deadline is on.
            let guard = fd_handle::acquire(handle, FdKind::Socket).expect("live");
            let read = if blocking {
                socket_read_blocking(&guard, 16)
            } else {
                socket_read_yielding(&guard, 16)
            };
            match read {
                Some(bytes) if !bytes.is_empty() => got += bytes.len(),
                _ => break,
            }
        }
        let waited = start.elapsed();
        assert!(got >= 2, "read {got} bytes before the deadline");
        assert!(waited >= Duration::from_millis(250), "gave up after {waited:?}");
        assert!(waited < Duration::from_millis(1500), "waited {waited:?}");
        assert!(fd_handle::close(handle));
        writer.join().unwrap();
    }
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
/// argument surviving the clamp; the reactor branch needs a green task and
/// is covered by the clamp being in `wait_ready` rather than in either arm.
#[test]
fn an_absurd_wait_is_clamped_rather_than_overflowing_a_deadline() {
    let (a, b) = socket_pair();
    assert_eq!(socket_write_yielding(&sock(b), b"x"), Ok(1));
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
    // ready immediately, so this does not actually block; what is being
    // checked is which deadline it resolved before waiting.
    assert_eq!(socket_write_yielding(&sock(b), b"x"), Ok(1));
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
/// socket. never returns: every arm is expected to abort.
fn make_the_bad_call(call: &str) -> ! {
    // a socket handle over a regular file: the bookkeeping mistake the
    // check exists for, made deliberately.
    let file = sock(open_a_file());
    match call {
        "read_yielding" => {
            socket_read_yielding(&file, 16);
        }
        "write_yielding" => {
            let _ = socket_write_yielding(&file, b"x");
        }
        "read_blocking" => {
            socket_read_blocking(&file, 16);
        }
        "write_blocking" => {
            let _ = socket_write_blocking(&file, b"x");
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

#[test]
fn deadline_and_chunk_size_clamp_extreme_inputs_without_panicking() {
    assert!(deadline_after_ms(0).is_none());
    assert!(deadline_after_ms(-5).is_none());
    assert!(deadline_after_ms(i64::MAX).is_some());
    assert_eq!(chunk_size(0), DEFAULT_CHUNK);
    assert_eq!(chunk_size(-10), DEFAULT_CHUNK);
    assert_eq!(chunk_size(i64::MAX), MAX_CHUNK);
}
