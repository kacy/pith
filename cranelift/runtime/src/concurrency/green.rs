//! green-thread backend (experimental, behind `PITH_GREEN`)
//!
//! an M:N scheduler: many pith tasks run as stackful coroutines on a small
//! fixed pool of worker OS threads. spawning a task is a userspace enqueue, not
//! a `std::thread::spawn`, so a fan-out of thousands of independent tasks costs
//! a handful of kernel threads instead of thousands. handing control between
//! tasks on the same worker is a coroutine switch (a stack swap in userspace),
//! not a kernel context switch — that is the win phase 0 measured.
//!
//! ## what P1a does and does not do
//!
//! this is the first real slice. it runs tasks to completion and joins them.
//! it deliberately does NOT yet make a *blocking* task yield its worker: when a
//! task awaits another task that is not finished, we block the calling OS thread
//! on a condvar until the joinee completes (call this "stance (a)"). that keeps
//! the code small and obviously correct, at one cost: if more tasks are mutually
//! blocked than there are workers, the pool deadlocks. so green mode is only
//! valid today for *independent* tasks (fan-out, then join). teaching a blocked
//! task to yield its worker back to the scheduler is P2 (the netpoller / channel
//! integration), not P1a.
//!
//! ## synchronization model
//!
//! - the task slab (`Vec<Option<Task>>`) is shared across workers under one
//!   mutex. we never hold that lock while *running* a coroutine — running pith
//!   code can spawn more tasks, which needs the same lock. so a worker *takes*
//!   the coroutine out of the slab, drops the lock, resumes it, then re-locks to
//!   record the result. see `run_task`.
//! - each worker owns a local run queue; a global injector receives spawns from
//!   non-worker threads (e.g. `main`). an idle worker checks local, then
//!   injector, then steals from a random peer. this is the standard work-
//!   stealing shape, written with plain `Mutex<VecDeque>` — readable first;
//!   lock-free deques are a later perf phase, not this one.
//! - a task's join state (done flag + result) lives behind its own
//!   `Arc<(Mutex, Condvar)>`, exactly like the os-thread backend. await clones
//!   the arc and waits; the worker flips done + notifies when the coroutine
//!   returns. reusing that shape keeps ARC/refcount balance identical to the
//!   os-thread path — the same pith code runs, retains and releases the same
//!   way, across the coroutine boundary.

use crate::handle_registry::{self, HandleKind};
use corosensei::{Coroutine, CoroutineResult};
use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Once, OnceLock};
use std::time::Duration;

/// index into the task slab. the pith-facing task handle is always `id + 1`, so
/// that handle 0 stays reserved for "no task" as in the os-thread backend.
type TaskId = usize;

/// coroutine stack size. pith programs default to 8 MiB OS-thread stacks; a
/// green task gets a fixed slab here. 1 MiB is generous for the leaf-ish task
/// bodies we run today and keeps a large fan-out affordable. a deeply recursive
/// task could overflow this — corosensei installs a guard page so that faults
/// loudly rather than corrupting memory. sizing per-task stacks is a later knob.
const STACK_SIZE: usize = 1024 * 1024;

/// run state of a slab task. `Yielded` cannot arise in P1a (nothing yields yet)
/// but the worker loop handles it so P2 can add yielding without reshaping this.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunState {
    Ready,
    Running,
    Yielded,
    Done,
}

/// the coroutine type every task runs: no input/yield, returns the pith result
/// as an `i64`.
type TaskCoroutine = Coroutine<(), (), i64>;

/// a coroutine wrapped so it can move between the spawning thread and the worker
/// that runs it. corosensei makes `Coroutine` `!Send` because, in general,
/// arbitrary data may live on a *suspended* coroutine's stack and it cannot
/// prove that data is `Send`. here we control the whole task body: the only
/// state captured before first resume is `closure_handle: i64` (trivially
/// `Send`), and in P1a a task never suspends mid-run (nothing yields), so a
/// coroutine we move is either fresh or finished — never carrying live non-Send
/// stack data. that makes the manual impl sound. when P2 adds real yielding,
/// this assumption must be revisited (a task suspended inside pith code could
/// hold non-Send locals), but the pith runtime values crossing the boundary are
/// all handles/ints today.
struct SendCoroutine(TaskCoroutine);

// SAFETY: see the type doc above — the task body captures only a `Send` i64 and
// does not suspend with live non-Send stack data in P1a.
unsafe impl Send for SendCoroutine {}

/// join channel between a task and whoever awaits it: the done flag plus the
/// result, guarded so an awaiting thread can block on the condvar until the
/// worker records completion. one allocation per task, shared via `Arc`.
struct Join {
    done: bool,
    result: i64,
}

