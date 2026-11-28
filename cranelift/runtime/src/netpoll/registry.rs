//! Waiter, deadline heap, and per-poller fd registration state.

use super::{lock_inner, Poller};
use crate::int_hash::IntMap;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::Instant;

/// which readiness a task waits for on an fd. one fd can carry a read waiter and
/// a write waiter at once.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum Interest {
    Read,
    Write,
}

impl Interest {
    /// the epoll event bit for this interest, also its bit in `FdReg::ready`.
    pub(super) fn epoll_bit(self) -> u32 {
        match self {
            Interest::Read => libc::EPOLLIN as u32,
            Interest::Write => libc::EPOLLOUT as u32,
        }
    }
}

// outcome of a wait, shared between the waiting task and the poller via an
// `AtomicI32`. it starts `PENDING`; whichever poller path handles the waiter
// first flips it to a terminal value with a compare-exchange, so a readiness and
// a timeout racing on the same waiter cannot both "win".
pub(super) const PENDING: i32 = 0;
pub(super) const READY: i32 = 1;
pub(super) const TIMED_OUT: i32 = 2;
/// the fd was closed out from under a still-parked waiter (see `on_close`). the
/// woken task reports it as an I/O error so its read/write unwinds cleanly
/// instead of hanging or retrying a syscall on a dead fd.
pub(super) const CLOSED: i32 = 3;

/// the epoll token used for the eventfd. real fds are stored as their own value
/// in `epoll_event.u64`; `u64::MAX` can never collide with a real fd.
pub(super) const EVENTFD_TOKEN: u64 = u64::MAX;

/// one blocked task waiting on an (fd, interest).
pub(super) struct Waiter {
    /// green task id, passed straight to `green::wake`.
    pub(super) task: usize,
    /// unique registration id, used to match a heap deadline entry back to the
    /// exact waiter (a stale heap entry whose waiter already fired finds nothing).
    pub(super) seq: u64,
    /// shared result cell; the poller writes `READY`, `TIMED_OUT` or `CLOSED`
    /// here before waking the task, which reads it back after `park_current`
    /// returns.
    pub(super) outcome: Arc<AtomicI32>,
}

impl Waiter {
    /// flip the outcome from `PENDING` to `result` and queue the task to wake.
    /// a waiter another path already resolved is left alone, so a readiness,
    /// a timeout and a close racing on one waiter resolve it exactly once.
    #[inline]
    pub(super) fn resolve(&self, result: i32, to_wake: &mut Vec<usize>) {
        if self
            .outcome
            .compare_exchange(PENDING, result, AtomicOrdering::AcqRel, AtomicOrdering::Acquire)
            .is_ok()
        {
            to_wake.push(self.task);
        }
    }
}

/// what a heap entry resolves when it expires: an fd wait that carried a
/// timeout, or a pure timer with no fd at all (a green task sleeping). the two
/// are distinct variants, rather than a sentinel fd smuggled through the io
/// shape, so the fd code (`register_io`, `on_close`) can never be handed a
/// value that is not a real fd.
pub(super) enum DeadlineTarget {
    Io { fd: RawFd, interest: Interest },
    Timer,
}

/// a finite-timeout registration, ordered by instant for the min-heap.
pub(super) struct Deadline {
    pub(super) at: Instant,
    pub(super) seq: u64,
    pub(super) target: DeadlineTarget,
}

