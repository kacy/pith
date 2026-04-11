//! task system for spawn/await

use std::sync::Mutex;
use std::thread::JoinHandle;

static TASKS: std::sync::OnceLock<Mutex<Vec<Option<TaskState>>>> = std::sync::OnceLock::new();

fn tasks() -> &'static Mutex<Vec<Option<TaskState>>> {
    TASKS.get_or_init(|| Mutex::new(Vec::new()))
}

struct TaskState {
    handle: JoinHandle<i64>,
}

#[no_mangle]
pub unsafe extern "C" fn forge_spawn(closure_handle: i64) -> i64 {
    if closure_handle == 0 {
        return 0;
    }

    let handle = std::thread::spawn(move || {
        let func_ptr = crate::forge_closure_get_fn(closure_handle);
        if func_ptr == 0 {
            return 0;
        }
        let func: extern "C" fn(i64) -> i64 = std::mem::transmute(func_ptr as *const ());
        func(closure_handle)
    });

    let mut t = tasks().lock().unwrap();
    let idx = t.len();
    t.push(Some(TaskState { handle }));
    (idx as i64) + 1
}

#[no_mangle]
pub unsafe extern "C" fn forge_await(task_handle: i64) -> i64 {
    if task_handle <= 0 {
        return 0;
    }
    let idx = (task_handle - 1) as usize;
    let state = {
        let mut t = tasks().lock().unwrap();
        if idx < t.len() {
            t[idx].take()
        } else {
            None
        }
    };

    match state {
        Some(task) => {
            match task.handle.join() {
                Ok(result) => result,
                Err(_) => 0,
            }
        }
        None => 0,
    }
}
