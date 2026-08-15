//! channel support for task communication
//!
//! a channel serves two kinds of blocked caller at once. an **os-thread** caller
//! (the main thread, or the whole os-thread task backend) that would block waits
//! on a channel `Condvar` *for its role*, exactly as it always has. a **green
//! task** (under `PITH_GREEN`) must not park its worker OS thread — instead it
//! registers itself on the channel's green-waiter list *for its role* (sender or
//! receiver) and suspends its coroutine back to the scheduler, freeing the worker
//! to run other tasks; a later send/recv re-enqueues it.
//!
//! ## role-split wakes (why two condvars)
//!
//! every state change wakes only the *opposite* role: a send wakes receivers, a
//! recv wakes senders. two things fall out of this:
//!
//!  * os-thread waiters park on one of two condvars — `senders` or `receivers` —
//!    so a "value available" notify can target receivers alone. with a single
//!    shared condvar a notify meant for a receiver could wake a parked sender,
//!    which re-blocks while the receiver is never woken; splitting the condvars
//!    makes that impossible. for a **buffered** channel one value unblocks exactly
//!    one receiver, so we `notify_one` rather than `notify_all` — waking all N so
//!    N-1 find nothing and re-block is the thundering herd that dominated the
//!    profile. an **unbuffered** channel is a rendezvous whose senders are not
//!    interchangeable, so it broadcasts; see `notify_role`.
//!  * green tasks are woken the same way: only the opposite role's list. waking a
//!    same-role sibling would only ever wake a task that still cannot proceed —
//!    harmless for a one-shot os-thread condvar wait, but under the green backend
//!    that sibling re-parks and re-notifies, so with two+ same-role green tasks on
//!    one worker they wake each other in a tight loop that starves everything else
//!    (see the role note on `wake_green`).
//!
//! notify is *skipped entirely* when the opposite role has no waiter
//! (`receiver_waiting`/`sender_waiting` is 0): a running peer picks the value or
//! slot up on its own next pass, no syscall needed. closing the channel is the one
//! notify that legitimately targets both roles at once — it `notify_all`s both
//! condvars and wakes both green lists so every parked caller resumes and observes
//! `closed`.
//!
//! ## lock order
//!
//! when the channel lock and the scheduler's locks nest, the channel lock is
//! always the outer one: `wake_green` runs while holding the channel lock
//! and calls `green::wake`, which takes the slab lock and then a queue lock —
//! channel -> slab -> queue. a parking green task does the reverse-free thing: it
//! *releases* the channel lock before it suspends (see `block_on_channel`),
//! because `suspend` hands control to the worker, which may re-enter this same
//! channel. so we never hold the channel lock across a suspend, and never take
//! the channel lock while holding a scheduler lock.
//!
//! ## the wake/park race
//!
//! a value can arrive between a green task's would-block check and its suspend.
//! that window is closed on the scheduler side: `green::wake` sees the task still
//! `Running` (not yet `Parked`) and records a `wake_pending` flag that the park
//! path re-checks, so the wake is never lost. here we just have to register the
//! waiter *under the channel lock* before releasing it, which we do.

use crate::concurrency::green;
use crate::handle_registry::{self, HandleKind};
use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

static SELECT_COUNTER: AtomicI64 = AtomicI64::new(0);

/// one slot of the lock-free buffered queue. `sequence` is the vyukov ticket:
/// a slot is free for the enqueuer holding position `pos` when its sequence is
/// exactly `pos`, and holds a value for the dequeuer at `pos` when its sequence
/// is `pos + 1`. after a dequeue the slot's sequence advances by the ring size,
/// handing it to the enqueuer one lap later. the sequence's release store is
/// what publishes the value in `cell`.
struct Slot {
    sequence: AtomicUsize,
    cell: UnsafeCell<i64>,
}

/// the lock-free core of a buffered channel: a fixed ring of slots with two
/// monotonically increasing positions. `try_enqueue`/`try_dequeue` are the
/// classic vyukov bounded mpmc — pure cas, no lock — and they alone carry the
/// throughput path. blocking, waking, green-task parking, and close all stay on
/// the channel mutex, which an op only touches when the ring is actually full
/// or empty (or when a gate says someone is parked).
/// a cache-line padded atomic position. senders hammer `enqueue_pos` and
/// receivers `dequeue_pos`; on one line they false-share and every CAS bounces
/// the line between roles.
#[repr(align(64))]
struct PaddedPos(AtomicUsize);

struct Ring {
    buffer: Box<[Slot]>,
    mask: usize,
    /// true when the requested capacity is a power of two, i.e. the ring size
    /// equals the capacity exactly: fullness is then fully encoded in the slot
    /// sequences and `try_enqueue` never needs to read `dequeue_pos` — which
    /// keeps senders off the receivers' cache line entirely.
    exact: bool,
    enqueue_pos: PaddedPos,
    dequeue_pos: PaddedPos,
}

unsafe impl Send for Ring {}
unsafe impl Sync for Ring {}

