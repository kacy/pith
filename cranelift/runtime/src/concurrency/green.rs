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
//! cheaply. P2b extends the same yield treatment to mutex/waitgroup/semaphore.
//!
//! P2c closes the last hard-park: **`await`**. a green task that awaits another
//! task now registers on the target's join-waiter list and suspends its
//! coroutine instead of parking the worker (see `green_await`); the completing
//! task wakes it the same way a channel send wakes a parked receiver. so a green
//! coordinator can fan out green children and await them from inside a task —
//! before, that hung at one worker, because awaiting the first child parked the
//! only worker and the child could never run. an os-thread awaiter (the main
//! thread, or the flag-off backend) still condvar-waits, byte for byte.
//!
//! the park/wake protocol lives partly here (`park_current`, `wake`, the
//! `Parked` state and `wake_pending` flag) and partly at the block sites:
//! `channel.rs` registers a would-block green task as a waiter under the channel
//! lock and then calls `park_current`, and `green_await` does the same under the
//! join lock. see those sites for the race and lock-order notes.
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
//! - the task slab (a generational slotmap, see `Slab`) is shared across workers
//!   under one mutex. we never hold that lock while *running* a coroutine — running pith
//!   code can spawn more tasks, which needs the same lock. so a worker *takes*
//!   the coroutine out of the slab, drops the lock, resumes it, then re-locks to
//!   record the result. see `run_task`.
//! - each worker owns three run queues: a `local` queue of fresh, stealable
//!   tasks; a `pinned` queue of woken started tasks that only the owner drains;
//!   and a `deferred` queue of *preempted* started tasks (P5), also owner-only but
//!   drained at the lowest priority. a global injector receives fresh spawns from
//!   non-worker threads (e.g. `main`). an idle worker checks pinned, then local,
//!   then injector, then steals from a random peer's *local* queue, and only if
//!   all of that is empty does it resume a `deferred` (preempted) task — so a
//!   compute hog that overran its quantum yields to every other ready task before
//!   it runs again. plain `Mutex<VecDeque>` — readable first; lock-free deques are
//!   a later perf phase.
//! - **lock order.** when a block-site lock (a channel lock, or a task's join
//!   lock) and the scheduler locks nest, the block-site lock is always taken
//!   first: a channel send holds the channel lock and calls `wake`, which takes
//!   the slab lock and then a queue lock. run_task and find_work take slab/queue
//!   locks but never a block-site lock, and a parking task *releases* its
//!   block-site lock before it suspends. `green_await` obeys the same rule: it
//!   holds a task's join lock only to check `done` and register a waiter, and
//!   releases it before suspending. so the acquire order is block-site -> slab ->
//!   queue, never the reverse — no deadlock cycle.
//! - a task's join state (done flag + result + green-waiter list) lives behind
//!   its own `Arc<(Mutex, Condvar)>`, exactly like the os-thread backend. await
//!   clones the arc; an os-thread awaiter waits on the condvar, a green awaiter
//!   registers on the waiter list and suspends. the worker flips done, notifies
//!   the condvar, and wakes the green waiters when the coroutine returns. reusing
//!   that shape keeps ARC/refcount balance identical to the os-thread path — the
//!   same pith code runs, retains and releases the same way, across the coroutine
//!   boundary.

use crate::handle_registry::{self, HandleKind};
use corosensei::{Coroutine, CoroutineResult, Yielder};
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::ptr;
use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Once, OnceLock};
use std::time::Duration;

// ---------------------------------------------------------------------------
// opt-in scheduler-locality profiling, gated behind `PITH_GREEN_STATS=1`. it
// counts how wakes break down for a run — same-worker vs cross-worker vs reactor
// handoffs — and how many actually cost a futex wake (the target's owner was
// parked) vs were absorbed by a busy pool. the dump prints once at process exit.
// default-off and read-only, so the normal path never pays for it; this is the
// lens that showed the pinned-wake herd, kept for the next scheduler pass.
// ---------------------------------------------------------------------------
static STATS_ON: OnceLock<bool> = OnceLock::new();
static WAKE_SAME: AtomicU64 = AtomicU64::new(0);
static WAKE_CROSS: AtomicU64 = AtomicU64::new(0);
static WAKE_REACTOR: AtomicU64 = AtomicU64::new(0);
static WAKE_FUTEX: AtomicU64 = AtomicU64::new(0);
static WAKE_ABSORBED: AtomicU64 = AtomicU64::new(0);
static PARKED_WORKERS: AtomicUsize = AtomicUsize::new(0);

fn stats_on() -> bool {
    *STATS_ON.get_or_init(|| {
        std::env::var("PITH_GREEN_STATS")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false)
    })
}

extern "C" fn dump_stats() {
    let same = WAKE_SAME.load(AtomicOrdering::Relaxed);
    let cross = WAKE_CROSS.load(AtomicOrdering::Relaxed);
    let reactor = WAKE_REACTOR.load(AtomicOrdering::Relaxed);
    let futex = WAKE_FUTEX.load(AtomicOrdering::Relaxed);
    let absorbed = WAKE_ABSORBED.load(AtomicOrdering::Relaxed);
    eprintln!(
        "[green-stats] wakes: same-worker={same} cross-worker={cross} reactor={reactor} \
         | futex-wakes={futex} absorbed={absorbed}"
    );
}

// ---------------------------------------------------------------------------
// cooperative preemption (P5). a compute-only green task that never blocks would
// otherwise hold its worker forever and starve every other task pinned to it.
// the fix is a safe-point at every loop back-edge: the cranelift backend emits,
// before the branch, a single load of `PITH_PREEMPT_REQUESTED` and — only when
// it is set — a call to `pith_green_maybe_yield`. a `sysmon` thread (see
// `monitor_loop`) watches the workers and sets the flag when a task outruns its
// quantum. the flag is process-global and the epoch is a coarse tick, so the hot
// path (flag clear) is one relaxed i8 load and a predicted-not-taken branch, no
// call and no clock syscall.
// ---------------------------------------------------------------------------

/// process-global preemption request flag. the monitor sets it to 1 when some
/// worker's running green task has outrun its quantum; the JIT-emitted
/// safe-points at loop back-edges load it inline and, only when it is nonzero,
/// call `pith_green_maybe_yield`. exported unmangled so the cranelift backend can
/// import it as an external data symbol and load it directly from generated code.
///
/// it is only ever set by the monitor thread, which runs only under the green
/// backend. under the os-thread backend the monitor never starts, so the flag
/// stays 0 and the inline safe-point branch is never taken — and even if it were,
/// `pith_green_maybe_yield` is a no-op off a green task (see there). that is what
/// makes emitting the check into all code harmless off-green.
#[no_mangle]
pub static PITH_PREEMPT_REQUESTED: AtomicU8 = AtomicU8::new(0);

/// coarse monotonic tick, bumped once per monitor pass (~10 ms). a worker stamps
/// its `running_since` with this value when it resumes a task; the monitor and
/// `pith_green_maybe_yield` compare against it to decide whether a task has
/// overrun. a tick rather than a wall clock keeps the hot resume path to a single
/// relaxed load instead of a clock syscall.
static PREEMPT_EPOCH: AtomicU64 = AtomicU64::new(0);

