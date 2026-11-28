//! Channel handle layout, refcounting, magic validation, and teardown.

use super::park::lock_state;
use super::ring::{Ring, Slot};
use crate::handle_registry;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

/// An in-flight unbuffered rendezvous deposit.
/// - `Blocking(v)`: deposited by a parked `channel_send_impl`; if `close()` runs
///   before delivery, `close()` clears it so the sender wakes with failure (0)
///   and retains ownership of `v`.
/// - `Detached(v)`: deposited by `channel_try_send_impl` which already returned
///   success (1), transferring ownership of `v` to the channel; `close()` leaves
///   it intact so a receiver can drain it or `drain_channel_elements` releases
///   it on channel free (preventing leaks).
#[derive(Clone, Copy)]
pub(super) enum PendingValue {
    Blocking(i64),
    Detached(i64),
}

impl PendingValue {
    #[inline(always)]
    pub(super) fn value(self) -> i64 {
        match self {
            PendingValue::Blocking(v) | PendingValue::Detached(v) => v,
        }
    }
}

pub(super) struct ChannelState {
    pub(super) capacity: usize,
    pub(super) closed: bool,
    pub(super) pending_value: Option<PendingValue>,
    /// Rendezvous handoffs completed on this channel. A parked unbuffered
    /// sender records this before it waits and compares afterwards: close()
    /// clears `pending_value` the same way a receiver taking it does, so
    /// without the counter a sender whose value *was* delivered, and which then
    /// woke to find the channel closed, reported failure. The caller would
    /// reasonably resend, and the value would be delivered twice.
    pub(super) deliveries: u64,
    pub(super) receiver_waiting: usize,
    pub(super) sender_waiting: usize,
    /// how many of the waiters counted above are *os-thread* waiters, parked in
    /// `cvar.wait`, maintained by `block_on_channel` under this lock. a wake
    /// only signals a role's condvar when this says an os-thread waiter is
    /// actually parked there: std's futex-based condvar pays an unconditional
    /// `futex(FUTEX_WAKE)` syscall on every notify, even with nobody waiting,
    /// and on an all-green channel that made every single wake a syscall, the
    /// dominant cost of the whole pipeline. green waiters are woken through
    /// their scheduler instead, no syscall involved.
    pub(super) os_receivers_waiting: usize,
    pub(super) os_senders_waiting: usize,
    /// ids of green *receivers* suspended on this channel, and of green
    /// *senders*, kept apart so a notify wakes only the opposite role (see the
    /// role-split note in the module header and on `wake_green`). closing the
    /// channel wakes both lists. empty (and so free) whenever no green task is
    /// parked here.
    pub(super) green_receivers: Vec<usize>,
    pub(super) green_senders: Vec<usize>,
}

impl ChannelState {
    /// the green waiter list for `role`.
    pub(super) fn green_waiters(&mut self, role: Role) -> &mut Vec<usize> {
        match role {
            Role::Receiver => &mut self.green_receivers,
            Role::Sender => &mut self.green_senders,
        }
    }
}

/// which side of the channel a green task is blocked on. a parking task registers
/// under its role so a later notify can wake only the opposite role (see
/// `green_receivers`/`green_senders`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    Sender,
    Receiver,
}

/// the channel behind a handle. a **buffered** channel carries its values in
/// the lock-free `ring`; the mutex/condvars exist only for parking and waking,
/// which an op touches only when the ring is full/empty or a `parked_*` gate
/// says someone is waiting. an **unbuffered** channel (`ring` is None) is a
/// rendezvous carried entirely under the mutex. `closed_flag` mirrors
/// `state.closed` so the lock-free path can observe close without the lock.
/// `parked_senders`/`parked_receivers` count every parked waiter of a role
/// (os-thread and green alike) and are maintained under the mutex but read
/// without it (the wake gate); the seq-cst fences in the fast paths close the
/// park/wake race (see `buffered_send`).
pub(super) struct ChannelInner {
    pub(super) ring: Option<Ring>,
    /// the requested capacity, fixed at construction. kept here rather than read
    /// back out of `state` because the buffered send path needs it on every
    /// message: taking the parking-lot mutex for an immutable value put a lock
    /// acquisition back on the path the ring exists to keep lock-free.
    pub(super) capacity: usize,
    pub(super) closed_flag: AtomicBool,
    pub(super) parked_senders: AtomicUsize,
    pub(super) parked_receivers: AtomicUsize,
    pub(super) state: Mutex<ChannelState>,
    pub(super) senders: Condvar,
    pub(super) receivers: Condvar,
}

/// "PCHA": the magic word at the front of every channel allocation. a channel
/// handle is validated by alignment + this tag rather than the global handle
/// registry: the registry's mutex was two global lock round trips per message
/// (send + recv), the hottest shared line left once the ring made the channel
/// itself lock-free. the word is scrubbed when the channel is freed, so a
/// stale handle fails the check with high probability and is answered like a
/// garbage one. a live handle can only be stale if the language miscounted
/// it, and that is the same practical guard every other counted kind (list,
/// closure, struct) relies on.
pub(super) const CHANNEL_MAGIC: u32 = 0x50434841;

