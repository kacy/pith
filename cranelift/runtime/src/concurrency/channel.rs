//! channel support for task communication
//!
//! a channel serves two kinds of blocked caller at once. an **os-thread** caller
//! (the main thread, or the whole os-thread task backend) that would block waits
//! on the channel's `Condvar`, exactly as it always has. a **green task** (under
//! `PITH_GREEN`) must not park its worker OS thread — instead it registers itself
//! on the channel's green-waiter list *for its role* (sender or receiver) and
//! suspends its coroutine back to the scheduler, freeing the worker to run other
//! tasks; a later send/recv re-enqueues it. every state change that would `notify`
//! a condvar waiter therefore also wakes the green tasks blocked on the opposite
//! role, so whichever kind is parked makes progress. waking only the opposite role
//! (never a same-role sibling) is what keeps the backend from live-locking — see
//! the role note on `wake_green`.
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

type PithChannelHandle = Arc<(Mutex<ChannelState>, Condvar)>;

unsafe fn channel_ref<'a>(handle: i64) -> Option<&'a PithChannelHandle> {
    if !handle_registry::is_valid(handle as *const (), HandleKind::Channel) {
        return None;
    }
    Some(&*(handle as *const PithChannelHandle))
}

fn lock_state(lock: &Mutex<ChannelState>) -> MutexGuard<'_, ChannelState> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_state<'a>(
    cvar: &Condvar,
    state: MutexGuard<'a, ChannelState>,
) -> MutexGuard<'a, ChannelState> {
    cvar.wait(state)
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// wake the green tasks parked on this channel in the given role so each
/// re-checks the condition it blocked on. paired with a matching
/// `cvar.notify_all()` so green and os-thread waiters are signaled together.
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

/// block the current caller until the next notify on this channel, returning the
/// re-acquired guard. an os-thread caller condvar-waits exactly as before. a
/// green task registers itself on its role's green-waiter list (under the channel
/// lock), releases the lock, and suspends its coroutine back to the scheduler; it
/// re-locks and continues its loop when a later send/recv wakes it.
fn block_on_channel<'a>(
    lock: &'a Mutex<ChannelState>,
    cvar: &Condvar,
    mut state: MutexGuard<'a, ChannelState>,
    green_task: Option<usize>,
    role: Role,
) -> MutexGuard<'a, ChannelState> {
    match green_task {
        None => wait_state(cvar, state),
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
            lock_state(lock)
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
    let channel = Arc::new((Mutex::new(state), Condvar::new()));
    let ptr = Box::into_raw(Box::new(channel));
    handle_registry::register(ptr as *const (), HandleKind::Channel);
    ptr as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_send(handle: i64, value: i64) -> i64 {
    let Some(channel) = channel_ref(handle) else {
        return 0;
    };
    let (lock, cvar) = &**channel;
    // is this send running inside a green task? if so it yields on a would-block
    // instead of parking the worker; if not (main thread / os-thread backend) it
    // condvar-waits exactly as before. computed once — the running task does not
    // change under us across a suspend/resume.
    let green_task = green::current_task();
    let mut state = lock_state(lock);

    if state.closed {
        return 0;
    }

    if state.capacity == 0 {
        while !state.closed {
            if state.receiver_waiting > 0 && state.pending_value.is_none() {
                state.pending_value = Some(value);
                cvar.notify_all();
                wake_green(&mut state, Role::Receiver);
                while !state.closed && state.pending_value.is_some() {
                    state = block_on_channel(lock, cvar, state, green_task, Role::Sender);
                }
                return if state.closed { 0 } else { 1 };
            }
            state.sender_waiting += 1;
            state = block_on_channel(lock, cvar, state, green_task, Role::Sender);
            state.sender_waiting -= 1;
        }
        return 0;
    }

    while !state.closed && state.queue.len() >= state.capacity {
        state.sender_waiting += 1;
        state = block_on_channel(lock, cvar, state, green_task, Role::Sender);
        state.sender_waiting -= 1;
    }

    if state.closed {
        return 0;
    }

    state.queue.push_back(value);
    cvar.notify_all();
    wake_green(&mut state, Role::Receiver);
    1
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_try_send(handle: i64, value: i64) -> i64 {
    let Some(channel) = channel_ref(handle) else {
        return 0;
    };
    let (lock, cvar) = &**channel;
    let mut state = lock_state(lock);

    if state.closed {
        return 0;
    }

    if state.capacity == 0 {
        if state.receiver_waiting == 0 || state.pending_value.is_some() {
            return 0;
        }
        state.pending_value = Some(value);
        cvar.notify_all();
        wake_green(&mut state, Role::Receiver);
        1
    } else {
        if state.queue.len() >= state.capacity {
            return 0;
        }
        state.queue.push_back(value);
        cvar.notify_all();
        wake_green(&mut state, Role::Receiver);
        1
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_recv(handle: i64) -> i64 {
    let Some(channel) = channel_ref(handle) else {
        return optional_tuple(false, 0);
    };
    let (lock, cvar) = &**channel;
    let green_task = green::current_task();
    let mut state = lock_state(lock);

    loop {
        if let Some(value) = state.queue.pop_front() {
            // took a queued value: a blocked sender may now have room.
            cvar.notify_all();
            wake_green(&mut state, Role::Sender);
            return optional_tuple(true, value);
        }

        if state.capacity == 0 {
            if let Some(value) = state.pending_value.take() {
                // completed a rendezvous: wake the sender waiting for its value
                // to be taken.
                cvar.notify_all();
                wake_green(&mut state, Role::Sender);
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
        cvar.notify_all();
        wake_green(&mut state, Role::Sender);
        state = block_on_channel(lock, cvar, state, green_task, Role::Receiver);
        state.receiver_waiting -= 1;
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_try_recv(handle: i64) -> i64 {
    let Some(channel) = channel_ref(handle) else {
        return optional_tuple(false, 0);
    };
    let (lock, cvar) = &**channel;
    let mut state = lock_state(lock);

    if let Some(value) = state.queue.pop_front() {
        cvar.notify_all();
        wake_green(&mut state, Role::Sender);
        return optional_tuple(true, value);
    }
    if state.capacity == 0 {
        if let Some(value) = state.pending_value.take() {
            cvar.notify_all();
            wake_green(&mut state, Role::Sender);
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
    let (lock, cvar) = &**channel;
    let mut state = lock_state(lock);
    if state.closed {
        return 0;
    }
    state.closed = true;
    state.pending_value = None;
    cvar.notify_all();
    // wake every parked green sender and receiver so they resume and observe
    // `closed` — the one notify that legitimately targets both roles.
    wake_green(&mut state, Role::Sender);
    wake_green(&mut state, Role::Receiver);
    1
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_len(handle: i64) -> i64 {
    let Some(channel) = channel_ref(handle) else {
        return 0;
    };
    let (lock, _) = &**channel;
    let state = lock_state(lock);
    state.queue.len() as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_cap(handle: i64) -> i64 {
    let Some(channel) = channel_ref(handle) else {
        return 0;
    };
    let (lock, _) = &**channel;
    let state = lock_state(lock);
    state.capacity as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_is_closed(handle: i64) -> i64 {
    let Some(channel) = channel_ref(handle) else {
        return 1;
    };
    let (lock, _) = &**channel;
    let state = lock_state(lock);
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
}