struct Task {
    /// the running coroutine. taken out (`None`) while a worker resumes it, put
    /// back if it yields, dropped when it returns.
    coro: Option<SendCoroutine>,
    /// the pith closure this task runs. kept for reference/debugging; the
    /// coroutine already closed over it.
    #[allow(dead_code)]
    closure_handle: i64,
    state: RunState,
    result: i64,
    join: Arc<(Mutex<Join>, Condvar)>,
}

/// one worker's private run queue plus the shared bits it needs. workers are
/// created once and live for the process; there is no shutdown path (the pool
/// dies with the process, like a daemon thread pool).
struct Worker {
    local: Mutex<VecDeque<TaskId>>,
}

/// the whole green runtime: the task slab, the worker queues, the injector for
/// off-worker spawns, and a single park spot idle workers wait on.
struct Scheduler {
    slab: Mutex<Vec<Option<Task>>>,
    workers: Vec<Worker>,
    injector: Mutex<VecDeque<TaskId>>,
    /// idle workers park here; every enqueue notifies. the guarded unit plus a
    /// short wait-timeout bounds any lost-wakeup window (see `park`).
    park_lock: Mutex<()>,
    park_cv: Condvar,
}

/// the scheduler is built lazily on the first green spawn so a process that
/// never spawns pays nothing (and, importantly, does not start worker threads).
static SCHEDULER: OnceLock<Scheduler> = OnceLock::new();

thread_local! {
    /// set to `Some(i)` on worker thread `i`; `None` on any other thread. green
    /// spawn uses it to route a new task to the current worker's local queue
    /// (spawned from inside a task) versus the global injector (spawned from
    /// `main`). a `Cell` because it is written once at worker startup.
    static CURRENT_WORKER: Cell<Option<usize>> = const { Cell::new(None) };
}

fn lock_slab() -> MutexGuard<'static, Vec<Option<Task>>> {
    scheduler()
        .slab
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock_join(lock: &Mutex<Join>) -> MutexGuard<'_, Join> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// build (once) and return the scheduler struct. this only allocates the queues
/// — it does NOT start worker threads (that would need a reentrant init, since
/// workers call back into `scheduler()`). worker startup is a separate `Once`,
/// driven from the first green spawn. `available_parallelism` picks the worker
/// count, with a floor of 1.
fn scheduler() -> &'static Scheduler {
    SCHEDULER.get_or_init(|| {
        let n = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let workers = (0..n)
            .map(|_| Worker {
                local: Mutex::new(VecDeque::new()),
            })
            .collect();
        Scheduler {
            slab: Mutex::new(Vec::new()),
            workers,
            injector: Mutex::new(VecDeque::new()),
            park_lock: Mutex::new(()),
            park_cv: Condvar::new(),
        }
    })
}

/// start the worker threads exactly once. called from the first green spawn, by
/// which point `scheduler()` is initialized, so each worker can resolve the
/// static safely inside its loop.
fn ensure_workers_started() {
    static WORKERS_STARTED: Once = Once::new();
    WORKERS_STARTED.call_once(|| {
        let n = scheduler().workers.len();
        for i in 0..n {
            std::thread::Builder::new()
                .name(format!("pith-green-{i}"))
                .spawn(move || worker_loop(i))
                .expect("spawn green worker");
        }
    });
}

/// push a ready task onto a queue and wake one idle worker. spawns from a worker
/// go to that worker's local queue (cache-friendly, keeps a task's children
/// near it); spawns from elsewhere go to the injector.
fn enqueue(id: TaskId) {
    let sched = scheduler();
    match CURRENT_WORKER.with(|c| c.get()) {
        Some(i) => sched.workers[i]
            .local
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push_back(id),
        None => sched
            .injector
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push_back(id),
    }
    // wake a parked worker. notify_all (not one) plus the re-check in `park`
    // keeps this correct even if several tasks land at once.
    sched.park_cv.notify_all();
}

/// find the next ready task for worker `index`: its own queue first, then the
/// injector, then a steal from a random peer. returns `None` if the whole pool
/// is momentarily empty.
fn find_work(index: usize) -> Option<TaskId> {
    let sched = scheduler();

    if let Some(id) = sched.workers[index]
        .local
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .pop_front()
    {
        return Some(id);
    }

    if let Some(id) = sched
        .injector
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .pop_front()
    {
        return Some(id);
    }

    // steal from a peer. start at a pseudo-random offset so workers do not all
    // hammer worker 0. we take from the victim's *front*; a fancier scheduler
    // steals from the back, but readable-first and it does not affect
    // correctness with independent tasks.
    let n = sched.workers.len();
    let start = steal_start(n);
    for k in 0..n {
        let victim = (start + k) % n;
        if victim == index {
            continue;
        }
        if let Some(id) = sched.workers[victim]
            .local
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pop_front()
        {
            return Some(id);
        }
    }
    None
}

