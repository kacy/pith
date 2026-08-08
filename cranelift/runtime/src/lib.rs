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

/// Stop the process with a diagnostic, the way every other runtime trap does.
///
/// This is deliberately not a `panic!`. The runtime is linked into user
/// programs, and a panic on a runtime thread does not reliably stop one:
///
/// - a panic raised anywhere the green spawn path reaches runs inside whatever
///   task called it, and `run_task` wraps every task resume in `catch_unwind`.
///   The task dies without ever setting `join.done`, so every awaiter blocks
///   forever — a silent wedge rather than a crash.
/// - a panic inside a `Once::call_once` (worker startup, reactor startup)
///   poisons the `Once`, so every later caller panics too.
/// - neither cargo profile sets `panic = "abort"`, so nothing turns either of
///   those into a process exit.
///
/// Exiting with a message turns all of that into one diagnosable death. Use it
/// for conditions with no recovery: the OS refusing a thread or a stack, a
/// capacity ceiling the runtime cannot grow past.
macro_rules! runtime_fatal {
    ($($arg:tt)*) => {{
        eprintln!("pith runtime error: {}", format_args!($($arg)*));
        // panic-guard: the runtime's fatal-trap idiom; see the doc comment above.
        std::process::exit(1);
    }};
}

pub mod argon2;
pub mod blake2b;
pub mod blocking;
pub mod bytes;
pub mod collections;
pub mod concurrency;
pub mod crypto;
pub mod dns;
pub mod encoding;
pub mod fdio;
pub mod ffi_util;
pub mod handle_registry;
pub mod zstd_codec;
pub mod host_fs;
pub mod json;
// the netpoller is epoll-based, so it only builds on linux. everywhere else a
// fallback with the same two entry points keeps `network` compiling — at the
// cost of green tasks parking their worker on socket i/o. see
// `netpoll_fallback` for what that means in practice.
#[cfg(target_os = "linux")]
pub mod netpoll;
#[cfg(not(target_os = "linux"))]
#[path = "netpoll_fallback.rs"]
pub mod netpoll;
pub mod network;
pub mod perf;
pub mod platform;
pub mod process;
pub mod process_io;
pub mod signals;
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
    pith_alloc, pith_copy_bytes_to_cstring, pith_cstring_empty, pith_layout, pith_strdup_string,
};

pub use concurrency::{
    pith_atomic_int_compare_set, pith_atomic_int_get, pith_atomic_int_new, pith_atomic_int_set,
    pith_mutex_lock, pith_mutex_new, pith_mutex_unlock, pith_semaphore_acquire, pith_semaphore_new,
    pith_semaphore_release, pith_waitgroup_add, pith_waitgroup_done, pith_waitgroup_new,
    pith_waitgroup_wait,
};
