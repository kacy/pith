//! file and process-environment entry points.
//!
//! ## why half of this file goes through `blocking`
//!
//! a regular file is always reported ready by epoll, however slow the device
//! behind it is, so the reactor that keeps socket i/o off a green worker cannot
//! help here. anything that opens, reads, writes, or walks a file therefore
//! hands the syscall to the file thread pool and parks the calling task, the
//! same way dns does: see `blocking` for the pool, the park, and the rule about
//! what may cross to a pool thread.
//!
//! that rule is the reason these functions are shaped the way they are. the job
//! that runs on the pool thread deals only in plain owned data — a `String`
//! path, a `Vec<u8>`, a `std::fs::File` — and every pith-visible value (a
//! `Bytes` handle, a C string, a list) is built afterwards, back on the task's
//! own thread, where the thread-local freelists it comes from live.
//!
//! metadata and name operations — `exists`, `size`, `mkdir`, `rename`, the
//! single-file `remove` — stay direct. each is one syscall that the kernel
//! answers from its dentry cache in the overwhelmingly common case, which is
//! the same order of cost as the handoff itself; paying a park to protect
//! against a slow network mount would make the ordinary case worse for every
//! program. `remove_tree` and `list_dir` do not get that argument — their cost
//! grows with the tree — so those are offloaded.

use crate::blocking::{self, Pool};
use crate::bytes::{pith_bytes_from_vec, pith_bytes_ref};
use crate::collections::list::{pith_list_new, pith_list_push_value};
use crate::ffi_util::{cstr_bytes, cstr_str};
use crate::runtime_core::optional_tuple;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::fs::File;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::OnceLock;

/// the threads that run file syscalls for green tasks. file i/o has its own
/// pool rather than sharing the resolver's so that one slow multi-megabyte
/// write cannot queue in front of a dial, or the reverse.
static POOL: Pool = Pool::new("pith-file", 4);

static FILE_HANDLES: OnceLock<Mutex<HashMap<i64, File>>> = OnceLock::new();
static NEXT_FILE_HANDLE: AtomicI64 = AtomicI64::new(1);

fn file_handles() -> &'static Mutex<HashMap<i64, File>> {
    FILE_HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// the open itself, with no pith types anywhere near it so it can run on a pool