/// cheap per-thread rotating start index for steal victim selection. no strong
/// randomness needed — just avoid every worker probing the same victim first.
fn steal_start(n: usize) -> usize {
    thread_local! {
        static COUNTER: Cell<usize> = const { Cell::new(0) };
    }
    COUNTER.with(|c| {
        let v = c.get().wrapping_add(1);
        c.set(v);
        v % n.max(1)
    })
}

/// park an idle worker until work likely exists. we re-check for work while
/// holding `park_lock` to shrink the window where an enqueue+notify slips
/// between our empty `find_work` and this wait; a short timeout closes the
/// window entirely, so a missed wakeup costs latency, never a hang.
fn park(index: usize) {
    let sched = scheduler();
    let guard = sched
        .park_lock
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    // one last check under the lock: if work appeared, don't sleep.
    if has_any_work(index) {
        return;
    }
    let _ = sched
        .park_cv
        .wait_timeout(guard, Duration::from_millis(1));
}

/// is there any work anywhere this worker could take? used only to avoid
/// parking on a false-empty; the authoritative pop is still `find_work`.
fn has_any_work(_index: usize) -> bool {
    let sched = scheduler();
    if !sched
        .injector
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_empty()
    {
        return true;
    }
    sched
        .workers
        .iter()
        .any(|w| !w.local.lock().unwrap_or_else(|p| p.into_inner()).is_empty())
}

/// the worker thread body: take work, run it, park when there is none.
fn worker_loop(index: usize) {
    CURRENT_WORKER.with(|c| c.set(Some(index)));
    loop {
        match find_work(index) {
            Some(id) => run_task(id),
            None => park(index),
        }
    }
}

/// resume one task's coroutine to its next suspension point. the coroutine is
/// taken out of the slab (so we don't hold the slab lock across arbitrary pith
/// code, which may itself spawn), resumed, then either dropped (returned) or put
/// back (yielded).
fn run_task(id: TaskId) {
    // take the coroutine out under the lock, mark it running.
    let mut coro = {
        let mut slab = lock_slab();
        match slab.get_mut(id).and_then(|slot| slot.as_mut()) {
            Some(task) if task.state == RunState::Ready => {
                task.state = RunState::Running;
                match task.coro.take() {
                    Some(c) => c,
                    // a Ready task with no coroutine should be impossible; skip
                    // defensively rather than panic on a corrupt slot.
                    None => return,
                }
            }
            // already running/done/absent: nothing to do.
            _ => return,
        }
    };

    // run outside the lock. SAFETY of the transmute-and-call lives inside the
    // coroutine body (see green_spawn); here we just drive the coroutine.
    match coro.0.resume(()) {
        CoroutineResult::Return(result) => {
            // record the result, flip done, wake awaiters. drop the coroutine
            // (and its stack) by simply not putting it back.
            let join = {
                let mut slab = lock_slab();
                if let Some(task) = slab.get_mut(id).and_then(|slot| slot.as_mut()) {
                    task.state = RunState::Done;
                    task.result = result;
                    task.join.clone()
                } else {
                    return;
                }
            };
            let (lock, cvar) = &*join;
            let mut j = lock_join(lock);
            j.done = true;
            j.result = result;
            cvar.notify_all();
        }
        CoroutineResult::Yield(()) => {
            // no task yields in P1a, but keep the loop honest: stash the
            // coroutine back and requeue so P2 can add real yielding here.
            {
                let mut slab = lock_slab();
                if let Some(task) = slab.get_mut(id).and_then(|slot| slot.as_mut()) {
                    task.coro = Some(coro);
                    task.state = RunState::Yielded;
                } else {
                    return;
                }
            }
            requeue_yielded(id);
        }
    }
}

/// a yielded task goes back to Ready and re-enters a run queue. unreachable in
/// P1a; present so the worker loop is complete for P2.
fn requeue_yielded(id: TaskId) {
    {
        let mut slab = lock_slab();
        if let Some(task) = slab.get_mut(id).and_then(|slot| slot.as_mut()) {
            task.state = RunState::Ready;
        }
    }
    enqueue(id);
}

