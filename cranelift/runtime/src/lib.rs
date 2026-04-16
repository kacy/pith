//! Forge Runtime - Core runtime library for the Forge language
//!
//! This crate provides the runtime support for Forge programs:
//! - Reference counting (ARC) with cycle collection
//! - String operations
//! - Collections (List, Map, Set)
//! - Concurrency primitives
//!
//! The runtime is designed to be called from Cranelift-generated code
//! via a C-compatible FFI boundary.

#![allow(clippy::missing_safety_doc)]

pub mod arc;
pub mod bytes;
pub mod collections;
pub mod concurrency;
pub mod encoding;
pub mod ffi_util;
pub mod host_fs;
pub mod network;
pub mod perf;
pub mod platform;
pub mod process;
pub mod string;
pub mod string_list;
pub mod utility;

use crate::bytes::{forge_bytes_from_vec, forge_bytes_ref};
pub use host_fs::*;
pub use network::*;
pub use encoding::*;
pub use perf::*;
pub use platform::*;
pub use process::*;
pub use string_list::*;
pub use utility::*;

pub(crate) fn forge_strdup_string(text: &str) -> *mut i8 {
    let owned = format!("{}\0", text);
    unsafe { forge_strdup(owned.as_ptr() as *const i8) }
}

const FORGE_CLOSURE_ENV_SLOTS: usize = 16;

struct ForgeClosure {
    func_ptr: i64,
    env: [i64; FORGE_CLOSURE_ENV_SLOTS],
}

unsafe fn forge_closure_mut<'a>(handle: i64) -> Option<&'a mut ForgeClosure> {
    if handle == 0 {
        return None;
    }
    Some(&mut *(handle as *mut ForgeClosure))
}

unsafe fn forge_closure_ref<'a>(handle: i64) -> Option<&'a ForgeClosure> {
    if handle == 0 {
        return None;
    }
    Some(&*(handle as *const ForgeClosure))
}

#[no_mangle]
pub extern "C" fn forge_closure_new(func_ptr: i64) -> i64 {
    Box::into_raw(Box::new(ForgeClosure {
        func_ptr,
        env: [0; FORGE_CLOSURE_ENV_SLOTS],
    })) as i64
}

#[no_mangle]
pub unsafe extern "C" fn forge_closure_get_fn(handle: i64) -> i64 {
    if let Some(closure) = forge_closure_ref(handle) {
        closure.func_ptr
    } else {
        0
    }
}

/// Set a captured variable slot on a specific closure handle.
#[no_mangle]
pub unsafe extern "C" fn forge_closure_set_env(handle: i64, slot: i64, value: i64) {
    if slot < 0 || (slot as usize) >= FORGE_CLOSURE_ENV_SLOTS {
        return;
    }
    if let Some(closure) = forge_closure_mut(handle) {
        closure.env[slot as usize] = value;
    }
}

/// Read a captured variable from a specific closure handle.
#[no_mangle]
pub unsafe extern "C" fn forge_closure_get_env(handle: i64, slot: i64) -> i64 {
    if slot < 0 || (slot as usize) >= FORGE_CLOSURE_ENV_SLOTS {
        return 0;
    }
    if let Some(closure) = forge_closure_ref(handle) {
        closure.env[slot as usize]
    } else {
        0
    }
}

/// Print a string to stdout
///
/// # Safety
/// s must be a valid ForgeString
#[no_mangle]
pub unsafe extern "C" fn forge_print(s: string::ForgeString) {
    if s.ptr.is_null() || s.len == 0 {
        println!();
        return;
    }

    let slice = std::slice::from_raw_parts(s.ptr, s.len as usize);
    if let Ok(str_ref) = std::str::from_utf8(slice) {
        println!("{}", str_ref);
    } else {
        println!();
    }
}

/// Print an integer (for testing)
#[no_mangle]
pub extern "C" fn forge_print_int(n: i64) {
    println!("{}", n);
}

