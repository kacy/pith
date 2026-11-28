//! Path, metadata, directory, and whole-file operations.

use super::POOL;
use crate::blocking;
use crate::bytes::{pith_bytes_from_vec, with_bytes};
use crate::collections::list::{pith_list_new, pith_list_push_value_owned};
use crate::ffi_util::{with_cstr_str, with_cstr_str2};

fn dir_exists_at(path_str: &str) -> bool {
    let path = std::path::Path::new(path_str);
    path.exists() && path.is_dir()
}

fn file_size_at(path_str: &str) -> i64 {
    std::fs::metadata(path_str).map_or(-1, |meta| meta.len() as i64)
}

fn file_mtime_at(path_str: &str) -> i64 {
    let Ok(modified) = std::fs::metadata(path_str).and_then(|meta| meta.modified()) else {
        return -1;
    };
    match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(since) => since.as_secs() as i64,
        Err(_) => -1,
    }
}

/// Check if a file exists
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_file_exists(path: *const i8) -> i64 {
    // SAFETY: Caller guarantees `path` is null or a valid null-terminated C string.
    i64::from(unsafe { with_cstr_str(path, |s| std::path::Path::new(s).exists()) }.unwrap_or(false))
}

/// Check if a directory exists
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_dir_exists(path: *const i8) -> i64 {
    // SAFETY: Caller guarantees `path` is null or a valid null-terminated C string.
    i64::from(unsafe { with_cstr_str(path, dir_exists_at) }.unwrap_or(false))
}

/// Create a directory
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_mkdir(path: *const i8) -> i64 {
    // SAFETY: Caller guarantees `path` is null or a valid null-terminated C string.
    i64::from(
        unsafe { with_cstr_str(path, |s| std::fs::create_dir_all(s).is_ok()) }.unwrap_or(false),
    )
}

/// Remove an empty directory
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_remove_dir(path: *const i8) -> i64 {
    // SAFETY: Caller guarantees `path` is null or a valid null-terminated C string.
    i64::from(unsafe { with_cstr_str(path, |s| std::fs::remove_dir(s).is_ok()) }.unwrap_or(false))
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
    // SAFETY: Caller guarantees `path` is null or a valid null-terminated C string.
    let Some(path) = (unsafe { with_cstr_str(path, str::to_owned) }) else {
        return 0;
    };
    blocking::run(&POOL, move || std::fs::remove_dir_all(path).is_ok()) as i64
}

/// Read file size in bytes.
/// Returns -1 when metadata cannot be read.
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_file_size(path: *const i8) -> i64 {
    // SAFETY: Caller guarantees `path` is null or a valid null-terminated C string.
    unsafe { with_cstr_str(path, file_size_at) }.unwrap_or(-1)
}

/// Read a file's modification time, in whole seconds since the unix epoch.
/// Returns -1 when metadata cannot be read or the time is before the epoch.
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_file_mtime(path: *const i8) -> i64 {
    // SAFETY: Caller guarantees `path` is null or a valid null-terminated C string.
    unsafe { with_cstr_str(path, file_mtime_at) }.unwrap_or(-1)
}

/// Remove a file
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_remove_file(path: *const i8) -> i64 {
    // SAFETY: Caller guarantees `path` is null or a valid null-terminated C string.
    i64::from(unsafe { with_cstr_str(path, |s| std::fs::remove_file(s).is_ok()) }.unwrap_or(false))
}

/// Rename a file
///
/// # Safety
/// Both paths must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn pith_rename_file(from: *const i8, to: *const i8) -> i64 {
    // SAFETY: Caller guarantees `from` and `to` are null or valid null-terminated C strings.
    i64::from(
        unsafe { with_cstr_str2(from, to, |f, t| std::fs::rename(f, t).is_ok()) }.unwrap_or(false),
    )
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

/// the errno behind a failed call, or -1 when the error carries none.
fn errno_of(err: std::io::Error) -> i64 {
    err.raw_os_error().map_or(-1, i64::from)
}

/// create a file that must not exist yet and write `content` to it. the open
/// is `O_CREAT|O_EXCL|O_NOFOLLOW`, so an existing file or a symlink planted at
/// the name fails with `EEXIST` instead of being followed or truncated, and the
/// file starts with `mode` (less the umask). returns 0 or the errno. a write
/// that fails after the create removes the file again.
pub(super) fn create_new_file_at(path: &str, content: &[u8], mode: u32) -> i64 {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let opened = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(mode)
        .open(path);
    let mut file = match opened {
        Ok(file) => file,
        Err(err) => return errno_of(err),
    };
    match file.write_all(content) {
        Ok(()) => 0,
        Err(err) => {
            let _ = std::fs::remove_file(path);
            errno_of(err)
        }
    }
}

fn fs_create_new_impl(path_str: &str, data: &[u8], mode: i64) -> i64 {
    let mode = (mode & 0o7777) as u32;
    // `blocking::run` reports a job that panicked as the default, and a
    // default 0 would read as success, so the job answers in an option.
    blocking::run_or_inline(
        &POOL,
        || Some(create_new_file_at(path_str, data, mode)),
        || {
            let (path, content) = (path_str.to_string(), data.to_vec());
            move || Some(create_new_file_at(&path, &content, mode))
        },
    )
    .unwrap_or(-1)
}

fn fs_mkdir_new_at(path_str: &str, mode: i64) -> i64 {
    use std::os::unix::fs::DirBuilderExt;

    let mode = (mode & 0o7777) as u32;
    match std::fs::DirBuilder::new().mode(mode).create(path_str) {
        Ok(()) => 0,
        Err(err) => errno_of(err),
    }
}

