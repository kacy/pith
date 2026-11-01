//! the synchronization types behind the protocols `make loom-check` explores.
//!
//! a normal build gets std's types. under `--cfg loom` the same names are
//! loom's, so a protocol written against this module (the blocking pool's
//! slot) runs unchanged inside a loom model, which tries every interleaving
//! and every weak-memory outcome it allows. code that is not model-checked
//! keeps importing std directly.

use std::sync::atomic::Ordering;

#[cfg(not(all(test, loom)))]
pub(crate) use std::sync::{atomic::AtomicU8, Mutex};

#[cfg(all(test, loom))]
pub(crate) use loom::sync::{atomic::AtomicU8, Mutex};

/// a 32-bit atomic count, for a protocol that runs on words it does not own.
/// a struct's counts live in its raw header, so they are always std atomics
/// in the runtime; a model hands loom's atomic to the same function instead.
pub(crate) trait Count32 {
    fn load(&self, order: Ordering) -> u32;
    fn compare_exchange_weak(&self, current: u32, new: u32, success: Ordering, failure: Ordering)
        -> Result<u32, u32>;
}

macro_rules! impl_count32 {
    ($atomic:ty) => {
        impl Count32 for $atomic {
            #[inline(always)]
            fn load(&self, order: Ordering) -> u32 {
                <$atomic>::load(self, order)
            }
            #[inline(always)]
            fn compare_exchange_weak(
                &self,
                current: u32,
                new: u32,
                success: Ordering,
                failure: Ordering,
            ) -> Result<u32, u32> {
                <$atomic>::compare_exchange_weak(self, current, new, success, failure)
            }
        }
    };
}

impl_count32!(std::sync::atomic::AtomicU32);
#[cfg(all(test, loom))]
impl_count32!(loom::sync::atomic::AtomicU32);
