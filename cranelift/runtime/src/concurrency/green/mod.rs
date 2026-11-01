//! green-thread backend, the default on linux (`PITH_GREEN` overrides).
//!
//! many pith tasks run as stackful coroutines on a fixed pool of worker
//! threads. spawning a task is a userspace enqueue, and switching between two
//! tasks on one worker is a stack swap, so a fan-out of thousands of tasks
//! costs a handful of kernel threads.
//!
//! a task that blocks (on a channel, mutex, waitgroup, semaphore or `await`)
//! suspends its coroutine instead of blocking its worker: the block site
//! registers the task as a waiter under its own lock and calls
//! `park_current`, and whoever unblocks it calls `wake`. an os thread that
//! blocks on the same primitive (the main thread, or any thread off the green
//! backend) waits on a condvar instead.
//!
//! ## where things live
//!
//! - `task`: per-task state. a task id names a `SyncBlock` in an arena whose
//!   chunks never move. it holds the scheduling word, the coroutine, the
//!   task's threadlocals and closure, and its join, so a wake finds the task
//!   by index and flips the word with one CAS.
//! - `queues` and `worker`: each worker has a `local` queue of fresh tasks
//!   that peers may steal, a `pinned` queue of woken tasks it resumes itself,
//!   and a `deferred` queue of preempted tasks it runs last. spawns from
//!   outside the pool go to the `injector`.
//! - `wake`: parking, waking and enqueueing. `run`: resuming a task and
//!   recording its completion.
//! - `preempt`: the loop back-edge safe-points and the monitor thread that
//!   asks an overrunning task to yield.
//! - `stack`: the coroutine stack pool. `stats`: `PITH_GREEN_STATS` counters.
//!
//! ## placement
//!
//! a suspended coroutine holds handles and plain values, never a value derived
//! from the thread it ran on (see the accessor notes above `current_task`), so
//! it may resume on any worker. the first worker to run a task becomes its
//! owner, and a wake enqueues it on the owner's pinned queue. two rules move
//! it: a worker that wakes a task parked on another worker takes it
//! (`claim_parked`; `PITH_GREEN_MIGRATE=0` turns this off), and an idle worker
//! takes a queued task off a peer that has been inside one long task for a
//! while (`steal_pinned`). a coroutine changes threads only while it is
//! off-CPU and held by exactly one thread, which is what makes
//! `SendCoroutine` sound.
//!
//! ## lock order
//!
//! a block-site lock (a channel lock, or a task's join lock) is always taken
//! before a scheduler queue lock, never after. `wake` takes only the
//! scheduling word and one queue lock, and a parking task releases its
//! block-site lock before it suspends.

use crate::runtime_core::TlsMap;
use corosensei::{Coroutine, CoroutineResult, Yielder};
use std::cell::{Cell, UnsafeCell};
use std::collections::VecDeque;
use std::ptr;
use std::sync::atomic::{
    AtomicBool, AtomicPtr, AtomicU64, AtomicU8, AtomicUsize, Ordering as AtomicOrdering,
};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Once, OnceLock};
use std::time::{Duration, Instant};

mod fiber;
mod preempt;
mod probe;
mod queues;
mod run;
mod stack;
mod stats;
mod task;
#[cfg(test)]
mod tests;
mod wake;
mod worker;

use fiber::*;
use preempt::*;
use queues::*;
use run::*;
use stack::*;
use stats::*;
use task::*;
use wake::*;
use worker::*;

pub use preempt::{pith_green_maybe_yield, PITH_PREEMPT_REQUESTED};
pub use probe::{encode_probe, pith_green_cross_thread_probe};
pub use stats::note_channel_wake;
pub(crate) use wake::{park_current, take_handoff, wake, wake_pool, wake_with};

