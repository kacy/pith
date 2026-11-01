//! a weak upgrade racing the last strong release (H10).
//!
//! the upgrade is the real `runtime_core::weak_upgrade`, run on loom's atomics.
//! the release mirrors the shared-count path of `struct_release` with a
//! destructor: the decrement, the guard count the destructor runs under, the
//! dead bit. the sole-owner shortcut is left out because a weak holder rules
//! it out, and local counts because a struct a weak reader on another task
//! can reach is marked shared.
//!
//! the struct's fields are a loom cell, so a read that is not ordered after
//! the destructor's write is reported as a data race, and a second run of the
//! destructor fails its assertion.

use crate::runtime_core::{weak_upgrade, STRUCT_DEAD_BIT, STRUCT_DTOR_GUARD};
use loom::cell::UnsafeCell;
use loom::sync::atomic::{fence, AtomicU32, Ordering};
use loom::sync::Arc;
use loom::thread;

struct Header {
    strong: AtomicU32,
    flags: AtomicU32,
    /// the fields: live while 1, destroyed once the destructor stores 0.
    field: UnsafeCell<i64>,
}

impl Header {
    /// a struct with one strong owner, also reachable through a weak reference.
    fn new() -> Header {
        Header {
            strong: AtomicU32::new(1),
            flags: AtomicU32::new(0),
            field: UnsafeCell::new(1),
        }
    }

    /// `struct_release` on a shared struct with a destructor, which runs
    /// under the guard count `guard`.
    fn release(&self, guard: u32) {
        let prev = self.strong.fetch_sub(1, Ordering::Release);
        if prev > 1 {
            return;
        }
        assert_eq!(prev, 1, "over-release");
        fence(Ordering::Acquire);
        self.strong.store(guard, Ordering::Release);
        // the destructor: it drops the fields, exactly once.
        self.field.with_mut(|field| unsafe {
            assert_eq!(*field, 1, "the destructor ran twice");
            *field = 0;
        });
        self.flags.fetch_or(STRUCT_DEAD_BIT, Ordering::Release);
    }

    /// a reader using the value it upgraded to.
    fn read(&self) {
        self.field.with(|field| unsafe {
            assert_eq!(*field, 1, "read a destroyed value");
        });
    }
}

fn race(upgrade: fn(&Header) -> bool, guard: u32) {
    loom::model(move || {
        let header = Arc::new(Header::new());
        let owner = Arc::clone(&header);
        let drop_last = thread::spawn(move || owner.release(guard));
        if upgrade(&header) {
            header.read();
            header.release(guard);
        }
        drop_last.join().unwrap();
        header.field.with(|field| unsafe {
            assert_eq!(*field, 0, "the value was never destroyed");
        });
    });
}

#[test]
fn loom_weak_upgrade_against_last_release() {
    race(|header| weak_upgrade(&header.strong, &header.flags), STRUCT_DTOR_GUARD);
}

/// the destructor guard used to be a count of one, which an upgrade cannot
/// tell from a live owner.
#[test]
#[should_panic]
fn loom_weak_pre_fix_guard_of_one_is_revived() {
    race(|header| weak_upgrade(&header.strong, &header.flags), 1);
}

/// the read before the fix: `pith_struct_weak_load` checked the dead bit and
/// handed out the pointer, and the emitted `.value()` retained it afterwards.
#[test]
#[should_panic]
fn loom_weak_pre_fix_check_then_retain() {
    race(|header| {
        if header.flags.load(Ordering::Acquire) & STRUCT_DEAD_BIT != 0 {
            return false;
        }
        header.strong.fetch_add(1, Ordering::Relaxed);
        true
    }, 1);
}