impl Ring {
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
    fn new(capacity: usize) -> Ring {
        let size = capacity.next_power_of_two().max(2);
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

    /// how many values are in the ring right now. approximate under concurrency
    /// (positions move independently), exact when the channel is quiescent.
    fn len(&self) -> usize {
        let enq = self.enqueue_pos.0.load(Ordering::Acquire);
        let deq = self.dequeue_pos.0.load(Ordering::Acquire);
        enq.saturating_sub(deq)
    }

    /// lock-free enqueue: false when the channel is at capacity.
    fn try_enqueue(&self, value: i64, capacity: usize) -> bool {
        let mut pos = self.enqueue_pos.0.load(Ordering::Relaxed);
        loop {
            // a non-power-of-two capacity leaves spare ring slots, so fullness
            // is not encoded in the slot sequences alone: enforce it here. when
            // the capacity is exact this check (and its read of the receivers'
            // cache line) disappears.
            if !self.exact
                && pos.saturating_sub(self.dequeue_pos.0.load(Ordering::Acquire)) >= capacity
            {
                return false;
            }
            let slot = &self.buffer[pos & self.mask];
            let seq = slot.sequence.load(Ordering::Acquire);
            if seq == pos {
                // slot free for this position: claim it by advancing enqueue_pos.
                match self.enqueue_pos.0.compare_exchange_weak(
                    pos,
                    pos + 1,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        unsafe { *slot.cell.get() = value };
                        // release-publish the value to the dequeuer at this pos.
                        slot.sequence.store(pos + 1, Ordering::Release);
                        return true;
                    }
                    Err(actual) => pos = actual,
                }
            } else if seq < pos {
                // the slot still holds a value from the previous lap: full.
                return false;
            } else {
                // another enqueuer already claimed this position; catch up.
                pos = self.enqueue_pos.0.load(Ordering::Relaxed);
            }
        }
    }