/// how many monitor epochs a task may run before it is asked to yield. with the
/// ~10 ms monitor pass, one epoch is roughly a 10-20 ms slice — the same ballpark
/// as go's 10 ms preemption quantum. small enough to keep a compute task from
/// starving its peers, large enough that ordinary cooperative code (which yields
/// far more often) never trips it.
const QUANTUM_EPOCHS: u64 = 1;

/// how often the monitor thread wakes to bump the epoch and scan the workers.
const MONITOR_INTERVAL: Duration = Duration::from_millis(10);

/// has the task the given worker is running exceeded its quantum as of `now`?
/// factored out so both the monitor and `pith_green_maybe_yield` share one
/// definition of "overrun", and so the epoch arithmetic is unit-testable.
fn task_overrun(running_since: u64, now: u64) -> bool {
    now.saturating_sub(running_since) >= QUANTUM_EPOCHS
}

/// safe-point slow path, called from JIT-emitted code at a loop back-edge only
/// when `PITH_PREEMPT_REQUESTED` is set. three cheap gates, in order:
///
/// 1. not inside a green task (os-thread backend, the main thread, between tasks)
///    → return immediately. this is the hard no-op that makes the flag-off and
///    os-thread paths safe even though the check is emitted into all code.
/// 2. inside a green task but this task has not actually outrun its quantum →
///    return. the flag is process-global and coarse, so a set flag wakes *every*
///    running task at its next back-edge; this gate absorbs those spurious wakes
///    so only the genuinely-overrunning task yields.
/// 3. overrun → set `wake_pending` so `run_task`'s yield path re-enqueues us onto
///    our owner worker (pinning holds — we resume on the same worker), then
///    `park_current` to suspend the coroutine back to the scheduler. this reuses
///    the P2 park/wake protocol wholesale; no new scheduler path is added.
///
/// # Safety
/// must be called only from generated pith code running on a worker thread, the
/// same contract as any other runtime FFI reached from JIT code.
#[no_mangle]
pub extern "C" fn pith_green_maybe_yield() {
    // gate 1: are we inside a green task at all?
    let Some(id) = current_task() else {
        return;
    };
    let Some(worker_index) = CURRENT_WORKER.with(|c| c.get()) else {
        return;
    };

    // gate 2: has *this* task actually outrun its quantum? read our worker's
    // running_since and compare to the current epoch; a task that just started
    // (or a spurious flag set) fails this and returns without yielding.
    let now = PREEMPT_EPOCH.load(AtomicOrdering::Relaxed);
    let running_since = scheduler().workers[worker_index]
        .running_since
        .load(AtomicOrdering::Relaxed);
    if !task_overrun(running_since, now) {
        return;
    }

    // gate 3: we are overrunning. flag ourselves for re-enqueue *before* parking,
    // so run_task's yield path re-enqueues us instead of leaving us parked with no
    // block site to wake us. we use a distinct `preempt_pending` flag (not the
    // channel/await `wake_pending`) so run_task routes us onto our owner's
    // lowest-priority `deferred` queue: a preempted compute task must yield to
    // every other ready task on the worker, or it would simply be re-selected and
    // keep monopolizing the worker (`pinned`/woken work is served first). nothing
    // else writes this flag and preemption registers no waiter, so there is no
    // wake/park race to close here.
    {
        let mut slab = lock_slab();
        if let Some(task) = slab.get_mut(id) {
            task.preempt_pending = true;
        } else {
            return;
        }
    }
    park_current();
}

/// index into the task slab.
///
/// the pith-facing task *handle* is not the raw index: it packs the index
/// together with the slot's generation (see `make_handle`). internally, though,
/// the scheduler passes bare indices around — a live task's index is always
/// valid while it is queued or running, and a slot is only ever reclaimed once
/// its task is `Done` and referenced by nobody, so no internal `TaskId` ever
/// goes stale. only the handle boundary (`green_await`, `green_is_done`,
/// `green_detach`, `join_for`) needs the generation check.
type TaskId = usize;

/// low 31 bits — the generation lives in bits 32..62 of the handle, leaving the
/// sign bit clear so a handle is always a positive `i64` (0 stays "no task").
const GEN_MASK: u32 = 0x7fff_ffff;

/// pack a slab index and its generation into the pith-facing task handle. the
/// low 32 bits hold `index + 1` (so index 0 still yields a nonzero handle and 0
/// stays reserved for "no task"); the generation sits above. for a slot's first
/// use the generation is 0, so the handle is exactly `index + 1` — identical to
/// the old encoding and to the os-thread backend.
fn make_handle(index: TaskId, generation: u32) -> i64 {
    (((generation & GEN_MASK) as u64) << 32 | ((index as u64) + 1)) as i64
}

/// split a task handle back into `(index, generation)`, or `None` for 0/garbage.
fn split_handle(handle: i64) -> Option<(TaskId, u32)> {
    if handle <= 0 {
        return None;
    }
    let bits = handle as u64;
    let low = bits & 0xffff_ffff;
    if low == 0 {
        return None;
    }
    Some(((low - 1) as TaskId, (bits >> 32) as u32 & GEN_MASK))
}

/// one slab entry: a task plus the generation stamp that makes its handle
/// unique across slot reuse. reclaiming a done task drops the `Task` (freeing
/// its tls map and join reference) and bumps `generation`, so any handle still
/// naming this slot no longer resolves — a late await returns 0 instead of
/// reading whatever task later reused the slot.
struct Slot {
    task: Option<Task>,
    generation: u32,
}

/// the task slab: a generational slotmap. `entries` grows on demand; `free`
/// holds the indices of reclaimed (done, unreferenced) slots ready for the next
/// spawn. without reclamation this vector grew by one entry per task ever
/// spawned and never shrank — an unbounded leak on a long-running server that
/// fans out one task per request. reusing a slot bumps its generation so a
/// stale handle can never alias the task that takes its place.
struct Slab {
    entries: Vec<Slot>,
    free: Vec<TaskId>,
}

impl Slab {
    fn new() -> Self {
        Slab {
            entries: Vec::new(),
            free: Vec::new(),
        }
    }

    /// place `task` in a slot and return its `(index, generation)`. reuses a
    /// reclaimed index when one is free, otherwise appends a fresh slot.
    fn insert(&mut self, task: Task) -> (TaskId, u32) {
        if let Some(index) = self.free.pop() {
            let slot = &mut self.entries[index];
            debug_assert!(slot.task.is_none(), "a free slot must be empty");
            slot.task = Some(task);
            (index, slot.generation)
        } else {
            let index = self.entries.len();
            self.entries.push(Slot {
                task: Some(task),
                generation: 0,
            });
            (index, 0)
        }
    }

    // used by the debug-only stealable assertion and the tests; a pure release
    // build has neither, so allow it to look unused there.
    #[allow(dead_code)]
    fn get(&self, id: TaskId) -> Option<&Task> {
        self.entries.get(id).and_then(|slot| slot.task.as_ref())
    }

    fn get_mut(&mut self, id: TaskId) -> Option<&mut Task> {
        self.entries.get_mut(id).and_then(|slot| slot.task.as_mut())
    }

