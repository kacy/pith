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
use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

static SELECT_COUNTER: AtomicI64 = AtomicI64::new(0);

struct ChannelState {
    queue: VecDeque<i64>,
    capacity: usize,
    closed: bool,
    pending_value: Option<i64>,
    receiver_waiting: usize,
    sender_waiting: usize,
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

/// the channel behind a handle: the guarded state plus one condvar per role.
/// os-thread senders park on `senders`, receivers on `receivers`, so a wake can
/// target one role without disturbing the other (see the role-split note above).
struct ChannelInner {
    state: Mutex<ChannelState>,
    senders: Condvar,
    receivers: Condvar,
}

type PithChannelHandle = Arc<ChannelInner>;

unsafe fn channel_ref<'a>(handle: i64) -> Option<&'a PithChannelHandle> {
    if !handle_registry::is_valid(handle as *const (), HandleKind::Channel) {
        return None;
    }
    Some(&*(handle as *const PithChannelHandle))
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
/// syscall entirely when none are parked (`waiting == 0`).
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
    notify_role(&inner.receivers, state.capacity, state.receiver_waiting);
    wake_green(state, Role::Receiver);
}

/// wake the senders parked on this channel (opposite role of a recv): symmetric
/// to `wake_receivers`.
fn wake_senders(inner: &ChannelInner, state: &mut ChannelState) {
    notify_role(&inner.senders, state.capacity, state.sender_waiting);
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
            let cvar = match role {
                Role::Sender => &inner.senders,
                Role::Receiver => &inner.receivers,
            };
            cvar.wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
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
    let cap = capacity.max(0) as usize;
    let state = ChannelState {
        queue: VecDeque::new(),
        capacity: cap,
        closed: false,
        pending_value: None,
        receiver_waiting: 0,
        sender_waiting: 0,
        green_receivers: Vec::new(),
        green_senders: Vec::new(),
    };
    let channel = Arc::new(ChannelInner {
        state: Mutex::new(state),
        senders: Condvar::new(),
        receivers: Condvar::new(),
    });
    let ptr = Box::into_raw(Box::new(channel));
    handle_registry::register(ptr as *const (), HandleKind::Channel);
    ptr as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_send(handle: i64, value: i64) -> i64 {
    let Some(channel) = channel_ref(handle) else {
        return 0;
    };
    let inner = &**channel;
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
                return if state.closed { 0 } else { 1 };
            }
            state.sender_waiting += 1;
            state = block_on_channel(inner, state, green_task, Role::Sender);
            state.sender_waiting -= 1;
        }
        return 0;
    }

    while !state.closed && state.queue.len() >= state.capacity {
        state.sender_waiting += 1;
        state = block_on_channel(inner, state, green_task, Role::Sender);
        state.sender_waiting -= 1;
    }

    if state.closed {
        return 0;
    }

    state.queue.push_back(value);
    wake_receivers(inner, &mut state);
    1
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_try_send(handle: i64, value: i64) -> i64 {
    let Some(channel) = channel_ref(handle) else {
        return 0;
    };
    let inner = &**channel;
    let mut state = lock_state(&inner.state);

    if state.closed {
        return 0;
    }

    if state.capacity == 0 {
        if state.receiver_waiting == 0 || state.pending_value.is_some() {
            return 0;
        }
        state.pending_value = Some(value);
        wake_receivers(inner, &mut state);
        1
    } else {
        if state.queue.len() >= state.capacity {
            return 0;
        }
        state.queue.push_back(value);
        wake_receivers(inner, &mut state);
        1
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_recv(handle: i64) -> i64 {
    let Some(channel) = channel_ref(handle) else {
        return optional_tuple(false, 0);
    };
    let inner = &**channel;
    let green_task = green::current_task();
    let mut state = lock_state(&inner.state);

    loop {
        if let Some(value) = state.queue.pop_front() {
            // took a queued value: a blocked sender may now have room.
            wake_senders(inner, &mut state);
            return optional_tuple(true, value);
        }

        if state.capacity == 0 {
            if let Some(value) = state.pending_value.take() {
                // completed a rendezvous: wake the sender waiting for its value
                // to be taken.
                wake_senders(inner, &mut state);
                return optional_tuple(true, value);
            }
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
    let mut state = lock_state(&inner.state);

    if let Some(value) = state.queue.pop_front() {
        wake_senders(inner, &mut state);
        return optional_tuple(true, value);
    }
    if state.capacity == 0 {
        if let Some(value) = state.pending_value.take() {
            wake_senders(inner, &mut state);
            return optional_tuple(true, value);
        }
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
    let state = lock_state(&channel.state);
    state.queue.len() as i64
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