/// Simple string concatenation for two C string pointers
/// Allocates new memory for the result
///
/// # Safety
/// Both pointers must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn forge_concat_cstr(a: *const i8, b: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if a.is_null() {
        return if b.is_null() {
            std::ptr::null_mut()
        } else {
            forge_strdup(b)
        };
    }
    if b.is_null() {
        return forge_strdup(a);
    }

    // Calculate lengths
    let len_a = crate::string::forge_cstring_len(a) as usize;
    let len_b = crate::string::forge_cstring_len(b) as usize;

    let total_len = len_a + len_b;
    let layout = Layout::from_size_align(total_len + 1, 1).unwrap();
    let result = alloc(layout) as *mut i8;

    if result.is_null() {
        return std::ptr::null_mut();
    }

    // Copy a
    std::ptr::copy_nonoverlapping(a, result, len_a);
    // Copy b
    std::ptr::copy_nonoverlapping(b, result.add(len_a), len_b);
    // Null terminator
    *result.add(total_len) = 0;

    result
}

/// Duplicate a C string
///
/// # Safety
/// ptr must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_strdup(ptr: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if ptr.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::forge_cstring_len(ptr) as usize;

    let layout = Layout::from_size_align(len + 1, 1).unwrap();
    let result = alloc(layout) as *mut i8;

    if !result.is_null() {
        std::ptr::copy_nonoverlapping(ptr, result, len + 1);
    }

    result
}

/// Print a C string (null-terminated)
///
/// # Safety
/// ptr must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_print_cstr(ptr: *const i8) {
    if ptr.is_null() {
        println!();
        return;
    }

    // Calculate length
    let len = crate::string::forge_cstring_len(ptr) as usize;

    let slice = std::slice::from_raw_parts(ptr as *const u8, len);
    if let Ok(str_ref) = std::str::from_utf8(slice) {
        println!("{}", str_ref);
    } else {
        println!();
    }
}

/// Print a C string to stderr (null-terminated)
///
/// # Safety
/// ptr must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_print_err(ptr: *const i8) {
    if ptr.is_null() {
        eprintln!();
        return;
    }

    // Calculate length
    let len = crate::string::forge_cstring_len(ptr) as usize;

    let slice = std::slice::from_raw_parts(ptr as *const u8, len);
    if let Ok(str_ref) = std::str::from_utf8(slice) {
        eprintln!("{}", str_ref);
    } else {
        eprintln!();
    }
}

/// Compare two C strings for equality (content-based, null-terminated)
///
/// # Safety
/// Both pointers must be valid null-terminated C strings or null
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_eq(a: *const i8, b: *const i8) -> i64 {
    // Handle null cases
    if a.is_null() && b.is_null() {
        return 1;
    }
    if a.is_null() || b.is_null() {
        return 0;
    }
    if std::ptr::eq(a, b) {
        return 1;
    }

    // Compare byte by byte
    let mut pa = a;
    let mut pb = b;

    loop {
        let ca = *pa;
        let cb = *pb;

        if ca != cb {
            return 0;
        }

        if ca == 0 {
            return 1;
        }

        pa = pa.add(1);
        pb = pb.add(1);
    }
}

/// Get ASCII value of first char in C string (ord)
#[no_mangle]
pub unsafe extern "C" fn forge_ord_cstr(s: *const i8) -> i64 {
    if s.is_null() || *s == 0 {
        return 0;
    }
    *s as i64
}

/// Convert ASCII value to single-char C string (chr)
#[no_mangle]
pub unsafe extern "C" fn forge_chr_cstr(n: i64) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    let layout = Layout::from_size_align(2, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;

    if !ptr.is_null() {
        *ptr = (n as u8) as i8;
        *ptr.add(1) = 0;
    }

    ptr
}

/// Test assertion helpers
static TEST_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Assert that condition is true
#[no_mangle]
pub extern "C" fn forge_assert(cond: i64) {
    if cond == 0 {
        TEST_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
        eprintln!("Assertion failed");
    }
}

