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
pub mod collections;
pub mod concurrency;
pub mod string;

use crate::collections::list::ForgeList;
use std::sync::atomic::AtomicUsize;

/// Global statistics for debugging
pub static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static LIVE_OBJECTS: AtomicUsize = AtomicUsize::new(0);

/// Initialize the runtime
///
/// # Safety
/// Must be called before any other runtime functions
#[no_mangle]
pub unsafe extern "C" fn forge_runtime_init() {
    arc::init_cycle_collector();
}

/// Clean up the runtime
///
/// # Safety
/// Should be called at program exit
#[no_mangle]
pub unsafe extern "C" fn forge_runtime_shutdown() {
    arc::shutdown_cycle_collector();
}

/// Print a string to stdout
///
/// # Safety
/// s must be a valid ForgeString
#[no_mangle]
pub unsafe extern "C" fn forge_print(s: string::ForgeString) {
    use std::io::Write;

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
    let mut len_a = 0;
    let mut p = a;
    while *p != 0 {
        len_a += 1;
        p = p.add(1);
    }

    let mut len_b = 0;
    let mut p = b;
    while *p != 0 {
        len_b += 1;
        p = p.add(1);
    }

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

    let mut len = 0;
    let mut p = ptr;
    while *p != 0 {
        len += 1;
        p = p.add(1);
    }

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
    let mut len = 0;
    let mut p = ptr;
    while *p != 0 {
        len += 1;
        p = p.add(1);
    }

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
    use std::io::Write;

    if ptr.is_null() {
        eprintln!();
        return;
    }

    // Calculate length
    let mut len = 0;
    let mut p = ptr;
    while *p != 0 {
        len += 1;
        p = p.add(1);
    }

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
pub unsafe extern "C" fn forge_cstring_eq(a: *const i8, b: *const i8) -> bool {
    // Handle null cases
    if a.is_null() && b.is_null() {
        return true;
    }
    if a.is_null() || b.is_null() {
        return false;
    }

    // Compare byte by byte
    let mut pa = a;
    let mut pb = b;

    loop {
        let ca = *pa;
        let cb = *pb;

        if ca != cb {
            return false;
        }

        if ca == 0 {
            return true;
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
static mut TEST_FAILED: bool = false;

/// Assert that condition is true
#[no_mangle]
pub extern "C" fn forge_assert(cond: i64) {
    unsafe {
        if cond == 0 {
            TEST_FAILED = true;
            eprintln!("Assertion failed");
        }
    }
}

/// Assert that two values are equal
#[no_mangle]
pub extern "C" fn forge_assert_eq(a: i64, b: i64) {
    unsafe {
        if a != b {
            TEST_FAILED = true;
            eprintln!("Assertion failed: {} != {}", a, b);
        }
    }
}

/// Assert that two values are not equal  
#[no_mangle]
pub extern "C" fn forge_assert_ne(a: i64, b: i64) {
    unsafe {
        if a == b {
            TEST_FAILED = true;
            eprintln!("Assertion failed: {} == {}", a, b);
        }
    }
}

/// Check if any test failed
#[no_mangle]
pub extern "C" fn forge_test_result() -> i64 {
    unsafe {
        if TEST_FAILED {
            1
        } else {
            0
        }
    }
}

/// Reset test state
#[no_mangle]
pub extern "C" fn forge_test_reset() {
    unsafe {
        TEST_FAILED = false;
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

/// Check if a file exists
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_file_exists(path: *const i8) -> i8 {
    if path.is_null() {
        return 0;
    }

    let mut len = 0;
    let mut p = path;
    while *p != 0 {
        len += 1;
        p = p.add(1);
    }

    let slice = std::slice::from_raw_parts(path as *const u8, len);
    if let Ok(path_str) = std::str::from_utf8(slice) {
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
pub unsafe extern "C" fn forge_dir_exists(path: *const i8) -> i8 {
    if path.is_null() {
        return 0;
    }

    let mut len = 0;
    let mut p = path;
    while *p != 0 {
        len += 1;
        p = p.add(1);
    }

    let slice = std::slice::from_raw_parts(path as *const u8, len);
    if let Ok(path_str) = std::str::from_utf8(slice) {
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
pub unsafe extern "C" fn forge_mkdir(path: *const i8) -> i8 {
    use std::fs;

    if path.is_null() {
        return 0;
    }

    let mut len = 0;
    let mut p = path;
    while *p != 0 {
        len += 1;
        p = p.add(1);
    }

    let slice = std::slice::from_raw_parts(path as *const u8, len);
    if let Ok(path_str) = std::str::from_utf8(slice) {
        if fs::create_dir_all(path_str).is_ok() {
            return 1;
        }
    }
    0
}

/// Remove a file
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_remove_file(path: *const i8) -> i8 {
    use std::fs;

    if path.is_null() {
        return 0;
    }

    let mut len = 0;
    let mut p = path;
    while *p != 0 {
        len += 1;
        p = p.add(1);
    }

    let slice = std::slice::from_raw_parts(path as *const u8, len);
    if let Ok(path_str) = std::str::from_utf8(slice) {
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
pub unsafe extern "C" fn forge_rename_file(from: *const i8, to: *const i8) -> i8 {
    use std::fs;

    if from.is_null() || to.is_null() {
        return 0;
    }

    let mut from_len = 0;
    let mut p = from;
    while *p != 0 {
        from_len += 1;
        p = p.add(1);
    }

    let mut to_len = 0;
    let mut p = to;
    while *p != 0 {
        to_len += 1;
        p = p.add(1);
    }

    let from_slice = std::slice::from_raw_parts(from as *const u8, from_len);
    let to_slice = std::slice::from_raw_parts(to as *const u8, to_len);

    if let (Ok(from_str), Ok(to_str)) = (
        std::str::from_utf8(from_slice),
        std::str::from_utf8(to_slice),
    ) {
        if fs::rename(from_str, to_str).is_ok() {
            return 1;
        }
    }
    0
}

/// Read entire file contents as a C string
/// Returns null pointer on error. Caller must free with forge_free.
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_read_file(path: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};
    use std::fs;

    if path.is_null() {
        return std::ptr::null_mut();
    }

    let mut len = 0;
    let mut p = path;
    while *p != 0 {
        len += 1;
        p = p.add(1);
    }

    let slice = std::slice::from_raw_parts(path as *const u8, len);
    if let Ok(path_str) = std::str::from_utf8(slice) {
        if let Ok(contents) = fs::read_to_string(path_str) {
            let content_len = contents.len();
            let layout = Layout::from_size_align(content_len + 1, 1).unwrap();
            let ptr = alloc(layout) as *mut i8;

            if !ptr.is_null() {
                std::ptr::copy_nonoverlapping(contents.as_ptr(), ptr as *mut u8, content_len);
                *ptr.add(content_len) = 0;
            }
            return ptr;
        }
    }
    std::ptr::null_mut()
}

/// Write string to file
/// Returns 1 on success, 0 on failure
///
/// # Safety
/// Both path and content must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn forge_write_file(path: *const i8, content: *const i8) -> i8 {
    use std::fs;

    if path.is_null() || content.is_null() {
        return 0;
    }

    let mut path_len = 0;
    let mut p = path;
    while *p != 0 {
        path_len += 1;
        p = p.add(1);
    }

    let mut content_len = 0;
    let mut p = content;
    while *p != 0 {
        content_len += 1;
        p = p.add(1);
    }

    let path_slice = std::slice::from_raw_parts(path as *const u8, path_len);
    let content_slice = std::slice::from_raw_parts(content as *const u8, content_len);

    if let (Ok(path_str), Ok(content_str)) = (
        std::str::from_utf8(path_slice),
        std::str::from_utf8(content_slice),
    ) {
        if fs::write(path_str, content_str).is_ok() {
            return 1;
        }
    }
    0
}

/// Append string to file
/// Returns 1 on success, 0 on failure
///
/// # Safety
/// Both path and content must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn forge_append_file(path: *const i8, content: *const i8) -> i8 {
    use std::fs::OpenOptions;
    use std::io::Write;

    if path.is_null() || content.is_null() {
        return 0;
    }

    let mut path_len = 0;
    let mut p = path;
    while *p != 0 {
        path_len += 1;
        p = p.add(1);
    }

    let mut content_len = 0;
    let mut p = content;
    while *p != 0 {
        content_len += 1;
        p = p.add(1);
    }

    let path_slice = std::slice::from_raw_parts(path as *const u8, path_len);
    let content_slice = std::slice::from_raw_parts(content as *const u8, content_len);

    if let (Ok(path_str), Ok(_content_str)) = (
        std::str::from_utf8(path_slice),
        std::str::from_utf8(content_slice),
    ) {
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path_str) {
            if file.write_all(content_slice).is_ok() {
                return 1;
            }
        }
    }
    0
}

/// Exit the program with given status code
#[no_mangle]
pub extern "C" fn forge_exit(code: i64) {
    std::process::exit(code as i32);
}

/// Sleep for given number of milliseconds
#[no_mangle]
pub extern "C" fn forge_sleep(ms: i64) {
    std::thread::sleep(std::time::Duration::from_millis(ms as u64));
}

/// Get current time in milliseconds since epoch
#[no_mangle]
pub extern "C" fn forge_time() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Get environment variable value
/// Returns null pointer if not found. Caller must not free.
///
/// # Safety
/// name must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_env(name: *const i8) -> *const i8 {
    use std::alloc::{alloc, Layout};

    if name.is_null() {
        return std::ptr::null();
    }

    let mut len = 0;
    let mut p = name;
    while *p != 0 {
        len += 1;
        p = p.add(1);
    }

    let slice = std::slice::from_raw_parts(name as *const u8, len);
    if let Ok(name_str) = std::str::from_utf8(slice) {
        if let Ok(var) = std::env::var(name_str) {
            let var_len = var.len();
            let layout = Layout::from_size_align(var_len + 1, 1).unwrap();
            let ptr = alloc(layout) as *mut i8;

            if !ptr.is_null() {
                std::ptr::copy_nonoverlapping(var.as_ptr(), ptr as *mut u8, var_len);
                *ptr.add(var_len) = 0;
                return ptr;
            }
        }
    }
    std::ptr::null()
}

/// Read a line from stdin
/// Returns C string. Caller must free with forge_free.
#[no_mangle]
pub unsafe extern "C" fn forge_input() -> *mut i8 {
    use std::alloc::{alloc, Layout};
    use std::io::{self, BufRead};

    let stdin = io::stdin();
    let mut line = String::new();
    if stdin.lock().read_line(&mut line).is_ok() {
        // Remove trailing newline
        if line.ends_with('\n') {
            line.pop();
        }
        if line.ends_with('\r') {
            line.pop();
        }

        let len = line.len();
        let layout = Layout::from_size_align(len + 1, 1).unwrap();
        let ptr = alloc(layout) as *mut i8;

        if !ptr.is_null() {
            std::ptr::copy_nonoverlapping(line.as_ptr(), ptr as *mut u8, len);
            *ptr.add(len) = 0;
        }
        return ptr;
    }
    std::ptr::null_mut()
}

/// Simple string list node for list_dir
#[repr(C)]
pub struct StringNode {
    data: *mut i8,
    next: *mut StringNode,
}

/// List directory contents
/// Returns linked list of strings. Caller must free.
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_list_dir(path: *const i8) -> *mut StringNode {
    use std::alloc::{alloc, Layout};
    use std::fs;

    if path.is_null() {
        return std::ptr::null_mut();
    }

    let mut len = 0;
    let mut p = path;
    while *p != 0 {
        len += 1;
        p = p.add(1);
    }

    let slice = std::slice::from_raw_parts(path as *const u8, len);
    if let Ok(path_str) = std::str::from_utf8(slice) {
        if let Ok(entries) = fs::read_dir(path_str) {
            let mut head: *mut StringNode = std::ptr::null_mut();
            let mut tail: *mut StringNode = std::ptr::null_mut();

            for entry in entries {
                if let Ok(entry) = entry {
                    if let Some(name) = entry.file_name().to_str() {
                        let name_len = name.len();
                        let name_layout = Layout::from_size_align(name_len + 1, 1).unwrap();
                        let name_ptr = alloc(name_layout) as *mut i8;

                        if !name_ptr.is_null() {
                            std::ptr::copy_nonoverlapping(
                                name.as_ptr(),
                                name_ptr as *mut u8,
                                name_len,
                            );
                            *name_ptr.add(name_len) = 0;

                            let node_layout = Layout::new::<StringNode>();
                            let node_ptr = alloc(node_layout) as *mut StringNode;

                            if !node_ptr.is_null() {
                                (*node_ptr).data = name_ptr;
                                (*node_ptr).next = std::ptr::null_mut();

                                if head.is_null() {
                                    head = node_ptr;
                                    tail = node_ptr;
                                } else {
                                    (*tail).next = node_ptr;
                                    tail = node_ptr;
                                }
                            }
                        }
                    }
                }
            }
            return head;
        }
    }
    std::ptr::null_mut()
}

/// Execute a command and return exit code
///
/// # Safety
/// command must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_exec(command: *const i8) -> i64 {
    use std::process::Command;

    if command.is_null() {
        return -1;
    }

    let mut len = 0;
    let mut p = command;
    while *p != 0 {
        len += 1;
        p = p.add(1);
    }

    let slice = std::slice::from_raw_parts(command as *const u8, len);
    if let Ok(cmd_str) = std::str::from_utf8(slice) {
        // Simple shell execution
        let parts: Vec<&str> = cmd_str.split_whitespace().collect();
        if parts.is_empty() {
            return -1;
        }

        let mut cmd = Command::new(parts[0]);
        if parts.len() > 1 {
            cmd.args(&parts[1..]);
        }

        if let Ok(status) = cmd.status() {
            if let Some(code) = status.code() {
                return code as i64;
            }
            return 0;
        }
    }
    -1
}

/// Random float between 0.0 and 1.0
#[no_mangle]
pub extern "C" fn forge_random_float() -> f64 {
    use std::num::Wrapping;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEED: AtomicU64 = AtomicU64::new(123456789);

    let s = SEED.load(Ordering::Relaxed);
    // Simple LCG
    let new_s = Wrapping(s) * Wrapping(6364136223846793005) + Wrapping(1);
    SEED.store(new_s.0, Ordering::Relaxed);

    // Convert to float in range [0, 1)
    (new_s.0 >> 11) as f64 / (1u64 << 53) as f64
}

/// Seed the random number generator
#[no_mangle]
pub extern "C" fn forge_random_seed(seed: i64) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEED: AtomicU64 = AtomicU64::new(123456789);
    SEED.store(seed as u64, Ordering::Relaxed);
}

/// Random integer in range [min, max]
#[no_mangle]
pub extern "C" fn forge_random_int(min: i64, max: i64) -> i64 {
    if min >= max {
        return min;
    }
    let range = (max - min + 1) as u64;
    let r = (forge_random_float() * range as f64) as i64;
    min + r
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

/// Format float with given precision
/// Returns C string. Caller must free.
///
/// # Safety
/// fmt must be a valid null-terminated format string
#[no_mangle]
pub unsafe extern "C" fn forge_fmt_float(n: f64, precision: i64) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    let s = format!("{:.1$}", n, precision as usize);
    let len = s.len();
    let layout = Layout::from_size_align(len + 1, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;

    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(s.as_ptr(), ptr as *mut u8, len);
        *ptr.add(len) = 0;
    }
    ptr
}

/// Generate random string of given length
/// Returns C string. Caller must free.
///
/// # Safety
/// Caller must free result with forge_free
#[no_mangle]
pub unsafe extern "C" fn forge_random_string(len: i64) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let n = len.max(0) as usize;

    let layout = Layout::from_size_align(n + 1, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;

    if !ptr.is_null() {
        for i in 0..n {
            let idx = (forge_random_float() * CHARSET.len() as f64) as usize % CHARSET.len();
            *ptr.add(i) = CHARSET[idx] as i8;
        }
        *ptr.add(n) = 0;
    }
    ptr
}

/// Free memory allocated by runtime
///
/// # Safety
/// ptr must have been allocated by the runtime
#[no_mangle]
pub unsafe extern "C" fn forge_free(ptr: *mut i8) {
    use std::alloc::{dealloc, Layout};

    if !ptr.is_null() {
        // We don't know the size, but we can deallocate with size 0
        // This is implementation-specific but works with system allocators
        std::alloc::dealloc(ptr as *mut u8, Layout::new::<u8>());
    }
}

/// Get command line arguments as a Forge list of C string pointers.
#[no_mangle]
pub unsafe extern "C" fn forge_args() -> ForgeList {
    use crate::collections::list::{forge_list_new, forge_list_push_value};
    use std::alloc::{alloc, Layout};
    use std::env;

    let list = forge_list_new(8, 1);

    for arg in env::args() {
        let arg_len = arg.len();
        let arg_layout = Layout::from_size_align(arg_len + 1, 1).unwrap();
        let arg_ptr = alloc(arg_layout) as *mut i8;

        if !arg_ptr.is_null() {
            std::ptr::copy_nonoverlapping(arg.as_ptr(), arg_ptr as *mut u8, arg_len);
            *arg_ptr.add(arg_len) = 0;
            forge_list_push_value(list, arg_ptr as i64);
        }
    }

    list
}

/// Extract substring from C string (start inclusive, end exclusive)
/// Returns newly allocated C string
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_substring(s: *const i8, start: i64, end: i64) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::forge_cstring_len(s);
    let start = start.max(0).min(len);
    let end = end.max(start).min(len);
    let sub_len = (end - start) as usize;

    if sub_len == 0 {
        let layout = Layout::from_size_align(1, 1).unwrap();
        let ptr = alloc(layout) as *mut i8;
        if !ptr.is_null() {
            *ptr = 0;
        }
        return ptr;
    }

    let layout = Layout::from_size_align(sub_len + 1, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;

    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(
            s.offset(start as isize) as *const u8,
            ptr as *mut u8,
            sub_len,
        );
        *ptr.add(sub_len) = 0;
    }
    ptr
}

/// Split a string by delimiter and return as a ForgeList of strings
///
/// # Safety
/// Both s and delim must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn forge_string_split_to_list(s: *const i8, delim: *const i8) -> ForgeList {
    use crate::collections::list::{forge_list_new, forge_list_push, ListTypeTag};
    use std::alloc::{alloc, Layout};

    if s.is_null() || delim.is_null() {
        return forge_list_new(8, 0); // Return empty list
    }

    let s_len = crate::string::forge_cstring_len(s) as usize;
    let delim_len = crate::string::forge_cstring_len(delim) as usize;

    if s_len == 0 {
        return forge_list_new(8, 0); // Return empty list for empty string
    }

    let s_slice = std::slice::from_raw_parts(s as *const u8, s_len);
    let delim_slice = if delim_len == 0 {
        &[] as &[u8]
    } else {
        std::slice::from_raw_parts(delim as *const u8, delim_len)
    };

    // Create a new list for strings (type_tag = 1 for strings)
    let mut list = forge_list_new(8, 1);

    let mut start = 0;
    for i in 0..=s_len {
        let is_delim = if delim_len == 0 {
            false
        } else if i + delim_len <= s_len {
            &s_slice[i..i + delim_len] == delim_slice
        } else {
            false
        };

        if is_delim || i == s_len {
            let part_len = i - start;
            if part_len > 0 {
                // Allocate and copy the substring
                let part_layout = Layout::from_size_align(part_len + 1, 1).unwrap();
                let part_ptr = alloc(part_layout) as *mut i8;

                if !part_ptr.is_null() {
                    std::ptr::copy_nonoverlapping(&s_slice[start], part_ptr as *mut u8, part_len);
                    *part_ptr.add(part_len) = 0;

                    // Push to list (as a pointer)
                    forge_list_push(&mut list, part_ptr as *const u8, 8);
                }
            }

            if delim_len > 0 {
                start = i + delim_len;
            } else {
                start = i + 1;
            }
        }
    }

    list
}

/// Trim ASCII whitespace from both ends of a C string.
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_trim(s: *const i8) -> *mut i8 {
    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::forge_cstring_len(s) as usize;
    let slice = std::slice::from_raw_parts(s as *const u8, len);

    let mut start = 0usize;
    let mut end = len;

    while start < end && matches!(slice[start], b' ' | b'\t' | b'\n' | b'\r') {
        start += 1;
    }
    while end > start && matches!(slice[end - 1], b' ' | b'\t' | b'\n' | b'\r') {
        end -= 1;
    }

    forge_cstring_substring(s, start as i64, end as i64)
}

/// Get a single character from a C string at index as a new C string.
/// Returns a newly allocated 1-character string (or empty string if out of bounds)
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_char_at(s: *const i8, index: i64) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        let ptr = alloc(Layout::from_size_align(1, 1).unwrap()) as *mut i8;
        if !ptr.is_null() {
            *ptr = 0;
        }
        return ptr;
    }

    let len = crate::string::forge_cstring_len(s);
    if index < 0 || index >= len {
        // Out of bounds - return empty string
        let ptr = alloc(Layout::from_size_align(1, 1).unwrap()) as *mut i8;
        if !ptr.is_null() {
            *ptr = 0;
        }
        return ptr;
    }

    // Allocate 2 bytes (1 char + null terminator)
    let ptr = alloc(Layout::from_size_align(2, 1).unwrap()) as *mut i8;
    if !ptr.is_null() {
        *ptr = *s.offset(index as isize);
        *ptr.add(1) = 0;
    }
    ptr
}

/// Trim ASCII whitespace from the left side of a C string.
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_trim_left(s: *const i8) -> *mut i8 {
    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::forge_cstring_len(s) as usize;
    let slice = std::slice::from_raw_parts(s as *const u8, len);

    let mut start = 0usize;
    while start < len && matches!(slice[start], b' ' | b'\t' | b'\n' | b'\r') {
        start += 1;
    }

    forge_cstring_substring(s, start as i64, len as i64)
}
