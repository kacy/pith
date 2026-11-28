//! Green-task and OS-thread parking/waking helpers for channels.

use super::handle::{ChannelInner, ChannelState, ParkCount, Role};
use super::ring::Ring;
use crate::concurrency::green;
use std::sync::atomic::Ordering;
use std::sync::{Condvar, Mutex, MutexGuard};

pub(super) fn lock_state(lock: &Mutex<ChannelState>) -> MutexGuard<'_, ChannelState> {
    crate::sync::lock(lock)
}

/// wake the green tasks parked on this channel in the given role so each
/// re-checks the condition it blocked on. the os-thread half of a wake is a
/// `notify_one` on the same role's condvar (see `wake_receivers`/`wake_senders`);
/// this drains the green side.
///
/// `role` says which side to wake, and it is always the *opposite* of whoever is
/// notifying: a send wakes receivers, a recv wakes senders, a close wakes both.
/// waking only the opposite role is what keeps the green backend from live-
/// locking: two receivers (or two senders) parked on one worker must never wake
/// each other, because the woken sibling cannot make progress and would just
/// re-park and re-notify forever. over-waking *within* the opposite role is still
/// safe: a woken task re-evaluates under the channel lock and re-parks if it
/// still cannot proceed, the same tolerance the condvar loops rely on.
///
/// called while holding the channel lock; `green::wake` takes a run-queue lock
/// under it (see the lock-order note above).
pub(super) fn wake_green(state: &mut ChannelState, role: Role) {
    // drain first: a woken task that re-parks will re-add itself, and draining
    // keeps a task from being enqueued twice for one notify.
    let list = state.green_waiters(role);
    if list.is_empty() {
        return;
    }
    for id in std::mem::take(list) {
        green::wake(id);
    }
}

/// notify the os-thread waiters parked on one role's condvar, skipping the
/// syscall entirely when none are parked (`waiting` is the role's *os-thread*
/// waiter count; green waiters are woken through their scheduler and must not
/// trigger a condvar futex, see `os_receivers_waiting`).
///
/// a **buffered** channel hands off one value or one freed slot at a time, and
/// any parked waiter of a role wants the same thing (a receiver wants a value, a
/// sender wants room), so exactly one can proceed: `notify_one`. that is the
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
pub(super) fn notify_role(cvar: &Condvar, capacity: usize, waiting: usize) {
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
pub(super) fn wake_receivers(inner: &ChannelInner, state: &mut ChannelState) {
    notify_role(&inner.receivers, state.capacity, state.os_receivers_waiting);
    wake_green(state, Role::Receiver);
}

/// wake the senders parked on this channel (opposite role of a recv): symmetric
/// to `wake_receivers`.
pub(super) fn wake_senders(inner: &ChannelInner, state: &mut ChannelState) {
    notify_role(&inner.senders, state.capacity, state.os_senders_waiting);
    wake_green(state, Role::Sender);
}

/// block the current caller until the next notify for its role, returning the
/// re-acquired guard. an os-thread caller waits on its role's condvar. a green
/// task registers itself on its role's green-waiter list (under the channel
/// lock), releases the lock, and suspends its coroutine back to the scheduler; it
/// re-locks and continues its loop when a later send/recv wakes it.
pub(super) fn block_on_channel<'a>(
    park: &ParkCount,
    inner: &'a ChannelInner,
    mut state: MutexGuard<'a, ChannelState>,
    green_task: Option<usize>,
    role: Role,
) -> MutexGuard<'a, ChannelState> {
    // about to park: hold a count of our own for the span of this call, so the
    // body outlives the suspension even if every language owner releases it
    // while we sleep (see `ParkCount`).
    park.take();
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
            // bracket's exit runs while holding the re-acquired channel lock.
            // that is safe because a stop that parks us there stops only
            // mutators, and the collection pass itself never takes a channel
            // lock.
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
            // release the lock *before* suspending: suspend returns control to
            // the worker, which may touch this same channel. registering under our
            // role lets the opposite side wake us without waking same-role peers.
            state.green_waiters(role).push(id);
            drop(state);
            green::park_current(id);
            let mut state = lock_state(&inner.state);
            // a waker that meant us took our entry off the list. one that is
            // still there means this wake came from elsewhere, and the caller
            // is about to re-check and maybe register again. a stale entry
            // would let a later send claim us while we are parked on some
            // other channel and hand us this channel's value.
            let list = state.green_waiters(role);
            if let Some(pos) = list.iter().rposition(|&waiter| waiter == id) {
                list.remove(pos);
            }
            state
        }
    }
}
/// after a successful ring op, wake one parked waiter of the opposite `role` if
/// the gate says any exist. the seq-cst fence orders our ring release-store
/// before the gate load, pairing with the fence a parking waiter issues between
/// its gate increment and its ring re-try, so either we see its increment here,
/// or its re-try sees our value (never both misses; see `buffered_send`).
///
/// a parked green *receiver* gets a direct handoff: one is popped and the ring
/// dequeued on its behalf under the claim (see `green::wake_with`), so it
/// resumes with the value in hand instead of re-locking and re-trying. dequeuing
/// through the ring, never around it, is what keeps fifo order. everything
/// else (os-thread waiters, green senders) gets the plain wake-and-re-check.
pub(super) fn wake_parked(inner: &ChannelInner, ring: &Ring, role: Role) {
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
    // them. the count is exact under this lock: an os-thread waiter
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
