//! terminal builtins: raw mode, window size, and byte-level i/o on a tty.
//!
//! everything above these six calls — escape parsing, cell rendering, the
//! session object — is pure pith in `std/term/`. this file owns exactly the
//! parts that need a syscall: `isatty`, termios save/raw/restore, `TIOCGWINSZ`,
//! a readiness-gated byte read, and a complete-write loop.
//!
//! two invariants the design leans on:
//!
//! - **stdin's fd flags are never touched.** fd 0's open file description is
//!   shared with the parent shell, and an `O_NONBLOCK` left behind would break
//!   it after we exit. reads instead poll first (parking a green task on the
//!   reactor) and then issue one blocking `read` that cannot stall: raw mode
//!   sets `VMIN=1`/`VTIME=0` and poll just said a byte is there.
//! - **restore runs even when the runtime traps.** every runtime trap exits
//!   through `std::process::exit`, and `libc::atexit` handlers run on `exit`
//!   (the same mechanism `perf.rs` uses for its stats dump). entering raw mode
//!   arms a hook that puts the saved termios back and writes a fixed reset
//!   string, so a trap mid-frame still hands back a working shell. SIGKILL and
//!   a segfault bypass `exit` and are not covered.

use crate::bytes::{pith_bytes_from_vec, pith_bytes_ref};
use crate::fdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// the termios state `pith_term_raw_enter` saved, keyed by the fd it came
/// from. one terminal per process is the supported shape; entering raw mode
/// twice keeps the first (cooked) state so restore returns to it.
static SAVED: Mutex<Option<(i32, libc::termios)>> = Mutex::new(None);

/// whether the atexit restore hook is armed. armed once, on the first raw
/// enter; atexit registration is append-only, so once is enough.
static ATEXIT_ARMED: AtomicBool = AtomicBool::new(false);

/// leave the alternate screen, show the cursor, disable mouse/paste/focus
/// reporting, and reset styling. every sequence here is a no-op on a terminal
/// that never turned the feature on, so writing the whole string blind is
/// safe — cheap insurance against a session that died between enter and
/// restore.
const RESET_SEQ: &[u8] = b"\x1b[?1049l\x1b[?25h\x1b[?1006l\x1b[?1002l\x1b[?2004l\x1b[?1004l\x1b[0m";

/// put the saved termios back, if any. returns true when the terminal is in
/// its original state afterwards — including the case where nothing was ever
/// saved, which is already that state.
fn restore_saved() -> bool {
    let mut saved = match SAVED.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    match saved.take() {
        Some((fd, termios)) => {
            // SAFETY: writing back a termios we read from this same fd.
            unsafe { libc::tcsetattr(fd, libc::TCSANOW, &termios) == 0 }
        }
        None => true,
    }
}

/// the atexit hook: restore cooked mode and un-wedge the screen. runs on every
/// `exit`, including the `exit(1)` inside runtime traps.
extern "C" fn restore_at_exit() {
    if restore_saved() {
        // only bother with the reset string when a session actually entered raw
        // mode at some point — SAVED starts None and restore_saved() drains it,
        // so reaching here always means the hook was armed by a raw enter.
        // SAFETY: one plain write to stdout at process exit.
        unsafe {
            libc::write(1, RESET_SEQ.as_ptr() as *const libc::c_void, RESET_SEQ.len());
        }
    }
}

/// is `fd` a terminal? 1 or 0.
#[no_mangle]
pub extern "C" fn pith_term_is_tty(fd: i64) -> i64 {
    if fd < 0 {
        return 0;
    }
    // SAFETY: isatty on any fd is a read-only query.
    unsafe { i64::from(libc::isatty(fd as i32) == 1) }
}

