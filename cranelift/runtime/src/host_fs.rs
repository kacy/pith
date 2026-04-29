use crate::bytes::{pith_bytes_from_vec, pith_bytes_ref};
use crate::collections::list::{pith_list_new, pith_list_push_value};
use crate::ffi_util::{cstr_bytes, cstr_str};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::fs::File;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::OnceLock;

static FILE_HANDLES: OnceLock<Mutex<HashMap<i64, File>>> = OnceLock::new();
static NEXT_FILE_HANDLE: AtomicI64 = AtomicI64::new(1);

fn file_handles() -> &'static Mutex<HashMap<i64, File>> {
    FILE_HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}

unsafe fn pith_open_file_with(path: *const i8, create: bool, write: bool, append: bool) -> i64 {
    use std::fs::OpenOptions;

    let Some(path_str) = cstr_str(path) else {
        return 0;
    };

    let mut options = OpenOptions::new();
    options.read(!write && !append);
    options.write(write || append);
    options.create(create || append);
    options.truncate(write && !append);
    options.append(append);

    match options.open(path_str) {
        Ok(file) => {
            let handle = NEXT_FILE_HANDLE.fetch_add(1, Ordering::Relaxed);
            file_handles().lock().insert(handle, file);
            handle
        }
        Err(_) => 0,
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
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_remove_tree(path: *const i8) -> i64 {
    use std::fs;

    if let Some(path_str) = cstr_str(path) {
        if fs::remove_dir_all(path_str).is_ok() {
            return 1;
        }
    }
    0
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
#[no_mangle]
pub unsafe extern "C" fn pith_read_file(path: *const i8) -> *mut i8 {
    use std::fs;

    if let Some(path_str) = cstr_str(path) {
        if let Ok(contents) = fs::read_to_string(path_str) {
            return crate::pith_copy_bytes_to_cstring(contents.as_bytes());
        }
    }
    std::ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn pith_read_file_bytes(path: *const i8) -> i64 {
    use std::fs;

    let Some(path_str) = cstr_str(path) else {
        return 0;
    };

    match fs::read(path_str) {
        Ok(contents) => pith_bytes_from_vec(contents),
        Err(_) => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_write_file(path: *const i8, content: *const i8) -> i64 {
    use std::fs;

    if let (Some(path_str), Some(content_str)) = (cstr_str(path), cstr_str(content)) {
        if fs::write(path_str, content_str).is_ok() {
            return 1;
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn pith_write_file_bytes(path: *const i8, content: i64) -> i64 {
    use std::fs;

    let Some(bytes) = pith_bytes_ref(content) else {
        return 0;
    };
    let Some(path_str) = cstr_str(path) else {
        return 0;
    };
    if fs::write(path_str, &bytes.data).is_ok() {
        return 1;
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn pith_append_file(path: *const i8, content: *const i8) -> i64 {
    use std::fs::OpenOptions;
    use std::io::Write;

    if let (Some(path_str), Some(content_str)) = (cstr_str(path), cstr_str(content)) {
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path_str) {
            if file.write_all(content_str.as_bytes()).is_ok() {
                return 1;
            }
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn pith_append_file_bytes(path: *const i8, content: i64) -> i64 {
    use std::fs::OpenOptions;
    use std::io::Write;

    let Some(bytes) = pith_bytes_ref(content) else {
        return 0;
    };
    let Some(path_str) = cstr_str(path) else {
        return 0;
    };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path_str) {
        if file.write_all(&bytes.data).is_ok() {
            return 1;
        }
    }
    0
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

#[no_mangle]
pub unsafe extern "C" fn pith_file_read(handle: i64, max_bytes: i64) -> *mut i8 {
    use std::io::Read;

    let size = if max_bytes > 0 {
        max_bytes as usize
    } else {
        4096
    };
    let mut handles = file_handles().lock();
    let Some(file) = handles.get_mut(&handle) else {
        return std::ptr::null_mut();
    };

    let mut buf = vec![0u8; size];
    match file.read(&mut buf) {
        Ok(0) => crate::pith_cstring_empty(),
        Ok(n) => {
            buf.truncate(n);
            crate::pith_copy_bytes_to_cstring(&buf)
        }
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_file_read_bytes(handle: i64, max_bytes: i64) -> i64 {
    use std::io::Read;

    let size = if max_bytes > 0 {
        max_bytes as usize
    } else {
        4096
    };
    let mut handles = file_handles().lock();
    let Some(file) = handles.get_mut(&handle) else {
        return 0;
    };

    let mut buf = vec![0u8; size];
    match file.read(&mut buf) {
        Ok(n) => {
            buf.truncate(n);
            pith_bytes_from_vec(buf)
        }
        Err(_) => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_file_write(handle: i64, data: *const i8) -> i64 {
    use std::io::Write;

    let Some(bytes) = cstr_bytes(data) else {
        return 0;
    };
    let mut handles = file_handles().lock();
    let Some(file) = handles.get_mut(&handle) else {
        return 0;
    };

    match file.write(bytes) {
        Ok(n) => n as i64,
        Err(_) => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_file_write_bytes(handle: i64, data: i64) -> i64 {
    use std::io::Write;

    let Some(bytes) = pith_bytes_ref(data) else {
        return 0;
    };
    let mut handles = file_handles().lock();
    let Some(file) = handles.get_mut(&handle) else {
        return 0;
    };

    match file.write(&bytes.data) {
        Ok(n) => n as i64,
        Err(_) => 0,
    }
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

#[no_mangle]
pub unsafe extern "C" fn pith_list_dir(path: *const i8) -> i64 {
    use std::fs;

    if let Some(path_str) = cstr_str(path) {
        if let Ok(entries) = fs::read_dir(path_str) {
            let list = pith_list_new(8, 1);

            for entry in entries {
                if let Ok(entry) = entry {
                    if let Some(name) = entry.file_name().to_str() {
                        let name_ptr = crate::pith_strdup_string(name) as i64;
                        pith_list_push_value(list, name_ptr);
                    }
                }
            }
            return list.ptr as i64;
        }
    }
    pith_list_new(8, 1).ptr as i64
}

#[cfg(test)]
mod tests {
    use super::*;

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
        }
    }
}