/// the whole green runtime: the per-task sync blocks, the per-worker queues
/// and park spots, and the injector for off-worker spawns.
struct Scheduler {
    /// every task's `SyncBlock`, indexed by task id.
    sync: SyncArena,
    workers: Vec<Worker>,
    injector: CacheLine<StealQueue>,
    /// workers looking for fresh work in other workers' queues. a fresh spawn
    /// wakes a parked worker only while this is zero, and the woken worker
    /// starts out counted here, so a burst of spawns wakes one worker rather
    /// than the pool. a searcher that finds work hands the search on to one
    /// more worker if work is left (see `stop_searching`), which spreads a
    /// fan-out across the pool one worker at a time. this is go's spinning-M
    /// protocol.
    searching: CacheLine<AtomicUsize>,
    /// a worker that wakes a task parked on another worker takes it over (see
    /// `claim_parked`); `PITH_GREEN_MIGRATE=0` turns that off. read once,
    /// here, so the wake path pays one load to consult it.
    migrate: bool,
    /// either stats switch is on, so a claimed wake is categorized
    /// (`note_wake`). folded into one flag here for the same reason.
    wake_stats: bool,
}

/// the scheduler is built lazily on the first green spawn so a process that
/// never spawns pays nothing (and, importantly, does not start worker threads).
static SCHEDULER: CacheLine<OnceLock<Scheduler>> = CacheLine(OnceLock::new());

/// a static on a cache line of its own. the linker packs statics together, so
/// without this a lock taken on every few spawns can share a line with a value
/// every worker reads, and each lock then evicts that line from every core.
#[repr(align(64))]
#[derive(Default)]
struct CacheLine<T>(T);

impl<T> std::ops::Deref for CacheLine<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

thread_local! {
    /// set to `Some(i)` on worker thread `i`; `None` on any other thread. green
    /// spawn uses it to route a new task to the current worker's local queue
    /// (spawned from inside a task) versus the global injector (spawned from
    /// `main`). a `Cell` because it is written once at worker startup.
    static CURRENT_WORKER: Cell<Option<usize>> = const { Cell::new(None) };

    /// the worker whose `WorkerCache` this thread owns. set once by
    /// `worker_loop` and never by a thread that only poses as a worker.
    static CACHE_OWNER: Cell<Option<usize>> = const { Cell::new(None) };

    /// raw pointer to the `tls` map of the task this worker is currently
    /// resuming, or null when no green task is running on this thread (between
    /// tasks, or on a non-worker thread). set immediately before `resume` and
    /// restored immediately after, both inside `run_task`. the runtime's
    /// `threadlocal` FFI reads it (via `current_task_tls`) to route accesses to
    /// the running task's own storage instead of the shared per-OS-thread map.
    static CURRENT_TLS: Cell<*mut TlsMap> = const { Cell::new(ptr::null_mut()) };

    /// the id of the green task running on this thread, or `None` between
    /// tasks and on any other thread. installed around `resume` in
    /// `run_task`. a blocking primitive reads it (via `current_task`) to
    /// choose between parking the task and waiting on a condvar, which is
    /// what lets one channel serve both kinds of waiter.
    static CURRENT_TASK: Cell<Option<TaskId>> = const { Cell::new(None) };
}

// ---------------------------------------------------------------------------
// the thread-local accessors, and the rule they enforce.
//
// a coroutine that parks on one worker may be resumed on another (that is
// what task migration is), so no frame that can be on the stack across a park
// may hold the *address* of a thread-local cell. the compiler is the threat,
// not the source. compiled as an ordinary library, a function that touches a
// thread-local fetches the thread's tls base once (`__tls_get_addr`, which
// the linker relaxes to a `%fs:0` load) and adds offsets to it for every
// access in the frame, so a read after a suspend goes through the base of the
// thread that ran the read before it. that was observed when migration was
// first tried: one os thread reporting two different values of
// `CURRENT_WORKER`, and a resumed receiver reading the previous thread's
// handoff and dropping a message.
//
// two things close it, and both are checked on the built archive by
// `tooling/check_tls_barriers.sh`:
//
// 1. the workspace is compiled with `-C relocation-model=pie`
//    (`.cargo/config.toml`). the runtime is only ever linked into an
//    executable, and telling the compiler so switches its thread-local model
//    to local-exec: a plain cell read or write becomes one `%fs:`-relative
//    instruction, the hardware applies the running thread's base on every
//    access, and there is no base register for a frame to keep. every
//    `CURRENT_*` access, the pool's fast-path slot and the collector's flag
//    compile to that shape, which is why the accessors below are ordinary
//    inlinable functions and cost nothing on the hot path.
//
// 2. a cell whose *address* has to exist as a value (the lazily initialized
//    handle behind the pool slot, the `RefCell` maps `TLS_GLOBALS` and
//    `MY_MUTATOR`) is touched only inside a `#[inline(never)]` function that
//    returns before anything can suspend. the optimizer may fold identical
//    address computations within one function; it may not fold them across
//    a call it cannot inline.
//
// what a frame that spans a park holds is then only values. of those, a task
// id and the tls map pointer belong to the task and stay right across a
// migration; a worker index belongs to the thread and must be re-read (call
// the accessor again) after any park.
// ---------------------------------------------------------------------------

