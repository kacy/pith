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
pub mod ffi_util;
pub mod string;

use crate::collections::list::ForgeList;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::fs::File;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::OnceLock;

/// Global statistics for debugging
pub static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static LIVE_OBJECTS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_RC_ALLOCS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_RC_RETAINS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_RC_RELEASES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_STRING_ALLOCS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_STRING_ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_BYTES_ALLOCS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_BYTES_ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_BYTE_BUFFER_NEWS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_BYTE_BUFFER_WRITES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_BYTE_BUFFER_WRITE_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_PUSHES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_GETS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_GET_VALUE_CALLS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_GET_VALUE_CHECKED_CALLS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_GET_VALUE_UNCHECKED_CALLS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_GET_BYTES_CALLS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_GET_ELEM8: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_GET_ELEM_OTHER: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_SETS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_INSERTS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_REMOVES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_INSERTS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_STRING_INSERTS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_GETS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_STRING_GETS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_CONTAINS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_STRING_CONTAINS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_REMOVES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_STRING_REMOVES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_FAST_INSERTS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_FAST_GETS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_FAST_CONTAINS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_FAST_REMOVES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_FALLBACK_INSERTS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_FALLBACK_GETS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_FALLBACK_CONTAINS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_FALLBACK_REMOVES: AtomicUsize = AtomicUsize::new(0);

static PERF_STATS_ENABLED: OnceLock<bool> = OnceLock::new();
static PERF_STATS_REGISTERED: AtomicBool = AtomicBool::new(false);

struct ProcessHandle {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
}

struct ProcessOutputHandle {
    status: i64,
    stdout: String,
    stderr: String,
}

struct ForgeBytes {
    data: Vec<u8>,
}

struct ForgeByteBuffer {
    data: Vec<u8>,
}

static PROCESS_HANDLES: OnceLock<Mutex<HashMap<i64, ProcessHandle>>> = OnceLock::new();
static NEXT_PROCESS_HANDLE: AtomicI64 = AtomicI64::new(1);
static PROCESS_OUTPUT_HANDLES: OnceLock<Mutex<HashMap<i64, ProcessOutputHandle>>> = OnceLock::new();
static NEXT_PROCESS_OUTPUT_HANDLE: AtomicI64 = AtomicI64::new(1);
static FILE_HANDLES: OnceLock<Mutex<HashMap<i64, File>>> = OnceLock::new();
static NEXT_FILE_HANDLE: AtomicI64 = AtomicI64::new(1);

pub fn perf_stats_enabled() -> bool {
    *PERF_STATS_ENABLED.get_or_init(|| {
        matches!(
            std::env::var("FORGE_PERF_STATS").ok().as_deref(),
            Some("1") | Some("true") | Some("yes")
        )
    })
}

pub fn perf_count(counter: &AtomicUsize, delta: usize) {
    if perf_stats_enabled() {
        counter.fetch_add(delta, Ordering::Relaxed);
    }
}

extern "C" fn forge_perf_dump_stats_at_exit() {
    dump_perf_stats();
}

pub fn ensure_perf_stats_registered() {
    if !perf_stats_enabled() {
        return;
    }
    if PERF_STATS_REGISTERED.swap(true, Ordering::Relaxed) {
        return;
    }
    unsafe {
        libc::atexit(forge_perf_dump_stats_at_exit);
    }
}

pub fn dump_perf_stats() {
    if !perf_stats_enabled() {
        return;
    }
    eprintln!("forge perf stats");
    eprintln!("  rc allocs: {}", PERF_RC_ALLOCS.load(Ordering::Relaxed));
    eprintln!("  rc retains: {}", PERF_RC_RETAINS.load(Ordering::Relaxed));
    eprintln!("  rc releases: {}", PERF_RC_RELEASES.load(Ordering::Relaxed));
    eprintln!(
        "  string allocs: {} bytes={}",
        PERF_STRING_ALLOCS.load(Ordering::Relaxed),
        PERF_STRING_ALLOC_BYTES.load(Ordering::Relaxed)
    );
    eprintln!(
        "  bytes allocs: {} bytes={}",
        PERF_BYTES_ALLOCS.load(Ordering::Relaxed),
        PERF_BYTES_ALLOC_BYTES.load(Ordering::Relaxed)
    );
    eprintln!(
        "  byte_buffer new: {} writes={} write_bytes={}",
        PERF_BYTE_BUFFER_NEWS.load(Ordering::Relaxed),
        PERF_BYTE_BUFFER_WRITES.load(Ordering::Relaxed),
        PERF_BYTE_BUFFER_WRITE_BYTES.load(Ordering::Relaxed)
    );
    eprintln!(
        "  list ops: push={} get={} get_value={} checked={} unchecked={} get_bytes={} elem8={} elem_other={} set={} insert={} remove={}",
        PERF_LIST_PUSHES.load(Ordering::Relaxed),
        PERF_LIST_GETS.load(Ordering::Relaxed),
        PERF_LIST_GET_VALUE_CALLS.load(Ordering::Relaxed),
        PERF_LIST_GET_VALUE_CHECKED_CALLS.load(Ordering::Relaxed),
        PERF_LIST_GET_VALUE_UNCHECKED_CALLS.load(Ordering::Relaxed),
        PERF_LIST_GET_BYTES_CALLS.load(Ordering::Relaxed),
        PERF_LIST_GET_ELEM8.load(Ordering::Relaxed),
        PERF_LIST_GET_ELEM_OTHER.load(Ordering::Relaxed),
        PERF_LIST_SETS.load(Ordering::Relaxed),
        PERF_LIST_INSERTS.load(Ordering::Relaxed),
        PERF_LIST_REMOVES.load(Ordering::Relaxed)
    );
    eprintln!(
        "  map int ops: insert={} get={} contains={} remove={}",
        PERF_MAP_INT_INSERTS.load(Ordering::Relaxed),
        PERF_MAP_INT_GETS.load(Ordering::Relaxed),
        PERF_MAP_INT_CONTAINS.load(Ordering::Relaxed),
        PERF_MAP_INT_REMOVES.load(Ordering::Relaxed)
    );
    eprintln!(
        "  map int path: fast_insert={} fast_get={} fast_contains={} fast_remove={} fallback_insert={} fallback_get={} fallback_contains={} fallback_remove={}",
        PERF_MAP_INT_FAST_INSERTS.load(Ordering::Relaxed),
        PERF_MAP_INT_FAST_GETS.load(Ordering::Relaxed),
        PERF_MAP_INT_FAST_CONTAINS.load(Ordering::Relaxed),
        PERF_MAP_INT_FAST_REMOVES.load(Ordering::Relaxed),
        PERF_MAP_INT_FALLBACK_INSERTS.load(Ordering::Relaxed),
        PERF_MAP_INT_FALLBACK_GETS.load(Ordering::Relaxed),
        PERF_MAP_INT_FALLBACK_CONTAINS.load(Ordering::Relaxed),
        PERF_MAP_INT_FALLBACK_REMOVES.load(Ordering::Relaxed)
    );
    eprintln!(
        "  map string ops: insert={} get={} contains={} remove={}",
        PERF_MAP_STRING_INSERTS.load(Ordering::Relaxed),
        PERF_MAP_STRING_GETS.load(Ordering::Relaxed),
        PERF_MAP_STRING_CONTAINS.load(Ordering::Relaxed),
        PERF_MAP_STRING_REMOVES.load(Ordering::Relaxed)
    );
}

fn process_handles() -> &'static Mutex<HashMap<i64, ProcessHandle>> {
    PROCESS_HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn process_output_handles() -> &'static Mutex<HashMap<i64, ProcessOutputHandle>> {
    PROCESS_OUTPUT_HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn file_handles() -> &'static Mutex<HashMap<i64, File>> {
    FILE_HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}

unsafe fn forge_optional_cstring(ptr: *const i8) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let len = crate::string::forge_cstring_len(ptr) as usize;
    let slice = std::slice::from_raw_parts(ptr as *const u8, len);
    std::str::from_utf8(slice).unwrap_or("").to_string()
}

unsafe fn forge_required_cstring(ptr: *const i8) -> Option<String> {
    let text = forge_optional_cstring(ptr);
    if text.is_empty() {
        return None;
    }
    Some(text)
}

