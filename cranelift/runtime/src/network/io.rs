//! Socket read, write, and readiness wait builtins.

use super::{is_green, socket};
use crate::bytes::pith_bytes_from_vec;
use crate::fdio;
use crate::ffi_util::{write_bytes_result, write_cstr_result, ResultPair};

/// one read from a connection handle, whichever backend is running: the green
/// path yields the task to the reactor on would-block, the os-thread path blocks
/// in the kernel. `None` is a failure (a stale handle, a closed connection, or
/// a handle closed by another task while this read was inside), and
/// `Some(empty)` is end of stream.
pub(super) fn socket_read(conn: i64, size: usize) -> Option<Vec<u8>> {
    let guard = socket(conn)?;
    if is_green() {
        fdio::socket_read_yielding(&guard, size)
    } else {
        fdio::socket_read_blocking(&guard, size)
    }
}

/// one write to a connection handle, the counterpart of `socket_read`. returns
/// the bytes accepted, `Ok(0)` for an empty buffer, or the reason the write
/// failed: a stale or closed handle, a closed peer (`EPIPE`, `ECONNRESET`), or
/// a handle closed by another task while the write was inside.
pub(super) fn socket_write(conn: i64, data: &[u8]) -> Result<usize, fdio::WriteError> {
    let Some(guard) = socket(conn) else {
        return Err("invalid or closed socket handle".to_string());
    };
    // no syscall for an empty buffer on either backend, as for a pipe or a
    // file: the blocking path's zero-byte `send` could only report the peer.
    if data.is_empty() {
        return Ok(0);
    }
    if is_green() {
        fdio::socket_write_yielding(&guard, data)
    } else {
        fdio::socket_write_blocking(&guard, data)
    }
}

/// TCP read: read up to `fdio::DEFAULT_CHUNK` bytes from a connection, returned
/// as a C string (lossy utf-8), or null on failure.
#[no_mangle]
pub extern "C" fn pith_tcp_read(conn_fd: i64) -> *mut i8 {
    match socket_read(conn_fd, fdio::DEFAULT_CHUNK) {
        Some(buf) => crate::pith_strdup_string(&String::from_utf8_lossy(&buf)),
        None => std::ptr::null_mut(),
    }
}

/// TCP read with max bytes limit
#[no_mangle]
pub extern "C" fn pith_tcp_read2(conn_fd: i64, max_bytes: i64) -> *mut i8 {
    match socket_read(conn_fd, fdio::chunk_size(max_bytes)) {
        Some(buf) => crate::pith_strdup_string(&String::from_utf8_lossy(&buf)),
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn pith_tcp_read_bytes(conn_fd: i64, max_bytes: i64) -> i64 {
    match socket_read(conn_fd, fdio::chunk_size(max_bytes)) {
        Some(buf) => pith_bytes_from_vec(buf),
        None => 0,
    }
}
/// wait for a handle to become readable or writable, the tri-state contract of
/// `fdio::wait_ready`: `1` ready, `0` timed out, `-1` an error, which covers
/// a stale handle and a handle closed while the wait was inside.
fn wait_handle(conn: i64, read: bool, timeout_ms: i64) -> i64 {
    let Some(guard) = socket(conn) else {
        return -1;
    };
    fdio::wait_guarded_level(&guard, read, timeout_ms)
}

#[no_mangle]
pub extern "C" fn pith_tcp_wait_readable(conn: i64, timeout_ms: i64) -> i64 {
    wait_handle(conn, true, timeout_ms)
}

#[no_mangle]
pub extern "C" fn pith_tcp_wait_writable(conn: i64, timeout_ms: i64) -> i64 {
    wait_handle(conn, false, timeout_ms)
}

/// one write of a string's bytes to a connection, as a result pair holding the
/// count (`0` for an empty string, which is a success) or the failure's
/// reason. the bytes go out as they are: a string that is not valid utf-8 is
/// still the bytes the caller asked to send.
#[no_mangle]
pub unsafe extern "C" fn pith_tcp_write(conn_fd: i64, data: *const i8) -> ResultPair {
    // SAFETY: Caller guarantees `data` is null or a valid null-terminated C string.
    unsafe { write_cstr_result("tcp_write", data, |text| socket_write(conn_fd, text)) }
}

/// the `Bytes` form of `pith_tcp_write`.
#[no_mangle]
pub unsafe extern "C" fn pith_tcp_write_bytes(conn_fd: i64, data: i64) -> ResultPair {
    // SAFETY: `write_bytes_result` validates `data` before borrowing its slice.
    unsafe { write_bytes_result("tcp_write_bytes", data, |bytes| socket_write(conn_fd, bytes)) }
}