/// the id of the green task running on this worker thread, or `None` when
/// no green task is active here (the main thread, a non-worker thread, or the
/// os-thread backend, where nothing ever sets this). every blocking primitive
/// uses it to choose between parking the task and condvar-waiting.
///
/// a task id is task-owned: reading it once at the top of a block site and
/// carrying it across a park is right, and every block site does exactly that.
#[inline]
pub(crate) fn current_task() -> Option<TaskId> {
    CURRENT_TASK.with(|c| c.get())
}

/// the green task running on this thread and the lowest address of its stack
/// mapping, where the guard page starts, or `None` when no task runs here.
/// the stack overflow handler calls this from a signal, so it only reads a
/// thread-local cell, the scheduler's `OnceLock` and the lock-free arena.
#[cfg(not(any(pith_asan, pith_tsan)))]
pub(crate) fn running_task_stack() -> Option<(TaskId, usize)> {
    let id = current_task()?;
    let block = SCHEDULER.get()?.sync.get(id)?;
    Some((id, block.stack_bottom.load(AtomicOrdering::Relaxed)))
}

/// the size of every green task's stack, in bytes.
#[cfg(not(any(pith_asan, pith_tsan)))]
pub(crate) fn task_stack_size() -> usize {
    stack::stack_size()
}

/// the index of the worker this os thread is, or `None` off the pool. the
/// netpoller keys its per-worker pollers by it. a worker index is
/// thread-owned: it must not be carried across a park, and a caller that
/// needs it after one calls this again.
#[inline]
pub(crate) fn current_worker() -> Option<usize> {
    CURRENT_WORKER.with(|c| c.get())
}

/// how many green workers the pool runs. builds the scheduler if needed, but
/// does not start its threads.
pub(crate) fn pool_size() -> usize {
    scheduler().workers.len()
}

/// stamp (or clear) this os thread's worker index. worker startup and the
/// tests that pose as a worker.
#[inline]
fn set_current_worker(index: Option<usize>) {
    CURRENT_WORKER.with(|c| c.set(index));
}

/// install a task's identity on this thread for the duration of a resume,
/// returning what was there before. the two cells are always written
/// together, which is what lets `pith_tls_get_or_init` take the per-task map
/// branch for every green task: a coroutine never runs with `CURRENT_TASK`
/// set and `CURRENT_TLS` null.
#[inline]
fn swap_task_identity(
    task: Option<TaskId>,
    tls: *mut TlsMap,
) -> (Option<TaskId>, *mut TlsMap) {
    let prev_task = CURRENT_TASK.with(|c| c.replace(task));
    let prev_tls = CURRENT_TLS.with(|c| c.replace(tls));
    (prev_task, prev_tls)
}

/// the `tls` map of the green task currently running on this thread, or `None`
/// when no green task is active here. the runtime's `threadlocal` FFI consults
/// this so that, under the green backend, each task reads and writes its own
/// thread-local globals rather than sharing the worker OS thread's map.
///
/// the returned pointer is task-owned: it targets the map in the task's
/// `SyncBlock`, which never moves and which only the running task touches.
/// so it stays right across a park, even when the task resumes on another
/// thread, and the `threadlocal` initializer thunk (pith code, which can
/// park) may run between a read of this and the map insert that follows.
#[inline]
pub(crate) fn current_task_tls() -> Option<*mut TlsMap> {
    let ptr = CURRENT_TLS.with(|c| c.get());
    if ptr.is_null() {
        None
    } else {
        Some(ptr)
    }
}