/// the largest buffered capacity a channel will allocate (16Mi values, 128 MiB
/// of slots). the ring is eager, so an unbounded capacity turns a typo into an
/// allocation abort; clamping keeps the failure mode "smaller buffer than you
/// asked for" instead of "process dies".
pub(super) const MAX_CHANNEL_CAPACITY: i64 = 1 << 24;

/// a channel is one allocation, owned by its count. the language retains a
/// handle on every copy and releases it at every scope exit, exactly as it does
/// for a string or a list, and the channel is freed when the count reaches
/// zero. an operation in flight is executed by a frame that holds a count for
/// the handle it passed (a binding, a field of a live struct, an element of a
/// live container, a capture of a live closure), so nothing can free the body
/// under it, and the fast path pays nothing: alignment check, magic check, and
/// the operation.
///
/// the one exception is a park. a caller may have borrowed its handle from a
/// holder another task owns (a channel in a shared struct, the sanctioned way
/// to share one), and if that task releases the holder while this one sleeps,
/// the last count could go while a suspended call still points into the body.
/// so a parking operation takes a count of its own before it blocks and gives
/// it back when the call completes (`ParkCount`), off the per-message path.
///
/// this replaced a permanent stub, per-thread hazard slots, and a limbo list
/// that let the runtime discover for itself when a channel could be freed:
/// measured at about 14% of os-thread channel throughput, all of it in the
/// per-operation guard. stale handles are caught by the magic word (see
/// `CHANNEL_MAGIC`).
#[repr(C)]
pub(super) struct TaggedChannel {
    pub(super) magic: u32,
    /// the element-tag code of the payload type (the same table lists use),
    /// recorded at construction because only the constructor site knows T. a
    /// free with values still queued releases each by this tag; 0 means a plain
    /// value with nothing to release.
    pub(super) payload_tag: u8,
    /// the requested capacity, fixed at construction.
    pub(super) capacity: u32,
    /// owners: one per binding, field, element, or capture holding the handle
    /// (`pith_channel_retain` / `pith_channel_release`), plus one per parked
    /// operation for the span of its call (`ParkCount`). the fast path never
    /// writes this; copies and parks do. it shares the header line with the
    /// magic word deliberately: padding it onto a line of its own was measured
    /// and read worse, not better, on os-thread fanout.
    pub(super) refs: AtomicUsize,
    pub(super) inner: ChannelInner,
}

/// Validate `handle` as a `TaggedChannel` for `op` and borrow it for the
/// duration of `f`. An aligned pointer whose first word is `CHANNEL_MAGIC` is
/// accepted; a freed channel had its magic scrubbed, so a stale handle is
/// rejected here like a garbage one.
///
/// # Safety
/// `handle` must be a live channel handle kept alive for `f`, or garbage
/// rejected by alignment/magic.
#[inline(always)]
pub(super) unsafe fn with_channel_for<R>(
    handle: i64,
    op: crate::quarantine::Op,
    f: impl FnOnce(&TaggedChannel) -> R,
) -> Option<R> {
    let ptr = handle as *const TaggedChannel;
    if !handle_registry::plausibly_aligned::<TaggedChannel>(ptr as *const ()) {
        return None;
    }
    // SAFETY: Alignment verified; `magic_is` validates the magic word before dereferencing.
    let valid = unsafe {
        crate::quarantine::magic_is(std::ptr::addr_of!((*ptr).magic), CHANNEL_MAGIC, op)
    };
    if !valid {
        return None;
    }
    // SAFETY: Magic word matched `CHANNEL_MAGIC`; handle stays live for `f`.
    let channel = unsafe { &*ptr };
    Some(f(channel))
}

/// Validate `handle` as a `TaggedChannel` for ordinary use and borrow it for
/// the duration of `f`.
///
/// # Safety
/// Same requirements as [`with_channel_for`].
#[inline(always)]
pub(super) unsafe fn with_channel<R>(handle: i64, f: impl FnOnce(&TaggedChannel) -> R) -> Option<R> {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { with_channel_for(handle, crate::quarantine::Op::Use, f) }
}

#[cfg(test)]
pub(super) static TEST_FREES: AtomicUsize = AtomicUsize::new(0);

/// the count an operation takes for itself when it is about to park, so the
/// body outlives the suspended call even if every language owner releases it
/// meanwhile. declared as the FIRST local of every entry point that can block:
/// rust drops locals in reverse order, so this releases last: after the state
/// guard, after the post-wake `parked_*` bookkeeping, after `take_handoff`, and
/// after the return value has been built. that declaration order is what
/// guarantees every post-wake read of the body happens before a free that this
/// release might perform. do not move it.
pub(super) struct ParkCount {
    t: *const TaggedChannel,
    taken: Cell<bool>,
}

impl ParkCount {
    pub(super) fn new(t: &TaggedChannel) -> ParkCount {
        ParkCount {
            t: t as *const TaggedChannel,
            taken: Cell::new(false),
        }
    }

