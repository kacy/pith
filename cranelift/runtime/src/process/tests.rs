use super::io::*;
use super::spawn::*;
use super::wait::*;
use super::*;

#[test]
fn closing_stdin_lets_a_child_read_to_the_end() {
    use crate::process_io::{pith_process_read, pith_process_write};
    use std::ffi::CStr;

    let handle = unsafe { pith_process_spawn(c"wc -l".as_ptr()) };
    assert_ne!(handle, 0);
    let written = unsafe { pith_process_write(handle, c"a\nb\nc\n".as_ptr()) };
    assert_eq!(written.is_ok, 1);
    assert_eq!(pith_process_close_stdin(handle), 1);
    assert_eq!(pith_process_close_stdin(handle), 0);
    let written = unsafe { pith_process_write(handle, c"d\n".as_ptr()) };
    assert_eq!(written.is_ok, 0);
    let out = unsafe { pith_process_read(handle, 4096) };
    let text = unsafe { CStr::from_ptr(out) }.to_str().unwrap();
    assert_eq!(text.trim(), "3");
    assert_eq!(pith_process_wait(handle), 0);
    pith_process_close(handle);
    assert_eq!(pith_process_close_stdin(handle), 0);
}

#[test]
fn a_child_closed_before_it_exits_is_reaped_later() {
    let child = Command::new("/bin/sh")
        .args(["-c", "sleep 0.05"])
        .spawn()
        .expect("sh should run");
    let pid = child.id() as libc::pid_t;
    let handle = pith_store_process_handle(child);
    pith_process_close(handle);
    let mut reaped = false;
    for _ in 0..400 {
        reap_unreaped();
        if !UNREAPED.lock().iter().any(|c| c.id() as libc::pid_t == pid) {
            reaped = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(reaped, "the closed child was never reaped");
    // the pid is gone: a reaped child cannot be waited for again
    // SAFETY: waitpid with WNOHANG on a pid this test spawned.
    let rc = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
    assert_eq!(rc, -1);
}

// unit tests never run on a green worker, so these exercise the inline arm
// of the offload split; the pool arm is the same closure on another thread.
#[test]
fn command_output_captures_both_pipes_and_the_exit_code() {
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "echo out; echo err >&2; exit 3"]);
    let output = command_output(cmd).expect("sh should run");
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(output.stdout, b"out\n");
    assert_eq!(output.stderr, b"err\n");
}

#[test]
fn command_status_reports_the_exit_code() {
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "exit 5"]);
    assert_eq!(command_status(cmd).expect("sh should run").code(), Some(5));
    assert!(command_status(Command::new("/nonexistent-program")).is_none());
}

#[test]
fn a_wait_without_a_pidfd_reports_a_signal_death_as_128_plus_the_signal() {
    let child = Command::new("/bin/sh")
        .args(["-c", "kill -9 $$"])
        .spawn()
        .expect("sh should run");
    let handle = pith_store_process_handle(child);
    assert_eq!(poll_until_exit(handle), 137);
    pith_process_close(handle);
}

#[test]
fn exit_code_reports_a_signal_death_as_128_plus_the_signal() {
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "kill -9 $$"]);
    assert_eq!(exit_code(command_status(cmd).expect("sh should run")), 137);
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "exit 4"]);
    assert_eq!(exit_code(command_status(cmd).expect("sh should run")), 4);
}

#[test]
fn invalid_process_handles_return_safe_defaults() {
    assert_eq!(pith_process_wait(12345), -1);
    assert_eq!(pith_process_kill(12345), 0);
    pith_process_close(12345);
}

#[test]
fn process_spawn_rejects_null_and_invalid_utf8() {
    let invalid = [0xffu8, 0x00];

    unsafe {
        assert_eq!(pith_process_spawn(std::ptr::null()), 0);
        assert_eq!(pith_process_spawn(invalid.as_ptr() as *const i8), 0);
    }
}

#[test]
fn closed_process_output_handles_are_rejected() {
    let handle = pith_store_process_output(7, "out".to_string(), "err".to_string());
    assert_eq!(pith_process_output_status(handle), 7);
    assert!(!pith_process_output_stdout(handle).is_null());
    assert!(!pith_process_output_stderr(handle).is_null());

    pith_process_output_close(handle);
    assert_eq!(pith_process_output_status(handle), -1);
    assert!(pith_process_output_stdout(handle).is_null());
    assert!(pith_process_output_stderr(handle).is_null());
    pith_process_output_close(handle);
}

use crate::ffi_util::unbox_result;

// a wait on one thread and a drain on another, off green. the wait used to
// hold the registry lock while it blocked, so the reader could not get its
// pipe and a child with more than a pipe buffer to write never exited.
#[test]
fn a_wait_does_not_block_a_reader_draining_the_same_child() {
    use crate::process::{pith_process_close, pith_process_spawn, pith_process_wait};

    let cmd = c"head -c 300000 /dev/zero";
    let handle = unsafe { pith_process_spawn(cmd.as_ptr()) };
    assert_ne!(handle, 0);
    let (done, waited) = std::sync::mpsc::channel();
    std::thread::spawn(move || done.send(pith_process_wait(handle)).unwrap());
    let (sent, drained) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut total = 0;
        while let PipeRead::Data(buf) = read_pipe(handle, Pipe::Stdout, 65536) {
            if buf.is_empty() {
                break;
            }
            total += buf.len();
        }
        sent.send(total).unwrap();
    });
    // the bound only turns the old hang into a failure.
    let limit = std::time::Duration::from_secs(30);
    assert_eq!(drained.recv_timeout(limit), Ok(300000));
    assert_eq!(waited.recv_timeout(limit), Ok(0));
    pith_process_close(handle);
}

#[test]
fn invalid_process_io_handles_return_safe_defaults() {
    unsafe {
        assert!(pith_process_read(12345, 16).is_null());
        assert_eq!(pith_process_read_bytes(12345, 16), 0);
        assert!(pith_process_read_err(12345, 16).is_null());
        assert_eq!(pith_process_read_err_bytes(12345, 16), 0);
        assert_eq!(
            unbox_result(pith_process_write(12345, std::ptr::null())),
            Err("process_write failed: invalid string".to_string())
        );
        assert_eq!(
            unbox_result(pith_process_write_bytes(12345, 0)),
            Err("process_write_bytes failed: invalid bytes value".to_string())
        );
        assert_eq!(
            unbox_result(pith_process_write(12345, c"".as_ptr())),
            Err("process_write failed: invalid or closed process handle".to_string())
        );
    }
}

#[test]
fn a_non_positive_read_size_asks_for_the_default_chunk() {
    assert_eq!(fdio::chunk_size(0), 4096);
    assert_eq!(fdio::chunk_size(-1), 4096);
    assert_eq!(fdio::chunk_size(7), 7);
}
