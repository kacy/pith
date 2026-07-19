//! green-thread backend (experimental, behind `PITH_GREEN`)
//!
//! an M:N scheduler: many pith tasks run as stackful coroutines on a small
//! fixed pool of worker OS threads. spawning a task is a userspace enqueue, not
//! a `std::thread::spawn`, so a fan-out of thousands of independent tasks costs
//! a handful of kernel threads instead of thousands. handing control between
//! tasks on the same worker is a coroutine switch (a stack swap in userspace),
//! not a kernel context switch — that is the win phase 0 measured.
//!
//! ## what P1a set up and what P2 adds
//!
//! P1a ran tasks to completion and joined them, but did NOT make a *blocking*
//! task yield its worker: a task that blocked (on `await`, or on a channel)
//! parked its whole worker OS thread on a condvar. that is small and obviously
//! correct, at one cost — if more tasks are mutually blocked than there are
//! workers, the pool deadlocks, so green mode was valid only for *independent*
//! tasks (fan-out, then join).
//!
//! P2 lifts that restriction for **channels**. a green task that would block on
//! a channel now suspends its coroutine back to the scheduler instead of parking
//! the worker; the worker runs other tasks; a later send/recv re-enqueues the
//! suspended task. so tasks that coordinate through channels (ping-pong,
//! producer/consumer, rings larger than the worker count) run correctly and
//! cheaply. `await` still uses stance (a) — a task that only ever awaits other
//! tasks is fine because the awaited tasks make progress on other workers; the
//! same yield treatment for mutex/waitgroup/semaphore is the next slice (P2b).
//!
//! the park/wake protocol lives partly here (`park_current`, `wake`, the
//! `Parked` state and `wake_pending` flag) and partly in `channel.rs`, which
//! registers a would-block green task as a waiter under the channel lock and
//! then calls `park_current`. see those sites for the race and lock-order notes.
//!
//! ## pinning (what keeps `SendCoroutine` sound under yielding)
//!
//! a *suspended* coroutine holds live pith stack across the suspension, so it
//! must never resume on a different OS thread than it first ran on. we guarantee
//! that by **pinning**: the first worker to resume a task becomes its `owner`;
//! from then on the task is never stealable and every wake re-enqueues it onto
//! its owner worker's private `pinned` queue. only *fresh* (never-resumed) tasks
//! — which capture nothing but a `Send` closure handle — are stealable. so a
//! coroutine carrying live stack is only ever touched by one worker. that, not
//! "nothing yields", is now what makes the `unsafe impl Send for SendCoroutine`
//! sound; see the type doc.
//!
//! ## synchronization model
//!
//! - the task slab (`Vec<Option<Task>>`) is shared across workers under one
//!   mutex. we never hold that lock while *running* a coroutine — running pith
//!   code can spawn more tasks, which needs the same lock. so a worker *takes*
//!   the coroutine out of the slab, drops the lock, resumes it, then re-locks to
//!   record the result. see `run_task`.
//! - each worker owns two run queues: a `local` queue of fresh, stealable tasks,
//!   and a `pinned` queue of woken started tasks that only the owner drains. a
//!   global injector receives fresh spawns from non-worker threads (e.g.
//!   `main`). an idle worker checks its pinned queue, then local, then injector,
//!   then steals from a random peer's *local* queue only. plain `Mutex<VecDeque>`
//!   — readable first; lock-free deques are a later perf phase.
//! - **lock order.** when the channel and scheduler locks nest, the channel lock
//!   is always taken first: a channel send holds the channel lock and calls
//!   `wake`, which takes the slab lock and then a queue lock. run_task and
//!   find_work take slab/queue locks but never a channel lock, and a parking
//!   task *releases* the channel lock before it suspends. so the acquire order
//!   is channel -> slab -> queue, never the reverse — no deadlock cycle.
//! - a task's join state (done flag + result) lives behind its own
//!   `Arc<(Mutex, Condvar)>`, exactly like the os-thread backend. await clones
//!   the arc and waits; the worker flips done + notifies when the coroutine
//!   returns. reusing that shape keeps ARC/refcount balance identical to the
//!   os-thread path — the same pith code runs, retains and releases the same
//!   way, across the coroutine boundary.

use crate::handle_registry::{self, HandleKind};
use corosensei::{Coroutine, CoroutineResult, Yielder};
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::ptr;
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

/// run state of a slab task.
///
/// - `Ready`: sitting in a run queue with its coroutine present, waiting to run.
/// - `Running`: coroutine taken out and being resumed on some worker.
/// - `Parked`: suspended on a channel; coroutine is back in the slab, the task
///   is in no queue, and the channel it parked on owns re-enqueueing it (see the
///   park/wake protocol). only reachable under P2.
/// - `Done`: coroutine returned; result recorded.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunState {
    Ready,
    Running,
    Parked,
    Done,
}