    /// idempotent: an unbuffered loop that parks repeatedly pays one count.
    #[inline]
    pub(super) fn take(&self) {
        if !self.taken.get() {
            // SAFETY: Caller's handle keeps `self.t` live when `take` is called.
            unsafe { &*self.t }.refs.fetch_add(1, Ordering::Relaxed);
            self.taken.set(true);
        }
    }
}

impl Drop for ParkCount {
    fn drop(&mut self) {
        if self.taken.get() {
            let should_free = {
                // SAFETY: `ParkCount` holds a refcount keeping `self.t` live through `dec_count`.
                let t = unsafe { &*self.t };
                dec_count(t)
            };
            if should_free {
                // SAFETY: `dec_count` released the last count and no reference to `self.t` remains.
                unsafe { free_channel(self.t as *mut TaggedChannel) };
            }
        }
    }
}

/// Decrement one owner count. Returns `true` if the last owner was just
/// released and the caller must free the channel.
#[inline(always)]
pub(super) fn dec_count(t: &TaggedChannel) -> bool {
    let prev = t.refs.fetch_sub(1, Ordering::Release);
    if prev > 1 {
        return false;
    }
    if prev == 0 {
        crate::quarantine::over_release(crate::quarantine::Kind::Channel);
        // an over-release: put the count back rather than wrap. the same guard
        // every other kind's release has.
        t.refs.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    acquire_last_count!(t.refs);
    true
}

/// Release any queued non-primitive payloads on a quiescent channel being freed.
pub(super) fn drain_channel_elements(t: &TaggedChannel) {
    let tag = crate::collections::list::element_tag_from_code(t.payload_tag as i32);
    if matches!(tag, crate::collections::list::ListTypeTag::Primitive) {
        return;
    }
    if let Some(ring) = &t.inner.ring {
        while let Some(v) = ring.try_dequeue() {
            // SAFETY: Channel owned one refcount on each queued payload matching `tag`.
            unsafe { crate::collections::list::release_element(tag, v) };
        }
    }
    if let Some(p) = lock_state(&t.inner.state).pending_value.take() {
        // SAFETY: Channel owned one refcount on the pending payload matching `tag`.
        unsafe { crate::collections::list::release_element(tag, p.value()) };
    }
}

/// nothing names the body any more (no owner, no parked call), so the ring is
/// quiescent and the drain is exact. values a program never received are
/// released by the payload tag; a payload that is itself a channel releases
/// through this same path, which holds no lock, so that is fine.
pub(super) unsafe fn free_channel(p: *mut TaggedChannel) {
    let bytes = {
        // SAFETY: Caller guarantees `p` is quiescent with no remaining owners.
        let t = unsafe { &*p };
        drain_channel_elements(t);
        channel_bytes(t.capacity as usize)
    };
    // a stale handle must fail validation from here on
    // SAFETY: No references to `*p` remain; `p` came from `Box::into_raw`.
    unsafe {
        (*p).magic = 0;
        let magic = std::ptr::addr_of_mut!((*p).magic);
        crate::quarantine::free_box(p, magic, crate::quarantine::Kind::Channel);
    }
    crate::perf_count(&crate::PERF_CHANNEL_FREES, 1);
    crate::perf_count(&crate::PERF_CHANNEL_FREED_BYTES, bytes);
    #[cfg(test)]
    TEST_FREES.fetch_add(1, Ordering::Relaxed);
}
#[inline(always)]
pub(super) fn retain_channel(t: &TaggedChannel) {
    t.refs.fetch_add(1, Ordering::Relaxed);
}

/// One more owner of this handle: a binding, a field, an element, or a
/// capture took a copy. Garbage handles are ignored, like every other kind.
#[no_mangle]
pub unsafe extern "C" fn pith_channel_retain(handle: i64) {
    // SAFETY: `handle` is validated by `with_channel_for` before borrowing its refcount.
    unsafe { with_channel_for(handle, crate::quarantine::Op::Retain, retain_channel) };
}

/// One owner fewer; the last owner frees the channel, releasing any values
/// still queued by the payload tag.
#[no_mangle]
pub unsafe extern "C" fn pith_channel_release(handle: i64) {
    // SAFETY: `handle` is validated by `with_channel_for` before borrowing its refcount.
    if unsafe { with_channel_for(handle, crate::quarantine::Op::Release, dec_count) } == Some(true)
    {
        // SAFETY: `dec_count` released the last count and the borrow of `TaggedChannel` has ended.
        unsafe { free_channel(handle as *mut TaggedChannel) };
    }
}

/// every byte one channel asks the allocator for: the channel and its ring's
/// slots. allocator rounding is on top of this, so the number under-reports
/// the real resident cost rather than inflating it.
pub(super) fn channel_bytes(capacity: usize) -> usize {
    let ring = if capacity > 0 {
        Ring::slot_count(capacity) * std::mem::size_of::<Slot>()
    } else {
        0
    };
    std::mem::size_of::<TaggedChannel>() + ring
}
