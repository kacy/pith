//! one green task's scheduling word, for the models that park and wake it.
//!
//! a mirror, not the real `SyncBlock`: the real block holds a coroutine, and
//! the scheduler around it is global state loom cannot run. what is real is
//! the decision each side makes on the word: a wake goes through `wake_step`
//! and a suspend through `after_yield`, the functions `claim_parked` and
//! `run_task` call. the CAS loops around them are copied from those two.
//!
//! the task is modeled as one loom thread that plays both the task and its
//! worker: `park` is the suspend, the yield arm, and the wait for the next
//! resume. the run queue is a flag under a mutex with a condvar, so a wake
//! that is lost leaves the task blocked forever and loom reports a deadlock.

use crate::concurrency::green::word::*;
use loom::sync::atomic::{AtomicU64, Ordering};
use loom::sync::{Condvar, Mutex};

/// the worker every modeled task is pinned to.
const OWNER: usize = 0;

pub(super) struct Task {
    word: AtomicU64,
    /// the owner worker's run queue, for this one task: true while queued.
    queued: Mutex<bool>,
    resumable: Condvar,
}

impl Task {
    /// a task that is running, as it is when it reaches a block site.
    pub(super) fn running() -> Task {
        Task {
            word: AtomicU64::new(owner_bits(OWNER) | S_RUNNING),
            queued: Mutex::new(false),
            resumable: Condvar::new(),
        }
    }

    /// `green::wake`: claim a parked task and queue it, or leave a pending
    /// flag on a running one.
    pub(super) fn wake(&self) {
        let mut word = self.word.load(Ordering::Acquire);
        loop {
            let (next, claimed) = match wake_step(word, |owner| owner) {
                WakeStep::Claim(next, _) => (next, true),
                WakeStep::Pend(next) => (next, false),
                WakeStep::Skip => return,
            };
            match self
                .word
                .compare_exchange_weak(word, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    if claimed {
                        self.enqueue();
                    }
                    return;
                }
                Err(current) => word = current,
            }
        }
    }

    /// `green::wake_with`: like `wake`, but a claimed task is handed a value
    /// first. true when the task was claimed and `fetch` ran.
    pub(super) fn wake_with(&self, handoff: &Mutex<Option<i64>>, fetch: impl FnOnce() -> Option<i64>) -> bool {
        let mut word = self.word.load(Ordering::Acquire);
        loop {
            let (next, claimed) = match wake_step(word, |owner| owner) {
                WakeStep::Claim(next, _) => (next, true),
                WakeStep::Pend(next) => (next, false),
                WakeStep::Skip => return false,
            };
            match self
                .word
                .compare_exchange_weak(word, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    if claimed {
                        *handoff.lock().unwrap() = fetch();
                        self.enqueue();
                    }
                    return claimed;
                }
                Err(current) => word = current,
            }
        }
    }

    /// `green::park_current` from the task's side, through to its next
    /// resume: the yield arm settles the word, and the worker later claims
    /// it back with the Ready -> Running CAS.
    pub(super) fn park(&self) {
        let mut word = self.word.load(Ordering::Acquire);
        let requeue = loop {
            assert_eq!(word & S_STATE_MASK, S_RUNNING, "a parking task must be running");
            let (next, requeue) = after_yield(word);
            match self
                .word
                .compare_exchange_weak(word, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break requeue,
                Err(current) => word = current,
            }
        };
        if requeue != Requeue::None {
            self.enqueue();
        }
        let mut queued = self.queued.lock().unwrap();
        while !*queued {
            queued = self.resumable.wait(queued).unwrap();
        }
        *queued = false;
        drop(queued);
        let word = self.word.load(Ordering::Acquire);
        assert_eq!(word & S_STATE_MASK, S_READY, "a queued task must be ready");
        self.word
            .compare_exchange(word, (word & !S_STATE_MASK) | S_RUNNING, Ordering::AcqRel, Ordering::Acquire)
            .expect("only the owner worker resumes the task");
    }

    /// a wake aimed at the task while it ran, still waiting to make its next
    /// park return at once.
    pub(super) fn wake_pending(&self) -> bool {
        self.word.load(Ordering::Acquire) & S_WAKE_PENDING != 0
    }

    fn enqueue(&self) {
        let mut queued = self.queued.lock().unwrap();
        assert!(!*queued, "a task was queued twice");
        *queued = true;
        self.resumable.notify_one();
    }
}