    /// lock-free dequeue: None when the channel is empty.
    fn try_dequeue(&self) -> Option<i64> {
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
                        let value = unsafe { *slot.cell.get() };
                        // hand the slot to the enqueuer one lap later.
                        slot.sequence
                            .store(pos + self.mask + 1, Ordering::Release);
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

struct ChannelState {
    capacity: usize,
    closed: bool,
    pending_value: Option<i64>,
    /// Rendezvous handoffs completed on this channel. A parked unbuffered
    /// sender records this before it waits and compares afterwards: close()
    /// clears `pending_value` the same way a receiver taking it does, so
    /// without the counter a sender whose value *was* delivered, and which then
    /// woke to find the channel closed, reported failure. The caller would
    /// reasonably resend, and the value would be delivered twice.
    deliveries: u64,
    receiver_waiting: usize,
    sender_waiting: usize,
    /// how many of the waiters counted above are *os-thread* waiters, parked in
    /// `cvar.wait` — maintained by `block_on_channel` under this lock. a wake
    /// only signals a role's condvar when this says an os-thread waiter is
    /// actually parked there: std's futex-based condvar pays an unconditional
    /// `futex(FUTEX_WAKE)` syscall on every notify, even with nobody waiting,
    /// and on an all-green channel that made every single wake a syscall — the
    /// dominant cost of the whole pipeline. green waiters are woken through
    /// their scheduler instead, no syscall involved.
    os_receivers_waiting: usize,
    os_senders_waiting: usize,
    /// slab ids of green *receivers* suspended on this channel, and of green
    /// *senders*, kept apart. a notify only ever needs to wake the opposite role:
    /// a receiver becomes runnable when a sender makes room or deposits a value,
    /// and vice versa. waking a same-role sibling would only ever wake a task that
    /// still cannot proceed — harmless for a one-shot os-thread condvar wait, but
    /// under the green backend that sibling re-parks and re-notifies, so with two+
    /// same-role green tasks on one worker they wake each other in a tight loop
    /// that starves everything else (see the role note on `wake_green`). closing
    /// the channel wakes both lists. empty (and so free) whenever no green task is
    /// parked here — the os-thread path is untouched.
    green_receivers: Vec<usize>,
    green_senders: Vec<usize>,
}

/// which side of the channel a green task is blocked on. a parking task registers
/// under its role so a later notify can wake only the opposite role (see
/// `green_receivers`/`green_senders`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Sender,
    Receiver,
}

/// the channel behind a handle. a **buffered** channel carries its values in
/// the lock-free `ring`; the mutex/condvars exist only for parking and waking,
/// which an op touches only when the ring is full/empty or a `parked_*` gate
/// says someone is waiting. an **unbuffered** channel (`ring` is None) is the
/// original mutex rendezvous, untouched. `closed_flag` mirrors `state.closed`
/// so the lock-free path can observe close without the lock. `parked_senders`/
/// `parked_receivers` count every parked waiter of a role — os-thread and green
/// alike — and are maintained under the mutex but read without it (the wake
/// gate); the seq-cst fences in the fast paths close the park/wake race (see
/// `buffered_send`).
struct ChannelInner {
    ring: Option<Ring>,
    /// the requested capacity, fixed at construction. kept here rather than read
    /// back out of `state` because the buffered send path needs it on every
    /// message: taking the parking-lot mutex for an immutable value put a lock
    /// acquisition back on the path the ring exists to keep lock-free.
    capacity: usize,
    closed_flag: AtomicBool,
    parked_senders: AtomicUsize,
    parked_receivers: AtomicUsize,
    state: Mutex<ChannelState>,
    senders: Condvar,
    receivers: Condvar,
}

type PithChannelHandle = Arc<ChannelInner>;

/// "PCHA": the magic word at the front of every channel allocation. a channel
/// handle is validated by alignment + this tag rather than the global handle
/// registry — the registry's mutex was two global lock round trips per message
/// (send + recv), the hottest shared line left once the ring made the channel
/// itself lock-free. channels are never freed, so the tag never needs scrubbing;
/// garbage handles fail the alignment or magic check and read as invalid.
const CHANNEL_MAGIC: u32 = 0x50434841;

/// the largest buffered capacity a channel will allocate (16Mi values, 128 MiB
/// of slots). the ring is eager, so an unbounded capacity turns a typo into an
/// allocation abort; clamping keeps the failure mode "smaller buffer than you
/// asked for" instead of "process dies".
const MAX_CHANNEL_CAPACITY: i64 = 1 << 24;

/// a channel allocation: the magic tag, then the shared handle.
struct TaggedChannel {
    magic: u32,
    channel: PithChannelHandle,
}

unsafe fn channel_ref<'a>(handle: i64) -> Option<&'a PithChannelHandle> {
    let ptr = handle as *const TaggedChannel;
    if !handle_registry::plausibly_aligned::<TaggedChannel>(ptr as *const ()) {
        return None;
    }
    if (*ptr).magic != CHANNEL_MAGIC {
        return None;
    }
    Some(&(*ptr).channel)
}

fn lock_state(lock: &Mutex<ChannelState>) -> MutexGuard<'_, ChannelState> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// wake the green tasks parked on this channel in the given role so each
/// re-checks the condition it blocked on. the os-thread half of a wake is a
/// `notify_one` on the same role's condvar (see `wake_receivers`/`wake_senders`);
/// this drains the green side.
///
/// `role` says which side to wake, and it is always the *opposite* of whoever is
/// notifying: a send wakes receivers, a recv wakes senders, a close wakes both.
/// waking only the opposite role is what keeps the green backend from live-
/// locking — two receivers (or two senders) parked on one worker must never wake
/// each other, because the woken sibling cannot make progress and would just
/// re-park and re-notify forever. over-waking *within* the opposite role is still
/// safe: a woken task re-evaluates under the channel lock and re-parks if it
/// still cannot proceed, the same tolerance the condvar loops rely on.
///
/// called while holding the channel lock; `green::wake` nests the slab and queue
/// locks under it (see the lock-order note above).
fn wake_green(state: &mut ChannelState, role: Role) {
    // drain first: a woken task that re-parks will re-add itself, and draining
    // keeps a task from being enqueued twice for one notify.
    let list = match role {
        Role::Receiver => &mut state.green_receivers,
        Role::Sender => &mut state.green_senders,
    };
    if list.is_empty() {
        return;
    }
    for id in std::mem::take(list) {
        green::wake(id);
    }
}

/// notify the os-thread waiters parked on one role's condvar, skipping the
/// syscall entirely when none are parked (`waiting` is the role's *os-thread*
/// waiter count — green waiters are woken through their scheduler and must not
/// trigger a condvar futex; see `os_receivers_waiting`).
///
/// a **buffered** channel hands off one value or one freed slot at a time, and
/// any parked waiter of a role wants the same thing (a receiver wants a value, a
/// sender wants room), so exactly one can proceed — `notify_one`. that is the
/// thundering-herd fix: the old `notify_all` woke all N, N-1 of which re-blocked.
///
/// an **unbuffered** channel is a rendezvous, and its senders are *not*
/// interchangeable: one is waiting to deposit, another is waiting for the value
/// it already deposited to be taken. a recv that takes a value must wake that
/// specific depositing sender, which a shared condvar cannot target by
/// `notify_one`, so it broadcasts. this matches the green backend, which always
/// drains a role's whole waiter list; over-waking within a role is safe because a
/// woken os-thread waiter re-checks under the lock and re-parks if it still cannot
/// proceed. unbuffered channels are handshakes, not throughput paths, so the
/// broadcast here is not the herd the buffered path cares about.
fn notify_role(cvar: &Condvar, capacity: usize, waiting: usize) {
    if waiting == 0 {
        return;
    }
    if capacity == 0 {
        cvar.notify_all();
    } else {
        cvar.notify_one();
    }
}

/// wake the receivers parked on this channel (opposite role of a send): one
/// os-thread receiver if buffered, all if a rendezvous, plus any green receivers.
fn wake_receivers(inner: &ChannelInner, state: &mut ChannelState) {
    notify_role(&inner.receivers, state.capacity, state.os_receivers_waiting);
    wake_green(state, Role::Receiver);
}

/// wake the senders parked on this channel (opposite role of a recv): symmetric
/// to `wake_receivers`.
fn wake_senders(inner: &ChannelInner, state: &mut ChannelState) {
    notify_role(&inner.senders, state.capacity, state.os_senders_waiting);
    wake_green(state, Role::Sender);
}

/// block the current caller until the next notify for its role, returning the
/// re-acquired guard. an os-thread caller waits on its role's condvar. a green
/// task registers itself on its role's green-waiter list (under the channel
/// lock), releases the lock, and suspends its coroutine back to the scheduler; it
/// re-locks and continues its loop when a later send/recv wakes it.
fn block_on_channel<'a>(
    inner: &'a ChannelInner,
    mut state: MutexGuard<'a, ChannelState>,
    green_task: Option<usize>,
    role: Role,
) -> MutexGuard<'a, ChannelState> {
    match green_task {
        None => {
            // count this os-thread waiter under the channel lock before
            // waiting, so a waker holding the same lock knows a condvar notify
            // is worth its futex syscall (see `os_receivers_waiting`).
            let cvar = match role {
                Role::Sender => {
                    state.os_senders_waiting += 1;
                    &inner.senders
                }
                Role::Receiver => {
                    state.os_receivers_waiting += 1;
                    &inner.receivers
                }
            };
            // the wait is a native window for the cycle collector: the thread
            // reads no heap handle until the condvar hands the lock back. the
            // bracket's exit runs while holding the re-acquired channel lock —
            // safe, because a stop that parks us there stops only mutators,
            // and the collection pass itself never takes a channel lock.
            let mut state = {
                let _native = crate::cycle::native_bracket();
                cvar.wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
            };
            match role {
                Role::Sender => state.os_senders_waiting -= 1,
                Role::Receiver => state.os_receivers_waiting -= 1,
            }
            state
        }
        Some(id) => {
            // register under the lock so a concurrent notify cannot miss us, then
            // release the lock *before* suspending — suspend returns control to
            // the worker, which may touch this same channel. registering under our
            // role lets the opposite side wake us without waking same-role peers.
            match role {
                Role::Receiver => state.green_receivers.push(id),
                Role::Sender => state.green_senders.push(id),
            }
            drop(state);
            green::park_current();
            lock_state(&inner.state)
        }
    }
}

