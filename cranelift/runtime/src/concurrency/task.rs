//! Task system for spawn/await
//!
//! spawn creates a thread that calls a function pointer with an argument.
//! await joins the thread and returns the result.

use std::sync::Mutex;
use std::thread::JoinHandle;

static TASKS: std::sync::OnceLock<Mutex<Vec<Option<TaskState>>>> = std::sync::OnceLock::new();

fn tasks() -> &'static Mutex<Vec<Option<TaskState>>> {
    TASKS.get_or_init(|| Mutex::new(Vec::new()))
}

struct TaskState {
    handle: JoinHandle<i64>,
}

/// Spawn a function call in a new thread.
/// fn_ptr: pointer to a function (fn(i64) -> i64)
/// arg: the argument to pass
/// Returns a task handle (index into TASKS array + 1, 1-based)
#[no_mangle]
pub unsafe extern "C" fn forge_spawn(fn_ptr: i64, arg: i64) -> i64 {
    // Cast fn_ptr to a callable function pointer
    let func: extern "C" fn(i64) -> i64 = std::mem::transmute(fn_ptr as *const ());

    let handle = std::thread::spawn(move || {
        func(arg)
    });

    let mut t = tasks().lock().unwrap();
    let idx = t.len();
    t.push(Some(TaskState { handle }));
    (idx as i64) + 1 // 1-based
}

/// Await a task: join the thread and return its result.
/// task_handle: 1-based index into TASKS array
/// Returns the function's return value
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