/// build (once) and return the scheduler struct. this only allocates the
/// queues. it does NOT start worker threads (that would need a reentrant init,
/// since workers call back into `scheduler()`). worker startup is a separate `Once`,
/// driven from the first green spawn. `available_parallelism` picks the worker
/// count, with a floor of 1.
fn scheduler() -> &'static Scheduler {
    SCHEDULER.get_or_init(|| {
        // register the profiling dump once, at first scheduler build, so the
        // breakdown prints as the process exits. no-op when stats are off.
        if stats_on() {
            // SAFETY: `dump_stats` is a plain `extern "C"` fn that only reads
            // atomics and writes to stderr; libc atexit accepts exactly this.
            unsafe {
                libc::atexit(dump_stats);
            }
        }
        let n = worker_count();
        let workers = (0..n)
            .map(|_| Worker {
                local: StealQueue::default(),
                pinned: Mutex::new(PinnedQueue::default()),
                pinned_fifo: AtomicUsize::new(0),
                deferred: Mutex::new(VecDeque::new()),
                cache: OwnerOnly::default(),
                parked: AtomicBool::new(false),
                search_token: AtomicBool::new(false),
                running_task: AtomicUsize::new(0),
                running_since: AtomicU64::new(0),
                resumes: AtomicU64::new(0),
            })
            .collect();
        Scheduler {
            sync: SyncArena::new(),
            workers,
            injector: CacheLine::default(),
            searching: CacheLine(AtomicUsize::new(0)),
            migrate: migration_on(),
            wake_stats: stats_on() || crate::perf_stats_enabled(),
        }
    })
}

/// whether a parked task moves to the worker that wakes it instead of going
/// back to the worker that first ran it. on unless `PITH_GREEN_MIGRATE` is
/// `0` or `false`; see the placement note in `docs/concurrency.md`.
fn migration_on() -> bool {
    std::env::var("PITH_GREEN_MIGRATE")
        .map(|v| v != "0" && v != "false")
        .unwrap_or(true)
}

/// how many worker threads to run: `available_parallelism`, at least 1.
/// `PITH_GREEN_WORKERS` overrides it with a positive count. the tests and
/// benchmarks use it to put many tasks on one worker.
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
/// static safely inside its loop. also starts the preemption monitor (sysmon)
/// thread, so the monitor exists exactly when green workers do.
fn ensure_workers_started() {
    static WORKERS_STARTED: Once = Once::new();
    WORKERS_STARTED.call_once(|| {
        let n = scheduler().workers.len();
        for i in 0..n {
            // the worker's cycle-collector slot is created and registered here,
            // on the spawning thread, so a stop-the-world rendezvous can never
            // scan the gap between the thread starting and the thread
            // registering itself: the slot exists before the thread does.
            let cycle_slot = crate::cycle::mutator_slot_for_spawn();
            if let Err(err) = std::thread::Builder::new()
                .name(format!("pith-green-{i}"))
                .spawn(move || worker_loop(i, cycle_slot))
            {
                runtime_fatal!("could not start green worker {i} of {n}: {err}")
            }
        }
        start_monitor();
    });
}

