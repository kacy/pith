//! the run queues a worker drains: `PinnedQueue` for woken tasks, and
//! `StealQueue` for fresh tasks any idle worker may take.

use super::*;

/// how many times running the affinity slot can win over the fifo before the
/// fifo gets a turn. high enough that an ordinary handoff never notices it, low
/// enough that a task queued behind a busy pair waits a bounded number of
/// exchanges rather than an unbounded one.
pub(super) const MAX_AFFINITY_STREAK: u32 = 32;

/// a worker's woken tasks: a fifo plus the single `next` slot a same-worker
/// wake fills (see the `pinned` field on `Worker`).
#[derive(Default)]
pub(super) struct PinnedQueue {
    pub(super) next: Option<TaskId>,
    pub(super) queue: VecDeque<TaskId>,
    /// consecutive `pop`s that have taken the affinity slot, reset whenever the
    /// fifo gets one.
    pub(super) affinity_streak: u32,
}

impl PinnedQueue {
    /// take the next task to resume: the affinity slot first, then the fifo.
    ///
    /// the preference for the slot is bounded. displacing on `push_next` alone
    /// does not bound it, because a ping-ponging pair empties the slot on every
    /// `pop` before refilling it, so the displacement never fires and a third
    /// task sitting in the fifo waits behind the pair forever. counting the
    /// streak closes that: after `MAX_AFFINITY_STREAK` exchanges the fifo goes
    /// first, once, and the pair carries on from there.
    pub(super) fn pop(&mut self) -> Option<TaskId> {
        if self.affinity_streak < MAX_AFFINITY_STREAK {
            if let Some(id) = self.next.take() {
                self.affinity_streak += 1;
                return Some(id);
            }
        }
        self.affinity_streak = 0;
        // with nothing queued there is no one to be fair to, so the slot still
        // runs — the streak only ever costs a turn when someone is waiting.
        self.queue.pop_front().or_else(|| self.next.take())
    }

    /// a cross-worker (or non-worker) wake: ordinary fifo.
    pub(super) fn push(&mut self, id: TaskId) {
        self.queue.push_back(id);
    }

    /// a same-worker wake: run this one next, displacing any previous occupant to
    /// the back of the fifo so the slot cannot be monopolized.
    pub(super) fn push_next(&mut self, id: TaskId) {
        if let Some(previous) = self.next.replace(id) {
            self.queue.push_back(previous);
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.next.is_none() && self.queue.is_empty()
    }
}

/// a queue of fresh, stealable tasks: a worker's `local` queue or the injector.
/// the length is mirrored in an atomic so an idle worker can tell an empty queue
/// from a full one without taking its lock. before the mirror, every idle probe
/// locked every worker's queue in turn, so the cost of looking for work grew
/// with the pool and a 32-worker pool spent most of its time in those locks.
///
/// the mirror is updated under the queue lock, so it is exact whenever the lock
/// is free. a reader that sees zero may miss a push in flight; that push wakes a
/// worker itself (see `wake_searcher`), so a stale zero costs nothing.
#[derive(Default)]
pub(super) struct StealQueue {
    pub(super) queue: Mutex<VecDeque<TaskId>>,
    pub(super) len: AtomicUsize,
}

/// the most tasks one steal moves, besides the one the thief runs.
pub(super) const STEAL_MAX: usize = 32;

impl StealQueue {
    pub(super) fn lock(&self) -> MutexGuard<'_, VecDeque<TaskId>> {
        self.queue.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.len.load(AtomicOrdering::SeqCst) == 0
    }

    pub(super) fn push(&self, id: TaskId) {
        let mut q = self.lock();
        q.push_back(id);
        self.len.store(q.len(), AtomicOrdering::SeqCst);
    }

    pub(super) fn pop(&self) -> Option<TaskId> {
        if self.is_empty() {
            return None;
        }
        let mut q = self.lock();
        let id = q.pop_front();
        self.len.store(q.len(), AtomicOrdering::SeqCst);
        id
    }

    /// pop the front task, unless another thread holds the queue. idle
    /// workers all look at the injector, and when they blocked on its lock
    /// they spent their time in the kernel instead of finding work elsewhere.
    /// a miss loses nothing: the queue's length still reads non-zero, so the
    /// caller looks again before it parks.
    pub(super) fn try_pop(&self) -> Option<TaskId> {
        if self.is_empty() {
            return None;
        }
        let mut q = self.queue.try_lock().ok()?;
        let id = q.pop_front();
        self.len.store(q.len(), AtomicOrdering::SeqCst);
        id
    }

    /// take the front task to run and move up to half of the rest onto
    /// `into`, so a thief that found a backlog does not have to come back for
    /// each task. like `try_pop`, a thief skips a queue another thread holds.
    pub(super) fn steal_half(&self, into: &StealQueue) -> Option<TaskId> {
        if self.is_empty() {
            return None;
        }
        let mut batch = [0; STEAL_MAX];
        let (id, taken) = {
            let mut q = self.queue.try_lock().ok()?;
            let id = q.pop_front()?;
            let taken = (q.len() / 2).min(STEAL_MAX);
            for (slot, stolen) in batch.iter_mut().zip(q.drain(..taken)) {
                *slot = stolen;
            }
            self.len.store(q.len(), AtomicOrdering::SeqCst);
            (id, taken)
        };
        if taken > 0 {
            let mut q = into.lock();
            q.extend(&batch[..taken]);
            into.len.store(q.len(), AtomicOrdering::SeqCst);
        }
        Some(id)
    }
}

