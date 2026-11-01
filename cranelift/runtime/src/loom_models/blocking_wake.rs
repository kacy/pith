//! the blocking pool's result handshake against the green park/wake word.
//!
//! the slot is the real `blocking::Slot`, built on loom's types through
//! `crate::sync`. the task's side mirrors `blocking::run` line for line, with
//! the timed spin reduced to the one `is_ready` poll that decides it (loom has
//! no clock; one poll covers both of its outcomes). the green word is the
//! mirror in `green_word`.
//!
//! the property is the one #1270 restored: when the call returns, no wake is
//! left pending on the task. a pending wake makes the task's next park, at an
//! unrelated block site, return at once; a channel receive woken that way
//! registered twice and later took a value sent on another channel.

use super::green_word::Task;
use crate::blocking::Slot;
use loom::sync::atomic::{fence, AtomicBool, Ordering};
use loom::sync::{Arc, Mutex};
use loom::thread;

/// the task's side of `blocking::run` after the job is queued.
fn run_task_side(task: &Task, slot: &Slot<i64>) -> i64 {
    if slot.is_ready() || !slot.claim_park() {
        return slot.take();
    }
    loop {
        task.park();
        if slot.is_ready() {
            return slot.take();
        }
    }
}

#[test]
fn loom_blocking_result_leaves_no_stale_wake() {
    loom::model(|| {
        let task = Arc::new(Task::running());
        let slot = Arc::new(Slot::<i64>::new());
        let (pool_task, publish) = (Arc::clone(&task), Arc::clone(&slot));
        let pool = thread::spawn(move || {
            if publish.complete(7) {
                pool_task.wake();
            }
        });
        assert_eq!(run_task_side(&task, &slot), 7);
        pool.join().unwrap();
        assert!(!task.wake_pending(), "the pool left a wake pending on the task");
    });
}

/// the slot as it was before #1270 (`git show d5c50df8~1:cranelift/runtime/src/blocking.rs`):
/// a ready flag, and a wake sent whether or not the task ever parked. kept so
/// the model is shown to catch the bug it was written for.
struct PreFixSlot {
    ready: AtomicBool,
    value: Mutex<Option<i64>>,
}

impl PreFixSlot {
    fn complete(&self, value: i64, task: &Task) {
        *self.value.lock().unwrap() = Some(value);
        self.ready.store(true, Ordering::Release);
        fence(Ordering::SeqCst);
        task.wake();
    }

    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    fn take(&self) -> Option<i64> {
        fence(Ordering::SeqCst);
        if !self.is_ready() {
            return None;
        }
        self.value.lock().unwrap().take()
    }
}

#[test]
#[should_panic(expected = "the pool left a wake pending on the task")]
fn loom_blocking_pre_fix_slot_leaves_a_stale_wake() {
    loom::model(|| {
        let task = Arc::new(Task::running());
        let slot = Arc::new(PreFixSlot {
            ready: AtomicBool::new(false),
            value: Mutex::new(None),
        });
        let (pool_task, publish) = (Arc::clone(&task), Arc::clone(&slot));
        let pool = thread::spawn(move || publish.complete(7, &pool_task));
        // the pre-fix `run`: the spin's one poll, then take-or-park.
        let value = if slot.is_ready() {
            slot.take()
        } else {
            loop {
                if let Some(value) = slot.take() {
                    break Some(value);
                }
                task.park();
            }
        };
        assert_eq!(value, Some(7));
        pool.join().unwrap();
        assert!(!task.wake_pending(), "the pool left a wake pending on the task");
    });
}
