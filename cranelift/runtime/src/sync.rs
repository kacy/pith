//! the synchronization types behind the protocols `make loom-check` explores.
//!
//! a normal build gets std's types. under `--cfg loom` the same names are
//! loom's, so a protocol written against this module (the blocking pool's
//! slot) runs unchanged inside a loom model, which tries every interleaving
//! and every weak-memory outcome it allows. code that is not model-checked
//! keeps importing std directly.

#[cfg(not(all(test, loom)))]
pub(crate) use std::sync::{atomic::AtomicU8, Mutex};

#[cfg(all(test, loom))]
pub(crate) use loom::sync::{atomic::AtomicU8, Mutex};
