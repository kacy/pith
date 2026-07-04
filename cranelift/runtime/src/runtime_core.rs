use crate::collections::list::{pith_list_new, pith_list_push_value};
use crate::handle_registry::{self, HandleKind};
use crate::string;
use std::alloc::{alloc, Layout};

pub(crate) fn pith_strdup_string(text: &str) -> *mut i8 {
    let owned = format!("{}\0", text);
    unsafe { pith_strdup(owned.as_ptr() as *const i8) }
}

#[no_mangle]
pub unsafe extern "C" fn pith_runtime_error(code: i64) -> i64 {
    let message = match code {
        1 => "division by zero",
        2 => "integer division overflow",
        3 => "allocation failed",
        4 => "invalid allocation layout",
        _ => "runtime error",
    };
    eprintln!("pith runtime error: {message}");
    std::process::exit(1);
}

pub(crate) fn pith_layout(size: usize, align: usize) -> Layout {
    match Layout::from_size_align(size, align) {
        Ok(layout) => layout,
        Err(_) => {
            eprintln!("pith runtime error: invalid allocation layout");
            std::process::exit(1);
        }
    }
}

pub(crate) unsafe fn pith_alloc(layout: Layout) -> *mut u8 {
    let ptr = alloc(layout);
    if ptr.is_null() {
        eprintln!("pith runtime error: allocation failed");
        std::process::exit(1);
    }
    ptr
}

// --- refcounted c strings ---
//
// generated code passes strings around as bare `*const i8`, so ownership
// has to live behind the pointer. every heap cstring carries a 16-byte
// header in front of its data:
//
//   [magic: u32][refcount: u32, atomic][data_len: u64][bytes...][NUL]
//
// static literals (strref data baked into the binary) have no header. the
// magic + alignment check below turns retain/release into no-ops for them,
// so the compiler can emit retain/release for every string register without
// caring where the string came from.

pub(crate) const CSTRING_MAGIC: u32 = 0x50435352; // "PCSR"
const CSTRING_HEADER_SIZE: usize = 16;
const CSTRING_ALIGN: usize = 8;

/// One shared empty string. It has no header, so retain/release skip it,
/// which is exactly right for a value that is never freed.
static CSTRING_EMPTY: &[u8] = b"\0";

fn cstring_layout(data_len: usize) -> Layout {
    pith_layout(CSTRING_HEADER_SIZE + data_len + 1, CSTRING_ALIGN)
}

/// Allocate a heap cstring with `data_len` data bytes, refcount 1, and the
/// trailing NUL already written. The caller fills bytes 0..data_len.
pub(crate) unsafe fn pith_alloc_cstring(data_len: usize) -> *mut i8 {
    let base = pith_alloc(cstring_layout(data_len));
    (base as *mut u32).write(CSTRING_MAGIC);
    (base.add(4) as *mut u32).write(1);
    (base.add(8) as *mut u64).write(data_len as u64);
    let data = base.add(CSTRING_HEADER_SIZE) as *mut i8;
    *data.add(data_len) = 0;
    data
}

/// Find the header of a heap cstring, or None for null pointers, static
/// literals, and anything else that never came from pith_alloc_cstring.
/// Heap cstring data is always 8-aligned, which rejects most literals
/// before the header word is ever read; an aligned literal gets one read
/// of the 16 bytes in front of it (mapped binary data) and fails the magic
/// compare. Same practical-guard contract as the collection magic words.
unsafe fn cstring_base(s: *const i8) -> Option<*mut u8> {
    let addr = s as usize;
    if addr < CSTRING_HEADER_SIZE || addr % CSTRING_ALIGN != 0 {
        return None;
    }
    let base = (s as *const u8).sub(CSTRING_HEADER_SIZE) as *mut u8;
    if (base as *const u32).read() != CSTRING_MAGIC {
        return None;
    }
    Some(base)
}

unsafe fn cstring_refcount(base: *mut u8) -> &'static std::sync::atomic::AtomicU32 {
    &*(base.add(4) as *const std::sync::atomic::AtomicU32)
}

