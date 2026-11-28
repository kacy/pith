//! Lock-free fast paths for buffered channels with spin-then-park fallback.

use super::handle::{ChannelInner, ParkCount, Role};
use super::park::{block_on_channel, lock_state, wake_parked};
use super::ring::{EnqueueStatus, Ring};
use crate::concurrency::green;
use std::sync::atomic::Ordering;

/// how many failed ring attempts an os-thread caller burns before parking. a
/// paired producer/consumer usually frees a slot within a few hundred cycles,
/// and a park/unpark round trip through the kernel costs microseconds, so a
/// short spin converts most would-parks into immediate retries. green tasks
/// never spin: their park is a userspace suspend, already cheap.
const SPIN_TRIES: usize = 64;

/// the buffered send: lock-free enqueue on the fast path; park under the mutex
/// only when the ring is full.
///
/// ## the park/wake race
/// a sender can find the ring full, then a receiver dequeues and checks the gate
/// before the sender registers: a lost wake, and a hang. it is closed the
/// standard way: the parker increments its gate (under the mutex), issues a
/// seq-cst fence, and *re-tries the ring op* before waiting; the waker issues a
/// seq-cst fence between its ring op and its gate load. in the fence order,
/// whichever fence comes second sees the other side's write: the waker sees the
/// incremented gate (and notifies under the mutex, which the parker holds until
/// it actually waits), or the parker's re-try sees the freed slot.
#[inline]
pub(super) fn buffered_send(park: &ParkCount, inner: &ChannelInner, ring: &Ring, value: i64) -> i64 {
    let capacity = inner.capacity;
    let green_task = green::current_task();
    let mut spins = if green_task.is_none() { SPIN_TRIES } else { 0 };
    loop {
        match ring.try_enqueue(value, capacity) {
            EnqueueStatus::Enqueued => {
                wake_parked(inner, ring, Role::Receiver);
                return 1;
            }
            EnqueueStatus::Closed => return 0,
            EnqueueStatus::Full => {}
        }
        if spins > 0 {
            spins -= 1;
            std::hint::spin_loop();
            continue;
        }
        // ring full: fall to the slow path and park as a sender.
        let mut state = lock_state(&inner.state);
        inner.parked_senders.fetch_add(1, Ordering::SeqCst);
        std::sync::atomic::fence(Ordering::SeqCst);
        match ring.try_enqueue(value, capacity) {
            EnqueueStatus::Enqueued => {
                inner.parked_senders.fetch_sub(1, Ordering::SeqCst);
                drop(state);
                wake_parked(inner, ring, Role::Receiver);
                return 1;
            }
            EnqueueStatus::Closed => {
                inner.parked_senders.fetch_sub(1, Ordering::SeqCst);
                return 0;
            }
            EnqueueStatus::Full => {}
        }
        state = block_on_channel(park, inner, state, green_task, Role::Sender);
        drop(state);
        inner.parked_senders.fetch_sub(1, Ordering::SeqCst);
    }
}

/// the buffered recv, mirror of `buffered_send`: lock-free dequeue on the fast
/// path; park under the mutex only when the ring is empty. a closed channel
/// still drains: the dequeue try comes before the closed check.
#[inline]
pub(super) fn buffered_recv(park: &ParkCount, inner: &ChannelInner, ring: &Ring) -> Option<i64> {
    let green_task = green::current_task();
    let mut spins = if green_task.is_none() { SPIN_TRIES } else { 0 };
    loop {
        if let Some(value) = ring.try_dequeue() {
            wake_parked(inner, ring, Role::Sender);
            return Some(value);
        }
        if spins > 0 {
            spins -= 1;
            std::hint::spin_loop();
            continue;
        }
        if inner.closed_flag.load(Ordering::SeqCst) {
            // one more look: a value enqueued just before close must be drained.
            if let Some(value) = ring.try_dequeue() {
                wake_parked(inner, ring, Role::Sender);
                return Some(value);
            }
            return None;
        }
        let mut state = lock_state(&inner.state);
        inner.parked_receivers.fetch_add(1, Ordering::SeqCst);
        std::sync::atomic::fence(Ordering::SeqCst);
        if let Some(value) = ring.try_dequeue() {
            inner.parked_receivers.fetch_sub(1, Ordering::SeqCst);
            drop(state);
            wake_parked(inner, ring, Role::Sender);
            return Some(value);
        }
        if inner.closed_flag.load(Ordering::SeqCst) {
            inner.parked_receivers.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        state = block_on_channel(park, inner, state, green_task, Role::Receiver);
        drop(state);
        inner.parked_receivers.fetch_sub(1, Ordering::SeqCst);
        // a green task may have been handed its value by the waker (see
        // wake_parked): the send dequeued on our behalf, so take it and skip the
        // re-try entirely. pass the id read before the park rather than asking
        // which task we are again (see the note on `green::park_current`).
        if let Some(id) = green_task {
            if let Some(value) = green::take_handoff(id) {
                return Some(value);
            }
        }
    }
}