/// the coroutine type every task runs: no input/yield, returns the pith result
/// as an `i64`.
type TaskCoroutine = Coroutine<(), (), i64>;

/// the yielder handed to a task's coroutine body. a green task suspends itself
/// through this (see `park_current`); `Coroutine<(), (), i64>` yields `()`.
type TaskYielder = Yielder<(), ()>;

/// a coroutine wrapped so it can move between the spawning thread and the worker
/// that runs it. corosensei makes `Coroutine` `!Send` because, in general,
/// arbitrary data may live on a *suspended* coroutine's stack and it cannot
/// prove that data is `Send`.
///
/// P2 makes tasks suspend mid-run (a channel op parks the coroutine with live
/// pith stack), so "nothing yields" is no longer the justification. what makes
/// the manual `Send` sound now is **pinning** (see the module doc): a coroutine
/// only ever *carries live stack* while it belongs to one worker. concretely —
///
/// - a *fresh* task's coroutine has never resumed; the only captured state is
///   `closure_handle: i64` (trivially `Send`). fresh tasks are the only ones the
///   scheduler moves between threads (spawn hand-off, work-stealing), and moving
///   a not-yet-started coroutine is sound.
/// - once a task resumes it is pinned to its `owner` worker: it is never stolen,
///   and every wake re-enqueues it onto the owner's private queue, so its
///   coroutine — now possibly holding live, non-`Send` pith locals across a
///   suspension — is only ever resumed and dropped on that one thread.
///
/// so the wrapper is moved across threads only while it is provably
/// stack-empty; the invariant is enforced by the scheduler, not the type system.
struct SendCoroutine(TaskCoroutine);

// SAFETY: soundness rests on the pinning discipline documented on the type and
// in the module header: only fresh (never-resumed, stack-empty, captures a
// `Send` i64) coroutines are ever moved between threads; once a coroutine has
// suspended with live stack it is pinned to its owner worker and never crosses a
// thread boundary again.
unsafe impl Send for SendCoroutine {}

/// a task's yielder pointer, stored in the slab (which is shared across
/// workers). the raw pointer would make `Task` `!Send`; this newtype carries it
/// with a documented `Send`. the value is only ever *dereferenced* on the task's
/// owner worker (via `park_current`), never off-thread — the same pinning
/// invariant that keeps `SendCoroutine` sound — so moving the address between
/// threads is harmless.
struct YielderPtr(*const TaskYielder);

// SAFETY: the pointer is only dereferenced on the owner worker while the task is
// the one running; sending the address itself between threads reads nothing.
unsafe impl Send for YielderPtr {}

/// join channel between a task and whoever awaits it: the done flag plus the
/// result, guarded so an awaiting thread can block on the condvar until the
/// worker records completion. one allocation per task, shared via `Arc`.
struct Join {
    done: bool,
    result: i64,
}

struct Task {
    /// the running coroutine. taken out (`None`) while a worker resumes it, put
    /// back if it parks, dropped when it returns.
    coro: Option<SendCoroutine>,
    /// the pith closure this task runs. kept for reference/debugging; the
    /// coroutine already closed over it.
    #[allow(dead_code)]
    closure_handle: i64,
    state: RunState,
    result: i64,
    /// the worker that first resumed this task, `None` until then. once set the
    /// task is pinned: it is never stolen and every wake re-enqueues it here (see
    /// the pinning note in the module doc). this is what keeps a suspended
    /// coroutine from ever resuming on a different OS thread.
    owner: Option<usize>,
    /// raw pointer to this coroutine's `Yielder`, captured the first time the
    /// body runs (the body writes it into `CURRENT_YIELDER`, which `run_task`
    /// reads back). re-installed before every later resume so a task parked deep
    /// in pith code can find its yielder via `park_current`. null until the body
    /// has run once; only ever dereferenced on the owner worker while this task
    /// is the one running.
    yielder_ptr: YielderPtr,
    /// set by `wake` when a wake arrives while the task is still `Running` (i.e.
    /// mid-suspend, before `run_task` has recorded it as `Parked`). the park path
    /// in `run_task` checks this and re-enqueues instead of parking, closing the
    /// wake/park race so a wake in that window is never lost.
    wake_pending: bool,
    join: Arc<(Mutex<Join>, Condvar)>,
    /// this task's own `threadlocal` module-global storage. under the green
    /// backend one worker OS thread runs many tasks, so the runtime's per-OS-
    /// thread `TLS_GLOBALS` map would be shared between them — task B would see
    /// task A's leftover values. giving each task its own map restores the
    /// "one set of thread-locals per logical thread of execution" semantics.
    ///
    /// boxed on purpose: the `Task` lives in the `Vec` slab, which can reallocate
    /// (and move every `Task`) when another task spawns mid-run. the `Box`'s heap
    /// allocation does not move, so a raw pointer to the map stays valid across
    /// such a realloc. see `run_task` and `current_task_tls`.
    tls: Box<HashMap<i64, i64>>,
}

