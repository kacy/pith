//! two os-thread awaiters on one task handle (H11).
//!
//! a mirror of the join channel in `concurrency::task`: `TaskShared` and its
//! condvar, the completion publish at the end of the task thread in
//! `os_thread_spawn`, and the wait-then-read in `os_thread_await`. the slab
//! and the `JoinHandle` are left out: only the first awaiter gets the join
//! handle, so the second always comes through the condvar wait modeled here.
//!
//! the result is an owned value (the task's count on what it returned), so
//! the property is that at most one awaiter receives it. each awaiter that
//! does releases it, as the emitted code after an `await` does, and a second
//! release is the double free.

use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::sync::{Arc, Condvar, Mutex};
use loom::thread;

/// what the task returns: any nonzero handle.
const RESULT: i64 = 42;

struct TaskShared {
    done: bool,
    result: i64,
    waiters: u32,
}

struct Join {
    shared: Mutex<TaskShared>,
    joined: Condvar,
    /// releases of the task's result.
    releases: AtomicUsize,
}

impl Join {
    /// the task thread's publish.
    fn finish(&self) {
        let mut state = self.shared.lock().unwrap();
        state.done = true;
        state.result = RESULT;
        if state.waiters > 0 {
            self.joined.notify_all();
        }
    }

    /// `os_thread_await` after the slab lookup, then the caller's release.
    /// `take` is the fix: read the result out under the lock.
    fn await_and_release(&self, take: bool) {
        let result = {
            let mut state = self.shared.lock().unwrap();
            while !state.done {
                state.waiters += 1;
                state = self.joined.wait(state).unwrap();
                state.waiters -= 1;
            }
            if take {
                std::mem::take(&mut state.result)
            } else {
                state.result
            }
        };
        if result != 0 {
            assert_eq!(result, RESULT);
            let before = self.releases.fetch_add(1, Ordering::Relaxed);
            assert_eq!(before, 0, "the task result was released twice");
        }
    }
}

fn two_awaiters(take: bool) {
    loom::model(move || {
        let join = Arc::new(Join {
            shared: Mutex::new(TaskShared {
                done: false,
                result: 0,
                waiters: 0,
            }),
            joined: Condvar::new(),
            releases: AtomicUsize::new(0),
        });
        let (a, b) = (Arc::clone(&join), Arc::clone(&join));
        let first = thread::spawn(move || a.await_and_release(take));
        let second = thread::spawn(move || b.await_and_release(take));
        join.finish();
        first.join().unwrap();
        second.join().unwrap();
        assert_eq!(join.releases.load(Ordering::Relaxed), 1, "nobody received the result");
    });
}

#[test]
fn loom_task_two_awaiters_one_result() {
    two_awaiters(true);
}

#[test]
#[should_panic(expected = "the task result was released twice")]
fn loom_task_pre_fix_two_awaiters_double_release() {
    two_awaiters(false);
}
