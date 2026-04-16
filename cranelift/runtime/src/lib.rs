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

/// Parse string to int — returns 0 on failure
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_parse_int(s: *const i8) -> i64 {
    if s.is_null() {
        return 0;
    }
    let len = crate::string::forge_cstring_len(s) as usize;
    let slice = std::slice::from_raw_parts(s as *const u8, len);
    if let Ok(str_ref) = std::str::from_utf8(slice) {
        str_ref.trim().parse::<i64>().unwrap_or(0)
    } else {
        0
    }
}

/// Parse string to float — returns 0.0 on failure
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_parse_float(s: *const i8) -> f64 {
    if s.is_null() {
        return 0.0;
    }
    let len = crate::string::forge_cstring_len(s) as usize;
    let slice = std::slice::from_raw_parts(s as *const u8, len);
    if let Ok(str_ref) = std::str::from_utf8(slice) {
        str_ref.trim().parse::<f64>().unwrap_or(0.0)
    } else {
        0.0
    }
}

/// Base64 encode a C string — returns newly allocated C string
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_b64_encode(s: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::forge_cstring_len(s) as usize;
    let input = std::slice::from_raw_parts(s as *const u8, len);

    // Simple base64 encoding
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out: Vec<u8> = Vec::with_capacity((len + 2) / 3 * 4 + 1);
    let mut i = 0;
    while i < len {
        let b0 = input[i];
        let b1 = if i + 1 < len { input[i + 1] } else { 0 };
        let b2 = if i + 2 < len { input[i + 2] } else { 0 };

        out.push(CHARS[(b0 >> 2) as usize]);
        out.push(CHARS[((b0 & 3) << 4 | b1 >> 4) as usize]);
        out.push(if i + 1 < len {
            CHARS[((b1 & 0xf) << 2 | b2 >> 6) as usize]
        } else {
            b'='
        });
        out.push(if i + 2 < len {
            CHARS[(b2 & 0x3f) as usize]
        } else {
            b'='
        });

        i += 3;
    }
    out.push(0); // null terminator

    let layout = Layout::from_size_align(out.len(), 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(out.as_ptr(), ptr as *mut u8, out.len());
    }
    ptr
}

/// Hex encode a C string — returns newly allocated C string
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_hex_encode(s: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::forge_cstring_len(s) as usize;
    let input = std::slice::from_raw_parts(s as *const u8, len);
    let hex_len = len * 2 + 1;
    let layout = Layout::from_size_align(hex_len, 1).unwrap();
    let ptr = alloc(layout) as *mut u8;

    if !ptr.is_null() {
        for (i, &byte) in input.iter().enumerate() {
            let hi = (byte >> 4) as usize;
            let lo = (byte & 0xf) as usize;
            const HEX: &[u8] = b"0123456789abcdef";
            *ptr.add(i * 2) = HEX[hi];
            *ptr.add(i * 2 + 1) = HEX[lo];
        }
        *ptr.add(len * 2) = 0;
    }
    ptr as *mut i8
}

/// Decode a hex string back to the original string
///
/// # Safety
/// s must be a valid null-terminated C string of hex digits
#[no_mangle]
pub unsafe extern "C" fn forge_from_hex(s: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::forge_cstring_len(s) as usize;
    if len % 2 != 0 {
        return std::ptr::null_mut();
    }
    let input = std::slice::from_raw_parts(s as *const u8, len);
    let out_len = len / 2;
    let layout = Layout::from_size_align(out_len + 1, 1).unwrap();
    let ptr = alloc(layout) as *mut u8;

    if !ptr.is_null() {
        for i in 0..out_len {
            let hi = hex_digit(input[i * 2]);
            let lo = hex_digit(input[i * 2 + 1]);
            *ptr.add(i) = (hi << 4) | lo;
        }
        *ptr.add(out_len) = 0;
    }
    ptr as *mut i8
}

fn hex_digit(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

/// Convert an integer to hex string (e.g., 255 → "ff")
///
/// # Safety
/// Returns heap-allocated null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_int_to_hex(n: i64) -> *mut i8 {
    use std::alloc::{alloc, Layout};
    let s = format!("{:x}", n);
    let bytes = s.as_bytes();
    let layout = Layout::array::<u8>(bytes.len() + 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
        *ptr.add(bytes.len()) = 0;
    }
    ptr
}

/// Convert an integer to octal string (e.g., 8 → "10")
///
/// # Safety
/// Returns heap-allocated null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_int_to_oct(n: i64) -> *mut i8 {
    use std::alloc::{alloc, Layout};
    let s = format!("{:o}", n);
    let bytes = s.as_bytes();
    let layout = Layout::array::<u8>(bytes.len() + 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
        *ptr.add(bytes.len()) = 0;
    }
    ptr
}

/// Convert an integer to binary string (e.g., 10 → "1010")
///
/// # Safety
/// Returns heap-allocated null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_int_to_bin(n: i64) -> *mut i8 {
    use std::alloc::{alloc, Layout};
    let s = format!("{:b}", n);
    let bytes = s.as_bytes();
    let layout = Layout::array::<u8>(bytes.len() + 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
        *ptr.add(bytes.len()) = 0;
    }
    ptr
}

/// SHA-256 hash of a C string — returns hex-encoded C string
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_sha256(s: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        // Return placeholder
        let placeholder = b"0000000000000000000000000000000000000000000000000000000000000000\0";
        let layout = Layout::from_size_align(placeholder.len(), 1).unwrap();
        let ptr = alloc(layout) as *mut i8;
        if !ptr.is_null() {
            std::ptr::copy_nonoverlapping(placeholder.as_ptr(), ptr as *mut u8, placeholder.len());
        }
        return ptr;
    }

    let len = crate::string::forge_cstring_len(s) as usize;
    let input = std::slice::from_raw_parts(s as *const u8, len);

    // Simple SHA-256 implementation (no external deps)
    let hash = sha256_compute(input);
    let mut hex = Vec::with_capacity(65);
    const HEX: &[u8] = b"0123456789abcdef";
    for &byte in &hash {
        hex.push(HEX[(byte >> 4) as usize]);
        hex.push(HEX[(byte & 0xf) as usize]);
    }
    hex.push(0);

    let layout = Layout::from_size_align(hex.len(), 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(hex.as_ptr(), ptr as *mut u8, hex.len());
    }
    ptr
}

/// Compute SHA-256 hash
fn sha256_compute(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let orig_len = data.len();
    let mut msg: Vec<u8> = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    let bit_len = (orig_len as u64) * 8;
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] =
            [h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut result = [0u8; 32];
    for (i, &word) in h.iter().enumerate() {
        result[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    result
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