/// one worker's private run queues. workers are created once and live for the
/// process; there is no shutdown path (the pool dies with the process, like a
/// daemon thread pool).
struct Worker {
    /// fresh, never-resumed tasks. stealable by peers because a fresh coroutine
    /// carries no live stack (see the pinning note).
    local: Mutex<VecDeque<TaskId>>,
    /// woken started tasks pinned to this worker. only this worker drains it, so
    /// a suspended coroutine only ever resumes on the thread that first ran it.
    pinned: Mutex<VecDeque<TaskId>>,
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

    /// raw pointer to the `tls` map of the task this worker is currently
    /// resuming, or null when no green task is running on this thread (between
    /// tasks, or on a non-worker thread). set immediately before `resume` and
    /// restored immediately after, both inside `run_task`. the runtime's
    /// `threadlocal` FFI reads it (via `current_task_tls`) to route accesses to
    /// the running task's own storage instead of the shared per-OS-thread map.
    static CURRENT_TLS: Cell<*mut HashMap<i64, i64>> = const { Cell::new(ptr::null_mut()) };

    /// the slab id of the green task currently running on this worker, or `None`
    /// between tasks and on any non-worker thread. installed around `resume` in
    /// `run_task`. `channel.rs` reads it (via `current_task`) to tell whether it
    /// is running inside a green task — if so it parks by suspending the
    /// coroutine; if not (the main thread, or the os-thread backend) it keeps the
    /// old condvar-wait behavior. this is the seam that lets one channel serve
    /// both kinds of waiter.
    static CURRENT_TASK: Cell<Option<TaskId>> = const { Cell::new(None) };

    /// raw pointer to the `Yielder` of the task currently running on this worker,
    /// or null between tasks. `run_task` installs the task's stored pointer before
    /// each resume; the coroutine body publishes it here on its first run (see
    /// `green_spawn`). `park_current` reads it to suspend the running task from
    /// deep inside pith code.
    static CURRENT_YIELDER: Cell<*const TaskYielder> = const { Cell::new(ptr::null()) };
}

/// the slab id of the green task running on this worker thread, or `None` when
/// no green task is active here (the main thread, a non-worker thread, or the
/// os-thread backend — where nothing ever sets this). `channel.rs` uses it to
/// choose between suspending the coroutine (green) and condvar-waiting (os).
pub(crate) fn current_task() -> Option<TaskId> {
    CURRENT_TASK.with(|c| c.get())
}

/// suspend the currently running green task back to its worker. must be called
/// only from inside a running green task (guarded by a `current_task()` check),
/// and only *after* releasing any channel lock — `suspend` returns control to
/// the worker, which may run other pith code that touches the same channel.
///
/// returns when the task is resumed by a later `wake`.
pub(crate) fn park_current() {
    let yielder = CURRENT_YIELDER.with(|c| c.get());
    debug_assert!(!yielder.is_null(), "park_current with no running coroutine");
    // SAFETY: `yielder` points at the `Yielder` owned by the coroutine that is
    // currently executing on this thread. `run_task` installs it before every
    // resume and it stays valid for the whole time this coroutine is on-CPU,
    // which is exactly the window in which pith code can reach this call. the
    // suspend is a stack switch back to the worker; the pointer is not retained.
    unsafe { &*yielder }.suspend(());
}

/// wake a parked green task: move it back onto its owner worker's queue so it
/// resumes and re-checks the channel condition it blocked on. called by
/// `channel.rs` from a send/recv/close while holding the channel lock, so it may
/// nest the slab and queue locks under the channel lock (see the lock-order note
/// in the module doc). safe to call for a task in any state — it acts only when
/// the task is actually parked, and defers via `wake_pending` if the task is
/// still mid-suspend, so a wake is never lost and never double-enqueued.
pub(crate) fn wake(id: TaskId) {
    let owner = {
        let mut slab = lock_slab();
        let Some(task) = slab.get_mut(id).and_then(|slot| slot.as_mut()) else {
            return;
        };
        match task.state {
            // parked and put back: flip to Ready and re-enqueue below.
            RunState::Parked => {
                task.state = RunState::Ready;
                task.owner
            }
            // still resuming/suspending: it has not yet recorded itself as
            // Parked, so remember the wake; the park path in `run_task` will see
            // this flag and re-enqueue instead of parking.
            RunState::Running => {
                task.wake_pending = true;
                return;
            }
            // already queued to re-check, or finished: nothing to do.
            RunState::Ready | RunState::Done => return,
        }
    };
    // enqueue outside the slab lock. a parked task always ran, so `owner` is set;
    // fall back to the injector only defensively.
    match owner {
        Some(w) => enqueue_woken(w, id),
        None => enqueue_fresh(id),
    }
}