// order deadlines by time (then seq) so a `BinaryHeap<Reverse<Deadline>>` pops the
// soonest first.
impl PartialEq for Deadline {
    fn eq(&self, other: &Self) -> bool {
        self.at == other.at && self.seq == other.seq
    }
}
impl Eq for Deadline {}
impl Ord for Deadline {
    fn cmp(&self, other: &Self) -> Ordering {
        self.at.cmp(&other.at).then(self.seq.cmp(&other.seq))
    }
}
impl PartialOrd for Deadline {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// the events every fd is registered for, once, on its first wait: both
/// directions and a peer hangup, edge-triggered.
pub(super) const FD_EVENTS: u32 = (libc::EPOLLIN | libc::EPOLLOUT | libc::EPOLLRDHUP | libc::EPOLLET) as u32;

/// one fd in a poller's epoll set. an fd is present in `Inner::fds` while it is
/// in the set. `ready` holds the interest bits (`Interest::epoll_bit`) of edges
/// that arrived while no task waited in that direction; the next wait in that
/// direction consumes its bit instead of parking.
pub(super) struct FdReg {
    pub(super) ready: u32,
}

/// one poller's registry, behind its mutex. the maps are keyed by fd numbers
/// and sequence numbers, so they use the runtime's integer hasher; siphash
/// would run inside the lock on every park and wake.
pub(super) struct Inner {
    pub(super) waiters: IntMap<(RawFd, Interest), Vec<Waiter>>,
    pub(super) fds: IntMap<RawFd, FdReg>,
    /// how many tasks are in `waiters`. an fd with nobody waiting on it gives a
    /// poll nothing to wake, so only a waiter makes the registry busy; an edge
    /// on an idle fd stays queued in the kernel until a poll collects it.
    pub(super) waiting: usize,
    pub(super) deadlines: BinaryHeap<std::cmp::Reverse<Deadline>>,
    /// sleeping tasks, keyed by the seq of their heap entry. a timer has no
    /// (fd, interest) to key on, and giving it its own map keeps it out of
    /// `waiters`, where everything is fd-shaped.
    pub(super) timers: IntMap<u64, Waiter>,
    pub(super) next_seq: u64,
    /// the heap size at which the next push first drops the entries whose
    /// waits have ended. see `push_deadline`.
    compact_at: usize,
}

/// the smallest heap `push_deadline` compacts: below it the stale entries
/// cost less than the scan that would drop them.
pub(super) const DEADLINE_COMPACT_MIN: usize = 1024;

impl Inner {
    pub(super) fn new() -> Self {
        Inner {
            waiters: IntMap::default(),
            fds: IntMap::default(),
            waiting: 0,
            deadlines: BinaryHeap::new(),
            timers: IntMap::default(),
            next_seq: 0,
            compact_at: DEADLINE_COMPACT_MIN,
        }
    }

    /// whether a deadline entry still has a wait to time out. a wait that
    /// fired ready, or was closed, left its entry behind in the heap.
    fn deadline_is_live(&self, deadline: &Deadline) -> bool {
        match deadline.target {
            DeadlineTarget::Timer => self.timers.contains_key(&deadline.seq),
            DeadlineTarget::Io { fd, interest } => self
                .waiters
                .get(&(fd, interest))
                .is_some_and(|waiters| waiters.iter().any(|w| w.seq == deadline.seq)),
        }
    }

    /// add a deadline to the heap. a wait that ends before its deadline
    /// leaves its entry in the heap until the deadline passes, so a server
    /// that gives every read a 5 s timeout kept five seconds of requests
    /// there: hundreds of megabytes across the workers, and a pop that walked
    /// a heap far larger than the cache. so once the heap reaches
    /// `compact_at`, the push first drops the stale entries and sets the next
    /// threshold at twice what is left, which keeps the heap within a small
    /// multiple of the live waits for a constant amortized cost per push.
    pub(super) fn push_deadline(&mut self, deadline: Deadline) {
        if self.deadlines.len() >= self.compact_at {
            let entries = std::mem::take(&mut self.deadlines).into_vec();
            let live: Vec<_> = entries
                .into_iter()
                .filter(|entry| self.deadline_is_live(&entry.0))
                .collect();
            self.deadlines = BinaryHeap::from(live);
            self.compact_at = (2 * self.deadlines.len()).max(DEADLINE_COMPACT_MIN);
        }
        self.deadlines.push(std::cmp::Reverse(deadline));
    }