unsafe fn forge_string_list_to_vec(list: ForgeList) -> Vec<String> {
    let len = crate::collections::list::forge_list_len(list);
    let mut values = Vec::with_capacity(len as usize);
    let mut i = 0;
    while i < len {
        let ptr = crate::collections::list::forge_list_get_value(list, i) as *const i8;
        values.push(forge_optional_cstring(ptr));
        i += 1;
    }
    values
}

fn forge_store_process_output(status: i64, stdout: String, stderr: String) -> i64 {
    let handle = NEXT_PROCESS_OUTPUT_HANDLE.fetch_add(1, Ordering::Relaxed);
    let entry = ProcessOutputHandle {
        status,
        stdout,
        stderr,
    };
    process_output_handles().lock().insert(handle, entry);
    handle
}

fn forge_strdup_string(text: &str) -> *mut i8 {
    let owned = format!("{}\0", text);
    unsafe { forge_strdup(owned.as_ptr() as *const i8) }
}

unsafe fn forge_build_command(
    program: *const i8,
    argv: ForgeList,
    cwd: *const i8,
    env_keys: ForgeList,
    env_values: ForgeList,
) -> Option<Command> {
    let program_text = forge_required_cstring(program)?;
    let mut command = Command::new(program_text);

    for arg in forge_string_list_to_vec(argv) {
        command.arg(arg);
    }

    let cwd_text = forge_optional_cstring(cwd);
    if !cwd_text.is_empty() {
        command.current_dir(cwd_text);
    }

    let keys = forge_string_list_to_vec(env_keys);
    let values = forge_string_list_to_vec(env_values);
    for (key, value) in keys.into_iter().zip(values.into_iter()) {
        command.env(key, value);
    }

    Some(command)
}

unsafe fn forge_bytes_ref<'a>(handle: i64) -> Option<&'a ForgeBytes> {
    if handle == 0 {
        return None;
    }
    Some(&*(handle as *const ForgeBytes))
}

unsafe fn forge_byte_buffer_mut<'a>(handle: i64) -> Option<&'a mut ForgeByteBuffer> {
    if handle == 0 {
        return None;
    }
    Some(&mut *(handle as *mut ForgeByteBuffer))
}

fn forge_bytes_from_vec(data: Vec<u8>) -> i64 {
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTES_ALLOCS, 1);
    perf_count(&PERF_BYTES_ALLOC_BYTES, data.len());
    Box::into_raw(Box::new(ForgeBytes { data })) as i64
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