/// start a task running the pith closure and return its handle (see
/// `make_handle`). the handle is never 0, matching `os_thread_spawn`.
///
/// # Safety
/// `closure_handle` must be a valid pith closure handle or 0.
pub(crate) unsafe fn green_spawn(closure_handle: i64) -> i64 {
    if closure_handle == 0 {
        return 0;
    }
    ensure_workers_started();

    // the coroutine body runs the pith closure exactly as the os-thread backend
    // does: look up the function pointer, transmute to the pith calling
    // convention, call it with the closure handle. it runs on the coroutine's
    // own stack instead of a fresh OS-thread stack; the pith code is identical,
    // so refcount retain/release stays balanced across the boundary.
    let stack = take_stack();
    let bounds = StackBounds::of(&stack);
    let coro = TaskCoroutine::with_stack(
        stack,
        move |yielder: &TaskYielder, _input: ()| -> i64 {
            // publish this coroutine's yielder into the task's own sync block so
            // pith code running on this stack can suspend the task (via
            // park_current). this runs once, on the first resume (the body's
            // top does not run again after a suspend), and the pointer stays
            // valid for the whole life of the body, so once is enough.
            let _finish = publish_yielder(yielder as *const TaskYielder);
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
                    // panic-guard: calling a compiler-emitted closure body through the pith closure abi.
                    std::mem::transmute(func_ptr as *const ());
                func(closure_handle)
            }
        },
    );

    let id = alloc_id();
    let generation = scheduler().sync.ensure(id).arm(SendCoroutine(coro), bounds, closure_handle);
    enqueue_fresh(id);
    make_handle(id, generation)
}

/// green await: return the target task's result, blocking until it completes.
///
/// how it blocks depends on the caller. an os-thread caller (the main thread,
/// or code off the green backend, where `current_task()` is `None`) waits on
/// the block's condvar. a green task must not block its worker, so it adds
/// itself to the join's `awaiters` and parks; the worker is then free to run
/// the very task being awaited, which is what lets a single worker await its
/// own children. `finish_task` wakes both kinds.
///
/// registering happens under the join lock, which `finish_task` also holds to
/// set `done` and drain the list, so the completion either sees the awaiter or
/// has already set `done`. a wake that lands while the awaiter is still
/// `Running` sets the pending flag and is not lost.
///
/// await consumes the handle: the first awaiter to see the result retires the
/// task, and any later await of the same handle returns 0.
///
/// # Safety
/// `task_handle` came from spawn or is garbage; garbage resolves to no task.
pub(crate) unsafe fn green_await(task_handle: i64) -> i64 {
    let Some((id, generation)) = split_handle(task_handle) else {
        return 0;
    };
    let Some(block) = sync_block(id) else {
        return 0;
    };
    let me = current_task();
    let mut join = lock_join(&block.join);
    loop {
        if !join.holds(generation) {
            return 0;
        }
        if join.done {
            break;
        }
        match me {
            None => {
                join.blocked += 1;
                // the wait is a native window for the cycle collector: an
                // os-thread awaiter reads no heap handle until the condvar
                // hands the join lock back.
                join = {
                    let _native = crate::cycle::native_bracket();
                    block.joined.wait(join).unwrap_or_else(|p| p.into_inner())
                };
                join.blocked -= 1;
            }
            Some(me) => {
                join.awaiters.push(me);
                drop(join);
                park_current(me);
                join = lock_join(&block.join);
            }
        }
    }
    let result = join.result;
    join.retire();
    drop(join);
    free_id(id);
    result
}

/// is this green task finished? mirrors the os-thread `pith_task_is_done`.
pub(crate) fn green_is_done(task_handle: i64) -> i64 {
    let Some((id, generation)) = split_handle(task_handle) else {
        return 0;
    };
    let Some(block) = sync_block(id) else {
        return 0;
    };
    let join = lock_join(&block.join);
    i64::from(join.holds(generation) && join.done)
}

/// detach a green task: a promise that no one will await it, so it is retired
/// the moment it finishes instead of holding its block forever. a fire-and-
/// forget server (`spawn handle_conn(...)` in an accept loop) depends on this.
/// a stale or invalid handle is a no-op.
pub(crate) fn green_detach(task_handle: i64) {
    let Some((id, generation)) = split_handle(task_handle) else {
        return;
    };
    let Some(block) = sync_block(id) else {
        return;
    };
    let mut join = lock_join(&block.join);
    if !join.holds(generation) {
        return;
    }
    if join.done {
        join.retire();
        drop(join);
        free_id(id);
    } else {
        join.detached = true;
    }
}

