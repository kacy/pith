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
    forge_copy_bytes_to_cstring, forge_cstring_empty, forge_strdup_string,
};

pub use concurrency::{
    forge_mutex_lock, forge_mutex_new, forge_mutex_unlock, forge_semaphore_acquire,
    forge_semaphore_new, forge_semaphore_release, forge_waitgroup_add, forge_waitgroup_done,
    forge_waitgroup_new, forge_waitgroup_wait,
};
