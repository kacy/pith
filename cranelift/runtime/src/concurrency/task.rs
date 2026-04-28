//! task system for spawn/await

use crate::handle_registry::{self, HandleKind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

static TASKS: std::sync::OnceLock<Mutex<Vec<Option<TaskState>>>> = std::sync::OnceLock::new();

fn tasks() -> &'static Mutex<Vec<Option<TaskState>>> {
    TASKS.get_or_init(|| Mutex::new(Vec::new()))
}

fn lock_tasks() -> MutexGuard<'static, Vec<Option<TaskState>>> {
    tasks()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock_shared(lock: &Mutex<TaskShared>) -> MutexGuard<'_, TaskShared> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_shared<'a>(
    cvar: &Condvar,
    state: MutexGuard<'a, TaskShared>,
) -> MutexGuard<'a, TaskShared> {
    cvar.wait(state)
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct TaskState {
    handle: Option<JoinHandle<()>>,
    shared: Arc<(Mutex<TaskShared>, Condvar)>,
}

struct TaskShared {
    done: bool,
    result: i64,
}

#[no_mangle]
pub unsafe extern "C" fn pith_spawn(closure_handle: i64) -> i64 {
    if closure_handle == 0 {
        return 0;
    }

    let shared = Arc::new((
        Mutex::new(TaskShared {
            done: false,
            result: 0,
        }),
        Condvar::new(),
    ));
    let shared_clone = shared.clone();

    let handle = std::thread::spawn(move || {
        let func_ptr = crate::pith_closure_get_fn(closure_handle);
        if func_ptr == 0 {
            let (lock, cvar) = &*shared_clone;
            let mut state = lock_shared(lock);
            state.done = true;
            state.result = 0;
            cvar.notify_all();
            return;
        }
        let func: extern "C" fn(i64) -> i64 = std::mem::transmute(func_ptr as *const ());
        let result = func(closure_handle);
        let (lock, cvar) = &*shared_clone;
        let mut state = lock_shared(lock);
        state.done = true;
        state.result = result;
        cvar.notify_all();
    });

    let mut t = lock_tasks();
    let idx = t.len();
    t.push(Some(TaskState {
        handle: Some(handle),
        shared,
    }));
    let task_handle = (idx as i64) + 1;
    handle_registry::register_id(task_handle, HandleKind::Task);
    task_handle
}

#[no_mangle]
pub unsafe extern "C" fn pith_await(task_handle: i64) -> i64 {
    if !handle_registry::is_valid_id(task_handle, HandleKind::Task) {
        return 0;
    }
    let idx = (task_handle - 1) as usize;
    let task_state = {
        let mut t = lock_tasks();
        if idx < t.len() {
            if let Some(task) = &mut t[idx] {
                Some((task.shared.clone(), task.handle.take()))
            } else {
                None
            }
        } else {
            None
        }
    };

    match task_state {
        Some((shared, handle)) => {
            if let Some(join_handle) = handle {
                let _ = join_handle.join();
            }
            let (lock, cvar) = &*shared;
            let mut state = lock_shared(lock);
            while !state.done {
                state = wait_shared(cvar, state);
            }
            state.result
        }
        None => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_task_is_done(task_handle: i64) -> i64 {
    if !handle_registry::is_valid_id(task_handle, HandleKind::Task) {
        return 0;
    }
    let idx = (task_handle - 1) as usize;
    let shared = {
        let t = lock_tasks();
        if idx < t.len() {
            if let Some(task) = &t[idx] {
                Some(task.shared.clone())
            } else {
                None
            }
        } else {
            None
        }
    };
    if let Some(shared) = shared {
        let (lock, _) = &*shared;
        let state = lock_shared(lock);
        return if state.done { 1 } else { 0 };
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn pith_task_detach(task_handle: i64) {
    if !handle_registry::is_valid_id(task_handle, HandleKind::Task) {
        return;
    }
    let idx = (task_handle - 1) as usize;
    let handle = {
        let mut t = lock_tasks();
        if idx < t.len() {
            if let Some(task) = &mut t[idx] {
                task.handle.take()
            } else {
                None
            }
        } else {
            None
        }
    };
    drop(handle);
}
