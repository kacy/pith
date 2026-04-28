//! Pith Runtime - Core runtime library for the Pith language
//!
//! This crate provides the runtime support for Pith programs:
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
pub mod crypto;
pub mod encoding;
pub mod ffi_util;
pub mod host_fs;
pub mod json;
pub mod network;
pub mod perf;
pub mod platform;
pub mod process;
pub mod process_io;
pub mod runtime_core;
pub mod string;
pub mod string_list;
pub mod utility;

pub use encoding::*;
pub use host_fs::*;
pub use network::*;
pub use perf::*;
pub use platform::*;
pub use process::*;
pub use process_io::*;
pub use runtime_core::*;
pub use string_list::*;
pub use utility::*;

pub(crate) use runtime_core::{
    pith_copy_bytes_to_cstring, pith_cstring_empty, pith_strdup_string,
};

pub use concurrency::{
    pith_mutex_lock, pith_mutex_new, pith_mutex_unlock, pith_semaphore_acquire,
    pith_semaphore_new, pith_semaphore_release, pith_waitgroup_add, pith_waitgroup_done,
    pith_waitgroup_new, pith_waitgroup_wait,
};