    /// resolve a handle-checked task: the slot must both exist and still carry
    /// `generation`, or this returns `None` (a stale handle to a reused slot).
    fn get_checked(&self, id: TaskId, generation: u32) -> Option<&Task> {
        let slot = self.entries.get(id)?;
        if slot.generation != generation {
            return None;
        }
        slot.task.as_ref()
    }

    /// reclaim a finished task's slot, but only if it still carries
    /// `expected_gen` and is actually `Done`. dropping the `Task` frees its tls
    /// map and join reference; bumping the generation invalidates the handle and
    /// the freed index is queued for reuse. the generation gate means that when
    /// several awaiters race, exactly the first reclaims and the rest no-op.
    /// returns `true` when it reclaimed.
    fn reclaim(&mut self, id: TaskId, expected_gen: u32) -> bool {
        let Some(slot) = self.entries.get_mut(id) else {
            return false;
        };
        if slot.generation != expected_gen {
            return false;
        }
        let done = matches!(slot.task.as_ref(), Some(t) if t.state == RunState::Done);
        if !done {
            return false;
        }
        slot.task = None;
        slot.generation = slot.generation.wrapping_add(1) & GEN_MASK;
        self.free.push(id);
        true
    }

    /// reclaim a just-finished task that was detached while it ran, returning
    /// the handle that was registered so the caller can unregister it. a no-op
    /// (returns `None`) unless the task is `Done` and flagged `reclaim_on_done`.
    fn reclaim_if_detached(&mut self, id: TaskId) -> Option<i64> {
        let slot = self.entries.get_mut(id)?;
        let task = slot.task.as_ref()?;
        if !(task.reclaim_on_done && task.state == RunState::Done) {
            return None;
        }
        let handle = make_handle(id, slot.generation);
        slot.task = None;
        slot.generation = slot.generation.wrapping_add(1) & GEN_MASK;
        self.free.push(id);
        Some(handle)
    }

    /// detach a task: reclaim it immediately if it is already done, otherwise
    /// flag it so `finish_task` reclaims it on completion. only acts on the slot
    /// that still carries `generation`. returns the handle to unregister when it
    /// reclaimed right now, else `None`.
    fn detach(&mut self, id: TaskId, generation: u32) -> Option<i64> {
        let slot = self.entries.get_mut(id)?;
        if slot.generation != generation {
            return None;
        }
        let task = slot.task.as_mut()?;
        if task.state == RunState::Done {
            let handle = make_handle(id, slot.generation);
            slot.task = None;
            slot.generation = slot.generation.wrapping_add(1) & GEN_MASK;
            self.free.push(id);
            Some(handle)
        } else {
            task.reclaim_on_done = true;
            None
        }
    }
}

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
/// - `Parked`: suspended on a blocking op (a channel, a P2b primitive, or an
///   await); coroutine is back in the slab, the task is in no queue, and the
///   block site it parked on owns re-enqueueing it (see the park/wake protocol).
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
///
/// a task may be awaited by an os-thread caller (the main thread, or the whole
/// os-thread backend), a green task, or both at once. an os-thread awaiter blocks
/// on the condvar; a green awaiter must not park its worker, so it registers its
/// slab id in `green_waiters` and suspends its coroutine instead. when the task
/// completes it flips `done` + `result`, `notify_all`s the condvar (os-thread
/// awaiters), and drains `green_waiters` to `green::wake` each green awaiter. the
/// list is empty (and so free) whenever no green task is awaiting — the os-thread
/// path is untouched. mirrors the channel's green-waiter list, guarded by this
/// same join mutex the way the channel's list is guarded by the channel lock.
struct Join {
    done: bool,
    result: i64,
    green_waiters: Vec<TaskId>,
}

struct Task {
    /// the running coroutine. taken out (`None`) while a worker resumes it, put
    /// back if it parks, dropped when it returns.
    coro: Option<SendCoroutine>,
    /// the pith closure this task runs. the coroutine already closed over it to
    /// call it; we keep the handle so `finish_task` can release the closure's one
    /// owning reference once the body returns (see there).
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
    /// set by `pith_green_maybe_yield` right before it parks a task that has
    /// overrun its quantum. distinct from `wake_pending`: `run_task`'s yield path
    /// routes a preempted task onto its owner's lowest-priority `deferred` queue
    /// (see `enqueue_preempted`) so it yields to all other ready work, whereas a
    /// woken task goes onto the higher-priority `pinned` queue to resume promptly.
    preempt_pending: bool,
    /// set by `green_detach` when a task is detached while still running: no one
    /// will ever await it, so `finish_task` reclaims its slab slot the moment it
    /// completes rather than leaking it for the life of the process. an awaited
    /// task is instead reclaimed by `green_await` once it reads the result.
    reclaim_on_done: bool,
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

/// one worker's private run queues plus its own park spot. workers are created
/// once and live for the process; there is no shutdown path (the pool dies with
/// the process, like a daemon thread pool).
struct Worker {
    /// fresh, never-resumed tasks. stealable by peers because a fresh coroutine
    /// carries no live stack (see the pinning note).
    local: Mutex<VecDeque<TaskId>>,
    /// woken started tasks pinned to this worker. only this worker drains it, so
    /// a suspended coroutine only ever resumes on the thread that first ran it.
    pinned: Mutex<VecDeque<TaskId>>,
    /// preempted started tasks pinned to this worker, drained at the *lowest*
    /// priority (after pinned, local, injector, and stealing). a compute task that
    /// overran its quantum lands here so it yields to every other ready task on the
    /// worker; without this it would sit at the front of `pinned` and simply be
    /// re-selected, right back into the loop that got it preempted. also pinned to
    /// the owner, so a suspended coroutine still never crosses a worker.
    deferred: Mutex<VecDeque<TaskId>>,
    /// this worker's private park spot. an idle worker waits on its own condvar,
    /// so a wake can target exactly the worker that is able to run the task —
    /// crucially, a *pinned* wake nudges only the task's owner instead of a
    /// pool-wide notify that also wakes idle peers which have nothing to run. on
    /// a single-connection pipeline every task pins to one worker, so a shared
    /// park spot made every wake needlessly wake (and CPU-spin, and lock-contend)
    /// the other worker — that thundering herd is what made two workers slower
    /// than one. the guarded unit plus the short wait-timeout in `park` bounds any
    /// lost-wakeup window exactly as the old single spot did.
    park_lock: Mutex<()>,
    park_cv: Condvar,
    /// the slab id (`id + 1`, 0 = none) of the task this worker is currently
    /// resuming. written right before `resume` and cleared right after, so the
    /// monitor thread can spot a task that has been on-CPU too long. the monitor
    /// only ever reads this (and `running_since`) via atomics — it never touches a
    /// queue or coroutine — so it cannot race the worker over shared structure.
    running_task: AtomicUsize,
    /// the epoch (`PREEMPT_EPOCH`) at which this worker began resuming its current
    /// task. the monitor and `pith_green_maybe_yield` compare it against the
    /// current epoch to detect a quantum overrun.
    running_since: AtomicU64,
}

/// the whole green runtime: the task slab, the per-worker queues + park spots,
/// and the injector for off-worker spawns.
struct Scheduler {
    slab: Mutex<Slab>,
    workers: Vec<Worker>,
    injector: Mutex<VecDeque<TaskId>>,
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
        let Some(task) = slab.get_mut(id) else {
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
    // profiling: categorize this wake by where the waker sits relative to the
    // task's owner worker (read-only, gated).
    if stats_on() {
        let caller = CURRENT_WORKER.with(|c| c.get());
        match (caller, owner) {
            (Some(w), Some(o)) if w == o => WAKE_SAME.fetch_add(1, AtomicOrdering::Relaxed),
            (Some(_), Some(_)) => WAKE_CROSS.fetch_add(1, AtomicOrdering::Relaxed),
            // waker is not a worker: the reactor thread or main. either way it
            // must cross into a worker's queue.
            _ => WAKE_REACTOR.fetch_add(1, AtomicOrdering::Relaxed),
        };
    }

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

fn lock_slab() -> MutexGuard<'static, Slab> {
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
                local: Mutex::new(VecDeque::new()),
                pinned: Mutex::new(VecDeque::new()),
                deferred: Mutex::new(VecDeque::new()),
                park_lock: Mutex::new(()),
                park_cv: Condvar::new(),
                running_task: AtomicUsize::new(0),
                running_since: AtomicU64::new(0),
            })
            .collect();
        Scheduler {
            slab: Mutex::new(Slab::new()),
            workers,
            injector: Mutex::new(VecDeque::new()),
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
/// static safely inside its loop. also starts the preemption monitor (sysmon)
/// thread, so the monitor exists exactly when — and only when — green workers do.
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
        start_monitor();
    });
}