/// Assert that two values are equal
#[no_mangle]
pub extern "C" fn forge_assert_eq(a: i64, b: i64) {
    if a != b {
        TEST_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
        eprintln!("Assertion failed: {} != {}", a, b);
    }
}

/// Assert that two values are not equal
#[no_mangle]
pub extern "C" fn forge_assert_ne(a: i64, b: i64) {
    if a == b {
        TEST_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
        eprintln!("Assertion failed: {} == {}", a, b);
    }
}

/// Bitwise AND
#[no_mangle]
pub extern "C" fn forge_bit_and(a: i64, b: i64) -> i64 {
    a & b
}

/// Bitwise OR
#[no_mangle]
pub extern "C" fn forge_bit_or(a: i64, b: i64) -> i64 {
    a | b
}

/// Bitwise XOR
#[no_mangle]
pub extern "C" fn forge_bit_xor(a: i64, b: i64) -> i64 {
    a ^ b
}

/// Bitwise NOT
#[no_mangle]
pub extern "C" fn forge_bit_not(a: i64) -> i64 {
    !a
}

/// Bitwise shift left
#[no_mangle]
pub extern "C" fn forge_bit_shl(a: i64, b: i64) -> i64 {
    a << b
}

/// Bitwise shift right (arithmetic)
#[no_mangle]
pub extern "C" fn forge_bit_shr(a: i64, b: i64) -> i64 {
    a >> b
}

/// Absolute value
#[no_mangle]
pub extern "C" fn forge_abs(n: i64) -> i64 {
    n.abs()
}

/// Minimum of two values
#[no_mangle]
pub extern "C" fn forge_min(a: i64, b: i64) -> i64 {
    if a < b {
        a
    } else {
        b
    }
}

/// Maximum of two values
#[no_mangle]
pub extern "C" fn forge_max(a: i64, b: i64) -> i64 {
    if a > b {
        a
    } else {
        b
    }
}

/// Clamp value between min and max
#[no_mangle]
pub extern "C" fn forge_clamp(n: i64, min: i64, max: i64) -> i64 {
    if n < min {
        min
    } else if n > max {
        max
    } else {
        n
    }
}

/// Power: a^b (floating point)
#[no_mangle]
pub extern "C" fn forge_pow(a: f64, b: f64) -> f64 {
    a.powf(b)
}

/// Square root
#[no_mangle]
pub extern "C" fn forge_sqrt(n: f64) -> f64 {
    n.sqrt()
}

/// Floor
#[no_mangle]
pub extern "C" fn forge_floor(n: f64) -> f64 {
    n.floor()
}

/// Ceiling
#[no_mangle]
pub extern "C" fn forge_ceil(n: f64) -> f64 {
    n.ceil()
}

/// Round
#[no_mangle]
pub extern "C" fn forge_round(n: f64) -> f64 {
    n.round()
}

// ===============================================================
// Trigonometric and logarithmic functions
// ===============================================================

#[no_mangle]
pub extern "C" fn forge_sin(n: f64) -> f64 { n.sin() }
#[no_mangle]
pub extern "C" fn forge_cos(n: f64) -> f64 { n.cos() }
#[no_mangle]
pub extern "C" fn forge_tan(n: f64) -> f64 { n.tan() }
#[no_mangle]
pub extern "C" fn forge_asin(n: f64) -> f64 { n.asin() }
#[no_mangle]
pub extern "C" fn forge_acos(n: f64) -> f64 { n.acos() }
#[no_mangle]
pub extern "C" fn forge_atan(n: f64) -> f64 { n.atan() }
#[no_mangle]
pub extern "C" fn forge_atan2(y: f64, x: f64) -> f64 { y.atan2(x) }
#[no_mangle]
pub extern "C" fn forge_log(n: f64) -> f64 { n.ln() }
#[no_mangle]
pub extern "C" fn forge_log10(n: f64) -> f64 { n.log10() }
#[no_mangle]
pub extern "C" fn forge_log2(n: f64) -> f64 { n.log2() }
#[no_mangle]
pub extern "C" fn forge_exp(n: f64) -> f64 { n.exp() }
#[no_mangle]
pub extern "C" fn forge_abs_float(n: f64) -> f64 { n.abs() }