    /// record in `busy` whether this registry has anything a poll could find.
    /// called at the end of every locked change.
    pub(super) fn publish(&self, busy: &AtomicBool) {
        let any = self.waiting > 0 || !self.deadlines.is_empty() || !self.timers.is_empty();
        busy.store(any, AtomicOrdering::SeqCst);
    }

    /// a fresh registration id for a waiter and its heap entry.
    pub(super) fn take_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        seq
    }

    /// add a waiter for `(fd, interest)`.
    pub(super) fn push_waiter(&mut self, fd: RawFd, interest: Interest, waiter: Waiter) {
        self.waiters.entry((fd, interest)).or_default().push(waiter);
        self.waiting += 1;
    }

    /// remove a specific waiter by `(fd, interest, seq)` if still present
    /// (for cleaning up after a spurious wake so no stale waiter remains).
    pub(super) fn remove_waiter(&mut self, fd: RawFd, interest: Interest, seq: u64) {
        let mut empty = false;
        if let Some(list) = self.waiters.get_mut(&(fd, interest)) {
            if let Some(idx) = list.iter().position(|w| w.seq == seq) {
                list.swap_remove(idx);
                self.waiting -= 1;
            }
            empty = list.is_empty();
        }
        if empty {
            self.waiters.remove(&(fd, interest));
        }
    }
}
/// the registration half of `sleep_task`, split out so it can be exercised
/// without a live coroutine to suspend. pushes a timer deadline and its waiter
/// under the lock.
pub(super) fn register_timer(poller: &Poller, at: Instant, task: usize, outcome: &Arc<AtomicI32>) {
    let mut inner = lock_inner(poller);
    let seq = inner.take_seq();
    inner.timers.insert(
        seq,
        Waiter {
            task,
            seq,
            outcome: outcome.clone(),
        },
    );
    inner.push_deadline(Deadline {
        at,
        seq,
        target: DeadlineTarget::Timer,
    });
    inner.publish(&poller.busy);
}
/// take all waiters for `(fd, interest)`, mark each ready, and queue it to wake.
/// returns whether there were any. the emptied list stays in the map, so the
/// next wait on the fd, which on a keep-alive connection comes a request later,
/// reuses its allocation; on_close removes it.
pub(super) fn take_ready(inner: &mut Inner, fd: RawFd, interest: Interest, to_wake: &mut Vec<usize>) -> bool {
    let Some(waiters) = inner.waiters.get_mut(&(fd, interest)) else {
        return false;
    };
    if waiters.is_empty() {
        return false;
    }
    inner.waiting -= waiters.len();
    for w in waiters.drain(..) {
        w.resolve(READY, to_wake);
    }
    true
}

/// pop every deadline at or before `now`, mark the matching still-pending waiter
/// timed out, remove it, and queue it to wake. a stale heap entry (its waiter
/// already fired ready and was removed) simply matches nothing. a timer entry
/// has no fd, so it wakes its sleeper directly and never touches epoll.
pub(super) fn collect_expired(inner: &mut Inner, now: Instant, to_wake: &mut Vec<usize>) {
    while let Some(top) = inner.deadlines.peek() {
        if top.0.at > now {
            break;
        }
        let Some(std::cmp::Reverse(Deadline { seq, target, .. })) = inner.deadlines.pop() else {
            break;
        };

        match target {
            DeadlineTarget::Timer => {
                // a sleep expiring is its success case; resolving through the
                // same compare-exchange keeps the resolve-once discipline even
                // though nothing else currently races for a timer.
                if let Some(w) = inner.timers.remove(&seq) {
                    w.resolve(TIMED_OUT, to_wake);
                }
            }
            DeadlineTarget::Io { fd, interest } => {
                if let Some(waiters) = inner.waiters.get_mut(&(fd, interest)) {
                    if let Some(pos) = waiters.iter().position(|w| w.seq == seq) {
                        let w = waiters.remove(pos);
                        inner.waiting -= 1;
                        w.resolve(TIMED_OUT, to_wake);
                        if waiters.is_empty() {
                            inner.waiters.remove(&(fd, interest));
                        }
                    }
                }
            }
        }
    }
}
