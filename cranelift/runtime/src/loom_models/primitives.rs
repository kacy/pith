//! the green arms of the mutex, the semaphore and the wait group: each
//! registers its task under a lock, re-checks, and parks, and the other side
//! drains the list and wakes. a lost wake leaves a task parked forever, which
//! loom reports as a deadlock.
//!
//! mirrors of `pith_mutex_lock`/`pith_mutex_unlock` (`lock_green`),
//! `pith_semaphore_acquire`/`pith_semaphore_release` and
//! `pith_waitgroup_wait`/`pith_waitgroup_done`, step for step, with the
//! waiters as `green_word::Task`s. the real types reach the scheduler through
//! `green::wake` and the handle registry, which loom cannot run.

use super::green_word::Task;
use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicU32, Ordering};
use loom::sync::{Arc, Mutex};
use loom::thread;
use std::collections::VecDeque;

struct PithMutex {
    state: AtomicU32,
    green_waiters: Mutex<Vec<usize>>,
    tasks: [Task; 2],
    /// what the lock protects.
    guarded: UnsafeCell<u32>,
}

impl PithMutex {
    fn lock(&self, id: usize) {
        loop {
            if self
                .state
                .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }
            let mut waiters = self.green_waiters.lock().unwrap();
            match self.state.compare_exchange(1, 2, Ordering::Acquire, Ordering::Relaxed) {
                Ok(_) | Err(2) => {}
                Err(_) => continue,
            }
            waiters.push(id);
            drop(waiters);
            self.tasks[id].park();
        }
    }

    fn unlock(&self) {
        if self.state.swap(0, Ordering::Release) == 2 {
            let mut waiters = self.green_waiters.lock().unwrap();
            for id in waiters.drain(..) {
                self.tasks[id].wake();
            }
        }
    }
}

#[test]
fn loom_mutex_two_green_tasks() {
    loom::model(|| {
        let m = Arc::new(PithMutex {
            state: AtomicU32::new(0),
            green_waiters: Mutex::new(Vec::new()),
            tasks: [Task::running(), Task::running()],
            guarded: UnsafeCell::new(0),
        });
        let other = Arc::clone(&m);
        let t = thread::spawn(move || {
            other.lock(1);
            other.guarded.with_mut(|n| unsafe { *n += 1 });
            other.unlock();
        });
        m.lock(0);
        m.guarded.with_mut(|n| unsafe { *n += 1 });
        m.unlock();
        t.join().unwrap();
        assert_eq!(m.guarded.with(|n| unsafe { *n }), 2);
    });
}

struct SemaphoreState {
    count: usize,
    max: usize,
    green_waiters: VecDeque<usize>,
}

struct Semaphore {
    state: Mutex<SemaphoreState>,
    tasks: [Task; 2],
}

impl Semaphore {
    fn acquire(&self, id: usize) {
        let mut state = self.state.lock().unwrap();
        while state.count == 0 {
            state.green_waiters.push_back(id);
            drop(state);
            self.tasks[id].park();
            state = self.state.lock().unwrap();
        }
        state.count -= 1;
    }

    fn release(&self) {
        let mut state = self.state.lock().unwrap();
        if state.count < state.max {
            state.count += 1;
        }
        if let Some(id) = state.green_waiters.pop_front() {
            self.tasks[id].wake();
        }
    }
}

#[test]
fn loom_semaphore_release_wakes_each_waiter() {
    super::model_bounded(|| {
        let sem = Arc::new(Semaphore {
            state: Mutex::new(SemaphoreState {
                count: 0,
                max: 2,
                green_waiters: VecDeque::new(),
            }),
            tasks: [Task::running(), Task::running()],
        });
        let (second, releaser) = (Arc::clone(&sem), Arc::clone(&sem));
        let t = thread::spawn(move || second.acquire(1));
        let r = thread::spawn(move || {
            releaser.release();
            releaser.release();
        });
        sem.acquire(0);
        t.join().unwrap();
        r.join().unwrap();
    });
}

struct WaitGroupState {
    count: usize,
    green_waiters: Vec<usize>,
}

struct WaitGroup {
    state: Mutex<WaitGroupState>,
    waiter: Task,
}

impl WaitGroup {
    fn done(&self) {
        let mut state = self.state.lock().unwrap();
        if state.count > 0 {
            state.count -= 1;
        }
        if state.count == 0 {
            for _ in std::mem::take(&mut state.green_waiters) {
                self.waiter.wake();
            }
        }
    }

    fn wait(&self) {
        let mut state = self.state.lock().unwrap();
        while state.count > 0 {
            state.green_waiters.push(0);
            drop(state);
            self.waiter.park();
            state = self.state.lock().unwrap();
        }
    }
}

#[test]
fn loom_waitgroup_last_done_wakes_the_waiter() {
    loom::model(|| {
        let wg = Arc::new(WaitGroup {
            state: Mutex::new(WaitGroupState {
                count: 2,
                green_waiters: Vec::new(),
            }),
            waiter: Task::running(),
        });
        let (a, b) = (Arc::clone(&wg), Arc::clone(&wg));
        let ta = thread::spawn(move || a.done());
        let tb = thread::spawn(move || b.done());
        wg.wait();
        ta.join().unwrap();
        tb.join().unwrap();
    });
}

