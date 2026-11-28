//! Lock-free Vyukov bounded MPMC ring buffer for buffered channels.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};

/// one slot of the lock-free buffered queue. `sequence` is the vyukov ticket:
/// a slot is free for the enqueuer holding position `pos` when its sequence is
/// exactly `pos`, and holds a value for the dequeuer at `pos` when its sequence
/// is `pos + 1`. after a dequeue the slot's sequence advances by the ring size,
/// handing it to the enqueuer one lap later. the sequence's release store is
/// what publishes the value in `cell`.
pub(super) struct Slot {
    sequence: AtomicUsize,
    cell: UnsafeCell<i64>,
}

/// a cache-line padded atomic position. senders hammer `enqueue_pos` and
/// receivers `dequeue_pos`; on one line they false-share and every CAS bounces
/// the line between roles.
#[repr(align(64))]
struct PaddedPos(AtomicUsize);

/// the lock-free core of a buffered channel: a fixed ring of slots with two
/// monotonically increasing positions. `try_enqueue`/`try_dequeue` are the
/// classic vyukov bounded mpmc (pure cas, no lock), and they alone carry the
/// throughput path. blocking, waking, green-task parking, and close all stay on
/// the channel mutex, which an op only touches when the ring is actually full
/// or empty (or when a gate says someone is parked).
pub(super) struct Ring {
    buffer: Box<[Slot]>,
    mask: usize,
    /// true when the requested capacity is a power of two, i.e. the ring size
    /// equals the capacity exactly: fullness is then fully encoded in the slot
    /// sequences and `try_enqueue` never needs to read `dequeue_pos`, which
    /// keeps senders off the receivers' cache line entirely.
    exact: bool,
    enqueue_pos: PaddedPos,
    dequeue_pos: PaddedPos,
}

// SAFETY: `Slot::cell` is only accessed after exclusively claiming the slot via
// atomic sequence/position CASes (Acquire/Release).
unsafe impl Send for Ring {}
// SAFETY: Concurrent senders and receivers synchronize every cell read/write
// through the Vyukov sequence protocol on `Slot::sequence`.
unsafe impl Sync for Ring {}

/// high bit of `Ring::enqueue_pos`, set atomically on channel close so no
/// concurrent lock-free `try_enqueue` can ever succeed after `close()` returns.
pub(super) const CLOSED_BIT: usize = 1usize << (usize::BITS - 1);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum EnqueueStatus {
    Enqueued,
    Full,
    Closed,
}

impl Ring {
    /// how many slots a ring for `capacity` values allocates. split out of
    /// `new` so the memory accounting in `pith_channel_new` reports the size
    /// the ring actually asks for rather than a second copy of this rule.
    pub(super) fn slot_count(capacity: usize) -> usize {
        capacity.next_power_of_two().max(2)
    }

    /// ring size = capacity rounded up to a power of two, so the position wraps
    /// with a mask. the extra slots beyond the requested capacity are kept out
    /// of service by `try_enqueue`'s explicit capacity check, so `len()` and
    /// blocking behavior honor the exact capacity the program asked for.
    ///
    /// the minimum size is 2: with a single slot, the sequence after an enqueue
    /// at position p (p + 1) equals the *next* enqueue position, so a full slot
    /// is indistinguishable from a free one and gets overwritten. two slots
    /// restore the invariant; a capacity-1 channel then runs with `exact` off
    /// and the explicit check enforcing its bound.
    pub(super) fn new(capacity: usize) -> Ring {
        let size = Ring::slot_count(capacity);
        let buffer: Vec<Slot> = (0..size)
            .map(|i| Slot {
                sequence: AtomicUsize::new(i),
                cell: UnsafeCell::new(0),
            })
            .collect();
        Ring {
            buffer: buffer.into_boxed_slice(),
            mask: size - 1,
            exact: capacity == size,
            enqueue_pos: PaddedPos(AtomicUsize::new(0)),
            dequeue_pos: PaddedPos(AtomicUsize::new(0)),
        }
    }