// ===============================================================
// String comparison (lexicographic, for < > <= >=)
// ===============================================================

#[no_mangle]
pub unsafe extern "C" fn forge_cstring_compare(a: *const i8, b: *const i8) -> i64 {
    if a.is_null() && b.is_null() { return 0; }
    if a.is_null() { return -1; }
    if b.is_null() { return 1; }
    let mut pa = a;
    let mut pb = b;
    loop {
        let ca = *pa as u8;
        let cb = *pb as u8;
        if ca != cb { return if ca < cb { -1 } else { 1 }; }
        if ca == 0 { return 0; }
        pa = pa.add(1);
        pb = pb.add(1);
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_cstring_lt(a: *const i8, b: *const i8) -> i64 {
    if forge_cstring_compare(a, b) < 0 { 1 } else { 0 }
}

#[no_mangle]
pub unsafe extern "C" fn forge_cstring_gt(a: *const i8, b: *const i8) -> i64 {
    if forge_cstring_compare(a, b) > 0 { 1 } else { 0 }
}

#[no_mangle]
pub unsafe extern "C" fn forge_cstring_lte(a: *const i8, b: *const i8) -> i64 {
    if forge_cstring_compare(a, b) <= 0 { 1 } else { 0 }
}

#[no_mangle]
pub unsafe extern "C" fn forge_cstring_gte(a: *const i8, b: *const i8) -> i64 {
    if forge_cstring_compare(a, b) >= 0 { 1 } else { 0 }
}

/// Convert int to C string (returns pointer, caller must free with forge_free)
#[no_mangle]
pub unsafe extern "C" fn forge_int_to_cstr(n: i64) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    let s = n.to_string();
    let len = s.len();
    let layout = Layout::from_size_align(len + 1, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;

    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(s.as_ptr(), ptr as *mut u8, len);
        *ptr.add(len) = 0;
    }

    ptr
}

/// Convert float (f64) to a C string
#[no_mangle]
pub extern "C" fn forge_float_to_cstr(n: f64) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    // Format with up to 6 decimal places, stripping trailing zeros
    let s = if n == n.floor() && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        let formatted = format!("{:.6}", n);
        formatted
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    };
    let len = s.len();
    let layout = Layout::from_size_align(len + 1, 1).unwrap();
    let ptr = unsafe { alloc(layout) as *mut i8 };

    if !ptr.is_null() {
        unsafe {
            std::ptr::copy_nonoverlapping(s.as_ptr(), ptr as *mut u8, len);
            *ptr.add(len) = 0;
        }
    }

    ptr
}

/// Convert bool to a C string
#[no_mangle]
pub extern "C" fn forge_bool_to_cstr(b: i64) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    let s = if b != 0 { "true" } else { "false" };
    let len = s.len();
    let layout = Layout::from_size_align(len + 1, 1).unwrap();
    let ptr = unsafe { alloc(layout) as *mut i8 };

    if !ptr.is_null() {
        unsafe {
            std::ptr::copy_nonoverlapping(s.as_ptr(), ptr as *mut u8, len);
            *ptr.add(len) = 0;
        }
    }

    ptr
}

/// Math sqrt (already defined but adding alias)
pub use forge_sqrt as forge_math_sqrt;

/// Math floor (already defined)
pub use forge_floor as forge_math_floor;

/// Math ceiling (already defined)
pub use forge_ceil as forge_math_ceil;

/// Math round (already defined)
pub use forge_round as forge_math_round;

/// Math pow (already defined)
pub use forge_pow as forge_math_pow;