/// Check if a file exists
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_file_exists(path: *const i8) -> i64 {
    if path.is_null() {
        return 0;
    }

    let len = crate::string::forge_cstring_len(path) as usize;

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
pub unsafe extern "C" fn forge_dir_exists(path: *const i8) -> i64 {
    if path.is_null() {
        return 0;
    }

    let len = crate::string::forge_cstring_len(path) as usize;

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
pub unsafe extern "C" fn forge_mkdir(path: *const i8) -> i64 {
    use std::fs;

    if path.is_null() {
        return 0;
    }

    let len = crate::string::forge_cstring_len(path) as usize;

    let slice = std::slice::from_raw_parts(path as *const u8, len);
    if let Ok(path_str) = std::str::from_utf8(slice) {
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
pub unsafe extern "C" fn forge_remove_dir(path: *const i8) -> i64 {
    use std::fs;

    if path.is_null() {
        return 0;
    }

    let len = crate::string::forge_cstring_len(path) as usize;
    let slice = std::slice::from_raw_parts(path as *const u8, len);
    if let Ok(path_str) = std::str::from_utf8(slice) {
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
pub unsafe extern "C" fn forge_remove_tree(path: *const i8) -> i64 {
    use std::fs;

    if path.is_null() {
        return 0;
    }

    let len = crate::string::forge_cstring_len(path) as usize;
    let slice = std::slice::from_raw_parts(path as *const u8, len);
    if let Ok(path_str) = std::str::from_utf8(slice) {
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
pub unsafe extern "C" fn forge_file_size(path: *const i8) -> i64 {
    if path.is_null() {
        return -1;
    }

    let len = crate::string::forge_cstring_len(path) as usize;
    let slice = std::slice::from_raw_parts(path as *const u8, len);
    if let Ok(path_str) = std::str::from_utf8(slice) {
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
pub unsafe extern "C" fn forge_remove_file(path: *const i8) -> i64 {
    use std::fs;

    if path.is_null() {
        return 0;
    }

    let len = crate::string::forge_cstring_len(path) as usize;

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
pub unsafe extern "C" fn forge_rename_file(from: *const i8, to: *const i8) -> i64 {
    use std::fs;

    if from.is_null() || to.is_null() {
        return 0;
    }

    let from_len = crate::string::forge_cstring_len(from) as usize;
    let to_len = crate::string::forge_cstring_len(to) as usize;

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

    let len = crate::string::forge_cstring_len(path) as usize;

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

#[no_mangle]
pub unsafe extern "C" fn forge_read_file_bytes(path: *const i8) -> i64 {
    use std::fs;

    if path.is_null() {
        return 0;
    }

    let len = crate::string::forge_cstring_len(path) as usize;
    let slice = std::slice::from_raw_parts(path as *const u8, len);
    let Ok(path_str) = std::str::from_utf8(slice) else {
        return 0;
    };

    match fs::read(path_str) {
        Ok(contents) => forge_bytes_from_vec(contents),
        Err(_) => 0,
    }
}

/// Write string to file
/// Returns 1 on success, 0 on failure
///
/// # Safety
/// Both path and content must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn forge_write_file(path: *const i8, content: *const i8) -> i64 {
    use std::fs;

    if path.is_null() || content.is_null() {
        return 0;
    }

    let path_len = crate::string::forge_cstring_len(path) as usize;
    let content_len = crate::string::forge_cstring_len(content) as usize;

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

#[no_mangle]
pub unsafe extern "C" fn forge_write_file_bytes(path: *const i8, content: i64) -> i64 {
    use std::fs;

    if path.is_null() {
        return 0;
    }
    let Some(bytes) = forge_bytes_ref(content) else {
        return 0;
    };
    let path_len = crate::string::forge_cstring_len(path) as usize;
    let path_slice = std::slice::from_raw_parts(path as *const u8, path_len);
    let Ok(path_str) = std::str::from_utf8(path_slice) else {
        return 0;
    };
    if fs::write(path_str, &bytes.data).is_ok() {
        return 1;
    }
    0
}

/// Append string to file
/// Returns 1 on success, 0 on failure
///
/// # Safety
/// Both path and content must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn forge_append_file(path: *const i8, content: *const i8) -> i64 {
    use std::fs::OpenOptions;
    use std::io::Write;

    if path.is_null() || content.is_null() {
        return 0;
    }

    let path_len = crate::string::forge_cstring_len(path) as usize;
    let content_len = crate::string::forge_cstring_len(content) as usize;

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

#[no_mangle]
pub unsafe extern "C" fn forge_append_file_bytes(path: *const i8, content: i64) -> i64 {
    use std::fs::OpenOptions;
    use std::io::Write;

    if path.is_null() {
        return 0;
    }
    let Some(bytes) = forge_bytes_ref(content) else {
        return 0;
    };
    let path_len = crate::string::forge_cstring_len(path) as usize;
    let path_slice = std::slice::from_raw_parts(path as *const u8, path_len);
    let Ok(path_str) = std::str::from_utf8(path_slice) else {
        return 0;
    };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path_str) {
        if file.write_all(&bytes.data).is_ok() {
            return 1;
        }
    }
    0
}

unsafe fn forge_open_file_with(path: *const i8, create: bool, write: bool, append: bool) -> i64 {
    use std::fs::OpenOptions;

    if path.is_null() {
        return 0;
    }

    let len = crate::string::forge_cstring_len(path) as usize;
    let slice = std::slice::from_raw_parts(path as *const u8, len);
    let Ok(path_str) = std::str::from_utf8(slice) else {
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

/// Open a file for reading and return a file handle
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_file_open_read(path: *const i8) -> i64 {
    forge_open_file_with(path, false, false, false)
}

/// Open a file for writing and return a file handle
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_file_open_write(path: *const i8) -> i64 {
    forge_open_file_with(path, true, true, false)
}

/// Open a file for appending and return a file handle
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_file_open_append(path: *const i8) -> i64 {
    forge_open_file_with(path, true, false, true)
}

/// Read a chunk from an open file handle
///
/// # Safety
/// handle must be a valid file handle
#[no_mangle]
pub unsafe extern "C" fn forge_file_read(handle: i64, max_bytes: i64) -> *mut i8 {
    use std::io::Read;

    let size = if max_bytes > 0 { max_bytes as usize } else { 4096 };
    let mut handles = file_handles().lock();
    let Some(file) = handles.get_mut(&handle) else {
        return std::ptr::null_mut();
    };

    let mut buf = vec![0u8; size];
    match file.read(&mut buf) {
        Ok(0) => forge_cstring_empty(),
        Ok(n) => {
            buf.truncate(n);
            forge_copy_bytes_to_cstring(&buf)
        }
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_file_read_bytes(handle: i64, max_bytes: i64) -> i64 {
    use std::io::Read;

    let size = if max_bytes > 0 { max_bytes as usize } else { 4096 };
    let mut handles = file_handles().lock();
    let Some(file) = handles.get_mut(&handle) else {
        return 0;
    };

    let mut buf = vec![0u8; size];
    match file.read(&mut buf) {
        Ok(n) => {
            buf.truncate(n);
            forge_bytes_from_vec(buf)
        }
        Err(_) => 0,
    }
}

/// Write a chunk to an open file handle
///
/// # Safety
/// handle must be a valid file handle and data must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_file_write(handle: i64, data: *const i8) -> i64 {
    use std::io::Write;

    if data.is_null() {
        return 0;
    }

    let len = crate::string::forge_cstring_len(data) as usize;
    let bytes = std::slice::from_raw_parts(data as *const u8, len);
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
pub unsafe extern "C" fn forge_file_write_bytes(handle: i64, data: i64) -> i64 {
    use std::io::Write;

    let Some(bytes) = forge_bytes_ref(data) else {
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

/// Close an open file handle
#[no_mangle]
pub extern "C" fn forge_file_close(handle: i64) {
    file_handles().lock().remove(&handle);
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

    let len = crate::string::forge_cstring_len(name) as usize;

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
    forge_strdup_string("")
}

/// Current working directory.
///
/// # Safety
/// returns a heap-allocated c string pointer or null on failure
#[no_mangle]
pub unsafe extern "C" fn forge_os_getcwd() -> *const i8 {
    if let Ok(path) = std::env::current_dir() {
        if let Some(text) = path.to_str() {
            return forge_strdup_string(text);
        }
    }
    std::ptr::null()
}

/// Change current working directory.
///
/// # Safety
/// path must be a valid null-terminated c string
#[no_mangle]
pub unsafe extern "C" fn forge_os_chdir(path: *const i8) -> i64 {
    if path.is_null() {
        return 0;
    }

    let len = crate::string::forge_cstring_len(path) as usize;
    let slice = std::slice::from_raw_parts(path as *const u8, len);
    if let Ok(path_str) = std::str::from_utf8(slice) {
        if std::env::set_current_dir(path_str).is_ok() {
            return 1;
        }
    }
    0
}

/// Return the platform temp directory.
///
/// # Safety
/// returns a heap-allocated c string pointer or null on failure
#[no_mangle]
pub unsafe extern "C" fn forge_os_temp_dir() -> *const i8 {
    let path = std::env::temp_dir();
    if let Some(text) = path.to_str() {
        return forge_strdup_string(text);
    }
    std::ptr::null()
}

/// Return the home directory from process environment when available.
///
/// # Safety
/// returns a heap-allocated c string pointer or null when unavailable
#[no_mangle]
pub unsafe extern "C" fn forge_os_home_dir() -> *const i8 {
    if let Ok(home) = std::env::var("HOME") {
        return forge_strdup_string(&home);
    }
    if let Ok(home) = std::env::var("USERPROFILE") {
        return forge_strdup_string(&home);
    }
    std::ptr::null()
}

/// Set an environment variable.
///
/// # Safety
/// both inputs must be valid null-terminated c strings
#[no_mangle]
pub unsafe extern "C" fn forge_os_set_env(name: *const i8, value: *const i8) -> i64 {
    if name.is_null() || value.is_null() {
        return 0;
    }

    let name_len = crate::string::forge_cstring_len(name) as usize;
    let value_len = crate::string::forge_cstring_len(value) as usize;
    let name_slice = std::slice::from_raw_parts(name as *const u8, name_len);
    let value_slice = std::slice::from_raw_parts(value as *const u8, value_len);
    if let (Ok(name_str), Ok(value_str)) = (
        std::str::from_utf8(name_slice),
        std::str::from_utf8(value_slice),
    ) {
        unsafe {
            std::env::set_var(name_str, value_str);
        }
        return 1;
    }
    0
}

/// Unset an environment variable.
///
/// # Safety
/// name must be a valid null-terminated c string
#[no_mangle]
pub unsafe extern "C" fn forge_os_unset_env(name: *const i8) -> i64 {
    if name.is_null() {
        return 0;
    }

    let name_len = crate::string::forge_cstring_len(name) as usize;
    let name_slice = std::slice::from_raw_parts(name as *const u8, name_len);
    if let Ok(name_str) = std::str::from_utf8(name_slice) {
        unsafe {
            std::env::remove_var(name_str);
        }
        return 1;
    }
    0
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

/// List directory contents
/// Returns a Forge List[String].
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_list_dir(path: *const i8) -> i64 {
    use crate::collections::list::{forge_list_new, forge_list_push_value};
    use std::fs;

    if path.is_null() {
        return forge_list_new(8, 1).ptr as i64;
    }

    let len = crate::string::forge_cstring_len(path) as usize;

    let slice = std::slice::from_raw_parts(path as *const u8, len);
    if let Ok(path_str) = std::str::from_utf8(slice) {
        if let Ok(entries) = fs::read_dir(path_str) {
            let list = forge_list_new(8, 1);

            for entry in entries {
                if let Ok(entry) = entry {
                    if let Some(name) = entry.file_name().to_str() {
                        let name_ptr = forge_strdup_string(name) as i64;
                        forge_list_push_value(list, name_ptr);
                    }
                }
            }
            return list.ptr as i64;
        }
    }
    forge_list_new(8, 1).ptr as i64
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

    let len = crate::string::forge_cstring_len(command) as usize;

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
    use std::alloc::Layout;

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
    use crate::collections::list::forge_list_new;
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

    // Create a new list storing cstring pointers as i64 values (type_tag = 0, primitive)
    let list = forge_list_new(8, 0);

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

                    // Push the pointer VALUE into the list (not the data at the pointer)
                    crate::collections::list::forge_list_push_value(list, part_ptr as i64);
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

/// Convert C string to uppercase
/// Returns newly allocated C string
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_to_upper(s: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::forge_cstring_len(s) as usize;
    let slice = std::slice::from_raw_parts(s as *const u8, len);

    let layout = Layout::from_size_align(len + 1, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;

    if !ptr.is_null() {
        for i in 0..len {
            let c = slice[i];
            *ptr.add(i) = (c as char).to_ascii_uppercase() as u8 as i8;
        }
        *ptr.add(len) = 0;
    }
    ptr
}

/// Convert C string to lowercase
/// Returns newly allocated C string
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_to_lower(s: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::forge_cstring_len(s) as usize;
    let slice = std::slice::from_raw_parts(s as *const u8, len);

    let layout = Layout::from_size_align(len + 1, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;

    if !ptr.is_null() {
        for i in 0..len {
            let c = slice[i];
            *ptr.add(i) = (c as char).to_ascii_lowercase() as u8 as i8;
        }
        *ptr.add(len) = 0;
    }
    ptr
}

/// Reverse a C string
/// Returns newly allocated C string
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_reverse(s: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::forge_cstring_len(s) as usize;
    let slice = std::slice::from_raw_parts(s as *const u8, len);

    let layout = Layout::from_size_align(len + 1, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;

    if !ptr.is_null() {
        for i in 0..len {
            *ptr.add(i) = slice[len - 1 - i] as i8;
        }
        *ptr.add(len) = 0;
    }
    ptr
}

/// Split string into a list of single-character strings (chars)
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_chars(s: *const i8) -> i64 {
    use crate::collections::list::{forge_list_new, forge_list_push_value};
    let list = forge_list_new(8, 0); // ForgeList by value — stores cstring pointers
    if !s.is_null() {
        let len = crate::string::forge_cstring_len(s) as usize;
        let bytes = std::slice::from_raw_parts(s as *const u8, len);
        for &b in bytes {
            let ch_ptr = forge_chr_cstr(b as i64);
            forge_list_push_value(list, ch_ptr as i64);
        }
    }
    // Return the internal pointer value (ForgeList.ptr), not a box pointer.
    // All list functions (forge_list_len, forge_list_join, etc.) receive
    // ForgeList by value, which is just this pointer field.
    list.ptr as i64
}

/// Sort a list of C-string pointers in-place (lexicographic order)
///
/// # Safety
/// list_ptr is i64 carrying the ForgeList's internal ptr value;
/// each 8-byte element is a *const i8 pointer to a null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn forge_list_sort_strings(list_ptr: i64) {
    use crate::collections::list::ForgeList;
    let list = ForgeList {
        ptr: list_ptr as *mut (),
    };
    if list.ptr.is_null() {
        return;
    }
    let impl_ref = &mut *(list.ptr as *mut crate::collections::list::ListImpl);
    impl_ref.elements.sort_by(|a, b| {
        let ap = if a.len() >= 8 {
            i64::from_ne_bytes(a[..8].try_into().unwrap_or([0; 8])) as *const i8
        } else {
            std::ptr::null()
        };
        let bp = if b.len() >= 8 {
            i64::from_ne_bytes(b[..8].try_into().unwrap_or([0; 8])) as *const i8
        } else {
            std::ptr::null()
        };
        if ap.is_null() && bp.is_null() {
            return std::cmp::Ordering::Equal;
        }
        if ap.is_null() {
            return std::cmp::Ordering::Less;
        }
        if bp.is_null() {
            return std::cmp::Ordering::Greater;
        }
        let a_str = std::ffi::CStr::from_ptr(ap);
        let b_str = std::ffi::CStr::from_ptr(bp);
        a_str.cmp(b_str)
    });
}

/// Sort a list of i64 values in-place
///
/// # Safety
/// list_ptr is i64 carrying the ForgeList's internal ptr value
#[no_mangle]
pub unsafe extern "C" fn forge_list_sort(list_ptr: i64) {
    use crate::collections::list::ForgeList;
    let list = ForgeList {
        ptr: list_ptr as *mut (),
    };
    if list.ptr.is_null() {
        return;
    }
    let impl_ref = &mut *(list.ptr as *mut crate::collections::list::ListImpl);
    impl_ref.elements.sort_by(|a, b| {
        let av = if a.len() >= 8 {
            i64::from_ne_bytes(a[..8].try_into().unwrap_or([0; 8]))
        } else {
            0
        };
        let bv = if b.len() >= 8 {
            i64::from_ne_bytes(b[..8].try_into().unwrap_or([0; 8]))
        } else {
            0
        };
        av.cmp(&bv)
    });
}

/// Get a sub-slice of a list
///
/// # Safety
/// list_ptr is i64 carrying the ForgeList's internal ptr value
#[no_mangle]
pub unsafe extern "C" fn forge_list_slice(list_ptr: i64, start: i64, end: i64) -> i64 {
    use crate::collections::list::{forge_list_new, forge_list_push_value, ForgeList};
    let new_list = forge_list_new(8, 0);
    let list = ForgeList {
        ptr: list_ptr as *mut (),
    };
    if !list.ptr.is_null() {
        let impl_ref = &*(list.ptr as *const crate::collections::list::ListImpl);
        let len = impl_ref.elements.len() as i64;
        let s = start.max(0).min(len) as usize;
        let e = end.max(0).min(len) as usize;
        for i in s..e {
            if let Some(elem) = impl_ref.elements.get(i) {
                let val = if elem.len() >= 8 {
                    i64::from_ne_bytes(elem[..8].try_into().unwrap_or([0; 8]))
                } else {
                    0
                };
                forge_list_push_value(new_list, val);
            }
        }
    }
    // Return the internal pointer value (ForgeList.ptr), not a box pointer
    new_list.ptr as i64
}

/// Replace all occurrences of `from` with `to` in `s`
/// Returns newly allocated C string
///
/// # Safety
/// All pointers must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_replace(
    s: *const i8,
    from: *const i8,
    to: *const i8,
) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        return std::ptr::null_mut();
    }

    let s_len = crate::string::forge_cstring_len(s) as usize;
    let from_len = if from.is_null() {
        0
    } else {
        crate::string::forge_cstring_len(from) as usize
    };
    let to_len = if to.is_null() {
        0
    } else {
        crate::string::forge_cstring_len(to) as usize
    };

    if from_len == 0 {
        // Nothing to replace — return copy of s
        let layout = Layout::from_size_align(s_len + 1, 1).unwrap();
        let out = alloc(layout) as *mut i8;
        if !out.is_null() {
            std::ptr::copy_nonoverlapping(s, out, s_len);
            *out.add(s_len) = 0;
        }
        return out;
    }

    let s_bytes = std::slice::from_raw_parts(s as *const u8, s_len);
    let from_bytes = std::slice::from_raw_parts(from as *const u8, from_len);
    let to_bytes = if to.is_null() {
        &[][..]
    } else {
        std::slice::from_raw_parts(to as *const u8, to_len)
    };

    // Find all occurrences and build result
    let mut result: Vec<u8> = Vec::with_capacity(s_len);
    let mut i = 0;
    while i <= s_len.saturating_sub(from_len) {
        if &s_bytes[i..i + from_len] == from_bytes {
            result.extend_from_slice(to_bytes);
            i += from_len;
        } else {
            result.push(s_bytes[i]);
            i += 1;
        }
    }
    // Copy remaining bytes
    while i < s_len {
        result.push(s_bytes[i]);
        i += 1;
    }

    let out_len = result.len();
    let layout = Layout::from_size_align(out_len + 1, 1).unwrap();
    let out = alloc(layout) as *mut i8;
    if !out.is_null() {
        std::ptr::copy_nonoverlapping(result.as_ptr(), out as *mut u8, out_len);
        *out.add(out_len) = 0;
    }
    out
}

/// Check if a C string is empty (null or zero-length)
///
/// # Safety
/// s must be null or a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_is_empty(s: *const i8) -> i64 {
    if s.is_null() {
        return 1;
    }
    let len = crate::string::forge_cstring_len(s);
    if len == 0 {
        1
    } else {
        0
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

/// Format time as string — takes unix timestamp (ms) and format string
/// Simple implementation: returns ISO-like date string
///
/// # Safety
/// fmt must be a valid null-terminated C string (or null for default)
#[no_mangle]
pub unsafe extern "C" fn forge_format_time_fmt(timestamp_ms: i64, _fmt: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    // Convert ms to seconds
    let secs = timestamp_ms / 1000;
    // Simple date formatting: just show the timestamp for now
    let s = format!("{}", secs);
    let bytes = s.as_bytes();
    let layout = Layout::from_size_align(bytes.len() + 1, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
        *ptr.add(bytes.len()) = 0;
    }
    ptr
}

/// Write string to file path
/// Returns 1 on success, 0 on failure
///
/// # Safety
/// Both pointers must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn forge_fs_write(path: *const i8, content: *const i8) -> i64 {
    if path.is_null() || content.is_null() {
        return 0;
    }
    let path_len = crate::string::forge_cstring_len(path) as usize;
    let content_len = crate::string::forge_cstring_len(content) as usize;
    let path_slice = std::slice::from_raw_parts(path as *const u8, path_len);
    let content_slice = std::slice::from_raw_parts(content as *const u8, content_len);

    if let (Ok(path_str), Ok(content_str)) = (
        std::str::from_utf8(path_slice),
        std::str::from_utf8(content_slice),
    ) {
        match std::fs::write(path_str, content_str) {
            Ok(_) => 1,
            Err(_) => 0,
        }
    } else {
        0
    }
}

/// Log an info message
///
/// # Safety
/// msg must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_log_info(msg: *const i8) {
    if msg.is_null() {
        eprintln!("[INFO] ");
        return;
    }
    let len = crate::string::forge_cstring_len(msg) as usize;
    let slice = std::slice::from_raw_parts(msg as *const u8, len);
    if let Ok(s) = std::str::from_utf8(slice) {
        eprintln!("[INFO] {}", s);
    }
}

/// Log a warning message
///
/// # Safety
/// msg must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_log_warn(msg: *const i8) {
    if msg.is_null() {
        eprintln!("[WARN] ");
        return;
    }
    let len = crate::string::forge_cstring_len(msg) as usize;
    let slice = std::slice::from_raw_parts(msg as *const u8, len);
    if let Ok(s) = std::str::from_utf8(slice) {
        eprintln!("[WARN] {}", s);
    }
}

/// Log an error message
///
/// # Safety
/// msg must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_log_error(msg: *const i8) {
    if msg.is_null() {
        eprintln!("[ERROR] ");
        return;
    }
    let len = crate::string::forge_cstring_len(msg) as usize;
    let slice = std::slice::from_raw_parts(msg as *const u8, len);
    if let Ok(s) = std::str::from_utf8(slice) {
        eprintln!("[ERROR] {}", s);
    }
}

/// Smart to_string for Unknown-typed values: distinguishes likely heap string
/// pointers from small integer-like values using address range heuristics.
#[no_mangle]
pub unsafe extern "C" fn forge_smart_to_string(val: i64) -> *mut i8 {
    // Small-magnitude values are treated as integers.
    // Large positive values are treated as heap-allocated C string pointers.
    if val <= 0 || (val > 0 && val < 1_000_000) {
        forge_int_to_cstr(val)
    } else {
        forge_strdup(val as *const i8)
    }
}

/// Spawn a child process and return a process handle
///
/// # Safety
/// cmd must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_process_spawn(cmd: *const i8) -> i64 {
    if cmd.is_null() {
        return 0;
    }
    let len = crate::string::forge_cstring_len(cmd) as usize;
    let slice = std::slice::from_raw_parts(cmd as *const u8, len);
    if let Ok(cmd_str) = std::str::from_utf8(slice) {
        match Command::new("/bin/sh")
            .arg("-lc")
            .arg(cmd_str)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                let handle = NEXT_PROCESS_HANDLE.fetch_add(1, Ordering::Relaxed);
                let entry = ProcessHandle {
                    stdin: child.stdin.take(),
                    stdout: child.stdout.take(),
                    stderr: child.stderr.take(),
                    child,
                };
                process_handles().lock().insert(handle, entry);
                handle
            }
            Err(_) => 0,
        }
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_process_spawn_argv(
    program: *const i8,
    argv: ForgeList,
    cwd: *const i8,
    env_keys: ForgeList,
    env_values: ForgeList,
) -> i64 {
    let Some(mut command) = forge_build_command(program, argv, cwd, env_keys, env_values) else {
        return 0;
    };

    match command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(mut child) => {
            let handle = NEXT_PROCESS_HANDLE.fetch_add(1, Ordering::Relaxed);
            let entry = ProcessHandle {
                stdin: child.stdin.take(),
                stdout: child.stdout.take(),
                stderr: child.stderr.take(),
                child,
            };
            process_handles().lock().insert(handle, entry);
            handle
        }
        Err(_) => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_process_output_argv(
    program: *const i8,
    argv: ForgeList,
    cwd: *const i8,
    env_keys: ForgeList,
    env_values: ForgeList,
) -> i64 {
    let Some(mut command) = forge_build_command(program, argv, cwd, env_keys, env_values) else {
        return 0;
    };

    match command.output() {
        Ok(output) => {
            let status = output.status.code().unwrap_or(-1) as i64;
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            forge_store_process_output(status, stdout, stderr)
        }
        Err(_) => 0,
    }
}

#[no_mangle]
pub extern "C" fn forge_process_output_status(handle: i64) -> i64 {
    let outputs = process_output_handles().lock();
    let Some(entry) = outputs.get(&handle) else {
        return -1;
    };
    entry.status
}

#[no_mangle]
pub extern "C" fn forge_process_output_close(handle: i64) {
    process_output_handles().lock().remove(&handle);
}

#[no_mangle]
pub extern "C" fn forge_process_output_stdout(handle: i64) -> *mut i8 {
    let outputs = process_output_handles().lock();
    let Some(entry) = outputs.get(&handle) else {
        return std::ptr::null_mut();
    };
    forge_strdup_string(&entry.stdout)
}

#[no_mangle]
pub extern "C" fn forge_process_output_stderr(handle: i64) -> *mut i8 {
    let outputs = process_output_handles().lock();
    let Some(entry) = outputs.get(&handle) else {
        return std::ptr::null_mut();
    };
    forge_strdup_string(&entry.stderr)
}

/// Execute command and capture output — returns stdout as C string
///
/// # Safety
/// cmd must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_exec_output(cmd: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if cmd.is_null() {
        return std::ptr::null_mut();
    }
    let len = crate::string::forge_cstring_len(cmd) as usize;
    let slice = std::slice::from_raw_parts(cmd as *const u8, len);
    if let Ok(cmd_str) = std::str::from_utf8(slice) {
        let parts: Vec<&str> = cmd_str.split_whitespace().collect();
        if parts.is_empty() {
            return std::ptr::null_mut();
        }
        match std::process::Command::new(parts[0])
            .args(&parts[1..])
            .output()
        {
            Ok(output) => {
                let stdout = &output.stdout;
                let layout = Layout::from_size_align(stdout.len() + 1, 1).unwrap();
                let ptr = alloc(layout) as *mut i8;
                if !ptr.is_null() {
                    std::ptr::copy_nonoverlapping(stdout.as_ptr(), ptr as *mut u8, stdout.len());
                    *ptr.add(stdout.len()) = 0;
                }
                ptr
            }
            Err(_) => std::ptr::null_mut(),
        }
    } else {
        std::ptr::null_mut()
    }
}

/// Base64 decode
///
/// # Safety
/// s must be a valid null-terminated base64-encoded C string
#[no_mangle]
pub unsafe extern "C" fn forge_b64_decode(s: *const i8) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    if s.is_null() {
        return std::ptr::null_mut();
    }

    let len = crate::string::forge_cstring_len(s) as usize;
    let input = std::slice::from_raw_parts(s as *const u8, len);

    // Standard base64 decoding
    const DECODE: [u8; 256] = {
        let mut t = [255u8; 256];
        let mut i = 0u8;
        while i < 26 { t[(b'A' + i) as usize] = i; i += 1; }
        i = 0;
        while i < 26 { t[(b'a' + i) as usize] = i + 26; i += 1; }
        i = 0;
        while i < 10 { t[(b'0' + i) as usize] = i + 52; i += 1; }
        t[b'+' as usize] = 62;
        t[b'/' as usize] = 63;
        t
    };

    // Calculate output size (strip padding)
    let mut in_len = len;
    while in_len > 0 && input[in_len - 1] == b'=' {
        in_len -= 1;
    }
    let out_len = in_len * 3 / 4;
    let layout = Layout::from_size_align(out_len + 1, 1).unwrap();
    let ptr = alloc(layout) as *mut u8;

    if ptr.is_null() {
        return std::ptr::null_mut();
    }

    let mut si = 0;
    let mut di = 0;
    while si + 3 < in_len {
        let a = DECODE[input[si] as usize] as u32;
        let b = DECODE[input[si + 1] as usize] as u32;
        let c = DECODE[input[si + 2] as usize] as u32;
        let d = DECODE[input[si + 3] as usize] as u32;
        let n = (a << 18) | (b << 12) | (c << 6) | d;
        if di < out_len { *ptr.add(di) = (n >> 16) as u8; di += 1; }
        if di < out_len { *ptr.add(di) = (n >> 8) as u8; di += 1; }
        if di < out_len { *ptr.add(di) = n as u8; di += 1; }
        si += 4;
    }
    // Handle remaining 2 or 3 chars
    if si + 1 < in_len {
        let a = DECODE[input[si] as usize] as u32;
        let b = DECODE[input[si + 1] as usize] as u32;
        let n = (a << 18) | (b << 12);
        if di < out_len { *ptr.add(di) = (n >> 16) as u8; di += 1; }
        if si + 2 < in_len {
            let c = DECODE[input[si + 2] as usize] as u32;
            let n2 = (a << 18) | (b << 12) | (c << 6);
            // Recalculate second byte with c included
            if di < out_len { *ptr.add(di) = ((n2 >> 8) & 0xff) as u8; di += 1; }
        }
    }
    *ptr.add(di.min(out_len)) = 0;

    ptr as *mut i8
}

/// FNV-1a hash — returns hash as i64
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_fnv1a(s: *const i8) -> i64 {
    if s.is_null() {
        return 0;
    }
    let len = crate::string::forge_cstring_len(s) as usize;
    let bytes = std::slice::from_raw_parts(s as *const u8, len);
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash as i64
}

/// Find first occurrence of needle in haystack, return index or -1
///
/// # Safety
/// Both must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_index_of(haystack: *const i8, needle: *const i8) -> i64 {
    if haystack.is_null() || needle.is_null() {
        return -1;
    }
    let h_len = crate::string::forge_cstring_len(haystack) as usize;
    let n_len = crate::string::forge_cstring_len(needle) as usize;
    if n_len == 0 {
        return 0;
    }
    let h_bytes = std::slice::from_raw_parts(haystack as *const u8, h_len);
    let n_bytes = std::slice::from_raw_parts(needle as *const u8, n_len);
    for i in 0..=(h_len.saturating_sub(n_len)) {
        if &h_bytes[i..i + n_len] == n_bytes {
            return i as i64;
        }
    }
    -1
}

/// Check if haystack string contains needle string
///
/// # Safety
/// Both must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_contains(haystack: *const i8, needle: *const i8) -> i64 {
    if haystack.is_null() || needle.is_null() {
        return 0;
    }
    let h_len = crate::string::forge_cstring_len(haystack) as usize;
    let n_len = crate::string::forge_cstring_len(needle) as usize;
    if n_len == 0 {
        return 1; // empty needle is always contained
    }
    if n_len > h_len {
        return 0;
    }
    let h_bytes = std::slice::from_raw_parts(haystack as *const u8, h_len);
    let n_bytes = std::slice::from_raw_parts(needle as *const u8, n_len);
    for i in 0..=(h_len - n_len) {
        if &h_bytes[i..i + n_len] == n_bytes {
            return 1;
        }
    }
    0
}

/// Check if string starts with prefix
///
/// # Safety
/// Both must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_starts_with(s: *const i8, prefix: *const i8) -> i64 {
    if s.is_null() || prefix.is_null() {
        return 0;
    }
    let s_len = crate::string::forge_cstring_len(s) as usize;
    let p_len = crate::string::forge_cstring_len(prefix) as usize;
    if p_len > s_len {
        return 0;
    }
    let s_bytes = std::slice::from_raw_parts(s as *const u8, p_len);
    let p_bytes = std::slice::from_raw_parts(prefix as *const u8, p_len);
    if s_bytes == p_bytes {
        1
    } else {
        0
    }
}

/// Check if string ends with suffix
///
/// # Safety
/// Both must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_ends_with(s: *const i8, suffix: *const i8) -> i64 {
    if s.is_null() || suffix.is_null() {
        return 0;
    }
    let s_len = crate::string::forge_cstring_len(s) as usize;
    let x_len = crate::string::forge_cstring_len(suffix) as usize;
    if x_len > s_len {
        return 0;
    }
    let s_bytes = std::slice::from_raw_parts(s as *const u8, s_len);
    let x_bytes = std::slice::from_raw_parts(suffix as *const u8, x_len);
    if &s_bytes[s_len - x_len..] == x_bytes {
        1
    } else {
        0
    }
}

/// Pad string on the left to given width
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_pad_left(
    s: *const i8,
    width: i64,
    fill: *const i8,
) -> *mut i8 {
    use std::alloc::{alloc, Layout};
    if s.is_null() {
        return std::ptr::null_mut();
    }
    let len = crate::string::forge_cstring_len(s) as usize;
    let w = width as usize;
    if len >= w {
        return forge_strdup(s);
    }
    // Use first char of fill string, default to space
    let fill_char = if !fill.is_null() && *fill != 0 {
        *fill
    } else {
        b' ' as i8
    };
    let pad = w - len;
    let total = w + 1;
    let layout = Layout::from_size_align(total, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        for i in 0..pad {
            *ptr.add(i) = fill_char;
        }
        std::ptr::copy_nonoverlapping(s, ptr.add(pad), len);
        *ptr.add(w) = 0;
    }
    ptr
}

/// Pad string on the right to given width
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_pad_right(
    s: *const i8,
    width: i64,
    fill: *const i8,
) -> *mut i8 {
    use std::alloc::{alloc, Layout};
    if s.is_null() {
        return std::ptr::null_mut();
    }
    let len = crate::string::forge_cstring_len(s) as usize;
    let w = width as usize;
    if len >= w {
        return forge_strdup(s);
    }
    // Use first char of fill string, default to space
    let fill_char = if !fill.is_null() && *fill != 0 {
        *fill
    } else {
        b' ' as i8
    };
    let total = w + 1;
    let layout = Layout::from_size_align(total, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(s, ptr, len);
        for i in len..w {
            *ptr.add(i) = fill_char;
        }
        *ptr.add(w) = 0;
    }
    ptr
}

/// Repeat a string n times
///
/// # Safety
/// s must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_repeat(s: *const i8, n: i64) -> *mut i8 {
    use std::alloc::{alloc, Layout};
    if s.is_null() || n <= 0 {
        return forge_cstring_empty();
    }
    let len = crate::string::forge_cstring_len(s) as usize;
    let total = len * n as usize + 1;
    let layout = Layout::from_size_align(total, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        for i in 0..n as usize {
            std::ptr::copy_nonoverlapping(s, ptr.add(i * len), len);
        }
        *ptr.add(len * n as usize) = 0;
    }
    ptr
}

unsafe fn forge_cstring_empty() -> *mut i8 {
    use std::alloc::{alloc, Layout};
    let layout = Layout::from_size_align(1, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        *ptr = 0;
    }
    ptr
}

unsafe fn forge_copy_bytes_to_cstring(bytes: &[u8]) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    let layout = Layout::from_size_align(bytes.len() + 1, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
        *ptr.add(bytes.len()) = 0;
    }
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn forge_bytes_from_string_utf8(s: *const i8) -> i64 {
    if s.is_null() {
        return forge_bytes_from_vec(Vec::new());
    }
    let len = crate::string::forge_cstring_len(s) as usize;
    let bytes = std::slice::from_raw_parts(s as *const u8, len);
    forge_bytes_from_vec(bytes.to_vec())
}

#[no_mangle]
pub unsafe extern "C" fn forge_bytes_to_string_utf8(handle: i64) -> *mut i8 {
    let Some(bytes) = forge_bytes_ref(handle) else {
        return std::ptr::null_mut();
    };
    if std::str::from_utf8(&bytes.data).is_err() {
        return std::ptr::null_mut();
    }
    forge_copy_bytes_to_cstring(&bytes.data)
}

#[no_mangle]
pub unsafe extern "C" fn forge_bytes_len(handle: i64) -> i64 {
    let Some(bytes) = forge_bytes_ref(handle) else {
        return 0;
    };
    bytes.data.len() as i64
}

#[no_mangle]
pub unsafe extern "C" fn forge_bytes_is_empty(handle: i64) -> i64 {
    let Some(bytes) = forge_bytes_ref(handle) else {
        return 1;
    };
    if bytes.data.is_empty() { 1 } else { 0 }
}

#[no_mangle]
pub unsafe extern "C" fn forge_bytes_get(handle: i64, idx: i64) -> i64 {
    let Some(bytes) = forge_bytes_ref(handle) else {
        return 0;
    };
    if idx < 0 {
        return 0;
    }
    bytes.data.get(idx as usize).copied().unwrap_or(0) as i64
}

#[no_mangle]
pub unsafe extern "C" fn forge_bytes_slice(handle: i64, start: i64, end: i64) -> i64 {
    let Some(bytes) = forge_bytes_ref(handle) else {
        return 0;
    };
    let len = bytes.data.len() as i64;
    let mut start_idx = start.max(0).min(len);
    let mut end_idx = end.max(0).min(len);
    if end_idx < start_idx {
        std::mem::swap(&mut start_idx, &mut end_idx);
    }
    forge_bytes_from_vec(bytes.data[start_idx as usize..end_idx as usize].to_vec())
}

#[no_mangle]
pub unsafe extern "C" fn forge_bytes_concat(a: i64, b: i64) -> i64 {
    let Some(a_bytes) = forge_bytes_ref(a) else {
        return 0;
    };
    let Some(b_bytes) = forge_bytes_ref(b) else {
        return 0;
    };
    let mut out = Vec::with_capacity(a_bytes.data.len() + b_bytes.data.len());
    out.extend_from_slice(&a_bytes.data);
    out.extend_from_slice(&b_bytes.data);
    forge_bytes_from_vec(out)
}

#[no_mangle]
pub unsafe extern "C" fn forge_bytes_eq(a: i64, b: i64) -> i64 {
    if a == 0 && b == 0 {
        return 1;
    }
    let Some(a_bytes) = forge_bytes_ref(a) else {
        return 0;
    };
    let Some(b_bytes) = forge_bytes_ref(b) else {
        return 0;
    };
    if a_bytes.data == b_bytes.data { 1 } else { 0 }
}

#[no_mangle]
pub extern "C" fn forge_byte_buffer_new() -> i64 {
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTE_BUFFER_NEWS, 1);
    Box::into_raw(Box::new(ForgeByteBuffer { data: Vec::new() })) as i64
}

#[no_mangle]
pub extern "C" fn forge_byte_buffer_with_capacity(capacity: i64) -> i64 {
    let cap = if capacity > 0 { capacity as usize } else { 0 };
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTE_BUFFER_NEWS, 1);
    Box::into_raw(Box::new(ForgeByteBuffer { data: Vec::with_capacity(cap) })) as i64
}

#[no_mangle]
pub unsafe extern "C" fn forge_byte_buffer_write(handle: i64, data: i64) -> i64 {
    let Some(buffer) = forge_byte_buffer_mut(handle) else {
        return 0;
    };
    let Some(bytes) = forge_bytes_ref(data) else {
        return 0;
    };
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTE_BUFFER_WRITES, 1);
    perf_count(&PERF_BYTE_BUFFER_WRITE_BYTES, bytes.data.len());
    buffer.data.extend_from_slice(&bytes.data);
    bytes.data.len() as i64
}

#[no_mangle]
pub unsafe extern "C" fn forge_byte_buffer_write_byte(handle: i64, value: i64) -> i64 {
    let Some(buffer) = forge_byte_buffer_mut(handle) else {
        return 0;
    };
    if !(0..=255).contains(&value) {
        return 0;
    }
    ensure_perf_stats_registered();
    perf_count(&PERF_BYTE_BUFFER_WRITES, 1);
    perf_count(&PERF_BYTE_BUFFER_WRITE_BYTES, 1);
    buffer.data.push(value as u8);
    1
}

#[no_mangle]
pub unsafe extern "C" fn forge_byte_buffer_bytes(handle: i64) -> i64 {
    let Some(buffer) = forge_byte_buffer_mut(handle) else {
        return 0;
    };
    forge_bytes_from_vec(buffer.data.clone())
}

#[no_mangle]
pub unsafe extern "C" fn forge_byte_buffer_clear(handle: i64) {
    if let Some(buffer) = forge_byte_buffer_mut(handle) {
        buffer.data.clear();
    }
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
    let mut handles = process_handles().lock();
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
    let mut handles = process_handles().lock();
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
    let mut handles = process_handles().lock();
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
    let mut handles = process_handles().lock();
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
    let mut handles = process_handles().lock();
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
    let mut handles = process_handles().lock();
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

/// TCP listen — bind and listen on addr:port, return server fd
#[no_mangle]
pub unsafe extern "C" fn forge_tcp_listen(addr: *const i8, port: i64) -> i64 {
    use std::net::TcpListener;
    let host = if addr.is_null() { "0.0.0.0" } else { std::ffi::CStr::from_ptr(addr).to_str().unwrap_or("0.0.0.0") };
    let bind_addr = format!("{}:{}", host, port);
    match TcpListener::bind(&bind_addr) {
        Ok(listener) => {
            use std::os::unix::io::IntoRawFd;
            listener.into_raw_fd() as i64
        }
        Err(_) => 0,
    }
}

/// TCP connect — connect to addr:port, return connection fd
#[no_mangle]
pub unsafe extern "C" fn forge_tcp_connect(addr: *const i8, port: i64) -> i64 {
    use std::net::TcpStream;
    let host = if addr.is_null() { "127.0.0.1" } else { std::ffi::CStr::from_ptr(addr).to_str().unwrap_or("127.0.0.1") };
    let connect_addr = format!("{}:{}", host, port);
    match TcpStream::connect(&connect_addr) {
        Ok(stream) => {
            // Set default read timeout of 5 seconds
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
            use std::os::unix::io::IntoRawFd;
            stream.into_raw_fd() as i64
        }
        Err(_) => 0,
    }
}

/// TCP accept — accept a connection on a server fd, return client fd
#[no_mangle]
pub extern "C" fn forge_tcp_accept(server_fd: i64) -> i64 {
    if server_fd <= 0 { return 0; }
    use std::net::TcpListener;
    use std::os::unix::io::FromRawFd;
    let listener = unsafe { TcpListener::from_raw_fd(server_fd as i32) };
    let result = match listener.accept() {
        Ok((stream, _addr)) => {
            use std::os::unix::io::IntoRawFd;
            stream.into_raw_fd() as i64
        }
        Err(_) => 0,
    };
    // Don't drop the listener — leak it back to keep the fd alive
    use std::os::unix::io::IntoRawFd;
    let _ = listener.into_raw_fd();
    result
}

/// TCP read — read up to 4096 bytes from connection fd, return as C string
#[no_mangle]
pub extern "C" fn forge_tcp_read(conn_fd: i64) -> *mut i8 {
    use std::io::Read;
    use std::net::TcpStream;
    use std::os::unix::io::FromRawFd;
    if conn_fd <= 0 { return std::ptr::null_mut(); }
    let mut stream = unsafe { TcpStream::from_raw_fd(conn_fd as i32) };
    let mut buf = vec![0u8; 4096];
    let result = match stream.read(&mut buf) {
        Ok(n) => {
            buf.truncate(n);
            let s = String::from_utf8_lossy(&buf).to_string();
            unsafe { forge_strdup(s.as_ptr() as *const i8) }
        }
        Err(_) => std::ptr::null_mut(),
    };
    use std::os::unix::io::IntoRawFd;
    let _ = stream.into_raw_fd();
    result
}

/// TCP read with max bytes limit
#[no_mangle]
pub extern "C" fn forge_tcp_read2(conn_fd: i64, max_bytes: i64) -> *mut i8 {
    use std::io::Read;
    use std::net::TcpStream;
    use std::os::unix::io::FromRawFd;
    if conn_fd <= 0 { return std::ptr::null_mut(); }
    let mut stream = unsafe { TcpStream::from_raw_fd(conn_fd as i32) };
    let size = if max_bytes > 0 { max_bytes as usize } else { 4096 };
    let mut buf = vec![0u8; size];
    let result = match stream.read(&mut buf) {
        Ok(n) => {
            buf.truncate(n);
            let s = String::from_utf8_lossy(&buf).to_string();
            unsafe { forge_strdup(s.as_ptr() as *const i8) }
        }
        Err(_) => std::ptr::null_mut(),
    };
    use std::os::unix::io::IntoRawFd;
    let _ = stream.into_raw_fd();
    result
}

#[no_mangle]
pub extern "C" fn forge_tcp_read_bytes(conn_fd: i64, max_bytes: i64) -> i64 {
    use std::io::Read;
    use std::net::TcpStream;
    use std::os::unix::io::FromRawFd;
    if conn_fd <= 0 {
        return 0;
    }
    let mut stream = unsafe { TcpStream::from_raw_fd(conn_fd as i32) };
    let size = if max_bytes > 0 { max_bytes as usize } else { 4096 };
    let mut buf = vec![0u8; size];
    let result = match stream.read(&mut buf) {
        Ok(n) => {
            buf.truncate(n);
            forge_bytes_from_vec(buf)
        }
        Err(_) => 0,
    };
    use std::os::unix::io::IntoRawFd;
    let _ = stream.into_raw_fd();
    result
}

fn forge_tcp_wait(fd: i64, events: i16, timeout_ms: i64) -> i64 {
    if fd <= 0 {
        return -1;
    }
    let mut poll_fd = libc::pollfd {
        fd: fd as i32,
        events,
        revents: 0,
    };
    let timeout = if timeout_ms < 0 {
        -1
    } else if timeout_ms > i32::MAX as i64 {
        i32::MAX
    } else {
        timeout_ms as i32
    };
    loop {
        let status = unsafe { libc::poll(&mut poll_fd, 1, timeout) };
        if status > 0 {
            let revents = poll_fd.revents;
            if (revents & libc::POLLNVAL) != 0 || (revents & libc::POLLERR) != 0 {
                return -1;
            }
            if (revents & events) != 0 || (revents & libc::POLLHUP) != 0 {
                return 1;
            }
            return -1;
        }
        if status == 0 {
            return 0;
        }
        let kind = std::io::Error::last_os_error().kind();
        if kind == std::io::ErrorKind::Interrupted {
            continue;
        }
        return -1;
    }
}

#[no_mangle]
pub extern "C" fn forge_tcp_wait_readable(fd: i64, timeout_ms: i64) -> i64 {
    forge_tcp_wait(fd, libc::POLLIN, timeout_ms)
}

#[no_mangle]
pub extern "C" fn forge_tcp_wait_writable(fd: i64, timeout_ms: i64) -> i64 {
    forge_tcp_wait(fd, libc::POLLOUT, timeout_ms)
}

/// TCP write — write data to connection fd, return bytes written
#[no_mangle]
pub unsafe extern "C" fn forge_tcp_write(conn_fd: i64, data: *const i8) -> i64 {
    use std::io::Write;
    use std::net::TcpStream;
    use std::os::unix::io::FromRawFd;
    if conn_fd <= 0 { return 0; }
    let mut stream = TcpStream::from_raw_fd(conn_fd as i32);
    let s = std::ffi::CStr::from_ptr(data).to_str().unwrap_or("");
    let result = match stream.write(s.as_bytes()) {
        Ok(n) => n as i64,
        Err(_) => 0,
    };
    let _ = stream.flush();
    use std::os::unix::io::IntoRawFd;
    let _ = stream.into_raw_fd();
    result
}

#[no_mangle]
pub unsafe extern "C" fn forge_tcp_write_bytes(conn_fd: i64, data: i64) -> i64 {
    use std::io::Write;
    use std::net::TcpStream;
    use std::os::unix::io::FromRawFd;

    let Some(bytes) = forge_bytes_ref(data) else {
        return 0;
    };
    if conn_fd <= 0 {
        return 0;
    }
    let mut stream = TcpStream::from_raw_fd(conn_fd as i32);
    let result = match stream.write(&bytes.data) {
        Ok(n) => n as i64,
        Err(_) => 0,
    };
    use std::os::unix::io::IntoRawFd;
    let _ = stream.into_raw_fd();
    result
}

/// TCP set read timeout in milliseconds (0 = no timeout)
#[no_mangle]
pub extern "C" fn forge_tcp_set_timeout(fd: i64, ms: i64) {
    if fd < 0 { return; }
    use std::net::TcpStream;
    use std::os::unix::io::FromRawFd;
    let stream = unsafe { TcpStream::from_raw_fd(fd as i32) };
    if ms <= 0 {
        let _ = stream.set_read_timeout(None);
    } else {
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(ms as u64)));
    }
    use std::os::unix::io::IntoRawFd;
    let _ = stream.into_raw_fd();
}

/// TCP close — close the file descriptor
#[no_mangle]
pub extern "C" fn forge_tcp_close(fd: i64) {
    if fd <= 0 { return; }
    use std::net::TcpStream;
    use std::os::unix::io::FromRawFd;
    drop(unsafe { TcpStream::from_raw_fd(fd as i32) });
}

/// DNS resolve — resolve hostname to IP address string
#[no_mangle]
pub unsafe extern "C" fn forge_dns_resolve(hostname: *const i8) -> *mut i8 {
    use std::net::ToSocketAddrs;
    if hostname.is_null() {
        return std::ptr::null_mut();
    }
    let host = std::ffi::CStr::from_ptr(hostname).to_str().unwrap_or("");
    let addr_str = format!("{}:0", host);
    match addr_str.to_socket_addrs() {
        Ok(mut addrs) => {
            if let Some(addr) = addrs.next() {
                let ip = format!("{}\0", addr.ip());
                forge_strdup(ip.as_ptr() as *const i8)
            } else {
                std::ptr::null_mut()
            }
        }
        Err(_) => std::ptr::null_mut(),
    }
}

/// Format a float with fixed decimal places: forge_float_fixed(value, decimals)
///
/// # Safety
/// Returns a heap-allocated null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_float_fixed(value: f64, decimals: i64) -> *mut i8 {
    use std::alloc::{alloc, Layout};
    let s = format!("{:.prec$}", value, prec = decimals as usize);
    let bytes = s.as_bytes();
    let layout = Layout::array::<u8>(bytes.len() + 1).unwrap();
    let ptr = alloc(layout) as *mut i8;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
        *ptr.add(bytes.len()) = 0;
    }
    ptr
}

/// Check if a path is a directory
///
/// # Safety
/// path must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_is_dir(path: i64) -> i64 {
    let path_ptr = path as *const i8;
    if path_ptr.is_null() {
        return 0;
    }
    let len = crate::string::forge_cstring_len(path_ptr) as usize;
    let slice = std::slice::from_raw_parts(path_ptr as *const u8, len);
    if let Ok(path_str) = std::str::from_utf8(slice) {
        if std::path::Path::new(path_str).is_dir() {
            1
        } else {
            0
        }
    } else {
        0
    }
}