/// the `tls` map of the green task currently running on this thread, or `None`
/// when no green task is active here. the runtime's `threadlocal` FFI consults
/// this so that, under the green backend, each task reads and writes its own
/// thread-local globals rather than sharing the worker OS thread's map.
///
/// the returned pointer is only valid to dereference synchronously, from this
/// same thread, before control returns to the scheduler — that is, from within
/// the pith call that produced this access. `run_task` guarantees the pointed-to
/// `Box` outlives the whole `resume`, and only this worker thread ever touches
/// its own current task, so there is no aliasing across threads.
pub(crate) fn current_task_tls() -> Option<*mut HashMap<i64, i64>> {
    let ptr = CURRENT_TLS.with(|c| c.get());
    if ptr.is_null() {
        None
    } else {
        Some(ptr)
    }
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
        let n = worker_count();
        let workers = (0..n)
            .map(|_| Worker {
                local: Mutex::new(VecDeque::new()),
                pinned: Mutex::new(VecDeque::new()),
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

/// how many worker threads to run. defaults to `available_parallelism` (floor
/// 1). `PITH_GREEN_WORKERS`, when set to a positive integer, overrides it — a
/// testing/experimental knob: pinning it to 1 forces many tasks to share one
/// worker, which is how the coordination tests provoke the pre-P2 deadlock
/// deterministically and how the ping-pong benchmark shows userspace-only task
/// handoffs. not a documented user setting.
fn worker_count() -> usize {
    if let Ok(raw) = std::env::var("PITH_GREEN_WORKERS") {
        if let Ok(n) = raw.trim().parse::<usize>() {
            if n >= 1 {
                return n;
            }
        }
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
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

/// push a *fresh* (never-resumed) task onto a stealable queue and wake one idle
/// worker. spawns from a worker go to that worker's local queue (cache-friendly,
/// keeps a task's children near it); spawns from elsewhere go to the injector.
/// only fresh tasks travel this path, so anything a peer steals from a `local`
/// queue is stack-empty and safe to move across threads.
fn enqueue_fresh(id: TaskId) {
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

/// re-enqueue a *woken started* task onto its owner worker's pinned queue and
/// wake the pool. this is the only path a task that has already resumed takes
/// back into scheduling, and it always targets the owner — never the waker's
/// worker — so a suspended coroutine only ever resumes on the thread that first
/// ran it (see the pinning note). the pinned queue is not stolen from.
fn enqueue_woken(owner: usize, id: TaskId) {
    let sched = scheduler();
    sched.workers[owner]
        .pinned
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push_back(id);
    // the owner may be parked; wake the pool so it comes back and drains its
    // pinned queue. notify_all + the re-check in `park` tolerates extra wakeups.
    sched.park_cv.notify_all();
}

/// find the next ready task for worker `index`: its pinned queue first (resume
/// suspended work it owns), then its own local queue, then the injector, then a
/// steal from a random peer's local queue. returns `None` if the whole pool is
/// momentarily empty.
fn find_work(index: usize) -> Option<TaskId> {
    let sched = scheduler();

    // pinned first: these are woken tasks this worker must resume itself.
    if let Some(id) = sched.workers[index]
        .pinned
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .pop_front()
    {
        return Some(id);
    }

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
    sched.workers.iter().any(|w| {
        !w.local.lock().unwrap_or_else(|p| p.into_inner()).is_empty()
            || !w.pinned.lock().unwrap_or_else(|p| p.into_inner()).is_empty()
    })
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
/// back (parked).
fn run_task(id: TaskId) {
    // take the coroutine out under the lock, mark it running, pin it to this
    // worker on first resume, and grab a raw pointer to this task's boxed
    // thread-local map plus its stored yielder pointer. all captured here, while
    // we still hold the lock, from a stable view of the slab; the `Box` the tls
    // pointer points into does not move even if the slab reallocates later (see
    // the `tls` field doc).
    let (mut coro, tls_ptr, yielder_ptr) = {
        let mut slab = lock_slab();
        match slab.get_mut(id).and_then(|slot| slot.as_mut()) {
            Some(task) if task.state == RunState::Ready => {
                task.state = RunState::Running;
                // pin on first resume: whoever runs it first owns it forever.
                if task.owner.is_none() {
                    task.owner = CURRENT_WORKER.with(|c| c.get());
                }
                let tls_ptr: *mut HashMap<i64, i64> = &mut *task.tls;
                let yielder_ptr = task.yielder_ptr.0;
                match task.coro.take() {
                    Some(c) => (c, tls_ptr, yielder_ptr),
                    // a Ready task with no coroutine should be impossible; skip
                    // defensively rather than panic on a corrupt slot.
                    None => return,
                }
            }
            // already running/parked/done/absent: nothing to do.
            _ => return,
        }
    };

    // install this task's thread-local map for the duration of the resume, then
    // restore whatever was there before (null in the common case; save/restore
    // keeps us correct even if resumes ever nest on one worker).
    //
    // SAFETY: `tls_ptr` points into the `Box<HashMap>` owned by this task's slab
    // entry. nothing removes a task from the slab (it only grows), the entry is
    // not dropped while this coroutine runs, and the `Box` heap allocation is
    // stable across slab reallocation — so the pointer is valid for the whole
    // `resume` call below. it is only ever dereferenced by the runtime's
    // `threadlocal` FFI, synchronously on this same worker thread while this task
    // is the one running, so no other thread and no other task aliases the map.
    // install this task's identity for the duration of the resume: its tls map
    // (P1b), its slab id (so channel.rs knows which task to park), and its
    // yielder pointer (so park_current can suspend it). all restored afterward.
    let prev_tls = CURRENT_TLS.with(|c| c.replace(tls_ptr));
    let prev_task = CURRENT_TASK.with(|c| c.replace(Some(id)));
    let prev_yielder = CURRENT_YIELDER.with(|c| c.replace(yielder_ptr));

    // run outside the lock. SAFETY of the transmute-and-call lives inside the
    // coroutine body (see green_spawn); here we just drive the coroutine.
    let outcome = coro.0.resume(());

    // on its first resume the body published its yielder pointer here; read it
    // back so we can re-install it on later resumes (the body's top does not run
    // again after a suspend).
    let new_yielder = CURRENT_YIELDER.with(|c| c.get());

    CURRENT_YIELDER.with(|c| c.set(prev_yielder));
    CURRENT_TASK.with(|c| c.set(prev_task));
    CURRENT_TLS.with(|c| c.set(prev_tls));

    match outcome {
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
            // the task parked on a channel (the only reason a green task yields).
            // put the coroutine back and record where its yielder lives. then, if
            // a wake landed while we were still Running (the wake/park race), the
            // channel already wants us runnable — re-enqueue instead of parking.
            // otherwise settle into Parked and let the channel re-enqueue us on a
            // later send/recv.
            let requeue_owner = {
                let mut slab = lock_slab();
                if let Some(task) = slab.get_mut(id).and_then(|slot| slot.as_mut()) {
                    task.coro = Some(coro);
                    task.yielder_ptr = YielderPtr(new_yielder);
                    if task.wake_pending {
                        task.wake_pending = false;
                        task.state = RunState::Ready;
                        Some(task.owner)
                    } else {
                        task.state = RunState::Parked;
                        None
                    }
                } else {
                    return;
                }
            };
            // enqueue outside the slab lock to keep the slab -> queue order clean.
            if let Some(owner) = requeue_owner {
                match owner {
                    Some(w) => enqueue_woken(w, id),
                    None => enqueue_fresh(id),
                }
            }
        }
    }
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
        move |yielder: &TaskYielder, _input: ()| -> i64 {
            // publish this coroutine's yielder so pith code running on its stack
            // can suspend the task (via park_current). this runs once, on the
            // first resume; run_task reads it back and re-installs it on later
            // resumes, since the body's top does not run again after a suspend.
            // the pointer stays valid for the whole life of the body.
            CURRENT_YIELDER.with(|c| c.set(yielder as *const TaskYielder));
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
            owner: None,
            yielder_ptr: YielderPtr(ptr::null()),
            wake_pending: false,
            join,
            // fresh, empty thread-local storage: this task starts with no
            // threadlocal slots materialized, exactly like a brand-new OS thread.
            tls: Box::new(HashMap::new()),
        }));
        id
    };

    let task_handle = (id as i64) + 1;
    handle_registry::register_id(task_handle, HandleKind::Task);
    enqueue_fresh(id);
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
