//! Process environment, working directory, temp/home directory builtins.

use crate::ffi_util::{with_cstr_str, with_cstr_str2};
use crate::runtime_core::optional_tuple;

fn env_cstring(name_str: &str) -> Option<*mut i8> {
    let var = crate::env_overlay::get(name_str)?;
    Some(crate::pith_copy_bytes_to_cstring(var.as_bytes()))
}

/// Get environment variable value
///
/// # Safety
/// name must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_env(name: *const i8) -> *const i8 {
    // SAFETY: Caller guarantees `name` is null or a valid null-terminated C string.
    unsafe { with_cstr_str(name, env_cstring) }
        .flatten()
        .unwrap_or_else(|| crate::pith_strdup_string("")) as *const i8
}

/// Get environment variable value wrapped in an Optional tuple. Returns
/// `Some(value)` when the variable is set and `None` otherwise: the
/// `pith_env` version collapses both to `""`, which is indistinguishable
/// from a variable set to the empty string.
///
/// # Safety
/// name must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_env_opt(name: *const i8) -> i64 {
    // SAFETY: Caller guarantees `name` is null or a valid null-terminated C string.
    match unsafe { with_cstr_str(name, env_cstring) }.flatten() {
        Some(cstr) => optional_tuple(true, cstr as i64),
        None => optional_tuple(false, 0),
    }
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
    // SAFETY: Caller guarantees `path` is null or a valid null-terminated C string.
    i64::from(
        unsafe { with_cstr_str(path, |p| std::env::set_current_dir(p).is_ok()) }.unwrap_or(false),
    )
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
    if let Some(home) = crate::env_overlay::get("HOME") {
        return crate::pith_strdup_string(&home);
    }
    if let Some(home) = crate::env_overlay::get("USERPROFILE") {
        return crate::pith_strdup_string(&home);
    }
    std::ptr::null()
}

/// Set an environment variable as the program sees it.
///
/// This records the value in the runtime's overlay (see `env_overlay`) rather
/// than calling `setenv`, which is not safe once the process has more than one
/// thread. `pith_env`, `pith_env_opt` and every child process spawned from here
/// read the overlay; a C library reading the real environment in this process
/// does not.
///
/// # Safety
/// name and value must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn pith_os_set_env(name: *const i8, value: *const i8) -> i64 {
    // SAFETY: Caller guarantees `name` and `value` are null or valid null-terminated C strings.
    i64::from(unsafe { with_cstr_str2(name, value, crate::env_overlay::set) }.is_some())
}

/// Remove an environment variable as the program sees it.
///
/// The counterpart to `pith_os_set_env`: it records a tombstone in the overlay
/// rather than calling `unsetenv`, so the name reads back absent here and is
/// removed from a child's environment at spawn.
///
/// # Safety
/// name must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_os_unset_env(name: *const i8) -> i64 {
    // SAFETY: Caller guarantees `name` is null or a valid null-terminated C string.
    i64::from(unsafe { with_cstr_str(name, crate::env_overlay::unset) }.is_some())
}