/// Free memory allocated by runtime
///
/// # Safety
/// ptr must have been allocated by the runtime
#[no_mangle]
pub unsafe extern "C" fn forge_free(ptr: *mut i8) {
    use std::alloc::Layout;

    if !ptr.is_null() {
        // We don't know the size, but we can deallocate with size 0
        // This is implementation-specific but works with system allocators
        std::alloc::dealloc(ptr as *mut u8, Layout::new::<u8>());
    }
}

// List is_empty and reverse are implemented in collections/list.rs

/// Convert i64 to f64
#[no_mangle]
pub extern "C" fn forge_int_to_float(n: i64) -> f64 {
    n as f64
}

/// Convert f64 to i64 (truncates)
#[no_mangle]
pub extern "C" fn forge_float_to_int(n: f64) -> i64 {
    n as i64
}

pub(crate) unsafe fn forge_cstring_empty() -> *mut i8 {
    use std::alloc::{alloc, Layout};
    let layout = Layout::from_size_align(1, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        *ptr = 0;
    }
    ptr
}

pub(crate) unsafe fn forge_copy_bytes_to_cstring(bytes: &[u8]) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    let layout = Layout::from_size_align(bytes.len() + 1, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
        *ptr.add(bytes.len()) = 0;
    }
    ptr
}

/// Generic second(a, b) — returns second argument
#[no_mangle]
pub extern "C" fn forge_second(_a: i64, b: i64) -> i64 {
    b
}

fn forge_read_process_stream<R: std::io::Read>(reader: &mut R, max_bytes: i64) -> *mut i8 {
    let size = if max_bytes > 0 { max_bytes as usize } else { 4096 };
    let mut buf = vec![0u8; size];
    match reader.read(&mut buf) {
        Ok(0) => unsafe { forge_cstring_empty() },
        Ok(n) => {
            buf.truncate(n);
            unsafe { forge_copy_bytes_to_cstring(&buf) }
        }
        Err(_) => std::ptr::null_mut(),
    }
}

fn forge_read_process_stream_bytes<R: std::io::Read>(reader: &mut R, max_bytes: i64) -> i64 {
    let size = if max_bytes > 0 { max_bytes as usize } else { 4096 };
    let mut buf = vec![0u8; size];
    match reader.read(&mut buf) {
        Ok(n) => {
            buf.truncate(n);
            forge_bytes_from_vec(buf)
        }
        Err(_) => 0,
    }
}

/// Read contents of a process's stdout
///
/// # Safety
/// handle must be a valid process handle
#[no_mangle]
pub unsafe extern "C" fn forge_process_read(handle: i64, max_bytes: i64) -> *mut i8 {
    let mut handles = crate::process::process_handles().lock();
    let Some(entry) = handles.get_mut(&handle) else {
        return std::ptr::null_mut();
    };
    let Some(stdout) = entry.stdout.as_mut() else {
        return unsafe { forge_cstring_empty() };
    };
    forge_read_process_stream(stdout, max_bytes)
}

#[no_mangle]
pub unsafe extern "C" fn forge_process_read_bytes(handle: i64, max_bytes: i64) -> i64 {
    let mut handles = crate::process::process_handles().lock();
    let Some(entry) = handles.get_mut(&handle) else {
        return 0;
    };
    let Some(stdout) = entry.stdout.as_mut() else {
        return forge_bytes_from_vec(Vec::new());
    };
    forge_read_process_stream_bytes(stdout, max_bytes)
}

/// Read contents of a process's stderr
///
/// # Safety
/// handle must be a valid process handle
#[no_mangle]
pub unsafe extern "C" fn forge_process_read_err(handle: i64, max_bytes: i64) -> *mut i8 {
    let mut handles = crate::process::process_handles().lock();
    let Some(entry) = handles.get_mut(&handle) else {
        return std::ptr::null_mut();
    };
    let Some(stderr) = entry.stderr.as_mut() else {
        return unsafe { forge_cstring_empty() };
    };
    forge_read_process_stream(stderr, max_bytes)
}

