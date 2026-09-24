//! reading and writing a child process's pipes.
//!
//! a pipe is pollable, so none of this needs a thread pool: the read and write
//! loops in `fdio` yield a green task to the epoll reactor when the pipe is not
//! ready and retry when it is. `process` puts the fds in non-blocking mode at
//! spawn time under green; off a green task the same loops block, exactly as
//! this file always did.
//!
//! the one rule the shape here exists to keep is that the handle registry's
//! lock is never held across that yield. each entry point takes the lock only
//! long enough to look up the pipe's handle, then takes its own hold on that
//! handle (`fd_handle::acquire`) and does its syscall outside the lock. holding
//! it across a park would deadlock at one worker — the parked task cannot
//! resume to release the lock while its own worker sits blocked on that lock
//! for another task. the hold is what keeps the pipe's descriptor from being
//! closed and its number reused while the syscall is inside; a process handle
//! closed meanwhile makes the call fail rather than read another fd.

use crate::bytes::{pith_bytes_from_vec, pith_bytes_ref};
use crate::fd_handle::{self, FdKind, Guard};
use crate::ffi_util::{cstr_bytes, write_result};
use crate::fdio;
use crate::handle_registry::{is_valid_id, HandleKind};
use crate::process::{process_handles, Pipe};

/// what came back from one read of a child's pipe.
enum PipeRead {
    /// the bytes read. empty means end of stream.
    Data(Vec<u8>),
    /// the child has no such pipe, reported as end of stream — which is what
    /// this file has always done with it.
    Absent,
    /// an unknown handle or a failed read.
    Failed,
}

/// the byte count one read asks for. a non-positive `max_bytes` means "a
/// default chunk", as it always has.
fn chunk_size(max_bytes: i64) -> usize {
    if max_bytes > 0 {
        max_bytes as usize
    } else {
        4096
    }
}

/// a hold on one of a child's pipes, taking the registry lock only for the
/// lookup. `None` for an unknown process handle, or a pipe whose handle has
/// been retired since; `Some(None)` for a handle whose pipe was never
/// captured.
fn pipe_guard(handle: i64, pipe: Pipe) -> Option<Option<Guard>> {
    if !is_valid_id(handle, HandleKind::Process) {
        return None;
    }
    let pipe_handle = {
        let handles = process_handles().lock();
        handles.get(&handle)?.pipe(pipe)
    };
    match pipe_handle {
        Some(pipe_handle) => Some(Some(fd_handle::acquire(pipe_handle, FdKind::Pipe)?)),
        None => Some(None),
    }
}

/// read one chunk from a child's pipe.
fn read_pipe(handle: i64, pipe: Pipe, max_bytes: i64) -> PipeRead {
    let guard = match pipe_guard(handle, pipe) {
        Some(Some(guard)) => guard,
        Some(None) => return PipeRead::Absent,
        None => return PipeRead::Failed,
    };
    match fdio::read_yielding(&guard, chunk_size(max_bytes)) {
        Some(buf) => PipeRead::Data(buf),
        None => PipeRead::Failed,
    }
}

/// write one buffer to a child's stdin, returning the bytes it accepted,
/// `Ok(0)` for an empty buffer, or the reason the write failed: an unknown or
/// closed process handle, a child whose stdin was not captured, or a failed
/// write, which for a child that has exited or closed its stdin is `EPIPE`.
fn write_stdin(handle: i64, data: &[u8]) -> Result<usize, fdio::WriteError> {
    match pipe_guard(handle, Pipe::Stdin) {
        Some(Some(guard)) => fdio::write_yielding(&guard, data),
        Some(None) => Err("the process has no stdin pipe".to_string()),
        None => Err("invalid or closed process handle".to_string()),
    }
}

/// a chunk as a pith C string: null on failure, an empty string on eof.
///
/// # Safety
/// the returned pointer is owned by the caller and freed with `pith_free`.
unsafe fn read_as_cstring(handle: i64, pipe: Pipe, max_bytes: i64) -> *mut i8 {
    match read_pipe(handle, pipe, max_bytes) {
        PipeRead::Data(buf) if buf.is_empty() => crate::pith_cstring_empty(),
        PipeRead::Data(buf) => crate::pith_copy_bytes_to_cstring(&buf),
        PipeRead::Absent => crate::pith_cstring_empty(),
        PipeRead::Failed => std::ptr::null_mut(),
    }
}

/// a chunk as a `Bytes` handle: 0 on failure, an empty `Bytes` on eof. the
/// handle is built here, on the calling task's thread, out of a plain vec.
fn read_as_bytes(handle: i64, pipe: Pipe, max_bytes: i64) -> i64 {
    match read_pipe(handle, pipe, max_bytes) {
        PipeRead::Data(buf) => pith_bytes_from_vec(buf),
        PipeRead::Absent => pith_bytes_from_vec(Vec::new()),
        PipeRead::Failed => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_process_read(handle: i64, max_bytes: i64) -> *mut i8 {
    read_as_cstring(handle, Pipe::Stdout, max_bytes)
}

#[no_mangle]
pub unsafe extern "C" fn pith_process_read_bytes(handle: i64, max_bytes: i64) -> i64 {
    read_as_bytes(handle, Pipe::Stdout, max_bytes)
}

#[no_mangle]
pub unsafe extern "C" fn pith_process_read_err(handle: i64, max_bytes: i64) -> *mut i8 {
    read_as_cstring(handle, Pipe::Stderr, max_bytes)
}

#[no_mangle]
pub unsafe extern "C" fn pith_process_read_err_bytes(handle: i64, max_bytes: i64) -> i64 {
    read_as_bytes(handle, Pipe::Stderr, max_bytes)
}

/// one write of a string's bytes to a child's stdin, as a result box holding
/// the count (`0` for an empty string, which is a success) or the failure's
/// reason. the bytes go out as they are, valid utf-8 or not.
#[no_mangle]
pub unsafe extern "C" fn pith_process_write(handle: i64, data: *const i8) -> i64 {
    let outcome = match cstr_bytes(data) {
        Some(text) => write_stdin(handle, text),
        None => Err("invalid string".to_string()),
    };
    write_result("process_write", outcome)
}

/// the `Bytes` form of `pith_process_write`.
#[no_mangle]
pub unsafe extern "C" fn pith_process_write_bytes(handle: i64, data: i64) -> i64 {
    let outcome = match pith_bytes_ref(data) {
        Some(bytes) => write_stdin(handle, &bytes.data),
        None => Err("invalid bytes value".to_string()),
    };
    write_result("process_write_bytes", outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi_util::unbox_result;

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
        assert_eq!(chunk_size(0), 4096);
        assert_eq!(chunk_size(-1), 4096);
        assert_eq!(chunk_size(7), 7);
    }
}
