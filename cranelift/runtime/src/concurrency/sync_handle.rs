//! The owner count shared by the four handle-backed sync primitives: Mutex,
//! WaitGroup, Semaphore and AtomicInt.
//!
//! Each primitive is one boxed allocation that starts with a `SyncHeader`,
//! and the language counts its handle the way it counts a channel's: retained
//! on every copy, released at every scope exit, freed at the last release.
//! The emitter gives all four one rc kind, so retain and release are one
//! entry point each (`pith_sync_retain` / `pith_sync_release`), and the
//! header's magic word picks the free. The handle a program holds is the
//! allocation's address, unchanged, so the per-type operations still take it
//! as they always have.
//!
//! As with a channel, a frame that operates on a handle holds a count for it
//! (a binding, a field of a live struct, an element of a live container, a
//! capture of a live closure), so the fast paths take no count of their own.
//! An operation that blocks is the exception: its caller may have borrowed the
//! handle from a holder another task owns, and that task may release the
//! holder while the call sleeps. A blocking operation therefore takes a count
//! before it waits and gives it back when it returns (`BlockCount`), off the
//! uncontended path.

use crate::handle_registry;
use crate::quarantine::{self, Op};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// "PMTX"
pub(crate) const MUTEX_MAGIC: u32 = 0x504d_5458;
/// "PWGR"
pub(crate) const WAITGROUP_MAGIC: u32 = 0x5057_4752;
/// "PSEM"
pub(crate) const SEMAPHORE_MAGIC: u32 = 0x5053_454d;
/// "PATM"
pub(crate) const ATOMIC_INT_MAGIC: u32 = 0x5041_544d;

/// the front of every sync primitive's allocation. `#[repr(C)]` on the
/// header and on each primitive pins the magic word to offset 0.
#[repr(C)]
pub(crate) struct SyncHeader {
    pub(crate) magic: u32,
    /// owners: one per binding, field, element or capture holding the
    /// handle, plus one per blocked operation for the span of its call.
    refs: AtomicUsize,
}

impl SyncHeader {
    /// the header of a new primitive, holding its creator's count.
    pub(crate) fn new(magic: u32) -> SyncHeader {
        SyncHeader {
            magic,
            refs: AtomicUsize::new(1),
        }
    }

    #[cfg(test)]
    pub(crate) fn refs_for_tests(&self) -> usize {
        self.refs.load(Ordering::Relaxed)
    }
}

fn is_sync_magic(word: u32) -> bool {
    matches!(
        word,
        MUTEX_MAGIC | WAITGROUP_MAGIC | SEMAPHORE_MAGIC | ATOMIC_INT_MAGIC
    )
}

/// the header behind a handle of the kind `magic` names, or None for anything
/// else. a freed primitive had its magic scrubbed, so a stale handle is
/// rejected here like a garbage one, and reported under quarantine.
///
/// # Safety
/// `ptr` must be readable when it is aligned, or be garbage the caller is
/// prepared to have rejected.
#[inline(always)]
pub(crate) unsafe fn header_of<'a>(ptr: *const (), magic: u32, op: Op) -> Option<&'a SyncHeader> {
    if !handle_registry::plausibly_aligned::<SyncHeader>(ptr) {
        return None;
    }
    let header = ptr as *const SyncHeader;
    if !quarantine::magic_is(std::ptr::addr_of!((*header).magic), magic, op) {
        return None;
    }
    Some(&*header)
}

/// `header_of` for a handle of any of the four kinds.
#[inline(always)]
unsafe fn any_header<'a>(handle: i64, op: Op) -> Option<&'a SyncHeader> {
    let ptr = handle as *const SyncHeader;
    if !handle_registry::plausibly_aligned::<SyncHeader>(ptr as *const ()) {
        return None;
    }
    let at = std::ptr::addr_of!((*ptr).magic);
    let word = at.read();
    if !is_sync_magic(word) {
        quarantine::miss(word, at, op);
        return None;
    }
    Some(&*ptr)
}

/// One more owner of this handle: a binding, a field, an element or a capture
/// took a copy. Garbage handles are ignored, like every other kind.
///
/// # Safety
/// `handle` must be a live sync primitive or garbage (magic-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_sync_retain(handle: i64) {
    if let Some(h) = any_header(handle, Op::Retain) {
        h.refs.fetch_add(1, Ordering::Relaxed);
    }
}

/// One owner fewer; the last owner frees the primitive.
///
/// # Safety
/// `handle` must be a live sync primitive or garbage (magic-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_sync_release(handle: i64) {
    if let Some(h) = any_header(handle, Op::Release) {
        release_count(h);
    }
}