/// Find last index of needle in haystack, returns -1 if not found
///
/// # Safety
/// Both arguments must be valid null-terminated C strings
#[no_mangle]
pub unsafe extern "C" fn forge_cstring_last_index_of(
    haystack: *const i8,
    needle: *const i8,
) -> i64 {
    if haystack.is_null() || needle.is_null() {
        return -1;
    }
    let h_len = crate::string::forge_cstring_len(haystack) as usize;
    let n_len = crate::string::forge_cstring_len(needle) as usize;
    let h_slice = std::slice::from_raw_parts(haystack as *const u8, h_len);
    let n_slice = std::slice::from_raw_parts(needle as *const u8, n_len);
    if let (Ok(h), Ok(n)) = (std::str::from_utf8(h_slice), std::str::from_utf8(n_slice)) {
        match h.rfind(n) {
            Some(idx) => idx as i64,
            None => -1,
        }
    } else {
        -1
    }
}

/// Wait for a spawned process to finish, returns exit code
#[no_mangle]
pub extern "C" fn forge_process_wait(handle: i64) -> i64 {
    let mut handles = process_handles().lock();
    let Some(entry) = handles.get_mut(&handle) else {
        return -1;
    };
    match entry.child.wait() {
        Ok(status) => status.code().unwrap_or(-1) as i64,
        Err(_) => -1,
    }
}

/// Send a kill signal to a process
#[no_mangle]
pub extern "C" fn forge_process_kill(handle: i64) -> i64 {
    let mut handles = process_handles().lock();
    let Some(entry) = handles.get_mut(&handle) else {
        return 0;
    };
    match entry.child.kill() {
        Ok(_) => 1,
        Err(_) => 0,
    }
}

/// Close and forget a process handle
#[no_mangle]
pub extern "C" fn forge_process_close(handle: i64) {
    let mut handles = process_handles().lock();
    handles.remove(&handle);
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
