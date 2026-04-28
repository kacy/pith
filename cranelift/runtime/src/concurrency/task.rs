//! task system for spawn/await

use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

static TASKS: std::sync::OnceLock<Mutex<Vec<Option<TaskState>>>> = std::sync::OnceLock::new();

fn tasks() -> &'static Mutex<Vec<Option<TaskState>>> {
    TASKS.get_or_init(|| Mutex::new(Vec::new()))
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
            if let Ok(mut state) = lock.lock() {
                state.done = true;
                state.result = 0;
                cvar.notify_all();
            }
            return;
        }
        let func: extern "C" fn(i64) -> i64 = std::mem::transmute(func_ptr as *const ());
        let result = func(closure_handle);
        let (lock, cvar) = &*shared_clone;
        if let Ok(mut state) = lock.lock() {
            state.done = true;
            state.result = result;
            cvar.notify_all();
        }
    });

    let mut t = tasks().lock().unwrap();
    let idx = t.len();
    t.push(Some(TaskState {
        handle: Some(handle),
        shared,
    }));
    (idx as i64) + 1
}

#[no_mangle]
pub unsafe extern "C" fn pith_await(task_handle: i64) -> i64 {
    if task_handle <= 0 {
        return 0;
    }
    let idx = (task_handle - 1) as usize;
    let task_state = {
        let mut t = tasks().lock().unwrap();
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
            let mut state = lock.lock().unwrap();
            while !state.done {
                state = cvar.wait(state).unwrap();
            }
            state.result
        }
        None => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_task_is_done(task_handle: i64) -> i64 {
    if task_handle <= 0 {
        return 0;
    }
    let idx = (task_handle - 1) as usize;
    let shared = {
        let t = tasks().lock().unwrap();
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
        if let Ok(state) = lock.lock() {
            return if state.done { 1 } else { 0 };
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn pith_task_detach(task_handle: i64) {
    if task_handle <= 0 {
        return;
    }
    let idx = (task_handle - 1) as usize;
    let handle = {
        let mut t = tasks().lock().unwrap();
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