/// Bump the refcount of a heap cstring. No-op for literals and null.
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_retain(s: *const i8) {
    if let Some(base) = cstring_base(s) {
        cstring_refcount(base).fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Drop one count; free the allocation when the last count dies. The magic
/// is scrubbed before the free so a stale pointer fails the header check
/// instead of double-freeing. No-op for literals and null.
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_release(s: *const i8) {
    if let Some(base) = cstring_base(s) {
        let last = cstring_refcount(base).fetch_sub(1, std::sync::atomic::Ordering::Release) == 1;
        if last {
            std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
            let data_len = (base.add(8) as *const u64).read() as usize;
            (base as *mut u32).write(0);
            std::alloc::dealloc(base, cstring_layout(data_len));
        }
    }
}

/// Test-only view of a heap cstring's refcount (None for literals/null).
#[cfg(test)]
pub(crate) unsafe fn cstring_refcount_for_tests(s: *const i8) -> Option<u32> {
    cstring_base(s).map(|base| cstring_refcount(base).load(std::sync::atomic::Ordering::Relaxed))
}

pub(crate) unsafe fn pith_copy_bytes_to_cstring(bytes: &[u8]) -> *mut i8 {
    let ptr = pith_alloc_cstring(bytes.len());
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
    ptr
}

pub(crate) unsafe fn pith_cstring_empty() -> *mut i8 {
    CSTRING_EMPTY.as_ptr() as *mut i8
}

const PITH_CLOSURE_ENV_SLOTS: usize = 16;

struct PithClosure {
    func_ptr: i64,
    env: [i64; PITH_CLOSURE_ENV_SLOTS],
}

unsafe fn pith_closure_mut<'a>(handle: i64) -> Option<&'a mut PithClosure> {
    if !handle_registry::is_valid(handle as *const (), HandleKind::Closure) {
        return None;
    }
    Some(&mut *(handle as *mut PithClosure))
}

unsafe fn pith_closure_ref<'a>(handle: i64) -> Option<&'a PithClosure> {
    if !handle_registry::is_valid(handle as *const (), HandleKind::Closure) {
        return None;
    }
    Some(&*(handle as *const PithClosure))
}

#[no_mangle]
pub extern "C" fn pith_closure_new(func_ptr: i64) -> i64 {
    let ptr = Box::into_raw(Box::new(PithClosure {
        func_ptr,
        env: [0; PITH_CLOSURE_ENV_SLOTS],
    }));
    handle_registry::register(ptr as *const (), HandleKind::Closure);
    ptr as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_closure_get_fn(handle: i64) -> i64 {
    if let Some(closure) = pith_closure_ref(handle) {
        closure.func_ptr
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_closure_set_env(handle: i64, slot: i64, value: i64) {
    if slot < 0 || (slot as usize) >= PITH_CLOSURE_ENV_SLOTS {
        return;
    }
    if let Some(closure) = pith_closure_mut(handle) {
        closure.env[slot as usize] = value;
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_closure_get_env(handle: i64, slot: i64) -> i64 {
    if slot < 0 || (slot as usize) >= PITH_CLOSURE_ENV_SLOTS {
        return 0;
    }
    if let Some(closure) = pith_closure_ref(handle) {
        closure.env[slot as usize]
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_print(s: string::PithString) {
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

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn heap_refcount(s: *const i8) -> u32 {
        let base = (s as *const u8).sub(CSTRING_HEADER_SIZE);
        (base.add(4) as *const u32).read()
    }

    #[test]
    fn heap_cstrings_carry_a_refcount() {
        unsafe {
            let s = pith_copy_bytes_to_cstring(b"hello");
            assert_eq!(heap_refcount(s), 1);
            pith_cstring_retain(s);
            assert_eq!(heap_refcount(s), 2);
            pith_cstring_release(s);
            assert_eq!(heap_refcount(s), 1);
            pith_cstring_release(s); // frees; magic is scrubbed first
        }
    }

    #[test]
    fn released_cstring_fails_the_header_check() {
        unsafe {
            let s = pith_copy_bytes_to_cstring(b"x");
            pith_cstring_release(s);
            // stale pointer: magic scrubbed, so this must be a no-op,
            // not a double free
            pith_cstring_release(s);
            pith_cstring_retain(s);
        }
    }

    #[test]
    fn literals_and_null_are_no_ops() {
        unsafe {
            let lit = b"static literal\0".as_ptr() as *const i8;
            pith_cstring_retain(lit);
            pith_cstring_release(lit);
            pith_cstring_release(lit);
            pith_cstring_retain(std::ptr::null());
            pith_cstring_release(std::ptr::null());
            // the shared empty string is header-less by design
            let empty = pith_cstring_empty();
            pith_cstring_release(empty);
            assert_eq!(*empty, 0);
        }
    }

    #[test]
    fn concat_and_strdup_produce_owned_cstrings() {
        unsafe {
            let a = pith_copy_bytes_to_cstring(b"ab");
            let b = pith_copy_bytes_to_cstring(b"cd");
            let joined = pith_concat_cstr(a, b);
            assert_eq!(heap_refcount(joined), 1);
            assert_eq!(crate::string::pith_cstring_len(joined), 4);
            let dup = pith_strdup(joined);
            assert_eq!(heap_refcount(dup), 1);
            pith_cstring_release(joined);
            pith_cstring_release(dup);
            pith_cstring_release(a);
            pith_cstring_release(b);
        }
    }

    #[test]
    fn invalid_closure_handles_return_safe_defaults() {
        unsafe {
            assert_eq!(pith_closure_get_fn(12345), 0);
            assert_eq!(pith_closure_get_env(12345, 0), 0);
            pith_closure_set_env(12345, 0, 99);
        }
    }

    #[test]
    fn closure_env_access_requires_valid_slot() {
        let handle = pith_closure_new(77);
        unsafe {
            pith_closure_set_env(handle, 0, 42);
            pith_closure_set_env(handle, -1, 99);
            pith_closure_set_env(handle, PITH_CLOSURE_ENV_SLOTS as i64, 99);
            assert_eq!(pith_closure_get_fn(handle), 77);
            assert_eq!(pith_closure_get_env(handle, 0), 42);
            assert_eq!(pith_closure_get_env(handle, -1), 0);
            assert_eq!(
                pith_closure_get_env(handle, PITH_CLOSURE_ENV_SLOTS as i64),
                0
            );
        }
    }
}

#[no_mangle]
pub extern "C" fn pith_print_int(n: i64) {
    println!("{}", n);
}

#[no_mangle]
pub unsafe extern "C" fn pith_concat_cstr(a: *const i8, b: *const i8) -> *mut i8 {
    if a.is_null() {
        return if b.is_null() {
            std::ptr::null_mut()
        } else {
            pith_strdup(b)
        };
    }
    if b.is_null() {
        return pith_strdup(a);
    }

    let len_a = string::pith_cstring_len(a) as usize;
    let len_b = string::pith_cstring_len(b) as usize;
    let result = pith_alloc_cstring(len_a + len_b);

    std::ptr::copy_nonoverlapping(a, result, len_a);
    std::ptr::copy_nonoverlapping(b, result.add(len_a), len_b);
    result
}

#[no_mangle]
pub unsafe extern "C" fn pith_strdup(ptr: *const i8) -> *mut i8 {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }

    let len = string::pith_cstring_len(ptr) as usize;
    let result = pith_alloc_cstring(len);
    std::ptr::copy_nonoverlapping(ptr, result, len);

    result
}

#[no_mangle]
pub unsafe extern "C" fn pith_print_cstr(ptr: *const i8) {
    if ptr.is_null() {
        println!();
        return;
    }

    let len = string::pith_cstring_len(ptr) as usize;
    let slice = std::slice::from_raw_parts(ptr as *const u8, len);
    if let Ok(str_ref) = std::str::from_utf8(slice) {
        println!("{}", str_ref);
    } else {
        println!();
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_print_err(ptr: *const i8) {
    if ptr.is_null() {
        eprintln!();
        return;
    }

    let len = string::pith_cstring_len(ptr) as usize;
    let slice = std::slice::from_raw_parts(ptr as *const u8, len);
    if let Ok(str_ref) = std::str::from_utf8(slice) {
        eprintln!("{}", str_ref);
    } else {
        eprintln!();
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_eq(a: *const i8, b: *const i8) -> i64 {
    if a.is_null() && b.is_null() {
        return 1;
    }
    if a.is_null() || b.is_null() {
        return 0;
    }
    if std::ptr::eq(a, b) {
        return 1;
    }

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

#[no_mangle]
pub unsafe extern "C" fn pith_ord_cstr(s: *const i8) -> i64 {
    if s.is_null() || *s == 0 {
        return 0;
    }
    *s as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_chr_cstr(n: i64) -> *mut i8 {
    let ptr = pith_alloc_cstring(1);
    *ptr = (n as u8) as i8;
    ptr
}

static TEST_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[no_mangle]
pub extern "C" fn pith_assert(cond: i64) {
    if cond == 0 {
        TEST_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
        eprintln!("Assertion failed");
    }
}

#[no_mangle]
pub extern "C" fn pith_assert_eq(a: i64, b: i64) {
    if a != b {
        TEST_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
        eprintln!("Assertion failed: {} != {}", a, b);
    }
}

#[no_mangle]
pub extern "C" fn pith_assert_ne(a: i64, b: i64) {
    if a == b {
        TEST_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
        eprintln!("Assertion failed: {} == {}", a, b);
    }
}

#[no_mangle]
pub extern "C" fn pith_bit_and(a: i64, b: i64) -> i64 {
    a & b
}

#[no_mangle]
pub extern "C" fn pith_bit_or(a: i64, b: i64) -> i64 {
    a | b
}

#[no_mangle]
pub extern "C" fn pith_bit_xor(a: i64, b: i64) -> i64 {
    a ^ b
}

#[no_mangle]
pub extern "C" fn pith_bit_not(a: i64) -> i64 {
    !a
}

#[no_mangle]
pub extern "C" fn pith_bit_shl(a: i64, b: i64) -> i64 {
    a << b
}

#[no_mangle]
pub extern "C" fn pith_bit_shr(a: i64, b: i64) -> i64 {
    ((a as u64) >> b) as i64
}

#[no_mangle]
pub extern "C" fn pith_uint(n: i64) -> i64 {
    n
}

#[no_mangle]
pub extern "C" fn pith_int8(n: i64) -> i64 {
    (n as i8) as i64
}

#[no_mangle]
pub extern "C" fn pith_int16(n: i64) -> i64 {
    (n as i16) as i64
}

#[no_mangle]
pub extern "C" fn pith_int32(n: i64) -> i64 {
    (n as i32) as i64
}

#[no_mangle]
pub extern "C" fn pith_int64(n: i64) -> i64 {
    n
}

#[no_mangle]
pub extern "C" fn pith_uint8(n: i64) -> i64 {
    (n as u8) as i64
}

#[no_mangle]
pub extern "C" fn pith_uint16(n: i64) -> i64 {
    (n as u16) as i64
}

#[no_mangle]
pub extern "C" fn pith_uint32(n: i64) -> i64 {
    (n as u32) as i64
}

#[no_mangle]
pub extern "C" fn pith_uint64(n: i64) -> i64 {
    n
}

#[no_mangle]
pub extern "C" fn pith_abs(n: i64) -> i64 {
    n.abs()
}

#[no_mangle]
pub extern "C" fn pith_min(a: i64, b: i64) -> i64 {
    if a < b {
        a
    } else {
        b
    }
}

#[no_mangle]
pub extern "C" fn pith_max(a: i64, b: i64) -> i64 {
    if a > b {
        a
    } else {
        b
    }
}

#[no_mangle]
pub extern "C" fn pith_clamp(n: i64, min: i64, max: i64) -> i64 {
    if n < min {
        min
    } else if n > max {
        max
    } else {
        n
    }
}

#[no_mangle]
pub extern "C" fn pith_pow(a: f64, b: f64) -> f64 {
    a.powf(b)
}

#[no_mangle]
pub extern "C" fn pith_sqrt(n: f64) -> f64 {
    n.sqrt()
}

#[no_mangle]
pub extern "C" fn pith_floor(n: f64) -> f64 {
    n.floor()
}

#[no_mangle]
pub extern "C" fn pith_ceil(n: f64) -> f64 {
    n.ceil()
}

#[no_mangle]
pub extern "C" fn pith_round(n: f64) -> f64 {
    n.round()
}

#[no_mangle]
pub extern "C" fn pith_sin(n: f64) -> f64 {
    n.sin()
}

#[no_mangle]
pub extern "C" fn pith_cos(n: f64) -> f64 {
    n.cos()
}

#[no_mangle]
pub extern "C" fn pith_tan(n: f64) -> f64 {
    n.tan()
}

#[no_mangle]
pub extern "C" fn pith_asin(n: f64) -> f64 {
    n.asin()
}

#[no_mangle]
pub extern "C" fn pith_acos(n: f64) -> f64 {
    n.acos()
}

#[no_mangle]
pub extern "C" fn pith_atan(n: f64) -> f64 {
    n.atan()
}

#[no_mangle]
pub extern "C" fn pith_atan2(y: f64, x: f64) -> f64 {
    y.atan2(x)
}

#[no_mangle]
pub extern "C" fn pith_log(n: f64) -> f64 {
    n.ln()
}

#[no_mangle]
pub extern "C" fn pith_log10(n: f64) -> f64 {
    n.log10()
}

#[no_mangle]
pub extern "C" fn pith_log2(n: f64) -> f64 {
    n.log2()
}

#[no_mangle]
pub extern "C" fn pith_exp(n: f64) -> f64 {
    n.exp()
}

#[no_mangle]
pub extern "C" fn pith_abs_float(n: f64) -> f64 {
    n.abs()
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_compare(a: *const i8, b: *const i8) -> i64 {
    if a.is_null() && b.is_null() {
        return 0;
    }
    if a.is_null() {
        return -1;
    }
    if b.is_null() {
        return 1;
    }
    let mut pa = a;
    let mut pb = b;
    loop {
        let ca = *pa as u8;
        let cb = *pb as u8;
        if ca != cb {
            return if ca < cb { -1 } else { 1 };
        }
        if ca == 0 {
            return 0;
        }
        pa = pa.add(1);
        pb = pb.add(1);
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_lt(a: *const i8, b: *const i8) -> i64 {
    if pith_cstring_compare(a, b) < 0 {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_gt(a: *const i8, b: *const i8) -> i64 {
    if pith_cstring_compare(a, b) > 0 {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_lte(a: *const i8, b: *const i8) -> i64 {
    if pith_cstring_compare(a, b) <= 0 {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_gte(a: *const i8, b: *const i8) -> i64 {
    if pith_cstring_compare(a, b) >= 0 {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_int_to_cstr(n: i64) -> *mut i8 {
    let s = n.to_string();
    let len = s.len();
    pith_copy_bytes_to_cstring(&s.as_bytes()[..len])
}

#[no_mangle]
pub unsafe extern "C" fn pith_uint_to_cstr(n: i64) -> *mut i8 {
    let s = (n as u64).to_string();
    let len = s.len();
    pith_copy_bytes_to_cstring(&s.as_bytes()[..len])
}

#[no_mangle]
pub extern "C" fn pith_float_to_cstr(n: f64) -> *mut i8 {
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
    unsafe { pith_copy_bytes_to_cstring(&s.as_bytes()[..len]) }
}

#[no_mangle]
pub extern "C" fn pith_bool_to_cstr(b: i64) -> *mut i8 {
    let s = if b != 0 { "true" } else { "false" };
    let len = s.len();
    unsafe { pith_copy_bytes_to_cstring(&s.as_bytes()[..len]) }
}

pub use pith_ceil as pith_math_ceil;
pub use pith_floor as pith_math_floor;
pub use pith_pow as pith_math_pow;
pub use pith_round as pith_math_round;
pub use pith_sqrt as pith_math_sqrt;

#[no_mangle]
pub unsafe extern "C" fn pith_free(ptr: *mut i8) {
    use std::alloc::Layout;

    if !ptr.is_null() {
        std::alloc::dealloc(ptr as *mut u8, Layout::new::<u8>());
    }
}

#[no_mangle]
pub extern "C" fn pith_int_to_float(n: i64) -> f64 {
    n as f64
}

#[no_mangle]
pub extern "C" fn pith_float_to_int(n: f64) -> i64 {
    n as i64
}

#[no_mangle]
pub extern "C" fn pith_second(_a: i64, b: i64) -> i64 {
    b
}

#[no_mangle]
pub unsafe extern "C" fn pith_struct_alloc(num_fields: i64) -> i64 {
    use std::alloc::alloc_zeroed;

    let size = (num_fields.max(0) as usize) * 8;
    if size == 0 {
        return 0;
    }

    let layout = pith_layout(size, 8);
    let ptr = alloc_zeroed(layout);
    if ptr.is_null() {
        eprintln!("pith runtime error: allocation failed");
        std::process::exit(1);
    }
    ptr as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_args_to_list() -> i64 {
    let list = pith_list_new(8, 0);

    for arg in std::env::args() {
        let arg_len = arg.len();
        let arg_ptr = pith_copy_bytes_to_cstring(&arg.as_bytes()[..arg_len]);
        pith_list_push_value(list, arg_ptr as i64);
    }

    list.ptr as i64
}