fn write_file_impl(path_str: &str, data: &[u8]) -> i64 {
    // handing the content to another thread means owning it, which for a write
    // means copying it. `run_or_inline` writes straight out of the caller's
    // buffer when there is no worker to protect, and copies the bytes (never
    // a pith handle, which belongs to the caller's thread-local machinery)
    // only when offloading to a pool thread.
    blocking::run_or_inline(
        &POOL,
        || write_file_at(path_str, data),
        || {
            let (path, content) = (path_str.to_string(), data.to_vec());
            move || write_file_at(&path, &content)
        },
    ) as i64
}

fn append_file_impl(path_str: &str, data: &[u8]) -> i64 {
    blocking::run_or_inline(
        &POOL,
        || append_file_at(path_str, data),
        || {
            let (path, content) = (path_str.to_string(), data.to_vec());
            move || append_file_at(&path, &content)
        },
    ) as i64
}

/// `create_new_file_at` for pith: `content` is a `Bytes` handle and `mode` the
/// permission bits. the bytes are copied before the job crosses to a pool
/// thread, as in `pith_write_file_bytes`.
#[no_mangle]
pub unsafe extern "C" fn pith_fs_create_new(path: *const i8, content: i64, mode: i64) -> i64 {
    // SAFETY: Caller guarantees `path` is null or a valid C string; `with_bytes` validates `content`.
    unsafe {
        with_cstr_str(path, |path_str| {
            with_bytes(content, |data| fs_create_new_impl(path_str, data, mode))
        })
    }
    .flatten()
    .unwrap_or(libc::EINVAL as i64)
}

/// create one directory with `mode` (less the umask). `mkdir(2)` fails with
/// `EEXIST` on any existing path, a symlink included, so a planted name is
/// never reused. returns 0 or the errno.
#[no_mangle]
pub unsafe extern "C" fn pith_fs_mkdir_new(path: *const i8, mode: i64) -> i64 {
    // SAFETY: Caller guarantees `path` is null or a valid null-terminated C string.
    unsafe { with_cstr_str(path, |p| fs_mkdir_new_at(p, mode)) }.unwrap_or(libc::EINVAL as i64)
}

#[no_mangle]
pub unsafe extern "C" fn pith_read_file(path: *const i8) -> *mut i8 {
    // SAFETY: Caller guarantees `path` is null or a valid null-terminated C string.
    let Some(path) = (unsafe { with_cstr_str(path, str::to_owned) }) else {
        return std::ptr::null_mut();
    };
    // the contents come back as a plain `String` and become a pith C string
    // here, on the calling task's thread.
    match blocking::run(&POOL, move || read_file_text_at(&path)) {
        Some(contents) => crate::pith_copy_bytes_to_cstring(contents.as_bytes()),
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_read_file_bytes(path: *const i8) -> i64 {
    // SAFETY: Caller guarantees `path` is null or a valid null-terminated C string.
    let Some(path) = (unsafe { with_cstr_str(path, str::to_owned) }) else {
        return 0;
    };
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
    // SAFETY: Caller guarantees `path` and `content` are null or valid null-terminated C strings.
    unsafe { with_cstr_str2(path, content, |p, c| write_file_impl(p, c.as_bytes())) }.unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn pith_write_file_bytes(path: *const i8, content: i64) -> i64 {
    // SAFETY: `with_bytes` validates `content`; caller guarantees `path` is null or a valid C string.
    unsafe { with_bytes(content, |data| with_cstr_str(path, |p| write_file_impl(p, data))) }
        .flatten()
        .unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn pith_append_file(path: *const i8, content: *const i8) -> i64 {
    // as in `pith_write_file`, non-utf-8 content fails rather than being
    // appended, which is what this entry point has always done.
    // SAFETY: Caller guarantees `path` and `content` are null or valid null-terminated C strings.
    unsafe { with_cstr_str2(path, content, |p, c| append_file_impl(p, c.as_bytes())) }.unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn pith_append_file_bytes(path: *const i8, content: i64) -> i64 {
    // SAFETY: `with_bytes` validates `content`; caller guarantees `path` is null or a valid C string.
    unsafe { with_bytes(content, |data| with_cstr_str(path, |p| append_file_impl(p, data))) }
        .flatten()
        .unwrap_or(0)
}
/// the entry names of a directory, in whatever order the filesystem gives them,
/// or `None` if it cannot be read. names that are not utf-8 are dropped, which
/// is what the pith-facing list has always done with them.
pub(super) fn list_dir_at(path: &str) -> Option<Vec<String>> {
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
    // SAFETY: Caller guarantees `path` is null or a valid null-terminated C string.
    let names = unsafe { with_cstr_str(path, str::to_owned) }
        .and_then(|path| blocking::run(&POOL, move || list_dir_at(&path)));

    // an unreadable directory is an empty list, not an error. the
    // list and its strings are pith objects, so they are built here rather than
    // on the pool thread.
    // each name is born with one count, which the String-tagged list takes
    // over; a plain push would add a second one nobody gives back.
    // SAFETY: Allocates a fresh String-tagged list.
    let list = unsafe { pith_list_new(8, 1) };
    for name in names.unwrap_or_default() {
        let cstr = crate::pith_strdup_string(&name) as i64;
        // SAFETY: `list` is a live String-tagged list and `cstr` transfers one owned count.
        unsafe { pith_list_push_value_owned(list, cstr) };
    }
    list.ptr as i64
}
