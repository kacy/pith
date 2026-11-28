//! Streaming open file handles (`pith_file_open_*`, `read`, `write`, `close`).

use super::POOL;
use crate::blocking;
use crate::bytes::pith_bytes_from_vec;
use crate::ffi_util::{with_cstr_str, write_bytes_result, write_cstr_result, ResultPair};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::fs::File;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::OnceLock;

/// open files by handle. a handle is a counter value that is never reused, so
/// a closed handle can never name a later file (see `fd_handle` for why
/// sockets need more than this).
static FILE_HANDLES: OnceLock<Mutex<HashMap<i64, File>>> = OnceLock::new();
static NEXT_FILE_HANDLE: AtomicI64 = AtomicI64::new(1);

fn file_handles() -> &'static Mutex<HashMap<i64, File>> {
    FILE_HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// the open itself, with no pith types anywhere near it so it can run on a pool
/// thread.
pub(super) fn open_file_at(path: &str, create: bool, write: bool, append: bool) -> Option<File> {
    use std::fs::OpenOptions;

    let mut options = OpenOptions::new();
    options.read(!write && !append);
    options.write(write || append);
    options.create(create || append);
    options.truncate(write && !append);
    options.append(append);
    options.open(path).ok()
}

/// give an opened file its pith-visible handle. always runs on the calling
/// task's thread, never on a pool thread.
fn register_file(file: File) -> i64 {
    let handle = NEXT_FILE_HANDLE.fetch_add(1, Ordering::Relaxed);
    file_handles().lock().insert(handle, file);
    handle
}

/// a dup of the open file behind `handle`, or `None` if the handle is unknown.
///
/// the offloaded read and write paths work on this clone so the handle table's
/// lock is never held across a park. holding it there would deadlock at one
/// worker: the parked task cannot resume to release the lock while its worker
/// sits blocked on that same lock for another task. `try_clone` is `dup(2)`, so
/// the clone shares the file description and therefore the offset: a
/// sequential read still picks up where the last one left off.
pub(super) fn clone_file(handle: i64) -> Option<File> {
    file_handles().lock().get(&handle)?.try_clone().ok()
}

/// read up to `size` bytes, `None` on error. `Some` of an empty vec is eof.
fn read_chunk(file: &mut File, size: usize) -> Option<Vec<u8>> {
    use std::io::Read;

    let mut buf = vec![0u8; size];
    match file.read(&mut buf) {
        Ok(n) => {
            buf.truncate(n);
            Some(buf)
        }
        Err(_) => None,
    }
}

/// write once, returning the bytes accepted. a short write is reported as it
/// happened; the caller loops. a failure is the os's description of it
/// (`Bad file descriptor (os error 9)` for a handle opened for reading).
fn write_chunk(file: &mut File, data: &[u8]) -> FileWrite {
    use std::io::Write;

    FileWrite(file.write(data).map_err(|err| err.to_string()))
}

/// the outcome of one file write, which may come back from a pool thread.
/// `blocking::run` reports a job that panicked as the default, so the default
/// is a failure with a reason of its own rather than a count.
struct FileWrite(Result<usize, String>);

impl Default for FileWrite {
    fn default() -> Self {
        FileWrite(Err("the write did not complete".to_string()))
    }
}

/// the reason a write names when its handle is not in the table: never
/// opened, or closed since.
const UNKNOWN_FILE_HANDLE: &str = "invalid or closed file handle";

/// open the file at `path` and register it, returning its handle or 0 on failure.
fn open_file_handle_str(path_str: &str, create: bool, write: bool, append: bool) -> i64 {
    // the path is the only thing to hand over and it is short, so this one does
    // not bother with a separate direct arm: `blocking::run` runs the open
    // right here when there is no worker to protect.
    let path = path_str.to_string();
    match blocking::run(&POOL, move || open_file_at(&path, create, write, append)) {
        Some(file) => register_file(file),
        None => 0,
    }
}