/// thread.
fn open_file_at(path: &str, create: bool, write: bool, append: bool) -> Option<File> {
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
/// the clone shares the file description and therefore the offset — a
/// sequential read still picks up where the last one left off.
fn clone_file(handle: i64) -> Option<File> {
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

/// the byte count a streaming write asks for, matching what the callers used to
/// compute inline: a non-positive `max_bytes` means "a default chunk".
fn chunk_size(max_bytes: i64) -> usize {
    if max_bytes > 0 {
        max_bytes as usize
    } else {
        4096
    }
}

/// write once, returning the bytes accepted. a short write is reported as it
/// happened; the caller loops.
fn write_chunk(file: &mut File, data: &[u8]) -> i64 {
    use std::io::Write;

    match file.write(data) {
        Ok(n) => n as i64,
        Err(_) => 0,
    }
}

unsafe fn pith_open_file_with(path: *const i8, create: bool, write: bool, append: bool) -> i64 {
    let Some(path_str) = cstr_str(path) else {
        return 0;
    };
    // the path is the only thing to hand over and it is short, so this one does
    // not bother with a separate direct arm: `blocking::run` runs the open
    // right here when there is no worker to protect.
    let path = path_str.to_string();
    match blocking::run(&POOL, move || open_file_at(&path, create, write, append)) {
        Some(file) => register_file(file),
        None => 0,
    }
}

/// Check if a file exists
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_file_exists(path: *const i8) -> i64 {
    if let Some(path_str) = cstr_str(path) {
        if std::path::Path::new(path_str).exists() {
            return 1;
        }
    }
    0
}

/// Check if a directory exists
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_dir_exists(path: *const i8) -> i64 {
    if let Some(path_str) = cstr_str(path) {
        let path = std::path::Path::new(path_str);
        if path.exists() && path.is_dir() {
            return 1;
        }
    }
    0
}

/// Create a directory
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_mkdir(path: *const i8) -> i64 {
    use std::fs;

    if let Some(path_str) = cstr_str(path) {
        if fs::create_dir_all(path_str).is_ok() {
            return 1;
        }
    }
    0
}

/// Remove an empty directory
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_remove_dir(path: *const i8) -> i64 {
    use std::fs;

    if let Some(path_str) = cstr_str(path) {
        if fs::remove_dir(path_str).is_ok() {
            return 1;
        }
    }
    0
}

/// Remove a directory tree recursively
///
/// Offloaded: unlinking a tree costs one syscall per entry in it, so unlike the
/// single-file removes this one can hold a worker for an unbounded time.
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_remove_tree(path: *const i8) -> i64 {
    let Some(path_str) = cstr_str(path) else {
        return 0;
    };
    let path = path_str.to_string();
    blocking::run(&POOL, move || std::fs::remove_dir_all(path).is_ok()) as i64
}

/// Read file size in bytes.
/// Returns -1 when metadata cannot be read.
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_file_size(path: *const i8) -> i64 {
    if let Some(path_str) = cstr_str(path) {
        if let Ok(meta) = std::fs::metadata(path_str) {
            return meta.len() as i64;
        }
    }
    -1
}

/// Remove a file
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_remove_file(path: *const i8) -> i64 {
    use std::fs;

    if let Some(path_str) = cstr_str(path) {
        if fs::remove_file(path_str).is_ok() {
            return 1;
        }
    }
    0
}

/// Rename a file
///
/// # Safety
/// Both paths must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn pith_rename_file(from: *const i8, to: *const i8) -> i64 {
    use std::fs;

    if let (Some(from_str), Some(to_str)) = (cstr_str(from), cstr_str(to)) {
        if fs::rename(from_str, to_str).is_ok() {
            return 1;
        }
    }
    0
}

/// Read entire file contents as a C string
/// Returns null pointer on error. Caller must free with pith_free.
///
/// # Safety
/// path must be a valid null-terminated C string
/// read a whole file as text, `None` if it cannot be read or is not utf-8.
fn read_file_text_at(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// replace a file's contents, or create it. true when the write landed.
fn write_file_at(path: &str, content: &[u8]) -> bool {
    std::fs::write(path, content).is_ok()
}

/// add to the end of a file, creating it if it is not there yet.
fn append_file_at(path: &str, content: &[u8]) -> bool {
    use std::fs::OpenOptions;
    use std::io::Write;

    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut file) => file.write_all(content).is_ok(),
        Err(_) => false,
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_read_file(path: *const i8) -> *mut i8 {
    let Some(path_str) = cstr_str(path) else {
        return std::ptr::null_mut();
    };
    let path = path_str.to_string();
    // the contents come back as a plain `String` and become a pith C string
    // here, on the calling task's thread.
    match blocking::run(&POOL, move || read_file_text_at(&path)) {
        Some(contents) => crate::pith_copy_bytes_to_cstring(contents.as_bytes()),
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_read_file_bytes(path: *const i8) -> i64 {
    let Some(path_str) = cstr_str(path) else {
        return 0;
    };
    let path = path_str.to_string();
    match blocking::run(&POOL, move || std::fs::read(&path).ok()) {
        Some(contents) => pith_bytes_from_vec(contents),
        None => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_write_file(path: *const i8, content: *const i8) -> i64 {
    // the content is read as a `str` rather than as raw bytes because that is
    // what this entry point has always done: a non-utf-8 payload fails the
    // write instead of landing on disk.
    let (Some(path_str), Some(content)) = (cstr_str(path), cstr_str(content)) else {
        return 0;
    };
    // handing the content to another thread means owning it, which for a write
    // means copying it. that copy is worth a syscall the worker would otherwise
    // sit through, but only when there is a worker to protect — so ask first
    // and write straight out of the caller's buffer when there is not.
    if !blocking::offloads() {
        return write_file_at(path_str, content.as_bytes()) as i64;
    }
    let (path, content) = (path_str.to_string(), content.as_bytes().to_vec());
    blocking::run(&POOL, move || write_file_at(&path, &content)) as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_write_file_bytes(path: *const i8, content: i64) -> i64 {
    let Some(bytes) = pith_bytes_ref(content) else {
        return 0;
    };
    let Some(path_str) = cstr_str(path) else {
        return 0;
    };
    if !blocking::offloads() {
        return write_file_at(path_str, &bytes.data) as i64;
    }
    // the copy is of the bytes, not of the handle: a `Bytes` object belongs to
    // the thread-local machinery of the task that made it and must not be
    // touched from a pool thread.
    let (path, content) = (path_str.to_string(), bytes.data.clone());
    blocking::run(&POOL, move || write_file_at(&path, &content)) as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_append_file(path: *const i8, content: *const i8) -> i64 {
    // as in `pith_write_file`, non-utf-8 content fails rather than being
    // appended, which is what this entry point has always done.
    let (Some(path_str), Some(content)) = (cstr_str(path), cstr_str(content)) else {
        return 0;
    };
    if !blocking::offloads() {
        return append_file_at(path_str, content.as_bytes()) as i64;
    }
    let (path, content) = (path_str.to_string(), content.as_bytes().to_vec());
    blocking::run(&POOL, move || append_file_at(&path, &content)) as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_append_file_bytes(path: *const i8, content: i64) -> i64 {
    let Some(bytes) = pith_bytes_ref(content) else {
        return 0;
    };
    let Some(path_str) = cstr_str(path) else {
        return 0;
    };
    if !blocking::offloads() {
        return append_file_at(path_str, &bytes.data) as i64;
    }
    let (path, content) = (path_str.to_string(), bytes.data.clone());
    blocking::run(&POOL, move || append_file_at(&path, &content)) as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_file_open_read(path: *const i8) -> i64 {
    pith_open_file_with(path, false, false, false)
}

#[no_mangle]
pub unsafe extern "C" fn pith_file_open_write(path: *const i8) -> i64 {
    pith_open_file_with(path, true, true, false)
}

#[no_mangle]
pub unsafe extern "C" fn pith_file_open_append(path: *const i8) -> i64 {
    pith_open_file_with(path, true, false, true)
}

/// read one chunk from an open handle, on a pool thread when there is a worker
/// to protect. `None` covers both an unknown handle and a read error, which is
/// what the callers already collapse them to.
fn read_handle_chunk(handle: i64, max_bytes: i64) -> Option<Vec<u8>> {
    let size = chunk_size(max_bytes);
    if !blocking::offloads() {
        let mut handles = file_handles().lock();
        return read_chunk(handles.get_mut(&handle)?, size);
    }
    let mut file = clone_file(handle)?;
    blocking::run(&POOL, move || read_chunk(&mut file, size))
}

/// write one chunk to an open handle, returning the bytes accepted. zero covers
/// an unknown handle, a failed write, and a genuinely empty one alike, as
/// before.
fn write_handle_chunk(handle: i64, data: &[u8]) -> i64 {
    if !blocking::offloads() {
        let mut handles = file_handles().lock();
        let Some(file) = handles.get_mut(&handle) else {
            return 0;
        };
        return write_chunk(file, data);
    }
    let Some(mut file) = clone_file(handle) else {
        return 0;
    };
    let data = data.to_vec();
    blocking::run(&POOL, move || write_chunk(&mut file, &data))
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

#[no_mangle]
pub unsafe extern "C" fn pith_file_write(handle: i64, data: *const i8) -> i64 {
    let Some(bytes) = cstr_bytes(data) else {
        return 0;
    };
    write_handle_chunk(handle, bytes)
}

#[no_mangle]
pub unsafe extern "C" fn pith_file_write_bytes(handle: i64, data: i64) -> i64 {
    let Some(bytes) = pith_bytes_ref(data) else {
        return 0;
    };
    write_handle_chunk(handle, &bytes.data)
}

#[no_mangle]
pub extern "C" fn pith_file_close(handle: i64) {
    file_handles().lock().remove(&handle);
}

/// Get environment variable value
///
/// # Safety
/// name must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_env(name: *const i8) -> *const i8 {
    if let Some(name_str) = cstr_str(name) {
        if let Ok(var) = std::env::var(name_str) {
            return crate::pith_copy_bytes_to_cstring(var.as_bytes());
        }
    }
    crate::pith_strdup_string("")
}

/// Get environment variable value wrapped in an Optional tuple. Returns
/// `Some(value)` when the variable is set and `None` otherwise — the
/// `pith_env` version collapses both to `""`, which is indistinguishable
/// from a variable set to the empty string.
///
/// # Safety
/// name must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_env_opt(name: *const i8) -> i64 {
    if let Some(name_str) = cstr_str(name) {
        if let Ok(var) = std::env::var(name_str) {
            let cstr = crate::pith_copy_bytes_to_cstring(var.as_bytes());
            return optional_tuple(true, cstr as i64);
        }
    }
    optional_tuple(false, 0)
}

#[no_mangle]
pub unsafe extern "C" fn pith_os_getcwd() -> *const i8 {
    if let Ok(path) = std::env::current_dir() {
        if let Some(text) = path.to_str() {
            return crate::pith_strdup_string(text);
        }
    }
    std::ptr::null()
}

#[no_mangle]
pub unsafe extern "C" fn pith_os_chdir(path: *const i8) -> i64 {
    if let Some(path_str) = cstr_str(path) {
        if std::env::set_current_dir(path_str).is_ok() {
            return 1;
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn pith_os_temp_dir() -> *const i8 {
    let path = std::env::temp_dir();
    if let Some(text) = path.to_str() {
        return crate::pith_strdup_string(text);
    }
    std::ptr::null()
}

#[no_mangle]
pub unsafe extern "C" fn pith_os_home_dir() -> *const i8 {
    if let Ok(home) = std::env::var("HOME") {
        return crate::pith_strdup_string(&home);
    }
    if let Ok(home) = std::env::var("USERPROFILE") {
        return crate::pith_strdup_string(&home);
    }
    std::ptr::null()
}

#[no_mangle]
pub unsafe extern "C" fn pith_os_set_env(name: *const i8, value: *const i8) -> i64 {
    if let (Some(name_str), Some(value_str)) = (cstr_str(name), cstr_str(value)) {
        std::env::set_var(name_str, value_str);
        return 1;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn pith_os_unset_env(name: *const i8) -> i64 {
    if let Some(name_str) = cstr_str(name) {
        std::env::remove_var(name_str);
        return 1;
    }
    0
}

/// the entry names of a directory, in whatever order the filesystem gives them,
/// or `None` if it cannot be read. names that are not utf-8 are dropped, which
/// is what the pith-facing list has always done with them.
fn list_dir_at(path: &str) -> Option<Vec<String>> {
    let entries = std::fs::read_dir(path).ok()?;
    Some(
        entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .collect(),
    )
}

/// Offloaded: a directory with many entries costs many syscalls to walk, so
/// this one is not the single cheap lookup the other metadata calls are.
#[no_mangle]
pub unsafe extern "C" fn pith_list_dir(path: *const i8) -> i64 {
    let names = match cstr_str(path) {
        Some(path_str) => {
            let path = path_str.to_string();
            blocking::run(&POOL, move || list_dir_at(&path))
        }
        None => None,
    };

    // an unreadable directory is an empty list, not an error, as before. the
    // list and its strings are pith objects, so they are built here rather than
    // on the pool thread.
    let list = pith_list_new(8, 1);
    for name in names.unwrap_or_default() {
        pith_list_push_value(list, crate::pith_strdup_string(&name) as i64);
    }
    list.ptr as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::io::Read;
    use std::sync::atomic::AtomicU32;

    /// a scratch path unique to the calling test, cleaned up when the returned
    /// value drops. tests run in one process, so a shared name would race.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let name = format!(
                "pith-host-fs-{}-{}-{}",
                tag,
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            Scratch(std::env::temp_dir().join(name))
        }

        fn cstr(&self) -> CString {
            CString::new(self.0.to_str().unwrap()).unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn cstring(text: &str) -> CString {
        CString::new(text).unwrap()
    }

    #[test]
    fn invalid_file_paths_return_safe_defaults() {
        let invalid = [0xffu8, 0x00];
        let ptr = invalid.as_ptr() as *const i8;

        unsafe {
            assert_eq!(pith_file_exists(ptr), 0);
            assert_eq!(pith_file_size(ptr), -1);
            assert_eq!(pith_write_file(ptr, b"x\0".as_ptr() as *const i8), 0);
            assert!(pith_read_file(ptr).is_null());
            assert_eq!(pith_os_chdir(ptr), 0);
            assert_eq!(pith_remove_tree(ptr), 0);
            assert_eq!(pith_file_open_read(ptr), 0);
        }
    }

    #[test]
    fn whole_file_write_append_and_read_round_trip() {
        let scratch = Scratch::new("round-trip");
        let path = scratch.cstr();
        let first = cstring("hello");
        let second = cstring(" world");

        unsafe {
            assert_eq!(pith_write_file(path.as_ptr(), first.as_ptr()), 1);
            assert_eq!(pith_append_file(path.as_ptr(), second.as_ptr()), 1);
        }
        assert_eq!(
            std::fs::read_to_string(&scratch.0).unwrap(),
            "hello world",
            "the two writes should land in order"
        );

        // read it back through the entry point that builds a pith C string.
        let read = unsafe { pith_read_file(path.as_ptr()) };
        assert!(!read.is_null());
        let text = unsafe { std::ffi::CStr::from_ptr(read) }.to_str().unwrap();
        assert_eq!(text, "hello world");
    }

    #[test]
    fn reading_a_missing_file_reports_failure() {
        let scratch = Scratch::new("missing");
        let path = scratch.cstr();
        unsafe {
            assert!(pith_read_file(path.as_ptr()).is_null());
            assert_eq!(pith_read_file_bytes(path.as_ptr()), 0);
        }
    }

    #[test]
    fn streaming_writes_then_reads_back_a_chunk_at_a_time() {
        let scratch = Scratch::new("stream");
        let path = scratch.cstr();

        unsafe {
            let out = pith_file_open_append(path.as_ptr());
            assert_ne!(out, 0);
            let chunk = cstring("abcdef");
            assert_eq!(pith_file_write(out, chunk.as_ptr()), 6);
            pith_file_close(out);

            let input = pith_file_open_read(path.as_ptr());
            assert_ne!(input, 0);
            // two short reads must not both start at the beginning: the second
            // one continues where the first stopped.
            let first = pith_file_read(input, 3);
            let second = pith_file_read(input, 3);
            assert_eq!(
                std::ffi::CStr::from_ptr(first).to_str().unwrap(),
                "abc",
                "first chunk"
            );
            assert_eq!(
                std::ffi::CStr::from_ptr(second).to_str().unwrap(),
                "def",
                "second chunk should resume after the first"
            );
            // and the read past the end is eof, not an error.
            let third = pith_file_read(input, 3);
            assert!(!third.is_null());
            assert_eq!(std::ffi::CStr::from_ptr(third).to_str().unwrap(), "");
            pith_file_close(input);
        }
    }

    #[test]
    fn unknown_handles_report_failure() {
        unsafe {
            assert!(pith_file_read(9_999_999, 16).is_null());
            assert_eq!(pith_file_read_bytes(9_999_999, 16), 0);
            let data = cstring("x");
            assert_eq!(pith_file_write(9_999_999, data.as_ptr()), 0);
        }
        assert!(clone_file(9_999_999).is_none());
    }

    #[test]
    fn a_cloned_handle_shares_the_files_offset() {
        // the offloaded read and write paths run on a dup of the open file, so
        // this sharing is what keeps sequential reads sequential.
        let scratch = Scratch::new("clone");
        std::fs::write(&scratch.0, b"abcdef").unwrap();
        let path = scratch.cstr();

        let handle = unsafe { pith_file_open_read(path.as_ptr()) };
        assert_ne!(handle, 0);

        let mut clone = clone_file(handle).expect("open handle should clone");
        let mut first = [0u8; 3];
        clone.read_exact(&mut first).unwrap();
        assert_eq!(&first, b"abc");

        // reading through the original handle picks up after the clone's read.
        let next = read_handle_chunk(handle, 3).expect("read should succeed");
        assert_eq!(next, b"def");
        pith_file_close(handle);
    }

    #[test]
    fn listing_a_directory_names_its_entries() {
        let scratch = Scratch::new("listing");
        std::fs::create_dir(&scratch.0).unwrap();
        std::fs::write(scratch.0.join("one.txt"), b"1").unwrap();
        std::fs::write(scratch.0.join("two.txt"), b"2").unwrap();

        let mut names = list_dir_at(scratch.0.to_str().unwrap()).expect("directory should read");
        names.sort();
        assert_eq!(names, vec!["one.txt", "two.txt"]);
        assert!(list_dir_at(&format!("{}/nope", scratch.0.display())).is_none());
    }

    #[test]
    fn removing_a_tree_takes_its_contents_with_it() {
        let scratch = Scratch::new("tree");
        std::fs::create_dir_all(scratch.0.join("nested")).unwrap();
        std::fs::write(scratch.0.join("nested/file.txt"), b"x").unwrap();

        let path = scratch.cstr();
        unsafe {
            assert_eq!(pith_remove_tree(path.as_ptr()), 1);
            assert_eq!(pith_dir_exists(path.as_ptr()), 0);
            // a second removal has nothing to do and says so.
            assert_eq!(pith_remove_tree(path.as_ptr()), 0);
        }
    }
}