#[no_mangle]
pub unsafe extern "C" fn forge_process_read_err_bytes(handle: i64, max_bytes: i64) -> i64 {
    let mut handles = crate::process::process_handles().lock();
    let Some(entry) = handles.get_mut(&handle) else {
        return 0;
    };
    let Some(stderr) = entry.stderr.as_mut() else {
        return forge_bytes_from_vec(Vec::new());
    };
    forge_read_process_stream_bytes(stderr, max_bytes)
}

/// Write data to a process's stdin
///
/// # Safety
/// handle must be a valid process handle and data must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_process_write(handle: i64, data: *const i8) -> i64 {
    use std::io::Write;

    if data.is_null() {
        return 0;
    }
    let mut handles = crate::process::process_handles().lock();
    let Some(entry) = handles.get_mut(&handle) else {
        return 0;
    };
    let Some(stdin) = entry.stdin.as_mut() else {
        return 0;
    };
    let text = std::ffi::CStr::from_ptr(data).to_str().unwrap_or("");
    match stdin.write(text.as_bytes()) {
        Ok(n) => {
            let _ = stdin.flush();
            n as i64
        }
        Err(_) => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_process_write_bytes(handle: i64, data: i64) -> i64 {
    use std::io::Write;

    let Some(bytes) = forge_bytes_ref(data) else {
        return 0;
    };
    let mut handles = crate::process::process_handles().lock();
    let Some(entry) = handles.get_mut(&handle) else {
        return 0;
    };
    let Some(stdin) = entry.stdin.as_mut() else {
        return 0;
    };
    match stdin.write(&bytes.data) {
        Ok(n) => {
            let _ = stdin.flush();
            n as i64
        }
        Err(_) => 0,
    }
}

/// Allocate a zeroed block of `num_fields * 8` bytes for struct storage.
/// Returns the pointer as i64.
///
/// # Safety
/// Caller must ensure the returned pointer is eventually freed.
#[no_mangle]
pub unsafe extern "C" fn forge_struct_alloc(num_fields: i64) -> i64 {
    use std::alloc::{alloc_zeroed, Layout};

    let size = (num_fields.max(0) as usize) * 8;
    if size == 0 {
        return 0;
    }

    let layout = Layout::from_size_align(size, 8).unwrap();
    let ptr = alloc_zeroed(layout);
    if ptr.is_null() {
        return 0;
    }
    ptr as i64
}

/// Build a ForgeList of C-string pointers from `std::env::args()` and return
/// the list pointer as i64.
///
/// # Safety
/// Caller receives ownership of the list and its string allocations.
#[no_mangle]
pub unsafe extern "C" fn forge_args_to_list() -> i64 {
    use crate::collections::list::{forge_list_new, forge_list_push_value};
    use std::alloc::{alloc, Layout};

    let list = forge_list_new(8, 0); // primitive list of i64 (cstring pointers)

    for arg in std::env::args() {
        let arg_len = arg.len();
        let arg_layout = Layout::from_size_align(arg_len + 1, 1).unwrap();
        let arg_ptr = alloc(arg_layout) as *mut i8;

        if !arg_ptr.is_null() {
            std::ptr::copy_nonoverlapping(arg.as_ptr(), arg_ptr as *mut u8, arg_len);
            *arg_ptr.add(arg_len) = 0;
            forge_list_push_value(list, arg_ptr as i64);
        }
    }

    list.ptr as i64
}

// Re-export concurrency primitive FFI functions
pub use concurrency::{
    forge_mutex_lock, forge_mutex_new, forge_mutex_unlock, forge_semaphore_acquire,
    forge_semaphore_new, forge_semaphore_release, forge_waitgroup_add, forge_waitgroup_done,
    forge_waitgroup_new, forge_waitgroup_wait,
};