    /// atomically seal the enqueue side so no subsequent `try_enqueue` can
    /// claim a slot.
    pub(super) fn close(&self) {
        self.enqueue_pos.0.fetch_or(CLOSED_BIT, Ordering::SeqCst);
    }

    /// how many values are in the ring right now. approximate under concurrency
    /// (positions move independently), exact when the channel is quiescent.
    pub(super) fn len(&self) -> usize {
        let enq = self.enqueue_pos.0.load(Ordering::Acquire) & !CLOSED_BIT;
        let deq = self.dequeue_pos.0.load(Ordering::Acquire);
        enq.saturating_sub(deq)
    }

    /// lock-free enqueue: rejects closed channels atomically via `CLOSED_BIT`
    /// in `enqueue_pos`, eliminating both the send-vs-close race and the extra
    /// `SeqCst` load on the fast path.
    #[inline(always)]
    pub(super) fn try_enqueue(&self, value: i64, capacity: usize) -> EnqueueStatus {
        let mut pos = self.enqueue_pos.0.load(Ordering::Acquire);
        loop {
            if pos & CLOSED_BIT != 0 {
                return EnqueueStatus::Closed;
            }
            // a non-power-of-two capacity leaves spare ring slots, so fullness
            // is not encoded in the slot sequences alone: enforce it here. when
            // the capacity is exact this check (and its read of the receivers'
            // cache line) disappears.
            if !self.exact
                && pos.saturating_sub(self.dequeue_pos.0.load(Ordering::Acquire)) >= capacity
            {
                return EnqueueStatus::Full;
            }
            let slot = &self.buffer[pos & self.mask];
            let seq = slot.sequence.load(Ordering::Acquire);
            if seq == pos {
                // slot free for this position: claim it by advancing enqueue_pos.
                // if `close()` sets `CLOSED_BIT` concurrently, this CAS fails
                // and the next iteration sees `CLOSED_BIT`.
                match self.enqueue_pos.0.compare_exchange_weak(
                    pos,
                    pos + 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        // SAFETY: Slot was claimed exclusively via `enqueue_pos` CAS and its
                        // sequence matched `pos`; the Release store below publishes the write.
                        unsafe { *slot.cell.get() = value };
                        // release-publish the value to the dequeuer at this pos.
                        slot.sequence.store(pos + 1, Ordering::Release);
                        return EnqueueStatus::Enqueued;
                    }
                    Err(actual) => pos = actual,
                }
            } else if seq < pos {
                // the slot still holds a value from the previous lap: full.
                return EnqueueStatus::Full;
            } else {
                // another enqueuer already claimed this position; catch up.
                pos = self.enqueue_pos.0.load(Ordering::Acquire);
            }
        }
    }

    /// lock-free dequeue: None when the channel is empty.
    #[inline(always)]
    pub(super) fn try_dequeue(&self) -> Option<i64> {
        let mut pos = self.dequeue_pos.0.load(Ordering::Relaxed);
        loop {
            let slot = &self.buffer[pos & self.mask];
            let seq = slot.sequence.load(Ordering::Acquire);
            if seq == pos + 1 {
                // slot holds the value for this position: claim it.
                match self.dequeue_pos.0.compare_exchange_weak(
                    pos,
                    pos + 1,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // SAFETY: Slot was claimed exclusively via `dequeue_pos` CAS and
                        // `seq == pos + 1` Acquire-synchronized with the enqueuer's Release store.
                        let value = unsafe { *slot.cell.get() };
                        // hand the slot to the enqueuer one lap later.
                        slot.sequence.store(pos + self.mask + 1, Ordering::Release);
                        return Some(value);
                    }
                    Err(actual) => pos = actual,
                }
            } else if seq <= pos {
                // the slot is still awaiting its enqueuer: empty.
                return None;
            } else {
                // another dequeuer already claimed this position; catch up.
                pos = self.dequeue_pos.0.load(Ordering::Relaxed);
            }
        }
    }
}