/// green spawn: allocate a slab task wrapping the pith closure in a coroutine,
/// enqueue it, and return its `id + 1` handle immediately. mirrors
/// `os_thread_spawn`'s handle contract exactly.
///
/// # Safety
/// `closure_handle` must be a valid pith closure handle or 0.
pub(crate) unsafe fn green_spawn(closure_handle: i64) -> i64 {
    if closure_handle == 0 {
        return 0;
    }
    ensure_workers_started();

    let join = Arc::new((
        Mutex::new(Join {
            done: false,
            result: 0,
        }),
        Condvar::new(),
    ));

    // the coroutine body runs the pith closure exactly as the os-thread backend
    // does: look up the function pointer, transmute to the pith calling
    // convention, call it with the closure handle. it runs on the coroutine's
    // own stack instead of a fresh OS-thread stack; the pith code is identical,
    // so refcount retain/release stays balanced across the boundary.
    let coro = TaskCoroutine::with_stack(
        corosensei::stack::DefaultStack::new(STACK_SIZE).expect("allocate coroutine stack"),
        move |_yielder: &corosensei::Yielder<(), ()>, _input: ()| -> i64 {
            // SAFETY: closure_handle was non-zero at spawn and pith owns the
            // closure's lifetime for the duration of the task; get_fn validates
            // the handle and returns 0 for anything bogus. the transmute matches
            // the pith closure calling convention `extern "C" fn(i64) -> i64`,
            // the same transmute the os-thread backend performs.
            unsafe {
                let func_ptr = crate::pith_closure_get_fn(closure_handle);
                if func_ptr == 0 {
                    return 0;
                }
                let func: extern "C" fn(i64) -> i64 =
                    std::mem::transmute(func_ptr as *const ());
                func(closure_handle)
            }
        },
    );

    let id = {
        let mut slab = lock_slab();
        let id = slab.len();
        slab.push(Some(Task {
            coro: Some(SendCoroutine(coro)),
            closure_handle,
            state: RunState::Ready,
            result: 0,
            join,
        }));
        id
    };

    let task_handle = (id as i64) + 1;
    handle_registry::register_id(task_handle, HandleKind::Task);
    enqueue(id);
    task_handle
}

/// green await (stance (a)): if the task is already done, return its result;
/// otherwise block *this* thread on the task's condvar until a worker completes
/// it. blocking the OS thread — rather than yielding the coroutine — is the P1a
/// simplification that constrains green mode to independent tasks (see module
/// docs). the arc keeps the join state alive even after the slab entry is
/// eventually reused.
///
/// # Safety
/// `task_handle` came from spawn or is garbage; validated against the registry.
pub(crate) unsafe fn green_await(task_handle: i64) -> i64 {
    let join = match join_for(task_handle) {
        Some(j) => j,
        None => return 0,
    };
    let (lock, cvar) = &*join;
    let mut j = lock_join(lock);
    while !j.done {
        j = cvar.wait(j).unwrap_or_else(|p| p.into_inner());
    }
    j.result
}

/// is this green task finished? mirrors the os-thread `pith_task_is_done`.
pub(crate) fn green_is_done(task_handle: i64) -> i64 {
    match join_for(task_handle) {
        Some(join) => {
            let (lock, _) = &*join;
            if lock_join(lock).done {
                1
            } else {
                0
            }
        }
        None => 0,
    }
}

/// detach a green task. worker threads own and drive coroutines regardless of
/// joins, so detach is just a no-op ack for a valid handle (there is no
/// JoinHandle to drop, unlike the os-thread backend). kept for handle-contract
/// parity so `task.detach()` behaves the same under either backend.
pub(crate) fn green_detach(task_handle: i64) {
    let _ = join_for(task_handle);
}

/// resolve a task handle to its shared join arc, or `None` for an invalid or
/// stale handle. the arc clone lets callers wait/inspect without holding the
/// slab lock.
fn join_for(task_handle: i64) -> Option<Arc<(Mutex<Join>, Condvar)>> {
    if !handle_registry::is_valid_id(task_handle, HandleKind::Task) {
        return None;
    }
    let id = (task_handle - 1) as usize;
    let slab = lock_slab();
    slab.get(id)
        .and_then(|slot| slot.as_ref())
        .map(|task| task.join.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    // a coroutine that returns a constant, driven straight through — no pith,
    // no FFI. exercises the corosensei integration in isolation.
    #[test]
    fn coroutine_runs_to_completion() {
        let mut coro: Coroutine<(), (), i64> = Coroutine::new(|_y, _in| 42);
        match coro.resume(()) {
            CoroutineResult::Return(v) => assert_eq!(v, 42),
            CoroutineResult::Yield(()) => panic!("unexpected yield"),
        }
    }
}
