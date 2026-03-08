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