/// start the preemption monitor (sysmon) thread. it is the only thing that ever
/// sets `PITH_PREEMPT_REQUESTED`, and it starts only here alongside the green
/// workers — so under the os-thread backend it never runs and the flag stays 0.
fn start_monitor() {
    std::thread::Builder::new()
        .name("pith-green-sysmon".to_string())
        .spawn(monitor_loop)
        .expect("spawn green monitor");
}

/// the monitor loop: every `MONITOR_INTERVAL`, bump the epoch and scan the
/// workers. if any worker's running task has outrun its quantum, request a
/// preemption; otherwise clear the request. it only ever reads/writes atomics —
/// each worker's `running_task`/`running_since` and the global flag/epoch — and
/// never touches a queue or a coroutine, so it cannot race the workers over
/// shared structure. the request is coarse (one process-global flag): a set flag
/// nudges *every* running task at its next back-edge, and each task's own
/// `pith_green_maybe_yield` decides whether it is the one that must yield.
fn monitor_loop() {
    let sched = scheduler();
    loop {
        std::thread::sleep(MONITOR_INTERVAL);
        // advance time by one tick. workers stamp `running_since` with the epoch
        // as they resume, so a task that spans a tick boundary shows an overrun.
        let now = PREEMPT_EPOCH.fetch_add(1, AtomicOrdering::Relaxed) + 1;

        let mut any_overrun = false;
        for worker in &sched.workers {
            // a running task shows a nonzero id here; 0 means the worker is idle
            // or between tasks, nothing to preempt.
            if worker.running_task.load(AtomicOrdering::Relaxed) == 0 {
                continue;
            }
            let running_since = worker.running_since.load(AtomicOrdering::Relaxed);
            if task_overrun(running_since, now) {
                any_overrun = true;
                break;
            }
        }

        // set when something is overrunning, clear otherwise. clearing each quiet
        // pass keeps a stale request from waking tasks after the offender yields.
        PITH_PREEMPT_REQUESTED.store(u8::from(any_overrun), AtomicOrdering::Relaxed);
    }
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
    // a fresh task is stealable by any worker, so nudge the whole pool: whoever
    // is idle picks it up (or steals it), which is what keeps an independent
    // fan-out spread across cores. fresh spawns are comparatively rare (task
    // startup, not the per-message hot path), so the pool-wide nudge here is not
    // the herd the pinned path had to shed.
    note_notify();
    wake_pool();
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
    // a pinned task can only ever run on its owner, and only the owner drains its
    // pinned queue — so nudge *only* the owner. if the owner is busy the notify is
    // a cheap no-op (no waiter), and it never wakes an idle peer that has nothing
    // to run. that is the locality lever: on a single-connection pipeline whose
    // tasks all pin to one worker, the other worker now stays parked instead of
    // spinning and contending on the shared slab lock.
    note_notify();
    wake_worker(owner);
}

/// re-enqueue a *preempted* started task onto its owner worker's `deferred`
/// queue, the lowest-priority spot. like `enqueue_woken` it always targets the
/// owner (pinning holds — the suspended coroutine resumes on the same worker) and
/// nudges only the owner. the difference is purely priority: `find_work` drains
/// this queue only when the worker has nothing else to do, so a preempted compute
/// task cannot crowd out other ready work — that is what makes preemption
/// actually relieve starvation rather than just re-select the same hog.
fn enqueue_preempted(owner: usize, id: TaskId) {
    let sched = scheduler();
    sched.workers[owner]
        .deferred
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push_back(id);
    note_notify();
    wake_worker(owner);
}

/// nudge one worker's private park spot. acquiring the worker's park lock before
/// notifying serializes against `park`'s check-then-wait, so a wake landing just
/// as the worker decides to sleep is not lost (and the wait-timeout backstops any
/// residual window). a `notify_one` on a worker that is not parked is a no-op.
fn wake_worker(index: usize) {
    let worker = &scheduler().workers[index];
    let _g = worker.park_lock.lock().unwrap_or_else(|p| p.into_inner());
    worker.park_cv.notify_one();
}

/// nudge every worker. used for fresh, stealable work (injector or a worker's
/// local queue), where any worker is a valid taker, so an idle peer wakes to run
/// or steal it. this keeps independent fan-out spread across cores; it is not on
/// the per-message hot path, so it is not a herd.
fn wake_pool() {
    let n = scheduler().workers.len();
    for i in 0..n {
        wake_worker(i);
    }
}

/// profiling helper: record whether a wake actually had to rouse a parked worker
/// (a real futex cost) or was absorbed because the pool was busy. read-only,
/// gated behind `PITH_GREEN_STATS`.
fn note_notify() {
    if !stats_on() {
        return;
    }
    if PARKED_WORKERS.load(AtomicOrdering::Relaxed) > 0 {
        WAKE_FUTEX.fetch_add(1, AtomicOrdering::Relaxed);
    } else {
        WAKE_ABSORBED.fetch_add(1, AtomicOrdering::Relaxed);
    }
}