fn optional_tuple(is_some: bool, value: i64) -> i64 {
    unsafe {
        let tuple = crate::pith_struct_alloc(2);
        if tuple == 0 {
            return 0;
        }
        let ptr = tuple as *mut i64;
        *ptr = if is_some { 1 } else { 0 };
        *ptr.add(1) = value;
        tuple
    }
}

#[no_mangle]
pub extern "C" fn pith_channel_new(capacity: i64) -> i64 {
    // the ring is allocated up front, so an absurd capacity would abort the
    // process on the allocation rather than fail the call. cap it: past this
    // many buffered values a program wants a queue it manages itself.
    let cap = capacity.max(0).min(MAX_CHANNEL_CAPACITY) as usize;
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
    let channel = Arc::new(ChannelInner {
        ring: if cap > 0 { Some(Ring::new(cap)) } else { None },
        capacity: cap,
        closed_flag: AtomicBool::new(false),
        parked_senders: AtomicUsize::new(0),
        parked_receivers: AtomicUsize::new(0),
        state: Mutex::new(state),
        senders: Condvar::new(),
        receivers: Condvar::new(),
    });
    let ptr = Box::into_raw(Box::new(TaggedChannel {
        magic: CHANNEL_MAGIC,
        channel,
    }));
    ptr as i64
}

/// after a successful ring op, wake one parked waiter of the opposite `role` if
/// the gate says any exist. the seq-cst fence orders our ring release-store
/// before the gate load, pairing with the fence a parking waiter issues between
/// its gate increment and its ring re-try — so either we see its increment here,
/// or its re-try sees our value (never both misses; see `buffered_send`).
///
/// a parked green *receiver* gets a direct handoff: one is popped and the ring
/// dequeued on its behalf under the claim (see `green::wake_with`), so it
/// resumes with the value in hand instead of re-locking and re-trying. dequeuing
/// through the ring — never bypassing it — is what keeps fifo order. everything
/// else (os-thread waiters, green senders) gets the plain wake-and-re-check.
fn wake_parked(inner: &ChannelInner, ring: &Ring, role: Role) {
    std::sync::atomic::fence(Ordering::SeqCst);
    let gate = match role {
        Role::Receiver => &inner.parked_receivers,
        Role::Sender => &inner.parked_senders,
    };
    if gate.load(Ordering::Relaxed) == 0 {
        return;
    }
    let mut state = lock_state(&inner.state);
    if role == Role::Receiver {
        if let Some(id) = state.green_receivers.pop() {
            green::wake_with(id, || ring.try_dequeue());
            drop(state);
            // the handoff dequeue freed a slot, so a parked sender may now have
            // room. gated like every wake, and the sender path never hands off,
            // so this cannot recurse.
            wake_parked(inner, ring, Role::Sender);
            return;
        }
    }
    crate::concurrency::green::note_channel_wake();
    // signal the role's condvar only when an os-thread waiter is actually
    // parked in it: std's condvar pays a futex syscall per notify regardless
    // of waiters, and on an all-green channel every message would eat two of
    // them. the count is exact under this lock — an os-thread waiter
    // increments it under the same lock before it waits (block_on_channel),
    // so it either sees our ring value on its pre-wait re-try or is counted
    // here. green waiters go through their scheduler below, no syscall.
    let (cvar, os_waiting) = match role {
        Role::Receiver => (&inner.receivers, state.os_receivers_waiting),
        Role::Sender => (&inner.senders, state.os_senders_waiting),
    };
    if os_waiting > 0 {
        cvar.notify_one();
    }
    wake_green(&mut state, role);
}