/// one owner fewer; the last one frees. shared by the language's release and
/// by a blocked operation's completion.
unsafe fn release_count(h: &SyncHeader) {
    let prev = h.refs.fetch_sub(1, Ordering::Release);
    if prev > 1 {
        return;
    }
    if prev == 0 {
        quarantine::over_release(quarantine::Kind::Sync);
        // an over-release: put the count back rather than wrap, the same
        // guard every other kind's release has.
        h.refs.fetch_add(1, Ordering::Relaxed);
        return;
    }
    acquire_last_count!(h.refs);
    let ptr = h as *const SyncHeader as *mut ();
    match h.magic {
        MUTEX_MAGIC => super::mutex::free_mutex(ptr as *mut super::mutex::PithMutex),
        WAITGROUP_MAGIC => super::waitgroup::free_waitgroup(ptr as *mut super::waitgroup::PithWaitGroup),
        SEMAPHORE_MAGIC => super::semaphore::free_semaphore(ptr as *mut super::semaphore::PithSemaphore),
        ATOMIC_INT_MAGIC => super::atomic_int::free_cell(ptr as *mut super::atomic_int::Cell),
        _ => {}
    }
}

/// free a boxed primitive whose last count is gone: scrub the magic so a
/// stale handle fails validation, then drop the box, or keep it poisoned
/// under quarantine.
///
/// # Safety
/// `p` must come from `Box::into_raw`, start with a `SyncHeader`, and have no
/// owner left.
pub(crate) unsafe fn free_box<T>(p: *mut T) {
    let magic = std::ptr::addr_of_mut!((*(p as *mut SyncHeader)).magic);
    *magic = 0;
    if !quarantine::bury_box(p, magic, quarantine::Kind::Sync) {
        drop(Box::from_raw(p));
    }
}

/// the count a blocking operation takes for itself before it waits, so the
/// primitive outlives the call even if every language owner releases it
/// meanwhile. declare it before anything that borrows the primitive (a lock
/// guard): rust drops locals in reverse order, so the count goes last.
pub(crate) struct BlockCount {
    h: *const SyncHeader,
    taken: Cell<bool>,
}

impl BlockCount {
    pub(crate) fn new(h: &SyncHeader) -> BlockCount {
        BlockCount {
            h: h as *const SyncHeader,
            taken: Cell::new(false),
        }
    }

    /// idempotent: an operation that waits repeatedly pays one count.
    #[inline]
    pub(crate) fn take(&self) {
        if !self.taken.get() {
            unsafe { (*self.h).refs.fetch_add(1, Ordering::Relaxed) };
            self.taken.set(true);
        }
    }
}

impl Drop for BlockCount {
    fn drop(&mut self) {
        if self.taken.get() {
            unsafe { release_count(&*self.h) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_handles_are_ignored() {
        unsafe {
            pith_sync_retain(0);
            pith_sync_release(0);
            pith_sync_retain(12345);
            pith_sync_release(12345);
            // aligned, readable, and not a sync primitive
            let word: Box<u64> = Box::new(7);
            let p = &*word as *const u64 as i64;
            pith_sync_retain(p);
            pith_sync_release(p);
            assert_eq!(*word, 7);
        }
    }

    #[test]
    fn each_kind_counts_and_frees_at_zero() {
        let handles = [
            super::super::mutex::pith_mutex_new() as i64,
            super::super::waitgroup::pith_waitgroup_new() as i64,
            super::super::semaphore::pith_semaphore_new(1) as i64,
            super::super::atomic_int::pith_atomic_int_new(5),
        ];
        for h in handles {
            unsafe {
                let header = &*(h as *const SyncHeader);
                assert_eq!(header.refs_for_tests(), 1);
                pith_sync_retain(h);
                assert_eq!(header.refs_for_tests(), 2);
                pith_sync_release(h);
                assert_eq!(header.refs_for_tests(), 1);
                // the creator's count: this one frees, which valgrind and
                // the leak cases check
                pith_sync_release(h);
            }
        }
    }

    #[test]
    fn a_blocked_count_keeps_the_primitive_alive() {
        let h = super::super::mutex::pith_mutex_new() as i64;
        unsafe {
            let header = &*(h as *const SyncHeader);
            let block = BlockCount::new(header);
            block.take();
            block.take();
            assert_eq!(header.refs_for_tests(), 2);
            // the language's last owner goes while the operation is in flight
            pith_sync_release(h);
            assert_eq!(header.refs_for_tests(), 1);
            assert_eq!(header.magic, MUTEX_MAGIC);
            // the operation's own count is the last, and frees
            drop(block);
        }
    }
}