/// debug-only invariant guard for a task pulled from a *stealable* source: a
/// worker's own `local` queue, the global injector, or a peer's `local` on a
/// steal. the pinning discipline the whole `SendCoroutine` soundness rests on
/// (see the module doc) is exactly: only fresh, never-resumed tasks are ever
/// moved between workers. a fresh task has no `owner` yet and sits `Ready`. if
/// either were false here we would be about to run — possibly on a different
/// worker than last time — a coroutine that may carry live pith stack, the
/// unsound case the manual `Send` forbids. cheap, and compiled out in release.
///
/// the slab lock is taken with no queue lock held (the caller pops into a local
/// first), so this never nests queue-inside-slab.
#[cfg(debug_assertions)]
fn debug_assert_stealable(id: TaskId) {
    let slab = lock_slab();
    if let Some(task) = slab.get(id) {
        debug_assert!(
            task.owner.is_none(),
            "stole/ran-fresh a task already pinned to a worker (id {id})"
        );
        debug_assert!(
            task.state == RunState::Ready,
            "a stealable task must be Ready, never mid-run (id {id})"
        );
    }
}
#[cfg(not(debug_assertions))]
#[inline(always)]
fn debug_assert_stealable(_id: TaskId) {}

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

    // own local queue (stealable): pop into a local so the queue lock is released
    // before the debug invariant check locks the slab.
    let from_local = sched.workers[index]
        .local
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .pop_front();
    if let Some(id) = from_local {
        debug_assert_stealable(id);
        return Some(id);
    }

    let from_injector = sched
        .injector
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .pop_front();
    if let Some(id) = from_injector {
        debug_assert_stealable(id);
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
        let stolen = sched.workers[victim]
            .local
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pop_front();
        if let Some(id) = stolen {
            debug_assert_stealable(id);
            return Some(id);
        }
    }

    // deferred last: only resume a preempted compute task once there is genuinely
    // nothing else — no owned woken work, no fresh work anywhere to run or steal.
    // this is what lets the other tasks on this worker make progress before the
    // hog runs again.
    if let Some(id) = sched.workers[index]
        .deferred
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .pop_front()
    {
        return Some(id);
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
    let worker = &scheduler().workers[index];
    let guard = worker.park_lock.lock().unwrap_or_else(|p| p.into_inner());
    // one last check under the lock: if work appeared, don't sleep.
    if has_any_work(index) {
        return;
    }
    // profiling: mark ourselves parked so a concurrent wake can be attributed as
    // a real futex wake vs one absorbed by a busy pool.
    if stats_on() {
        PARKED_WORKERS.fetch_add(1, AtomicOrdering::Relaxed);
    }
    let _ = worker.park_cv.wait_timeout(guard, Duration::from_millis(1));
    if stats_on() {
        PARKED_WORKERS.fetch_sub(1, AtomicOrdering::Relaxed);
    }
}