/// the buffered send: lock-free enqueue on the fast path; park under the mutex
/// only when the ring is full.
///
/// ## the park/wake race
/// a sender can find the ring full, then a receiver dequeues and checks the gate
/// before the sender registers — a lost wake, and a hang. it is closed the
/// standard way: the parker increments its gate (under the mutex), issues a
/// seq-cst fence, and *re-tries the ring op* before waiting; the waker issues a
/// seq-cst fence between its ring op and its gate load. in the fence order,
/// whichever fence comes second sees the other side's write — the waker sees the
/// incremented gate (and notifies under the mutex, which the parker holds until
/// it actually waits), or the parker's re-try sees the freed slot.
/// how many failed ring attempts an os-thread caller burns before parking. a
/// paired producer/consumer usually frees a slot within a few hundred cycles,
/// and a park/unpark round trip through the kernel costs microseconds — a short
/// spin converts most would-parks into immediate retries. green tasks never
/// spin: their park is a userspace suspend, already cheap.
const SPIN_TRIES: usize = 64;

unsafe fn buffered_send(inner: &ChannelInner, ring: &Ring, value: i64) -> i64 {
    let capacity = inner.capacity;
    let green_task = green::current_task();
    let mut spins = if green_task.is_none() { SPIN_TRIES } else { 0 };
    loop {
        if inner.closed_flag.load(Ordering::SeqCst) {
            return 0;
        }
        if ring.try_enqueue(value, capacity) {
            wake_parked(inner, ring, Role::Receiver);
            return 1;
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
        if inner.closed_flag.load(Ordering::SeqCst) {
            inner.parked_senders.fetch_sub(1, Ordering::SeqCst);
            return 0;
        }
        if ring.try_enqueue(value, capacity) {
            inner.parked_senders.fetch_sub(1, Ordering::SeqCst);
            drop(state);
            wake_parked(inner, ring, Role::Receiver);
            return 1;
        }
        state = block_on_channel(inner, state, green_task, Role::Sender);
        drop(state);
        inner.parked_senders.fetch_sub(1, Ordering::SeqCst);
    }
}

/// the buffered recv, mirror of `buffered_send`: lock-free dequeue on the fast
/// path; park under the mutex only when the ring is empty. a closed channel
/// still drains — the dequeue try comes before the closed check.
unsafe fn buffered_recv(inner: &ChannelInner, ring: &Ring) -> i64 {
    let green_task = green::current_task();
    let mut spins = if green_task.is_none() { SPIN_TRIES } else { 0 };
    loop {
        if let Some(value) = ring.try_dequeue() {
            wake_parked(inner, ring, Role::Sender);
            return optional_tuple(true, value);
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
                return optional_tuple(true, value);
            }
            return optional_tuple(false, 0);
        }
        let mut state = lock_state(&inner.state);
        inner.parked_receivers.fetch_add(1, Ordering::SeqCst);
        std::sync::atomic::fence(Ordering::SeqCst);
        if let Some(value) = ring.try_dequeue() {
            inner.parked_receivers.fetch_sub(1, Ordering::SeqCst);
            drop(state);
            wake_parked(inner, ring, Role::Sender);
            return optional_tuple(true, value);
        }
        if inner.closed_flag.load(Ordering::SeqCst) {
            inner.parked_receivers.fetch_sub(1, Ordering::SeqCst);
            return optional_tuple(false, 0);
        }
        state = block_on_channel(inner, state, green_task, Role::Receiver);
        drop(state);
        inner.parked_receivers.fetch_sub(1, Ordering::SeqCst);
        // a green task may have been handed its value by the waker (see
        // wake_parked): the send dequeued on our behalf, so take it and skip the
        // re-try entirely.
        if green_task.is_some() {
            if let Some(value) = green::take_handoff() {
                return optional_tuple(true, value);
            }
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_send(handle: i64, value: i64) -> i64 {
    let Some(channel) = channel_ref(handle) else {
        return 0;
    };
    let inner = &**channel;
    // a buffered channel's values live in the lock-free ring; the mutex is only
    // the parking lot. the unbuffered rendezvous below is untouched.
    if let Some(ring) = &inner.ring {
        return buffered_send(inner, ring, value);
    }
    // is this send running inside a green task? if so it yields on a would-block
    // instead of parking the worker; if not (main thread / os-thread backend) it
    // condvar-waits exactly as before. computed once — the running task does not
    // change under us across a suspend/resume.
    let green_task = green::current_task();
    let mut state = lock_state(&inner.state);

    if state.closed {
        return 0;
    }

    if state.capacity == 0 {
        while !state.closed {
            if state.receiver_waiting > 0 && state.pending_value.is_none() {
                let handoffs_before = state.deliveries;
                state.pending_value = Some(value);
                wake_receivers(inner, &mut state);
                // count ourselves as a waiting sender while we wait for the
                // receiver to take the value: the recv that takes it gates its
                // wake on `sender_waiting`, so an uncounted sender here would
                // never be notified (a hang on the rendezvous handshake).
                state.sender_waiting += 1;
                while !state.closed && state.pending_value.is_some() {
                    state = block_on_channel(inner, state, green_task, Role::Sender);
                }
                state.sender_waiting -= 1;
                // delivered is the authority, not `closed`: a receiver may have
                // taken the value and the channel closed before this sender got
                // the lock back.
                let delivered = state.deliveries != handoffs_before;
                return if delivered { 1 } else { 0 };
            }
            state.sender_waiting += 1;
            state = block_on_channel(inner, state, green_task, Role::Sender);
            state.sender_waiting -= 1;
        }
        return 0;
    }

    // buffered sends never reach here: they took the ring path above.
    0
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_try_send(handle: i64, value: i64) -> i64 {
    let Some(channel) = channel_ref(handle) else {
        return 0;
    };
    let inner = &**channel;
    if let Some(ring) = &inner.ring {
        if inner.closed_flag.load(Ordering::SeqCst) {
            return 0;
        }
        if ring.try_enqueue(value, inner.capacity) {
            wake_parked(inner, ring, Role::Receiver);
            return 1;
        }
        return 0;
    }
    let mut state = lock_state(&inner.state);

    if state.closed {
        return 0;
    }

    if state.receiver_waiting == 0 || state.pending_value.is_some() {
        return 0;
    }
    state.pending_value = Some(value);
    wake_receivers(inner, &mut state);
    1
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_recv(handle: i64) -> i64 {
    let Some(channel) = channel_ref(handle) else {
        return optional_tuple(false, 0);
    };
    let inner = &**channel;
    if let Some(ring) = &inner.ring {
        return buffered_recv(inner, ring);
    }
    let green_task = green::current_task();
    let mut state = lock_state(&inner.state);

    loop {
        if let Some(value) = state.pending_value.take() {
            // completed a rendezvous: wake the sender waiting for its value
            // to be taken.
            state.deliveries += 1;
            wake_senders(inner, &mut state);
            return optional_tuple(true, value);
        }

        if state.closed {
            return optional_tuple(false, 0);
        }

        // announce that a receiver is now waiting so a blocked sender can deposit
        // (the unbuffered rendezvous handshake). only senders need this — waking a
        // sibling receiver here is exactly what caused the single-worker livelock.
        state.receiver_waiting += 1;
        wake_senders(inner, &mut state);
        state = block_on_channel(inner, state, green_task, Role::Receiver);
        state.receiver_waiting -= 1;
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_try_recv(handle: i64) -> i64 {
    let Some(channel) = channel_ref(handle) else {
        return optional_tuple(false, 0);
    };
    let inner = &**channel;
    if let Some(ring) = &inner.ring {
        if let Some(value) = ring.try_dequeue() {
            wake_parked(inner, ring, Role::Sender);
            return optional_tuple(true, value);
        }
        return optional_tuple(false, 0);
    }
    let mut state = lock_state(&inner.state);
    if let Some(value) = state.pending_value.take() {
        state.deliveries += 1;
        wake_senders(inner, &mut state);
        return optional_tuple(true, value);
    }
    optional_tuple(false, 0)
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_close(handle: i64) -> i64 {
    let Some(channel) = channel_ref(handle) else {
        return 0;
    };
    let inner = &**channel;
    let mut state = lock_state(&inner.state);
    if state.closed {
        return 0;
    }
    state.closed = true;
    // publish close to the lock-free path before the wakes below, so a woken
    // buffered waiter's re-check observes it.
    inner.closed_flag.store(true, Ordering::SeqCst);
    state.pending_value = None;
    // wake every parked caller of both roles so they resume and observe `closed` —
    // the one notify that legitimately targets both sides. notify_all rather than
    // notify_one because there is no bound on how many are parked.
    inner.senders.notify_all();
    inner.receivers.notify_all();
    wake_green(&mut state, Role::Sender);
    wake_green(&mut state, Role::Receiver);
    1
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_len(handle: i64) -> i64 {
    let Some(channel) = channel_ref(handle) else {
        return 0;
    };
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
pub unsafe extern "C" fn pith_channel_cap(handle: i64) -> i64 {
    let Some(channel) = channel_ref(handle) else {
        return 0;
    };
    let state = lock_state(&channel.state);
    state.capacity as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_is_closed(handle: i64) -> i64 {
    let Some(channel) = channel_ref(handle) else {
        return 1;
    };
    let state = lock_state(&channel.state);
    if state.closed {
        1
    } else {
        0
    }
}

#[no_mangle]
pub extern "C" fn pith_select_next_index(count: i64) -> i64 {
    if count <= 1 {
        return 0;
    }
    let next = SELECT_COUNTER.fetch_add(1, Ordering::Relaxed);
    next.rem_euclid(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    // an ALIGNED but bogus handle: the magic check is what has to reject this,
    // since alignment alone lets the pointer be dereferenced. the older
    // registry lookup rejected every unknown address; the tag scheme rejects
    // anything whose first word is not CHANNEL_MAGIC, which is what this pins.
    // (a pointer into unmapped memory is out of contract either way — the same
    // practical guard strings, closures, and structs already use.)
    #[test]
    fn aligned_but_untagged_handle_is_rejected() {
        let scratch: Box<[u64; 4]> = Box::new([0; 4]);
        let handle = Box::into_raw(scratch) as i64;
        assert_eq!(handle % 8, 0, "test needs an aligned allocation");
        unsafe {
            assert_eq!(pith_channel_send(handle, 7), 0);
            assert_eq!(pith_channel_try_send(handle, 7), 0);
            assert_eq!(pith_channel_close(handle), 0);
            assert_eq!(pith_channel_len(handle), 0);
            assert_eq!(pith_channel_cap(handle), 0);
            assert_eq!(pith_channel_is_closed(handle), 1);
            let recv = pith_channel_try_recv(handle) as *const i64;
            assert_eq!(*recv, 0);
            drop(Box::from_raw(handle as *mut [u64; 4]));
        }
    }

    // a buffered channel's capacity is clamped rather than allocated blindly, so
    // an absurd request degrades to a smaller buffer instead of aborting the
    // process on a 16 GiB allocation.
    #[test]
    fn absurd_capacity_is_clamped() {
        unsafe {
            let ch = pith_channel_new(i64::MAX);
            assert_eq!(pith_channel_cap(ch), MAX_CHANNEL_CAPACITY);
            assert_eq!(pith_channel_try_send(ch, 1), 1);
            let t = pith_channel_try_recv(ch) as *const i64;
            assert_eq!((*t, *t.add(1)), (1, 1));
        }
    }

    // an unbuffered send whose value a receiver took must report success even
    // if the channel closes before the sender wakes up. close() clears the
    // pending slot the same way a receiver taking it does, so a sender that
    // looked only at `closed` reported failure for a value that had already
    // been delivered — and a caller that retries on failure sends it twice.
    #[test]
    fn unbuffered_send_reports_delivery_not_closure() {
        unsafe {
            let ch = pith_channel_new(0);
            let sender = std::thread::spawn(move || unsafe { pith_channel_send(ch, 42) });

            // take the value, then close while the sender is still parked
            let t = pith_channel_recv(ch) as *const i64;
            assert_eq!((*t, *t.add(1)), (1, 42));
            pith_channel_close(ch);

            assert_eq!(sender.join().unwrap(), 1, "delivered value reported as failed");
        }
    }

    // the other side of the same race: a sender parked with nobody to take its
    // value gets a failure when the channel closes, so the caller knows the
    // value never landed.
    #[test]
    fn unbuffered_send_fails_when_closed_undelivered() {
        unsafe {
            let ch = pith_channel_new(0);
            let sender = std::thread::spawn(move || unsafe { pith_channel_send(ch, 7) });
            std::thread::sleep(std::time::Duration::from_millis(50));
            pith_channel_close(ch);
            assert_eq!(sender.join().unwrap(), 0);
        }
    }

    #[test]
    fn invalid_channel_handles_return_safe_defaults() {
        unsafe {
            assert_eq!(pith_channel_send(12345, 7), 0);
            assert_eq!(pith_channel_try_send(12345, 7), 0);
            assert_eq!(pith_channel_close(12345), 0);
            assert_eq!(pith_channel_len(12345), 0);
            assert_eq!(pith_channel_cap(12345), 0);
            assert_eq!(pith_channel_is_closed(12345), 1);

            let recv = pith_channel_try_recv(12345) as *const i64;
            assert!(!recv.is_null());
            assert_eq!(*recv, 0);
        }
    }

    // the barrier shape: several senders park on one unbuffered (rendezvous)
    // channel at the same time, and a single receiver drains them. a recv that
    // takes a value must wake the *specific* depositing sender; a role-gated
    // notify_one that woke the wrong same-role sender would leave the depositor
    // parked forever, so a regression here hangs rather than fails. the counted
    // total proves every send was matched by exactly one recv.
    #[test]
    fn unbuffered_rendezvous_many_senders_one_receiver() {
        unsafe {
            let ch = pith_channel_new(0);
            let n = 8i64;
            let senders: Vec<_> = (1..=n)
                .map(|v| std::thread::spawn(move || assert_eq!(pith_channel_send(ch, v), 1)))
                .collect();

            let mut total = 0i64;
            for _ in 0..n {
                let t = pith_channel_recv(ch) as *const i64;
                assert_eq!(*t, 1, "expected a value, not an empty optional");
                total += *t.add(1);
            }
            for h in senders {
                h.join().unwrap();
            }
            assert_eq!(total, n * (n + 1) / 2);
        }
    }

    // capacity 1 is the vyukov edge: a single-slot ring cannot distinguish full
    // from free (the sequence after an enqueue equals the next enqueue
    // position), so the ring holds two slots with the explicit capacity check
    // enforcing the bound of one. the second try_send must refuse.
    #[test]
    fn buffered_capacity_one_refuses_second_send() {
        unsafe {
            let ch = pith_channel_new(1);
            assert_eq!(pith_channel_try_send(ch, 4), 1);
            assert_eq!(pith_channel_try_send(ch, 5), 0, "capacity 1 is full");
            assert_eq!(pith_channel_len(ch), 1);
            let t = pith_channel_try_recv(ch) as *const i64;
            assert_eq!((*t, *t.add(1)), (1, 4));
            assert_eq!(pith_channel_try_send(ch, 6), 1, "room again after recv");
        }
    }

    // a non-power-of-two capacity leaves spare ring slots, so fullness is
    // enforced by the explicit capacity check rather than the slot sequences:
    // try_send must refuse the (cap+1)th value, len must report the exact
    // capacity, and draining must return every value in order.
    #[test]
    fn buffered_ring_honors_non_power_of_two_capacity() {
        unsafe {
            let ch = pith_channel_new(3);
            assert_eq!(pith_channel_cap(ch), 3);
            for v in 1..=3 {
                assert_eq!(pith_channel_try_send(ch, v), 1);
            }
            assert_eq!(pith_channel_try_send(ch, 99), 0, "over capacity");
            assert_eq!(pith_channel_len(ch), 3);
            for v in 1..=3 {
                let t = pith_channel_try_recv(ch) as *const i64;
                assert_eq!(*t, 1);
                assert_eq!(*t.add(1), v);
            }
            let t = pith_channel_try_recv(ch) as *const i64;
            assert_eq!(*t, 0, "drained");
        }
    }

    // values enqueued before close must still drain; recvs after the drain see
    // the empty optional, and sends after close are refused.
    #[test]
    fn buffered_close_drains_then_reports_closed() {
        unsafe {
            let ch = pith_channel_new(4);
            assert_eq!(pith_channel_send(ch, 41), 1);
            assert_eq!(pith_channel_send(ch, 42), 1);
            pith_channel_close(ch);
            assert_eq!(pith_channel_send(ch, 43), 0, "send after close");
            let a = pith_channel_recv(ch) as *const i64;
            assert_eq!((*a, *a.add(1)), (1, 41));
            let b = pith_channel_recv(ch) as *const i64;
            assert_eq!((*b, *b.add(1)), (1, 42));
            let c = pith_channel_recv(ch) as *const i64;
            assert_eq!(*c, 0, "closed and drained");
            assert_eq!(pith_channel_is_closed(ch), 1);
        }
    }

    // the fanout shape: several producers and consumers over one bounded channel,
    // exactly what `bench/chan_fanout` measures. guards the buffered notify_one
    // hand-off — a producer that enqueues a value wakes one consumer, and close
    // wakes every blocked consumer so each observes the drained-and-closed state.
    #[test]
    fn buffered_fanout_many_producers_consumers() {
        use std::sync::atomic::{AtomicI64, Ordering};
        use std::sync::Arc;
        unsafe {
            let ch = pith_channel_new(4);
            let producers = 4i64;
            let per = 500i64;
            let total_msgs = producers * per;
            let sum = Arc::new(AtomicI64::new(0));
            let count = Arc::new(AtomicI64::new(0));

            let consumers: Vec<_> = (0..4)
                .map(|_| {
                    let sum = Arc::clone(&sum);
                    let count = Arc::clone(&count);
                    std::thread::spawn(move || loop {
                        let t = pith_channel_recv(ch) as *const i64;
                        if *t == 0 {
                            break; // empty optional: channel closed and drained
                        }
                        sum.fetch_add(*t.add(1), Ordering::Relaxed);
                        count.fetch_add(1, Ordering::Relaxed);
                    })
                })
                .collect();

            let prod: Vec<_> = (0..producers)
                .map(|p| {
                    std::thread::spawn(move || {
                        for i in 0..per {
                            assert_eq!(pith_channel_send(ch, p * per + i), 1);
                        }
                    })
                })
                .collect();

            for h in prod {
                h.join().unwrap();
            }
            pith_channel_close(ch);
            for h in consumers {
                h.join().unwrap();
            }

            assert_eq!(count.load(Ordering::Relaxed), total_msgs);
            // every id in 0..total_msgs was seen exactly once
            assert_eq!(sum.load(Ordering::Relaxed), total_msgs * (total_msgs - 1) / 2);
        }
    }
}
