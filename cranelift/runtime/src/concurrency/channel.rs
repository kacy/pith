//! channel support for task communication

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
    let mut state = lock_state(lock);

    if state.closed {
        return 0;
    }

    if state.capacity == 0 {
        while !state.closed {
            if state.receiver_waiting > 0 && state.pending_value.is_none() {
                state.pending_value = Some(value);
                cvar.notify_all();
                while !state.closed && state.pending_value.is_some() {
                    state = wait_state(cvar, state);
                }
                return if state.closed { 0 } else { 1 };
            }
            state.sender_waiting += 1;
            state = wait_state(cvar, state);
            state.sender_waiting -= 1;
        }
        return 0;
    }

    while !state.closed && state.queue.len() >= state.capacity {
        state.sender_waiting += 1;
        state = wait_state(cvar, state);
        state.sender_waiting -= 1;
    }

    if state.closed {
        return 0;
    }

    state.queue.push_back(value);
    cvar.notify_all();
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
        1
    } else {
        if state.queue.len() >= state.capacity {
            return 0;
        }
        state.queue.push_back(value);
        cvar.notify_all();
        1
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_channel_recv(handle: i64) -> i64 {
    let Some(channel) = channel_ref(handle) else {
        return optional_tuple(false, 0);
    };
    let (lock, cvar) = &**channel;
    let mut state = lock_state(lock);

    loop {
        if let Some(value) = state.queue.pop_front() {
            cvar.notify_all();
            return optional_tuple(true, value);
        }

        if state.capacity == 0 {
            if let Some(value) = state.pending_value.take() {
                cvar.notify_all();
                return optional_tuple(true, value);
            }
        }

        if state.closed {
            return optional_tuple(false, 0);
        }

        state.receiver_waiting += 1;
        cvar.notify_all();
        state = wait_state(cvar, state);
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
        return optional_tuple(true, value);
    }
    if state.capacity == 0 {
        if let Some(value) = state.pending_value.take() {
            cvar.notify_all();
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
