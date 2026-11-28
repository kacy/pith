//! Core Pith runtime primitives: allocator helpers, refcounted C strings,
//! closures, structs & weak references, per-thread object pools, TLS globals,
//! test harness, and scalar/display builtins.

use std::alloc::{alloc, Layout};
use std::sync::atomic::AtomicU32;

pub(crate) mod closure;
pub(crate) mod cstring;
pub(crate) mod pool;
pub(crate) mod primitives;
pub(crate) mod struct_rc;
pub(crate) mod test_harness;
pub(crate) mod tls;
#[cfg(test)]
mod tests;

pub use closure::*;
pub use cstring::*;
pub use primitives::*;
pub use struct_rc::*;
pub use test_harness::*;
pub use tls::*;

#[no_mangle]
pub unsafe extern "C" fn pith_runtime_error(code: i64) -> i64 {
    let message = match code {
        1 => "division by zero",
        2 => "integer division overflow",
        3 => "allocation failed",
        4 => "invalid allocation layout",
        5 => "call to a function the compiler could not resolve — a compiler bug, not a problem in your program",
        6 => "value() on none",
        7 => "integer overflow",
        _ => "runtime error",
    };
    // a failed allocation and a compiler bug stop the program wherever they
    // happen. the rest are program errors, which an isolated call (an http
    // handler, a grpc method) survives: see `failure`.
    if matches!(code, 3..=5) {
        eprintln!("pith runtime error: {message}");
        // panic-guard: the compiler's runtime-error trap for conditions no caller can recover from.
        std::process::exit(1);
    }
    crate::failure::fail_static(message)
}

pub(crate) fn pith_layout(size: usize, align: usize) -> Layout {
    match Layout::from_size_align(size, align) {
        Ok(layout) => layout,
        Err(_) => {
            eprintln!("pith runtime error: invalid allocation layout");
            // panic-guard: an invalid allocation layout is a compiler bug with no way to continue.
            std::process::exit(1);
        }
    }
}

pub(crate) unsafe fn pith_alloc(layout: Layout) -> *mut u8 {
    // SAFETY: Caller guarantees `layout` has non-zero size.
    let ptr = unsafe { alloc(layout) };
    if ptr.is_null() {
        eprintln!("pith runtime error: allocation failed");
        // panic-guard: out of memory.
        std::process::exit(1);
    }
    ptr
}

/// Drop one count and return the count before, like `fetch_sub(1, Release)`.
/// A count of one belongs to the caller alone: no other owner is left to
/// retain or release concurrently, and the acquire load orders every earlier
/// owner's release before it. So the last release, the common case for a
/// temporary, is a load and a plain store instead of a locked
/// read-modify-write.
#[inline(always)]
pub(crate) fn count_release(count: &AtomicU32) -> u32 {
    use std::sync::atomic::Ordering;
    if count.load(Ordering::Acquire) == 1 {
        count.store(0, Ordering::Relaxed);
        return 1;
    }
    count.fetch_sub(1, Ordering::Release)
}

#[no_mangle]
pub unsafe extern "C" fn pith_free(ptr: *mut i8) {
    use std::alloc::Layout;

    if !ptr.is_null() {
        // SAFETY: Caller guarantees `ptr` was allocated with `Layout::new::<u8>()`.
        unsafe { std::alloc::dealloc(ptr as *mut u8, Layout::new::<u8>()) };
    }
}
#[no_mangle]
pub extern "C" fn pith_second(_a: i64, b: i64) -> i64 {
    b
}
/// Build a pith `T?` optional value in the layout the codegen expects: none
/// is the null pointer, and Some is a two-slot struct with 1 in field 0 and
/// the payload in field 1. Used by runtime accessors that want to hand back
/// "not present" without collapsing it to a zero/empty sentinel.
pub(crate) fn optional_tuple(is_some: bool, value: i64) -> i64 {
    if !is_some {
        return 0;
    }
    // SAFETY: `pith_struct_alloc(2)` allocates a fresh 2-field struct.
    let tuple = unsafe { pith_struct_alloc(2) };
    if tuple == 0 {
        return 0;
    }
    // SAFETY: `tuple` points to 2 newly allocated writable `i64` field slots.
    unsafe {
        let ptr = tuple as *mut i64;
        *ptr = 1;
        *ptr.add(1) = value;
    }
    tuple
}

/// An optional getter's answer as a `T?`: see `optional_tuple`.
pub(crate) fn optional_from(found: Option<i64>) -> i64 {
    match found {
        Some(value) => optional_tuple(true, value),
        None => 0,
    }
}

/// An optional getter's answer as two registers, for a compiled caller that
/// unpacks it on the spot and never needs the Some shell: (1, value) for
/// Some and (0, 0) for none.
pub(crate) fn optional_pair(found: Option<i64>) -> crate::ffi_util::ResultPair {
    match found {
        Some(value) => crate::ffi_util::ResultPair {
            is_ok: 1,
            payload: value,
        },
        None => crate::ffi_util::ResultPair {
            is_ok: 0,
            payload: 0,
        },
    }
}