/// open the file at `path` (a C string) and register it, returning its handle
/// or 0 on failure. the shared body of the three `pith_file_open_*` exports.
unsafe fn open_file_handle(path: *const i8, create: bool, write: bool, append: bool) -> i64 {
    // SAFETY: Caller guarantees `path` is null or a valid null-terminated C string.
    unsafe { with_cstr_str(path, |s| open_file_handle_str(s, create, write, append)) }.unwrap_or(0)
}
#[no_mangle]
pub unsafe extern "C" fn pith_file_open_read(path: *const i8) -> i64 {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { open_file_handle(path, false, false, false) }
}

#[no_mangle]
pub unsafe extern "C" fn pith_file_open_write(path: *const i8) -> i64 {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { open_file_handle(path, true, true, false) }
}

#[no_mangle]
pub unsafe extern "C" fn pith_file_open_append(path: *const i8) -> i64 {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { open_file_handle(path, true, false, true) }
}

/// read one chunk from an open handle, on a pool thread when there is a worker
/// to protect. `None` covers both an unknown handle and a read error, which is
/// what the callers already collapse them to.
pub(super) fn read_handle_chunk(handle: i64, max_bytes: i64) -> Option<Vec<u8>> {
    let size = crate::fdio::chunk_size(max_bytes);
    let mut file = clone_file(handle)?;
    blocking::run(&POOL, move || read_chunk(&mut file, size))
}

/// write one chunk to an open handle, on a pool thread when there is a worker
/// to protect. returns the bytes accepted, `Ok(0)` for an empty chunk, or the
/// reason the write failed: an unknown or closed handle, or the os error. an
/// empty chunk makes no syscall on either path, so it succeeds on any open
/// handle, one opened for reading included, as an empty pipe or socket write
/// does.
fn write_handle_chunk(handle: i64, data: &[u8]) -> Result<usize, String> {
    let cloned = match file_handles().lock().get(&handle) {
        Some(_) if data.is_empty() => return Ok(0),
        Some(file) => file.try_clone(),
        None => return Err(UNKNOWN_FILE_HANDLE.to_string()),
    };
    let mut file = cloned.map_err(|err| err.to_string())?;
    if !blocking::offloads() {
        let _native = crate::cycle::native_bracket();
        return write_chunk(&mut file, data).0;
    }
    let data = data.to_vec();
    blocking::run(&POOL, move || write_chunk(&mut file, &data)).0
}

#[no_mangle]
pub unsafe extern "C" fn pith_file_read(handle: i64, max_bytes: i64) -> *mut i8 {
    match read_handle_chunk(handle, max_bytes) {
        Some(buf) if buf.is_empty() => crate::pith_cstring_empty(),
        Some(buf) => crate::pith_copy_bytes_to_cstring(&buf),
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_file_read_bytes(handle: i64, max_bytes: i64) -> i64 {
    match read_handle_chunk(handle, max_bytes) {
        // the `Bytes` object is built here, on the calling task's thread, out
        // of the plain vec the read produced.
        Some(buf) => pith_bytes_from_vec(buf),
        None => 0,
    }
}

/// one write of a string's bytes to an open file, as a result pair holding the
/// count (`0` for an empty string, which is a success) or the failure's
/// reason.
#[no_mangle]
pub unsafe extern "C" fn pith_file_write(handle: i64, data: *const i8) -> ResultPair {
    // SAFETY: Caller guarantees `data` is null or a valid null-terminated C string.
    unsafe { write_cstr_result("file_write", data, |text| write_handle_chunk(handle, text)) }
}

/// the `Bytes` form of `pith_file_write`.
#[no_mangle]
pub unsafe extern "C" fn pith_file_write_bytes(handle: i64, data: i64) -> ResultPair {
    // SAFETY: `write_bytes_result` validates `data` before borrowing its slice.
    unsafe { write_bytes_result("file_write_bytes", data, |bytes| write_handle_chunk(handle, bytes)) }
}

#[no_mangle]
pub extern "C" fn pith_file_close(handle: i64) {
    file_handles().lock().remove(&handle);
}