/// save `fd`'s termios and enter raw mode (`cfmakeraw`, `VMIN=1`/`VTIME=0`).
/// arms the atexit restore hook the first time. 1 on success, 0 on failure or
/// a non-tty. entering twice is idempotent: the first saved state is kept.
#[no_mangle]
pub extern "C" fn pith_term_raw_enter(fd: i64) -> i64 {
    if fd < 0 {
        return 0;
    }
    let fd = fd as i32;
    // SAFETY: zeroed termios filled by tcgetattr before any read of it.
    unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut termios) != 0 {
            return 0;
        }
        let saved_state = termios;
        let mut raw = termios;
        libc::cfmakeraw(&mut raw);
        // one byte at a time, no read timeout: the poll in `pith_term_read`
        // owns all waiting, so a read issued after a successful poll returns
        // immediately with at least one byte.
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
            return 0;
        }
        let mut saved = match SAVED.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if saved.is_none() {
            *saved = Some((fd, saved_state));
        }
    }
    if !ATEXIT_ARMED.swap(true, Ordering::SeqCst) {
        // SAFETY: registering a plain extern "C" hook; atexit accepts exactly
        // this (see green.rs and perf.rs for the precedent).
        unsafe {
            libc::atexit(restore_at_exit);
        }
    }
    1
}

/// restore the termios saved by `pith_term_raw_enter`. 1 when the terminal is
/// back in its original state (including when nothing was saved), 0 when the
/// restore syscall itself failed. idempotent.
#[no_mangle]
pub extern "C" fn pith_term_restore(_fd: i64) -> i64 {
    i64::from(restore_saved())
}

/// the terminal size as `(rows << 16) | cols`, or -1 when `fd` is not a tty
/// or the query fails. packed so the ABI stays one plain integer; the pith
/// wrapper unpacks with shifts.
#[no_mangle]
pub extern "C" fn pith_term_size(fd: i64) -> i64 {
    if fd < 0 {
        return -1;
    }
    // SAFETY: ioctl writing a winsize of exactly the size we declare.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(fd as i32, libc::TIOCGWINSZ, &mut ws) != 0 {
            return -1;
        }
        if ws.ws_col == 0 && ws.ws_row == 0 {
            return -1;
        }
        ((ws.ws_row as i64) << 16) | (ws.ws_col as i64)
    }
}

/// read up to `max` bytes from `fd`, waiting at most `timeout_ms` (-1 waits
/// forever) for the first byte. returns a `Bytes` handle: non-empty on data,
/// **empty on timeout** (normal — the escape-sequence parser uses short
/// timeouts constantly), and `0` — the failure sentinel — on EOF or a hard
/// error, both of which mean the terminal session is over.
///
/// the wait parks a green task on the reactor exactly like a socket read; the
/// read itself is a plain blocking `read(2)` that poll has just cleared, so
/// stdin's fd flags never change.
#[no_mangle]
pub extern "C" fn pith_term_read(fd: i64, max: i64, timeout_ms: i64) -> i64 {
    if fd < 0 || max <= 0 {
        return 0;
    }
    match fdio::wait_ready_unguarded(fd, true, timeout_ms) {
        0 => return pith_bytes_from_vec(Vec::new()),
        1 => {}
        _ => return 0,
    }
    let size = std::cmp::min(max, 65536) as usize;
    let mut buf = vec![0u8; size];
    loop {
        // SAFETY: reading up to `size` bytes into a buffer we own, on an fd
        // poll just reported readable.
        let n = unsafe { libc::read(fd as i32, buf.as_mut_ptr() as *mut libc::c_void, size) };
        if n > 0 {
            buf.truncate(n as usize);
            return pith_bytes_from_vec(buf);
        }
        if n == 0 {
            // EOF: the terminal went away.
            return 0;
        }
        let err = fdio::errno();
        if err == libc::EINTR {
            continue;
        }
        return 0;
    }
}

