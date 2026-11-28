//! Channel[T]: thread-safe bounded and unbuffered message-passing channels.

mod buffered;
mod handle;
mod park;
mod ring;
#[cfg(test)]
mod tests;

use buffered::{buffered_recv, buffered_send};
use handle::{
    channel_bytes, with_channel, ChannelInner, ChannelState, ParkCount, PendingValue, Role,
    TaggedChannel, CHANNEL_MAGIC, MAX_CHANNEL_CAPACITY,
};
use park::{block_on_channel, lock_state, wake_green, wake_parked, wake_receivers, wake_senders};
use ring::{EnqueueStatus, Ring};
use crate::concurrency::green;
use crate::ffi_util::ResultPair;
use crate::runtime_core::{optional_from, optional_pair};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

pub use handle::{pith_channel_release, pith_channel_retain};

/// round-robin start for `pith_select_next_index`, so a `select` over several
/// ready channels does not always favor the first arm.
static SELECT_COUNTER: AtomicI64 = AtomicI64::new(0);

#[no_mangle]
pub extern "C" fn pith_channel_new(capacity: i64, payload_tag: i64) -> i64 {
    // the ring is allocated up front, so an absurd capacity would abort the
    // process on the allocation rather than fail the call. cap it: past this
    // many buffered values a program wants a queue it manages itself.
    let cap = capacity.clamp(0, MAX_CHANNEL_CAPACITY) as usize;
    let state = ChannelState {
        capacity: cap,
        closed: false,
        pending_value: None,
        deliveries: 0,
        receiver_waiting: 0,
        sender_waiting: 0,
        os_receivers_waiting: 0,
        os_senders_waiting: 0,
        green_receivers: Vec::new(),
        green_senders: Vec::new(),
    };
    let ptr = Box::into_raw(Box::new(TaggedChannel {
        magic: CHANNEL_MAGIC,
        payload_tag: payload_tag.clamp(0, 255) as u8,
        capacity: cap as u32,
        // the creator's count
        refs: AtomicUsize::new(1),
        inner: ChannelInner {
            ring: if cap > 0 { Some(Ring::new(cap)) } else { None },
            capacity: cap,
            closed_flag: AtomicBool::new(false),
            parked_senders: AtomicUsize::new(0),
            parked_receivers: AtomicUsize::new(0),
            state: Mutex::new(state),
            senders: Condvar::new(),
            receivers: Condvar::new(),
        },
    }));
    crate::perf_count(&crate::PERF_CHANNEL_NEWS, 1);
    crate::perf_count(&crate::PERF_CHANNEL_RETAINED_BYTES, channel_bytes(cap));
    ptr as i64
}
#[inline(always)]
fn channel_send_impl(t: &TaggedChannel, value: i64) -> i64 {
    // declared first so it drops last (see `ParkCount`).
    let park = ParkCount::new(t);
    let inner = &t.inner;
    // a buffered channel's values live in the lock-free ring; the mutex is only
    // the parking lot. an unbuffered channel is the mutex rendezvous below.
    if let Some(ring) = &inner.ring {
        return buffered_send(&park, inner, ring, value);
    }
    // is this send running inside a green task? if so it parks the task on a
    // would-block instead of blocking the worker; if not (main thread,
    // os-thread backend) it condvar-waits. computed once: the running task
    // does not change under us across a suspend/resume.
    let green_task = green::current_task();
    let mut state = lock_state(&inner.state);

    // only an unbuffered channel gets here: a buffered one took the ring
    // path above. a closed channel refuses the send.
    while !state.closed {
        // offer the value as soon as the slot is free, not only once a
        // receiver is parked: a receiver that polls (`try_recv`, which
        // `select` and `recv_ctx` use) never parks, so a sender that
        // waited for one never met it and the select ran to its
        // timeout. the send still returns only once the value is taken.
        if state.pending_value.is_none() {
            let handoffs_before = state.deliveries;
            state.pending_value = Some(PendingValue::Blocking(value));
            wake_receivers(inner, &mut state);
            // count ourselves as a waiting sender while we wait for the
            // receiver to take the value: the recv that takes it gates its
            // wake on `sender_waiting`, so an uncounted sender here would
            // never be notified (a hang on the rendezvous handshake).
            state.sender_waiting += 1;
            while !state.closed && state.deliveries == handoffs_before {
                state = block_on_channel(&park, inner, state, green_task, Role::Sender);
            }
            state.sender_waiting -= 1;
            // delivered is the authority, not `closed`: a receiver may have
            // taken the value and the channel closed before this sender got
            // the lock back.
            let delivered = state.deliveries != handoffs_before;
            return if delivered { 1 } else { 0 };
        }
        state.sender_waiting += 1;
        state = block_on_channel(&park, inner, state, green_task, Role::Sender);
        state.sender_waiting -= 1;
    }

    0
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_send(handle: i64, value: i64) -> i64 {
    // an unknown or freed handle gets the closed channel's answer: the send is
    // refused.
    // SAFETY: `handle` is validated by `with_channel` before borrowing the channel.
    unsafe { with_channel(handle, |t| channel_send_impl(t, value)) }.unwrap_or(0)
}

#[inline(always)]
fn channel_try_send_impl(t: &TaggedChannel, value: i64) -> i64 {
    let inner = &t.inner;
    if let Some(ring) = &inner.ring {
        return match ring.try_enqueue(value, inner.capacity) {
            EnqueueStatus::Enqueued => {
                wake_parked(inner, ring, Role::Receiver);
                1
            }
            EnqueueStatus::Full | EnqueueStatus::Closed => 0,
        };
    }
    let mut state = lock_state(&inner.state);

    if state.closed {
        return 0;
    }

    if state.receiver_waiting == 0 || state.pending_value.is_some() {
        return 0;
    }
    state.pending_value = Some(PendingValue::Detached(value));
    wake_receivers(inner, &mut state);
    1
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_try_send(handle: i64, value: i64) -> i64 {
    // SAFETY: `handle` is validated by `with_channel` before borrowing the channel.
    unsafe { with_channel(handle, |t| channel_try_send_impl(t, value)) }.unwrap_or(0)
}

#[inline(always)]
fn channel_recv_impl(t: &TaggedChannel) -> Option<i64> {
    // declared first so it drops last (see `ParkCount`).
    let park = ParkCount::new(t);
    let inner = &t.inner;
    if let Some(ring) = &inner.ring {
        return buffered_recv(&park, inner, ring);
    }
    let green_task = green::current_task();
    let mut state = lock_state(&inner.state);

    loop {
        if let Some(pending) = state.pending_value.take() {
            // completed a rendezvous: wake the sender waiting for its value
            // to be taken.
            state.deliveries += 1;
            wake_senders(inner, &mut state);
            return Some(pending.value());
        }

        if state.closed {
            return None;
        }

        // announce that a receiver is now waiting so a blocked sender can deposit
        // (the unbuffered rendezvous handshake). only senders need this; waking a
        // sibling receiver here is exactly what caused the single-worker livelock.
        state.receiver_waiting += 1;
        wake_senders(inner, &mut state);
        state = block_on_channel(&park, inner, state, green_task, Role::Receiver);
        state.receiver_waiting -= 1;
    }
}

#[inline(always)]
unsafe fn channel_recv(handle: i64) -> Option<i64> {
    // an unknown or freed handle yields the empty optional, exactly what a
    // closed and drained channel yields.
    // SAFETY: `handle` is validated by `with_channel` before borrowing the channel.
    unsafe { with_channel(handle, channel_recv_impl) }.flatten()
}

#[inline(always)]
fn channel_try_recv_impl(t: &TaggedChannel) -> Option<i64> {
    let inner = &t.inner;
    if let Some(ring) = &inner.ring {
        if let Some(value) = ring.try_dequeue() {
            wake_parked(inner, ring, Role::Sender);
            return Some(value);
        }
        // the empty case is the one that can observe a closed, drained ring.
        return None;
    }
    let mut state = lock_state(&inner.state);
    if let Some(pending) = state.pending_value.take() {
        state.deliveries += 1;
        wake_senders(inner, &mut state);
        return Some(pending.value());
    }
    None
}

#[inline(always)]
unsafe fn channel_try_recv(handle: i64) -> Option<i64> {
    // SAFETY: `handle` is validated by `with_channel` before borrowing the channel.
    unsafe { with_channel(handle, channel_try_recv_impl) }.flatten()
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_recv(handle: i64) -> i64 {
    // SAFETY: Preconditions forwarded from caller.
    optional_from(unsafe { channel_recv(handle) })
}

/// `pith_channel_recv` as a register pair; see `optional_pair`.
#[no_mangle]
pub unsafe extern "C" fn pith_channel_recv_pair(handle: i64) -> ResultPair {
    // SAFETY: Preconditions forwarded from caller.
    optional_pair(unsafe { channel_recv(handle) })
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_try_recv(handle: i64) -> i64 {
    // SAFETY: Preconditions forwarded from caller.
    optional_from(unsafe { channel_try_recv(handle) })
}

/// `pith_channel_try_recv` as a register pair; see `optional_pair`.
#[no_mangle]
pub unsafe extern "C" fn pith_channel_try_recv_pair(handle: i64) -> ResultPair {
    // SAFETY: Preconditions forwarded from caller.
    optional_pair(unsafe { channel_try_recv(handle) })
}

#[inline(always)]
fn channel_close_impl(t: &TaggedChannel) -> i64 {
    let inner = &t.inner;
    let mut state = lock_state(&inner.state);
    if state.closed {
        return 0;
    }
    state.closed = true;
    // seal the lock-free ring atomically before publishing `closed_flag` and
    // waking waiters, so no racing `try_enqueue` can succeed after `close()`.
    if let Some(ring) = &inner.ring {
        ring.close();
    }
    // publish close to the lock-free recv path before the wakes below, so a
    // woken buffered waiter's re-check observes it.
    inner.closed_flag.store(true, Ordering::SeqCst);
    // only discard a blocking sender's deposit (the sender wakes with 0 and
    // retains ownership); a detached `try_send` deposit already transferred
    // ownership to the channel and is either drained by a receiver or released
    // in `drain_channel_elements`.
    if matches!(state.pending_value, Some(PendingValue::Blocking(_))) {
        state.pending_value = None;
    }
    // wake every parked caller of both roles so they resume and observe `closed`,
    // the one notify that legitimately targets both sides. notify_all rather than
    // notify_one because there is no bound on how many are parked.
    inner.senders.notify_all();
    inner.receivers.notify_all();
    wake_green(&mut state, Role::Sender);
    wake_green(&mut state, Role::Receiver);
    crate::perf_count(&crate::PERF_CHANNEL_CLOSES, 1);
    1
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_close(handle: i64) -> i64 {
    // an unknown or freed handle reports false, the same as a second close.
    // SAFETY: `handle` is validated by `with_channel` before borrowing the channel.
    unsafe { with_channel(handle, channel_close_impl) }.unwrap_or(0)
}

#[inline(always)]
fn channel_len_impl(t: &TaggedChannel) -> i64 {
    let channel = &t.inner;
    if let Some(ring) = &channel.ring {
        return ring.len() as i64;
    }
    // unbuffered: the only value "in" the channel is a rendezvous deposit
    // waiting to be taken.
    let state = lock_state(&channel.state);
    if state.pending_value.is_some() {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_len(handle: i64) -> i64 {
    // an unknown or freed handle has length 0.
    // SAFETY: `handle` is validated by `with_channel` before borrowing the channel.
    unsafe { with_channel(handle, channel_len_impl) }.unwrap_or(0)
}

#[inline(always)]
fn channel_cap_impl(t: &TaggedChannel) -> i64 {
    t.capacity as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_cap(handle: i64) -> i64 {
    // SAFETY: `handle` is validated by `with_channel` before borrowing the channel.
    unsafe { with_channel(handle, channel_cap_impl) }.unwrap_or(0)
}

#[inline(always)]
fn channel_is_closed_impl(t: &TaggedChannel) -> i64 {
    i64::from(t.inner.closed_flag.load(Ordering::Acquire))
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_is_closed(handle: i64) -> i64 {
    // an unknown or freed handle reports closed, by convention.
    // SAFETY: `handle` is validated by `with_channel` before borrowing the channel.
    unsafe { with_channel(handle, channel_is_closed_impl) }.unwrap_or(1)
}

#[no_mangle]
pub extern "C" fn pith_select_next_index(count: i64) -> i64 {
    if count <= 1 {
        return 0;
    }
    let next = SELECT_COUNTER.fetch_add(1, Ordering::Relaxed);
    next.rem_euclid(count)
}