/// is there any work *this* worker could actually take? used only to avoid
/// parking on a false-empty; the authoritative pop is still `find_work`, and it
/// must consider exactly the same sources: this worker's own pinned queue, its
/// own local queue, the injector, and any peer's *local* (stealable) queue.
///
/// it must NOT count a *peer's pinned* queue: a pinned task only ever runs on
/// its owner, so peer-pinned work this worker can never reach would otherwise
/// keep it falsely "busy" — spinning through `find_work` and re-checking here
/// instead of parking. that false-busy spin (a worker kept awake by the other
/// worker's pinned pipeline) was a large part of why two workers lost to one.
fn has_any_work(index: usize) -> bool {
    let sched = scheduler();
    // own pinned first — the woken work this worker must resume itself.
    if !sched.workers[index]
        .pinned
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_empty()
    {
        return true;
    }
    if !sched
        .injector
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_empty()
    {
        return true;
    }
    // any worker's local queue is stealable (own or a peer's).
    if sched
        .workers
        .iter()
        .any(|w| !w.local.lock().unwrap_or_else(|p| p.into_inner()).is_empty())
    {
        return true;
    }
    // own deferred (preempted) work — reachable only by this worker, so like the
    // pinned queue it must count here or the worker would park on a task it owns.
    !sched.workers[index]
        .deferred
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_empty()
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

/// how `run_task` re-enqueues a task that just suspended, carrying the task's
/// owner worker. `Woken` and `Preempted` differ only in queue priority (see the
/// `Yield` arm of `run_task`); `None` means the task parked and its block site
/// owns waking it.
enum Requeue {
    Woken(Option<usize>),
    Preempted(Option<usize>),
    None,
}

/// record a task's completion and unblock everyone awaiting it: flip the slab
/// entry to `Done` with `result`, then set the join's `done` flag, notify
/// os-thread awaiters on the condvar, and `wake` every registered green awaiter.
///
/// both `run_task` completion paths funnel through here — a clean return and the
/// panic boundary — so an awaiter gets the same defined outcome whether its task
/// returned normally or its coroutine panicked. once `done` is set under the join
/// lock, a fresh `green_await` sees it and returns without registering, so no
/// waiter can be added after we take the list: draining here loses none.
fn finish_task(id: TaskId, result: i64) {
    let (join, closure_handle) = {
        let mut slab = lock_slab();
        if let Some(task) = slab.get_mut(id) {
            task.state = RunState::Done;
            task.result = result;
            (task.join.clone(), task.closure_handle)
        } else {
            return;
        }
    };
    // the task body has returned, so its spawn closure (which held the captured
    // arguments) is no longer needed — release the one reference the task owned.
    // the emitter moves the closure into `spawn` without releasing it, relying on
    // the runtime to own it for the task's life; without this every spawned task
    // leaks its closure environment for the whole process, the dominant per-task
    // leak on a fan-out server. released outside the slab lock (closure release
    // takes its own registry lock).
    if closure_handle != 0 {
        unsafe {
            crate::pith_closure_release(closure_handle);
        }
    }
    let (lock, cvar) = &*join;
    // flip done + notify os-thread awaiters and take the green-waiter list under
    // the join lock; wake the green awaiters after releasing it, since
    // `green::wake` takes the slab and queue locks and the join lock must stay
    // out of that nest (join is the outer lock, like a channel lock).
    let green_waiters = {
        let mut j = lock_join(lock);
        j.done = true;
        j.result = result;
        cvar.notify_all();
        std::mem::take(&mut j.green_waiters)
    };
    for waiter in green_waiters {
        wake(waiter);
    }
    // if this task was detached while it ran, no one will await it — reclaim its
    // slab slot now that it is done and its coroutine is dropped. safe here:
    // `finish_task` runs from `run_task`'s completion arm after the coroutine has
    // returned, the task is in no run queue, and its awaiter list is drained, so
    // nothing else references this slot. an awaited task is left for `green_await`
    // to reclaim once it reads the result.
    if let Some(handle) = lock_slab().reclaim_if_detached(id) {
        handle_registry::unregister_id(handle, HandleKind::Task);
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
        match slab.get_mut(id) {
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

    // publish this task as the worker's running task and stamp the epoch, so the
    // monitor thread can detect a quantum overrun and set the preempt flag. only
    // the monitor reads these, and only via atomics, so this never races the
    // coroutine. cleared right after the resume returns.
    let worker_index = CURRENT_WORKER.with(|c| c.get());
    if let Some(w) = worker_index {
        let worker = &scheduler().workers[w];
        worker
            .running_since
            .store(PREEMPT_EPOCH.load(AtomicOrdering::Relaxed), AtomicOrdering::Relaxed);
        worker
            .running_task
            .store(id + 1, AtomicOrdering::Relaxed);
    }

    // run outside the lock. SAFETY of the transmute-and-call lives inside the
    // coroutine body (see green_spawn); here we just drive the coroutine.
    //
    // wrap the resume in `catch_unwind` so a panic raised in pith JIT/FFI code
    // (a trap that surfaces as a Rust panic, an FFI panic) does not unwind
    // through the coroutine into the worker thread and kill it. neither cargo
    // profile sets `panic = "abort"`, so without this a single panicking task
    // would silently retire a worker AND leave its `join.done` unset, hanging
    // every awaiter forever. corosensei re-raises a coroutine panic out of
    // `resume` (it has already unwound the coroutine's own stack, so `coro` is
    // complete and must never be resumed again — the panic arm drops it).
    //
    // AssertUnwindSafe: `coro` (a `SendCoroutine`) is not `UnwindSafe`, but a
    // caught panic here does not observe it in a broken state — we never resume
    // the coroutine after a panic, we drop it. no other captured state crosses
    // the boundary. so asserting unwind-safety is sound.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| coro.0.resume(())));

    // no task running on this worker until it picks up the next one.
    if let Some(w) = worker_index {
        scheduler().workers[w]
            .running_task
            .store(0, AtomicOrdering::Relaxed);
    }

    // on its first resume the body published its yielder pointer here; read it
    // back so we can re-install it on later resumes (the body's top does not run
    // again after a suspend).
    let new_yielder = CURRENT_YIELDER.with(|c| c.get());

    CURRENT_YIELDER.with(|c| c.set(prev_yielder));
    CURRENT_TASK.with(|c| c.set(prev_task));
    CURRENT_TLS.with(|c| c.set(prev_tls));

    match outcome {
        Ok(CoroutineResult::Return(result)) => {
            // clean return: record the result, flip done, wake awaiters of both
            // kinds, and drop the coroutine (and its stack) by not putting it
            // back. see `finish_task` for the join-lock / wake ordering.
            finish_task(id, result);
        }
        Err(_panic) => {
            // the coroutine's body panicked. corosensei already unwound the
            // coroutine's own stack, so `coro` is complete — we drop it here and
            // never resume it again. the default panic hook has already printed
            // the message to stderr; our job is to make sure the task's awaiters
            // do not hang. mark the task done with a zero result (a failed task
            // reports 0, matching the os-thread path when a closure lookup fails)
            // and wake every awaiter so they unblock with a defined outcome. the
            // worker itself is unharmed and goes on to run the next task.
            finish_task(id, 0);
        }
        Ok(CoroutineResult::Yield(())) => {
            // the task suspended: either it parked on a blocking op (a channel, a
            // P2b primitive, or an await), or it was preempted at a safe-point. put
            // the coroutine back and record where its yielder lives, then decide how
            // to re-enqueue:
            //   - `preempt_pending`: overran its quantum -> `deferred` (lowest
            //     priority), so it yields to every other ready task on the worker.
            //   - `wake_pending`: a block-site wake landed while we were still
            //     Running (the wake/park race) -> `pinned`, resume promptly.
            //   - neither: a plain block -> Parked; the block site re-enqueues us.
            let requeue = {
                let mut slab = lock_slab();
                if let Some(task) = slab.get_mut(id) {
                    task.coro = Some(coro);
                    task.yielder_ptr = YielderPtr(new_yielder);
                    if task.preempt_pending {
                        task.preempt_pending = false;
                        task.state = RunState::Ready;
                        Requeue::Preempted(task.owner)
                    } else if task.wake_pending {
                        task.wake_pending = false;
                        task.state = RunState::Ready;
                        Requeue::Woken(task.owner)
                    } else {
                        task.state = RunState::Parked;
                        Requeue::None
                    }
                } else {
                    return;
                }
            };
            // enqueue outside the slab lock to keep the slab -> queue order clean.
            match requeue {
                Requeue::Woken(Some(w)) => enqueue_woken(w, id),
                Requeue::Preempted(Some(w)) => enqueue_preempted(w, id),
                // a task that yielded must have already run, so `owner` is set; fall
                // back to the fresh path only defensively.
                Requeue::Woken(None) | Requeue::Preempted(None) => enqueue_fresh(id),
                Requeue::None => {}
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
            green_waiters: Vec::new(),
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

    let (id, generation) = {
        let mut slab = lock_slab();
        slab.insert(Task {
            coro: Some(SendCoroutine(coro)),
            closure_handle,
            state: RunState::Ready,
            result: 0,
            owner: None,
            yielder_ptr: YielderPtr(ptr::null()),
            wake_pending: false,
            preempt_pending: false,
            reclaim_on_done: false,
            join,
            // fresh, empty thread-local storage: this task starts with no
            // threadlocal slots materialized, exactly like a brand-new OS thread.
            tls: Box::new(HashMap::new()),
        })
    };

    let task_handle = make_handle(id, generation);
    handle_registry::register_id(task_handle, HandleKind::Task);
    enqueue_fresh(id);
    task_handle
}

/// green await: return the target task's result, blocking until it completes.
///
/// how it blocks depends on the caller. an **os-thread** caller (the main thread,
/// or the whole os-thread backend, where `current_task()` is always `None`) waits
/// on the target's condvar exactly as it always has — byte for byte. a **green
/// task** must not park its worker OS thread, so instead of condvar-waiting it
/// registers its slab id on the target's join-waiter list (under the join lock)
/// and suspends its coroutine back to the scheduler; the worker is then free to
/// run the very task being awaited. when the target completes it drains that list
/// and `wake`s each green awaiter (see the `Return` path in `run_task`).
///
/// this is what lets a green coordinator await green children from inside a task:
/// under the old condvar-wait, awaiting the first child parked the only worker
/// and the child could never run, so a single-worker fan-in hung. now the await
/// yields and the child runs on the freed worker.
///
/// the wake/park race is closed the same way the channel path closes it: we
/// register the waiter *under the join lock*, and the lock serializes our
/// check-and-register against the completer's set-done-and-drain — so the
/// completer either sees our id (and wakes us) or has already set `done` (and we
/// return without parking). if the wake lands while we are still `Running`
/// (between releasing the lock and suspending), `wake` records `wake_pending` and
/// the park path re-enqueues us, so the wake is never lost. the arc keeps the
/// join state alive even after the slab entry is eventually reused.
///
/// # Safety
/// `task_handle` came from spawn or is garbage; validated against the registry.
pub(crate) unsafe fn green_await(task_handle: i64) -> i64 {
    let join = match join_for(task_handle) {
        Some(j) => j,
        None => return 0,
    };
    let (lock, cvar) = &*join;
    // resolve once whether we are inside a green task; the running task does not
    // change under us across a suspend/resume.
    let green_task = current_task();
    let mut j = lock_join(lock);
    while !j.done {
        match green_task {
            // os-thread awaiter: condvar-wait, unchanged from the P1a path.
            None => j = cvar.wait(j).unwrap_or_else(|p| p.into_inner()),
            // green awaiter: register under the join lock, release it, and suspend
            // the coroutine. re-lock and re-check `done` on resume — a completing
            // task drains the list, so we are woken exactly when `done` is set.
            Some(id) => {
                j.green_waiters.push(id);
                drop(j);
                park_current();
                j = lock_join(lock);
            }
        }
    }
    // read the result out of the join (an `Arc`, so it outlives the slab entry)
    // and drop the join lock before touching the slab — `finish_task` takes the
    // slab lock then the join lock, so we must never hold both at once here.
    let result = j.result;
    drop(j);
    // the task is finished and we have consumed its result, so this handle will
    // not be observed again (await consumes it, Rust-style). reclaim its slab
    // slot now. the reclaim is gated on the handle's generation, so if the task
    // was already reclaimed — detached, or another awaiter of the same handle
    // beat us — this is a safe no-op and we skip the unregister.
    if let Some((index, generation)) = split_handle(task_handle) {
        if lock_slab().reclaim(index, generation) {
            handle_registry::unregister_id(task_handle, HandleKind::Task);
        }
    }
    result
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

/// detach a green task: a promise that no one will await it, so its slab slot
/// can be reclaimed the instant it finishes instead of leaking. if the task is
/// already done we reclaim right here; otherwise we flag it and `finish_task`
/// reclaims it on completion. this is what keeps a fire-and-forget server —
/// `spawn handle_conn(...)` in an accept loop — from growing the slab without
/// bound. a stale or invalid handle is a no-op.
pub(crate) fn green_detach(task_handle: i64) {
    let Some((index, generation)) = split_handle(task_handle) else {
        return;
    };
    if !handle_registry::is_valid_id(task_handle, HandleKind::Task) {
        return;
    }
    if let Some(handle) = lock_slab().detach(index, generation) {
        handle_registry::unregister_id(handle, HandleKind::Task);
    }
}

/// resolve a task handle to its shared join arc, or `None` for an invalid or
/// stale handle. the arc clone lets callers wait/inspect without holding the
/// slab lock. the generation check rejects a handle whose slot has since been
/// reclaimed and reused.
fn join_for(task_handle: i64) -> Option<Arc<(Mutex<Join>, Condvar)>> {
    if !handle_registry::is_valid_id(task_handle, HandleKind::Task) {
        return None;
    }
    let (index, generation) = split_handle(task_handle)?;
    let slab = lock_slab();
    slab.get_checked(index, generation)
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

    // the preemption tests share the process-global epoch/flag and the task slab,
    // so they must not run concurrently. this lock serializes them; the rest of
    // the suite never touches those globals.
    static PREEMPT_TEST_LOCK: Mutex<()> = Mutex::new(());

    // the overrun predicate is the single definition of "this task has run too
    // long" shared by the monitor and the safe-point. check the boundary: a task
    // stamped at the current epoch has not overrun; one epoch later it has.
    #[test]
    fn task_overrun_triggers_after_one_epoch() {
        assert!(!task_overrun(5, 5));
        assert!(task_overrun(5, 5 + QUANTUM_EPOCHS));
        assert!(task_overrun(5, 100));
        // saturating: a stale `now` behind `running_since` never reports overrun.
        assert!(!task_overrun(10, 3));
    }

    // off a green task (`current_task()` is None — the os-thread backend, main,
    // or between tasks), the safe-point slow path must do nothing even with the
    // flag forced on. this is the invariant that makes emitting the check into
    // all code harmless off-green.
    #[test]
    fn maybe_yield_is_noop_without_running_task() {
        let _guard = PREEMPT_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        assert!(current_task().is_none());
        PITH_PREEMPT_REQUESTED.store(1, AtomicOrdering::Relaxed);
        // must return promptly without parking; if it tried to park with no
        // running coroutine the debug_assert in park_current would fire.
        pith_green_maybe_yield();
        PITH_PREEMPT_REQUESTED.store(0, AtomicOrdering::Relaxed);
    }

    // full path: a green task that overruns its quantum hits a safe-point, parks,
    // and is re-enqueued onto its owner worker's pinned queue — then resumes there
    // and runs to completion. drives the real run_task/maybe_yield/park machinery
    // on the test thread (no worker threads started), pinning the task to worker 0.
    #[test]
    fn maybe_yield_parks_and_requeues_on_owner() {
        let _guard = PREEMPT_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // act as worker 0 for the duration of this test.
        CURRENT_WORKER.with(|c| c.set(Some(0)));

        // a task body that, on its first resume, simulates the monitor observing
        // time pass (bumps the epoch past the quantum) and then hits a safe-point;
        // after it is resumed the second time it runs to completion.
        let coro = TaskCoroutine::with_stack(
            corosensei::stack::DefaultStack::new(STACK_SIZE).expect("stack"),
            move |yielder: &TaskYielder, _input: ()| -> i64 {
                CURRENT_YIELDER.with(|c| c.set(yielder as *const TaskYielder));
                PREEMPT_EPOCH.fetch_add(QUANTUM_EPOCHS + 1, AtomicOrdering::Relaxed);
                pith_green_maybe_yield();
                99
            },
        );

        let join = Arc::new((
            Mutex::new(Join {
                done: false,
                result: 0,
                green_waiters: Vec::new(),
            }),
            Condvar::new(),
        ));
        let id = {
            let mut slab = lock_slab();
            slab.insert(Task {
                coro: Some(SendCoroutine(coro)),
                closure_handle: 0,
                state: RunState::Ready,
                result: 0,
                owner: None,
                yielder_ptr: YielderPtr(ptr::null()),
                wake_pending: false,
                preempt_pending: false,
                reclaim_on_done: false,
                join,
                tls: Box::new(HashMap::new()),
            })
            .0
        };

        // force the flag on so the safe-point takes its slow path.
        PITH_PREEMPT_REQUESTED.store(1, AtomicOrdering::Relaxed);

        // first resume: the body overruns and parks at the safe-point. run_task's
        // yield path must see preempt_pending and re-enqueue onto owner 0's
        // lowest-priority deferred queue.
        run_task(id);
        {
            let slab = lock_slab();
            let task = slab.get(id).unwrap();
            assert_eq!(task.owner, Some(0), "task should pin to worker 0");
            assert!(task.state == RunState::Ready, "should be re-enqueued, not parked");
        }
        {
            let mut deferred = scheduler().workers[0]
                .deferred
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            assert_eq!(
                deferred.pop_front(),
                Some(id),
                "preempted task should be on owner's deferred queue"
            );
        }

        // second resume: runs to completion, records the result.
        run_task(id);
        {
            let slab = lock_slab();
            let task = slab.get(id).unwrap();
            assert!(task.state == RunState::Done);
            assert_eq!(task.result, 99);
        }

        PITH_PREEMPT_REQUESTED.store(0, AtomicOrdering::Relaxed);
        CURRENT_WORKER.with(|c| c.set(None));
    }

    // push a Ready task wrapping `coro` (with its own fresh join) into the slab
    // and return its id. shared by the preempt and panic tests to cut the
    // slab-push boilerplate.
    fn push_ready_task(coro: TaskCoroutine, join: Arc<(Mutex<Join>, Condvar)>) -> TaskId {
        let mut slab = lock_slab();
        slab.insert(Task {
            coro: Some(SendCoroutine(coro)),
            closure_handle: 0,
            state: RunState::Ready,
            result: 0,
            owner: None,
            yielder_ptr: YielderPtr(ptr::null()),
            wake_pending: false,
            preempt_pending: false,
            reclaim_on_done: false,
            join,
            tls: Box::new(HashMap::new()),
        })
        .0
    }

    fn fresh_join() -> Arc<(Mutex<Join>, Condvar)> {
        Arc::new((
            Mutex::new(Join {
                done: false,
                result: 0,
                green_waiters: Vec::new(),
            }),
            Condvar::new(),
        ))
    }

    // the panic boundary: a green task whose body panics (a pith trap or an FFI
    // panic surfaces exactly as a Rust panic unwinding out of the coroutine) must
    // NOT take the worker down with it. run_task's `catch_unwind` catches the
    // panic, marks the task done (so its awaiters unblock instead of hanging),
    // drops the dead coroutine, and returns normally — and the same worker goes on
    // to run the next task. drives run_task directly on the test thread, standing
    // in for a worker, so a missing boundary would unwind this test and fail it.
    #[test]
    fn panicking_task_marks_done_and_keeps_worker_alive() {
        let _guard = PREEMPT_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        CURRENT_WORKER.with(|c| c.set(Some(0)));

        let coro = TaskCoroutine::with_stack(
            corosensei::stack::DefaultStack::new(STACK_SIZE).expect("stack"),
            move |yielder: &TaskYielder, _input: ()| -> i64 {
                CURRENT_YIELDER.with(|c| c.set(yielder as *const TaskYielder));
                panic!("boom from a green task");
            },
        );
        let join = fresh_join();
        let awaiter_view = join.clone();
        let id = push_ready_task(coro, join);

        // must return, not unwind. without the boundary this call propagates the
        // panic and the test aborts.
        run_task(id);

        // the task is recorded done and its coroutine dropped (never re-enqueued).
        {
            let slab = lock_slab();
            let task = slab.get(id).unwrap();
            assert!(task.state == RunState::Done, "panicked task must be Done");
            assert!(task.coro.is_none(), "dead coroutine must not be put back");
        }
        // an awaiter sees done=true, so it unblocks with a defined (zero) result
        // rather than hanging forever.
        {
            let (lock, _) = &*awaiter_view;
            let j = lock_join(lock);
            assert!(j.done, "awaiters must see done=true");
            assert_eq!(j.result, 0, "a failed task reports a zero result");
        }

        // the worker (this thread) survived: a fresh task still runs to completion.
        let coro2 = TaskCoroutine::with_stack(
            corosensei::stack::DefaultStack::new(STACK_SIZE).expect("stack"),
            move |yielder: &TaskYielder, _input: ()| -> i64 {
                CURRENT_YIELDER.with(|c| c.set(yielder as *const TaskYielder));
                7
            },
        );
        let id2 = push_ready_task(coro2, fresh_join());
        run_task(id2);
        {
            let slab = lock_slab();
            let task = slab.get(id2).unwrap();
            assert!(task.state == RunState::Done);
            assert_eq!(task.result, 7, "worker still runs tasks after a panic");
        }

        CURRENT_WORKER.with(|c| c.set(None));
    }

    // a minimal task with a trivial coroutine, for the slab-logic tests below.
    // reclamation never touches the coroutine, so its body is irrelevant.
    fn dummy_task(state: RunState) -> Task {
        let coro = TaskCoroutine::with_stack(
            corosensei::stack::DefaultStack::new(STACK_SIZE).expect("stack"),
            move |_y: &TaskYielder, _in: ()| -> i64 { 0 },
        );
        Task {
            coro: Some(SendCoroutine(coro)),
            closure_handle: 0,
            state,
            result: 0,
            owner: None,
            yielder_ptr: YielderPtr(ptr::null()),
            wake_pending: false,
            preempt_pending: false,
            reclaim_on_done: false,
            join: fresh_join(),
            tls: Box::new(HashMap::new()),
        }
    }

    // the handle encoding: generation 0 is exactly the old `id + 1`, a
    // generation rides above, and the sign bit stays clear so 0 means "no task".
    #[test]
    fn handle_codec_round_trips() {
        assert_eq!(make_handle(0, 0), 1);
        assert_eq!(make_handle(41, 0), 42);
        assert_eq!(split_handle(42), Some((41, 0)));

        let h = make_handle(41, 7);
        assert_eq!(split_handle(h), Some((41, 7)));
        assert!(h > 0);

        assert_eq!(split_handle(0), None);
        assert_eq!(split_handle(-5), None);

        // the top generation bit is masked off, so the handle never goes negative.
        let hi = make_handle(3, GEN_MASK);
        assert!(hi > 0);
        assert_eq!(split_handle(hi), Some((3, GEN_MASK)));
    }

    // reclaiming a done task frees its slot; the next insert reuses the index at
    // a bumped generation, so the old handle can never alias the new task.
    #[test]
    fn slab_reclaim_reuses_slot_with_bumped_generation() {
        let mut slab = Slab::new();
        let (id, gen) = slab.insert(dummy_task(RunState::Ready));
        assert_eq!((id, gen), (0, 0));
        assert!(slab.get_checked(id, gen).is_some());

        // a task that is not Done is never reclaimed.
        assert!(!slab.reclaim(id, gen));

        slab.get_mut(id).unwrap().state = RunState::Done;
        assert!(slab.reclaim(id, gen));
        assert!(slab.get_checked(id, gen).is_none(), "stale handle must not resolve");

        let (id2, gen2) = slab.insert(dummy_task(RunState::Ready));
        assert_eq!(id2, id, "freed index is reused");
        assert_ne!(gen2, gen, "generation bumped");

        // reclaiming again with the stale generation is a no-op.
        assert!(!slab.reclaim(id, gen));
    }

    // reclaim only touches the slot that still carries the expected generation.
    #[test]
    fn slab_reclaim_wrong_generation_is_noop() {
        let mut slab = Slab::new();
        let (id, gen) = slab.insert(dummy_task(RunState::Done));
        assert!(!slab.reclaim(id, gen.wrapping_add(1)));
        assert!(slab.get_checked(id, gen).is_some(), "slot survives a mismatched reclaim");
    }

    // detaching a still-running task flags it; reclamation happens on finish.
    #[test]
    fn slab_detach_running_task_reclaims_on_finish() {
        let mut slab = Slab::new();
        let (id, gen) = slab.insert(dummy_task(RunState::Running));
        assert_eq!(slab.detach(id, gen), None);
        assert!(slab.reclaim_if_detached(id).is_none(), "not done yet");
        assert!(slab.get_checked(id, gen).is_some());

        slab.get_mut(id).unwrap().state = RunState::Done;
        assert_eq!(slab.reclaim_if_detached(id), Some(make_handle(id, gen)));
        assert!(slab.get_checked(id, gen).is_none());
    }

    // detaching an already-done task reclaims it immediately.
    #[test]
    fn slab_detach_done_task_reclaims_immediately() {
        let mut slab = Slab::new();
        let (id, gen) = slab.insert(dummy_task(RunState::Done));
        assert_eq!(slab.detach(id, gen), Some(make_handle(id, gen)));
        assert!(slab.get_checked(id, gen).is_none());
        // a second detach on the stale handle is a no-op.
        assert_eq!(slab.detach(id, gen), None);
    }

    // a done task nobody detached is left alone by the finish-path reclaim — the
    // awaited case is reclaimed by `green_await`, not here.
    #[test]
    fn slab_undetached_done_task_survives_finish_reclaim() {
        let mut slab = Slab::new();
        let (id, _gen) = slab.insert(dummy_task(RunState::Done));
        assert!(slab.reclaim_if_detached(id).is_none());
    }
}