/// write all of a `Bytes` value to `fd`, looping over partial writes. returns
/// the byte count on success, -1 on a bad handle or an error mid-write. no
/// newline is appended — that is the point.
#[no_mangle]
pub extern "C" fn pith_term_write(fd: i64, data: i64) -> i64 {
    if fd < 0 {
        return -1;
    }
    // SAFETY: the magic check inside rejects anything that is not a live
    // Bytes handle.
    let bytes = match unsafe { pith_bytes_ref(data) } {
        Some(b) => b,
        None => return -1,
    };
    let buf = &bytes.data;
    let mut written = 0usize;
    while written < buf.len() {
        let rest = &buf[written..];
        // SAFETY: writing from a slice we borrow for the duration of the call.
        let n = unsafe { libc::write(fd as i32, rest.as_ptr() as *const libc::c_void, rest.len()) };
        if n > 0 {
            written += n as usize;
            continue;
        }
        let err = fdio::errno();
        if err == libc::EINTR {
            continue;
        }
        if fdio::is_would_block(err) {
            // stdout stays blocking under this design, but a caller pointing
            // this at a non-blocking fd still deserves progress, not a lie.
            if fdio::wait_ready_unguarded(fd, false, -1) == 1 {
                continue;
            }
            return -1;
        }
        return -1;
    }
    written as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    /// tests that enter raw mode share the process-wide SAVED slot, so they
    /// take this lock to keep their save/restore pairs from interleaving.
    static RAW_LOCK: Mutex<()> = Mutex::new(());

    /// a pty pair: master fd for the "terminal side", slave fd for the
    /// process side. built from posix_openpt so the tests need no real tty.
    fn open_pty() -> (i32, i32) {
        unsafe {
            let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            assert!(master >= 0, "posix_openpt failed");
            assert_eq!(libc::grantpt(master), 0);
            assert_eq!(libc::unlockpt(master), 0);
            let name = libc::ptsname(master);
            assert!(!name.is_null());
            let slave = libc::open(name, libc::O_RDWR | libc::O_NOCTTY);
            assert!(slave >= 0, "open slave failed");
            (master, slave)
        }
    }

    fn close_pair(master: i32, slave: i32) {
        unsafe {
            libc::close(slave);
            libc::close(master);
        }
    }

    fn bytes_slice(handle: i64) -> Vec<u8> {
        unsafe { pith_bytes_ref(handle).expect("valid bytes handle").data.clone() }
    }

    #[test]
    fn is_tty_distinguishes_pty_from_pipe() {
        let (master, slave) = open_pty();
        assert_eq!(pith_term_is_tty(slave as i64), 1);
        let mut fds = [0i32; 2];
        unsafe { assert_eq!(libc::pipe(fds.as_mut_ptr()), 0) };
        assert_eq!(pith_term_is_tty(fds[0] as i64), 0);
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
        close_pair(master, slave);
    }

    #[test]
    fn raw_enter_and_restore_round_trip() {
        let _guard = RAW_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (master, slave) = open_pty();
        let mut before: libc::termios = unsafe { std::mem::zeroed() };
        unsafe { assert_eq!(libc::tcgetattr(slave, &mut before), 0) };
        assert_eq!(pith_term_raw_enter(slave as i64), 1);
        let mut raw: libc::termios = unsafe { std::mem::zeroed() };
        unsafe { assert_eq!(libc::tcgetattr(slave, &mut raw), 0) };
        // raw mode turned canonical processing and echo off.
        assert_eq!(raw.c_lflag & libc::ICANON, 0);
        assert_eq!(raw.c_lflag & libc::ECHO, 0);
        // and ISIG: ctrl-c must arrive as a byte, not a signal.
        assert_eq!(raw.c_lflag & libc::ISIG, 0);
        assert_eq!(raw.c_cc[libc::VMIN], 1);
        assert_eq!(raw.c_cc[libc::VTIME], 0);
        assert_eq!(pith_term_restore(slave as i64), 1);
        let mut after: libc::termios = unsafe { std::mem::zeroed() };
        unsafe { assert_eq!(libc::tcgetattr(slave, &mut after), 0) };
        assert_eq!(after.c_lflag, before.c_lflag);
        // restoring again with nothing saved is a clean no-op.
        assert_eq!(pith_term_restore(slave as i64), 1);
        close_pair(master, slave);
    }

    #[test]
    fn size_reports_what_the_pty_was_set_to() {
        let (master, slave) = open_pty();
        let ws = libc::winsize {
            ws_row: 40,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        unsafe { assert_eq!(libc::ioctl(slave, libc::TIOCSWINSZ, &ws), 0) };
        let packed = pith_term_size(slave as i64);
        assert_eq!(packed >> 16, 40);
        assert_eq!(packed & 0xffff, 120);
        close_pair(master, slave);
    }

    #[test]
    fn size_fails_on_a_pipe() {
        let mut fds = [0i32; 2];
        unsafe { assert_eq!(libc::pipe(fds.as_mut_ptr()), 0) };
        assert_eq!(pith_term_size(fds[0] as i64), -1);
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    #[test]
    fn read_returns_data_timeout_and_eof() {
        let _guard = RAW_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (master, slave) = open_pty();
        // raw mode, or the pty's canonical line buffer holds bytes until a
        // newline and the first read below times out instead of seeing data.
        assert_eq!(pith_term_raw_enter(slave as i64), 1);
        // data waiting: read returns it.
        let sent = b"ab";
        unsafe {
            assert_eq!(
                libc::write(master, sent.as_ptr() as *const libc::c_void, sent.len()),
                2
            )
        };
        let handle = pith_term_read(slave as i64, 4096, 1000);
        assert_ne!(handle, 0);
        assert_eq!(bytes_slice(handle), sent);
        // nothing waiting: a short timeout yields an empty Bytes, not a fail.
        let handle = pith_term_read(slave as i64, 4096, 30);
        assert_ne!(handle, 0);
        assert!(bytes_slice(handle).is_empty());
        // master closed: EOF is the 0 sentinel.
        assert_eq!(pith_term_restore(slave as i64), 1);
        unsafe { libc::close(master) };
        let handle = pith_term_read(slave as i64, 4096, 1000);
        assert_eq!(handle, 0);
        unsafe { libc::close(slave) };
    }

    #[test]
    fn ctrl_c_is_a_byte_under_raw_mode() {
        let _guard = RAW_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (master, slave) = open_pty();
        assert_eq!(pith_term_raw_enter(slave as i64), 1);
        unsafe {
            let b = [0x03u8];
            assert_eq!(libc::write(master, b.as_ptr() as *const libc::c_void, 1), 1);
        }
        let handle = pith_term_read(slave as i64, 16, 1000);
        assert_ne!(handle, 0);
        assert_eq!(bytes_slice(handle), vec![0x03]);
        assert_eq!(pith_term_restore(slave as i64), 1);
        close_pair(master, slave);
    }

    #[test]
    fn write_moves_every_byte_and_appends_nothing() {
        let (master, slave) = open_pty();
        let payload = b"abc".to_vec();
        let handle = pith_bytes_from_vec(payload.clone());
        assert_eq!(pith_term_write(slave as i64, handle), 3);
        let mut buf = [0u8; 16];
        let n = unsafe { libc::read(master, buf.as_mut_ptr() as *mut libc::c_void, 16) };
        assert_eq!(n, 3);
        assert_eq!(&buf[..3], payload.as_slice());
        close_pair(master, slave);
    }

    #[test]
    fn write_rejects_a_garbage_handle_and_a_bad_fd() {
        assert_eq!(pith_term_write(1, 12345), -1);
        let handle = pith_bytes_from_vec(b"x".to_vec());
        assert_eq!(pith_term_write(-1, handle), -1);
        assert_eq!(pith_term_read(-1, 16, 0), 0);
    }

    #[test]
    fn ptsname_smoke() {
        // keep CStr imported for future name-based assertions; also proves
        // the pty path yields a real device name.
        let (master, slave) = open_pty();
        let name = unsafe { CStr::from_ptr(libc::ptsname(master)) };
        assert!(name.to_string_lossy().starts_with("/dev/pts/"));
        close_pair(master, slave);
    }
}
