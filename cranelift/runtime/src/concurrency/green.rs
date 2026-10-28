//! green-thread backend (the default on linux; `PITH_GREEN` overrides)
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
//! ## pinning, and moving a task (what keeps `SendCoroutine` sound)
//!
//! a *suspended* coroutine holds live pith stack across the suspension. what
//! that stack holds is handles and plain values: pith code owns nothing
//! thread-affine, and the runtime frames that park (a channel block site,
//! `green_await`, the reactor and blocking-pool waits) drop every lock and
//! bracket before they suspend and derive nothing from the thread they parked
//! on afterwards (see the accessor block above `current_task`). so a
//! suspended coroutine may, in principle, resume on any worker.
//!
//! what the scheduler does with that is a policy. the first worker to resume
//! a task becomes its `owner`, and a wake enqueues it on the owner's `pinned`
//! queue. a worker that wakes a task parked on another worker takes it over in
//! the same CAS that claims it (see `claim_parked`), so a pair of tasks that
//! talk to each other converge onto one worker instead of paying a
//! cross-thread wake per message (`PITH_GREEN_MIGRATE=0` turns that off). an
//! idle worker may also take a task off a peer's pinned queue when that peer
//! has been inside one long task for a while (see `steal_pinned`), so work
//! that converged onto one worker spreads out again. in every case a
//! coroutine changes threads only while it is off-CPU and claimed by exactly
//! one thread: fresh and stealable, `Parked` and claimed by its waker, or
//! `Ready` and popped from a pinned queue under its lock. that hand-over is
//! what makes the `unsafe impl Send for SendCoroutine` sound; see the type
//! doc.
//!
//! ## synchronization model
//!
//! - the task slab (a generational slotmap, see `Slab`) is shared across workers
//!   under one mutex, and holds each task's *cold* bookkeeping: the join arc,
//!   the closure handle, the tls map's owning `Box`, and the reclamation state.
//!   the slab lock is taken a handful of times per task *lifetime* (spawn,
//!   finish, reclaim), never per resume.
//! - everything the hot wake/resume cycle touches — a task's run state,
//!   wake/preempt flags, owner pin, coroutine, yielder, and handoff — lives in
//!   a lock-free side arena of per-task sync blocks (see `SyncArena`). a wake
//!   is the hottest thing the scheduler does (two per channel message at
//!   saturation), and routing it through the slab mutex made every waker and
//!   every resume serialize on one global lock. the arena's chunks never move,
//!   so `wake` finds a task's state by index math and flips it with a CAS; the
//!   state word doubles as a claim ticket that hands the block's cells (the
//!   coroutine among them) from waker to worker without a lock — see the claim
//!   protocol on `SyncBlock`.
//! - each worker owns three run queues: a `local` queue of fresh, stealable
//!   tasks; a `pinned` queue of woken started tasks that the owner drains and
//!   an idle peer may steal from when the owner is stuck in one long task;
//!   and a `deferred` queue of *preempted* started tasks (P5), also owner-only but
//!   drained at the lowest priority. a global injector receives fresh spawns from
//!   non-worker threads (e.g. `main`). an idle worker checks pinned, then local,
//!   then injector, then steals from a random peer's *local* queue, then from a
//!   busy peer's *pinned* queue, and only if all of that is empty does it
//!   resume a `deferred` (preempted) task — so a
//!   compute hog that overran its quantum yields to every other ready task before
//!   it runs again. a worker that never runs dry still serves one deferred task
//!   every `POLL_EVERY` tasks, so a preempted task waits a bounded number of
//!   turns. plain `Mutex<VecDeque>` — readable first; lock-free deques are a
//!   later perf phase.
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
static WAKE_MIGRATED: AtomicU64 = AtomicU64::new(0);
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
    let migrated = WAKE_MIGRATED.load(AtomicOrdering::Relaxed);
    eprintln!(
        "[green-stats] wakes: same-worker={same} cross-worker={cross} reactor={reactor} \
         migrated={migrated} | futex-wakes={futex} absorbed={absorbed}"
    );
    dump_contention();
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

/// how many monitor epochs a task may run before it is asked to yield. a task
/// stamped with epoch `e` counts as overrun once the epoch reaches
/// `e + QUANTUM_EPOCHS`, so with the ~10 ms monitor pass two epochs is a 10-20 ms
/// slice — the same ballpark as go's 10 ms preemption quantum. small enough to
/// keep a compute task from starving its peers, large enough that ordinary
/// cooperative code (which yields far more often) never trips it.
///
/// one epoch would be a 0-10 ms slice: any task that happened to be running
/// when the tick landed counted as overrun, however briefly it had run. on a
/// busy pool some worker is always mid-task at a tick, so the request flag
/// stayed up nearly all the time, every loop back-edge in every task took the
/// slow path, and short tasks such as a server's per-request work were
/// preempted for no reason.
const QUANTUM_EPOCHS: u64 = 2;

/// how often the monitor thread wakes to bump the epoch and scan the workers.
const MONITOR_INTERVAL: Duration = Duration::from_millis(10);

/// the park timeout a worker starts from, and the ceiling it backs off to
/// while it keeps finding no work. the minimum is the latency bound that
/// matters: it only covers the sliver where an enqueue's notify races our
/// decision to sleep, and any enqueue that loses that race still wakes us
/// through `wake_worker`. the maximum only bounds how long a worker can
/// sleep through a wake that never arrives — nothing depends on it for
/// correctness, since both the enqueue path and the collector's stop
/// request notify the pool explicitly.
const PARK_TIMEOUT_MIN: Duration = Duration::from_millis(1);
const PARK_TIMEOUT_MAX: Duration = Duration::from_millis(32);

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
    // gate 0: a stop-the-world request from the cycle collector. checked before
    // the task gates so *any* thread frozen at a compiled safe-point parks for
    // the collector, green task or not — the frame it holds is off-CPU for the
    // whole park, never mutated. an accelerator for the worker-loop gate (which
    // alone is sufficient between tasks), not a replacement for it. one relaxed
    // load when the collector flag is off.
    crate::cycle::mutator_gate();

    // profiling: how often a safe-point took the slow path at all, so a run can
    // be asked what preemption actually cost it rather than what it might. only
    // reached when the monitor has set the flag, and one relaxed load when the
    // perf counters are off.
    if crate::perf_stats_enabled() {
        crate::perf_record1(&crate::PERF_GREEN_PREEMPT_CHECKS, 1);
    }

    // gate 1: are we inside a green task at all?
    let Some(id) = current_task() else {
        return;
    };
    let Some(worker_index) = current_worker() else {
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
    // block site to wake us. the distinct preempt flag (not the channel/await
    // wake-pending flag) makes run_task route us onto our owner's lowest-priority
    // `deferred` queue: a preempted compute task must yield to every other ready
    // task on the worker, or it would simply be re-selected and keep monopolizing
    // the worker (`pinned`/woken work is served first). we are the running task,
    // so our word is `Running` and a fetch_or cannot race the yield arm (which
    // only runs after we suspend); preemption registers no waiter, so there is no
    // wake/park race to close here.
    let Some(block) = sync_block(id) else {
        return;
    };
    block.word.fetch_or(S_PREEMPT_PENDING, AtomicOrdering::AcqRel);
    if crate::perf_stats_enabled() {
        crate::perf_record1(&crate::PERF_GREEN_PREEMPTIONS, 1);
    }
    park_current(id);
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

// ---------------------------------------------------------------------------
// per-task scheduling word (the lock-free wake path)
//
// a task's run state used to live in its slab entry, which meant every wake —
// the hottest operation in a channel pipeline, two per message at saturation —
// took the slab mutex just to flip Parked -> Ready and read the owner. the
// state now lives in a side arena of per-task sync blocks reachable without
// any lock: fixed-size chunks that never move (the slab's entries Vec moves
// when it grows, which is exactly why this state cannot be a pointer into it),
// found by index math from the task id.
//
// the whole scheduling state packs into one AtomicU64 so every transition is a
// single CAS and all the pieces travel together:
//
//   bits 0..2   run state: Ready / Running / Parked / Done
//   bit  2      wake_pending: a wake landed while the task was still Running
//   bit  3      preempt_pending: the task overran its quantum (see P5)
//   bits 32..   owner worker + 1 (0 = not yet pinned)
//
// the transitions mirror the old mutex-guarded state machine exactly:
//
//   wake         Parked -> Ready (claim + enqueue) | Running -> set wake_pending
//                | Ready / Done -> no-op
//   run_task     Ready -> Running (claim; pins the owner on first resume)
//   yield arm    Running -> Ready (a pending flag was set; re-enqueue)
//                | Running -> Parked (the block site owns the wake)
//   finish_task  -> Done (plain store; every later wake is a no-op)
//
// slot reuse and stale wakes: a reclaimed task's slot (and so its sync block)
// is reused by a later spawn, and a wake against a stale id must stay
// harmless. it is, by the same argument the slab path relied on: between
// reclaim and re-init the word reads Done (no-op), and after re-init it reads
// Ready (no-op). a stale wake that catches the reused slot's new task
// genuinely Parked wakes it spuriously — it re-checks its block condition and
// re-parks, the tolerance every block site already has. in practice stale
// wakes do not occur: a parked task's id lives in exactly one waiter list, and
// every list removes the id at wake time.
// ---------------------------------------------------------------------------

/// run states of the scheduling word (bits 0..2).
///
/// - `S_READY`: sitting in a run queue with its coroutine present.
/// - `S_RUNNING`: coroutine taken out and being resumed on some worker.
/// - `S_PARKED`: suspended on a blocking op; coroutine back in the slab, the
///   task in no queue, and the block site it parked on owns re-enqueueing it.
/// - `S_DONE`: coroutine returned (or the slot holds no task at all — a fresh
///   arena chunk reads `Done` so a wake against a never-used index no-ops).
const S_STATE_MASK: u64 = 0b11;
const S_READY: u64 = 0;
const S_RUNNING: u64 = 1;
const S_PARKED: u64 = 2;
const S_DONE: u64 = 3;
/// a wake landed while the task was still `Running` (mid-suspend, before the
/// yield arm recorded it as `Parked`): the yield arm re-enqueues instead of
/// parking, closing the wake/park race so a wake in that window is never lost.
const S_WAKE_PENDING: u64 = 1 << 2;
/// set by `pith_green_maybe_yield` right before it parks a task that overran
/// its quantum: the yield arm routes it onto the owner's lowest-priority
/// `deferred` queue instead of the prompt `pinned` queue.
const S_PREEMPT_PENDING: u64 = 1 << 3;
const S_OWNER_SHIFT: u32 = 32;
const S_OWNER_MASK: u64 = !0u64 << S_OWNER_SHIFT;

/// the owner worker packed in a scheduling word, if the task has been pinned.
fn word_owner(word: u64) -> Option<usize> {
    match word >> S_OWNER_SHIFT {
        0 => None,
        w => Some(w as usize - 1),
    }
}

/// the owner bits for a worker index (stored as `worker + 1`, 0 = unpinned).
fn owner_bits(worker: usize) -> u64 {
    ((worker as u64) + 1) << S_OWNER_SHIFT
}

/// one task's lock-free scheduling state: the word above plus the cells the
/// hot resume path needs. cache-line aligned so two tasks' wake CASes never
/// false-share a line.
///
/// ## the claim protocol (what makes the cells safe without a lock)
///
/// the word is a claim ticket. exactly one thread holds the claim at a time:
///
/// - the spawn path initializes the cells *before* the word first reads
///   `Ready` and before the task is enqueued anywhere.
/// - a worker that wins the `Ready -> Running` CAS owns the cells for the
///   whole resume: it takes the coroutine out, and (on a yield) writes it back
///   before the `Running -> Parked/Ready` CAS republishes the task.
/// - a waker that wins the `Parked -> Ready` CAS owns the cells until its
///   enqueue makes the task claimable again — that is the window `wake_with`
///   uses to deposit a handoff value.
///
/// every hand-over goes through a CAS on `word` (release on publish, acquire
/// on claim), so the cell writes of the previous holder are visible to the
/// next; the run-queue mutex between an enqueue and the owner's dequeue adds
/// its own ordering on top. this is exactly the exclusion the slab mutex used
/// to provide, minus the lock.
#[repr(align(64))]
struct SyncBlock {
    word: AtomicU64,
    /// the task's coroutine, present exactly while the word reads `Ready` or
    /// `Parked`; taken out by the claiming worker for the duration of a
    /// resume, gone for good once the task is `Done`.
    coro: UnsafeCell<Option<SendCoroutine>>,
    /// raw pointer to the coroutine's `Yielder`, captured on the body's first
    /// run (see `green_spawn`) and re-installed before every later resume.
    /// null until the body has run once; only ever dereferenced on the owner
    /// worker while this task is the one running.
    yielder: UnsafeCell<*const TaskYielder>,
    /// this task's own `threadlocal` storage. one worker thread runs many
    /// tasks, so a per-thread map would let task B see task A's values. the
    /// block never moves, so the pointer `run_task` installs for the resume
    /// stays valid.
    tls: UnsafeCell<TlsMap>,
    /// the closure the task runs. set at spawn and released by `finish_task`
    /// once the body returns, which is when the captured arguments die.
    closure: UnsafeCell<i64>,
    /// a value handed to this task by its waker (see `wake_with`): written by
    /// the waker right after it claims the task (the Parked -> Ready CAS),
    /// taken by the task itself once it resumes.
    handoff: UnsafeCell<Option<i64>>,
    /// the task's completion, shared with whoever awaits it.
    join: Mutex<Join>,
    /// os-thread awaiters wait here for `finish_task` to record the result.
    joined: Condvar,
}

// SAFETY: `word` is an atomic; the cells are guarded by the claim protocol
// documented on the type (a single holder at a time, hand-overs ordered by
// the word's CAS pairs and the run-queue mutex). the coroutine cell only ever
// crosses threads under the pinning discipline `SendCoroutine` documents.
unsafe impl Sync for SyncBlock {}

impl SyncBlock {
    /// an inert block: `Done` reads as "no task here", so a wake against an
    /// index whose slot has never held a task is a no-op.
    fn new() -> SyncBlock {
        SyncBlock {
            word: AtomicU64::new(S_DONE),
            coro: UnsafeCell::new(None),
            yielder: UnsafeCell::new(ptr::null()),
            tls: UnsafeCell::new(TlsMap::new()),
            closure: UnsafeCell::new(0),
            handoff: UnsafeCell::new(None),
            join: Mutex::new(Join::default()),
            joined: Condvar::new(),
        }
    }

    /// arm the block for a freshly spawned task and return the generation its
    /// handle carries. the word reads `Ready` only once the cells are set, and
    /// the caller enqueues the task after that, so nothing sees a half-armed
    /// block. the previous task was retired, so a wake aimed at it reads
    /// `Done` until the store below and no-ops.
    fn arm(&self, coro: SendCoroutine, closure: i64) -> u32 {
        // SAFETY: the id came off a free pool, so no task owns this block and
        // no thread touches its cells until the task is enqueued below.
        unsafe {
            *self.coro.get() = Some(coro);
            *self.yielder.get() = ptr::null();
            *self.closure.get() = closure;
            *self.handoff.get() = None;
            debug_assert!((*self.tls.get()).is_empty(), "a retired task left threadlocals");
        }
        let generation = {
            let mut join = lock_join(&self.join);
            join.live = true;
            join.done = false;
            join.result = 0;
            join.detached = false;
            join.generation
        };
        self.word.store(S_READY, AtomicOrdering::Release);
        generation
    }
}

/// arena geometry: chunks of `ARENA_CHUNK` blocks hang off a fixed spine of
/// `ARENA_SPINE` chunk pointers. chunks are allocated on demand and never
/// freed or moved — the property that lets `wake` reach a block with index
/// math and no lock. the spine bounds the live task count at 2M; every live
/// green task also owns a 1 MiB coroutine stack, so real programs exhaust
/// address space long before this bound, and hitting it is a loud panic
/// rather than a silent corruption.
const ARENA_CHUNK: usize = 256;
const ARENA_SPINE: usize = 8192;

/// the side arena of per-task sync blocks, indexed by slab task id.
struct SyncArena {
    /// chunk pointers, null until a task id in that chunk's range first exists.
    spine: Box<[AtomicPtr<SyncBlock>]>,
}

impl SyncArena {
    fn new() -> SyncArena {
        SyncArena {
            spine: (0..ARENA_SPINE)
                .map(|_| AtomicPtr::new(ptr::null_mut()))
                .collect(),
        }
    }

    /// the sync block for `id`, or `None` when no task with that index has
    /// ever existed. a garbage id — e.g. the reactor probing an id far beyond
    /// any real task — must read as "no task", never allocate.
    fn get(&self, id: TaskId) -> Option<&SyncBlock> {
        let chunk = self.spine.get(id / ARENA_CHUNK)?.load(AtomicOrdering::Acquire);
        if chunk.is_null() {
            return None;
        }
        // SAFETY: a non-null chunk pointer is an immortal allocation of
        // ARENA_CHUNK blocks (see `ensure`), and the offset is in range.
        Some(unsafe { &*chunk.add(id % ARENA_CHUNK) })
    }

    /// the sync block for `id`, allocating its chunk on first use. spawn-path
    /// only; the hot paths use `get`.
    fn ensure(&self, id: TaskId) -> &SyncBlock {
        let Some(slot) = self.spine.get(id / ARENA_CHUNK) else {
            runtime_fatal!(
                "green task index {id} exceeds the sync arena bound of {}",
                ARENA_CHUNK * ARENA_SPINE
            )
        };
        let mut chunk = slot.load(AtomicOrdering::Acquire);
        if chunk.is_null() {
            let fresh: Box<[SyncBlock]> = (0..ARENA_CHUNK).map(|_| SyncBlock::new()).collect();
            let fresh = Box::into_raw(fresh) as *mut SyncBlock;
            match slot.compare_exchange(
                ptr::null_mut(),
                fresh,
                AtomicOrdering::AcqRel,
                AtomicOrdering::Acquire,
            ) {
                // installed: the chunk is now immortal (never freed or moved).
                Ok(_) => chunk = fresh,
                // a racing spawner installed the chunk first; free ours.
                Err(existing) => {
                    // SAFETY: `fresh` came from Box::into_raw above and was
                    // never shared.
                    unsafe {
                        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                            fresh,
                            ARENA_CHUNK,
                        )));
                    }
                    chunk = existing;
                }
            }
        }
        // SAFETY: as in `get`.
        unsafe { &*chunk.add(id % ARENA_CHUNK) }
    }
}

/// the sync block for `id`, or `None` for an index that has never held a task.
fn sync_block(id: TaskId) -> Option<&'static SyncBlock> {
    scheduler().sync.get(id)
}


/// where task ids come from. an id names one block of the sync arena for as
/// long as a task holds it; retiring the task returns the id to a pool for the
/// next spawn. a worker keeps a pool of its own (`WorkerCache::ids`), so a
/// spawn and the await that retires the task usually touch no shared state;
/// a thread that is not a worker, or a worker whose pool ran dry or filled
/// up, uses the shared pool here. when both are empty a spawn claims a fresh
/// run of `ARENA_CHUNK` ids.
static SHARED_IDS: CacheLine<Mutex<Vec<TaskId>>> = CacheLine(Mutex::new(Vec::new()));
static NEXT_ID_CHUNK: AtomicUsize = AtomicUsize::new(0);

/// how many free ids a worker keeps before it hands the rest to the shared
/// pool. a worker that retires more tasks than it spawns (a consumer of
/// detached tasks) would otherwise hold ids its peers need.
const WORKER_IDS_MAX: usize = 2 * ARENA_CHUNK;

/// an id for a new task.
fn alloc_id() -> TaskId {
    if let Some(Some(id)) = with_own_cache(|cache| cache.ids.pop()) {
        return id;
    }
    if let Some(id) = lock_ids().pop() {
        return id;
    }
    let chunk = NEXT_ID_CHUNK.fetch_add(1, AtomicOrdering::Relaxed);
    let first = chunk * ARENA_CHUNK;
    // allocate the chunk's blocks before any of its ids is handed out.
    scheduler().sync.ensure(first);
    // reversed, so the lowest id is handed out first.
    let rest = (first + 1..first + ARENA_CHUNK).rev();
    if with_own_cache(|cache| cache.ids.extend(rest.clone())).is_none() {
        lock_ids().extend(rest);
    }
    first
}

/// give a retired task's id back for reuse.
fn free_id(id: TaskId) {
    let kept = with_own_cache(|cache| {
        if cache.ids.len() < WORKER_IDS_MAX {
            cache.ids.push(id);
            true
        } else {
            false
        }
    });
    if kept != Some(true) {
        lock_ids().push(id);
    }
}

fn lock_ids() -> MutexGuard<'static, Vec<TaskId>> {
    SHARED_IDS.lock().unwrap_or_else(|p| p.into_inner())
}

/// coroutine stack size. pith programs default to 8 MiB OS-thread stacks; a
/// green task gets a fixed slab here. 1 MiB is generous for the leaf-ish task
/// bodies we run today and keeps a large fan-out affordable. a deeply recursive
/// task could overflow this — corosensei installs a guard page so that faults
/// loudly rather than corrupting memory. sizing per-task stacks is a later knob.
const STACK_SIZE: usize = 1024 * 1024;

/// finished coroutines donate their stacks here and green_spawn reuses them.
/// a fresh stack is an mmap + a guard-page mprotect, and freeing one is an
/// munmap that costs a TLB shootdown across every core — per task, that made
/// spawn dominated by the kernel's address-space bookkeeping (a 20k spawn/join
/// probe spent 22k page faults where go spends 1.7k). reuse skips all of it,
/// and the already-faulted pages come back warm.
///
/// the pool is capped so a burst cannot hoard address space forever: beyond
/// STACK_POOL_MAX a returned stack just drops (one munmap, as before). stacks
/// keep whatever pages the previous task dirtied — same-process memory, and the
/// coroutine's setup overwrites what it uses, so stale bytes are inert.
const STACK_POOL_MAX: usize = 64;
static STACK_POOL: CacheLine<Mutex<Vec<corosensei::stack::DefaultStack>>> =
    CacheLine(Mutex::new(Vec::new()));

/// how many stacks each worker keeps for itself before returning them to the
/// shared `STACK_POOL`. a spawn takes a stack and a finish returns one, so
/// with only the shared pool every spawn and finish on every worker took its
/// lock.
const WORKER_STACK_MAX: usize = 16;

/// a pooled stack if one is free (this worker's own first, then the shared
/// pool), otherwise a fresh allocation.
fn take_stack() -> corosensei::stack::DefaultStack {
    if let Some(Some(stack)) = with_own_cache(|cache| cache.stacks.pop()) {
        return stack;
    }
    if let Some(stack) = STACK_POOL.lock().unwrap_or_else(|p| p.into_inner()).pop() {
        return stack;
    }
    match corosensei::stack::DefaultStack::new(STACK_SIZE) {
        Ok(stack) => stack,
        Err(err) => {
            runtime_fatal!("could not allocate a {STACK_SIZE}-byte green task stack: {err}")
        }
    }
}

/// recycle a finished coroutine's stack. `into_stack` requires the coroutine to
/// be complete, which both a clean return and an unwound panic guarantee; the
/// defensive `done()` check keeps a hypothetical incomplete coroutine on the
/// old drop path instead of aborting.
fn recycle_stack(coro: SendCoroutine) {
    if !coro.0.done() {
        return;
    }
    let mut stack = Some(coro.0.into_stack());
    with_own_cache(|cache| {
        if cache.stacks.len() < WORKER_STACK_MAX {
            cache.stacks.extend(stack.take());
        }
    });
    let Some(stack) = stack else {
        return;
    };
    let mut pool = STACK_POOL.lock().unwrap_or_else(|p| p.into_inner());
    if pool.len() < STACK_POOL_MAX {
        pool.push(stack);
    }
}

/// what a worker keeps for itself: free coroutine stacks and free task ids.
/// only the worker's own thread touches it (see `with_own_cache`), so it
/// needs no lock.
#[derive(Default)]
struct WorkerCache {
    stacks: Vec<corosensei::stack::DefaultStack>,
    ids: Vec<TaskId>,
}

/// a value that only one thread ever touches. `Sync` so it can sit in the
/// shared `Worker`; the accessor is what keeps other threads out.
#[derive(Default)]
struct OwnerOnly<T>(UnsafeCell<T>);

// SAFETY: see `with_own_cache`, the only place the cell is reached.
unsafe impl<T: Send> Sync for OwnerOnly<T> {}

/// run `f` on the calling worker thread's own cache, or return `None` on any
/// other thread. keyed by `CACHE_OWNER`, which only `worker_loop` sets, so a
/// test or probe that poses as a worker with `set_current_worker` uses the
/// shared pools instead of racing the real worker. `f` must not call back in.
fn with_own_cache<R>(f: impl FnOnce(&mut WorkerCache) -> R) -> Option<R> {
    let w = CACHE_OWNER.with(|c| c.get())?;
    // SAFETY: worker `w`'s cache is reached only from worker `w`'s own thread
    // (the one that set `CACHE_OWNER`), and `f` does not re-enter, so this is
    // the only reference.
    Some(f(unsafe { &mut *scheduler().workers[w].cache.0.get() }))
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
/// the manual `Send` sound is the hand-over discipline (see the pinning
/// section of the module doc): a coroutine crosses a thread boundary only
/// while it is off-CPU and held by exactly one thread, and nothing it holds
/// is tied to the thread it last ran on. concretely —
///
/// - a *fresh* task's coroutine has never resumed; the only captured state is
///   `closure_handle: i64` (trivially `Send`). fresh tasks are stealable, and
///   moving a not-yet-started coroutine is sound.
/// - a *parked* task's coroutine sits in its sync block, in no queue, and the
///   waker that wins the Parked -> Ready CAS is its only holder until it is
///   enqueued. by default that waker sends it back to its owner worker; with
///   migration on it may name the waking worker instead. what the stack holds
///   across the park is pith handles and runtime values, none of them derived
///   from the parking thread — the accessor block above `current_task` is the
///   rule that keeps it so, and `tooling/check_tls_barriers.sh` checks the
///   built archive for it.
///
/// the invariant is enforced by the scheduler's claim protocol, not the type
/// system.
struct SendCoroutine(TaskCoroutine);

// SAFETY: a coroutine only ever changes threads while off-CPU and held by one
// thread — fresh and stealable, or Parked and claimed by its waker through the
// scheduling word's CAS — and nothing on a suspended stack is derived from the
// thread it last ran on (see the type doc and the accessor block above
// `current_task`).
unsafe impl Send for SendCoroutine {}

/// a task's completion, shared between the worker that finishes it and
/// whoever awaits it. it lives in the task's sync block, which is reused by
/// later tasks, so `generation` tells them apart: a handle carries the
/// generation its task was spawned under, and retiring the task bumps it.
///
/// an os-thread awaiter (the main thread, or code off the green backend)
/// blocks on the block's `joined` condvar. a green awaiter must not block its
/// worker, so it adds its id to `awaiters` and parks; `finish_task` wakes
/// both kinds.
#[derive(Default)]
struct Join {
    generation: u32,
    /// a task holds the block: from spawn until it is retired. a handle to a
    /// block that is not live, or of another generation, finds nothing.
    live: bool,
    done: bool,
    result: i64,
    /// no one will await the task, so `finish_task` retires it on completion.
    detached: bool,
    /// green tasks parked in `green_await` on this one.
    awaiters: Vec<TaskId>,
    /// os-thread awaiters blocked on `joined`. completion signals the condvar
    /// only when this is non-zero.
    blocked: u32,
}

impl Join {
    /// whether `generation` names the task that holds this block.
    fn holds(&self, generation: u32) -> bool {
        self.live && self.generation == generation
    }

    /// end the task's hold on its block. its handle stops resolving, and the
    /// caller gives the id back with `free_id` once the join lock is released.
    fn retire(&mut self) {
        self.live = false;
        self.generation = self.generation.wrapping_add(1) & GEN_MASK;
    }
}

/// one worker's private run queues plus its own park spot. workers are created
/// once and live for the process; there is no shutdown path (the pool dies with
/// the process, like a daemon thread pool).
/// a worker's pinned work: a fifo of woken tasks plus the single `next` slot a
/// same-worker wake fills (see the `pinned` field note).
/// how many times running the affinity slot can win over the fifo before the
/// fifo gets a turn. high enough that an ordinary handoff never notices it, low
/// enough that a task queued behind a busy pair waits a bounded number of
/// exchanges rather than an unbounded one.
const MAX_AFFINITY_STREAK: u32 = 32;

#[derive(Default)]
struct PinnedQueue {
    next: Option<TaskId>,
    queue: VecDeque<TaskId>,
    /// consecutive `pop`s that have taken the affinity slot, reset whenever the
    /// fifo gets one.
    affinity_streak: u32,
}

impl PinnedQueue {
    /// take the next task to resume: the affinity slot first, then the fifo.
    ///
    /// the preference for the slot is bounded. displacing on `push_next` alone
    /// does not bound it, because a ping-ponging pair empties the slot on every
    /// `pop` before refilling it, so the displacement never fires and a third
    /// task sitting in the fifo waits behind the pair forever. counting the
    /// streak closes that: after `MAX_AFFINITY_STREAK` exchanges the fifo goes
    /// first, once, and the pair carries on from there.
    fn pop(&mut self) -> Option<TaskId> {
        if self.affinity_streak < MAX_AFFINITY_STREAK {
            if let Some(id) = self.next.take() {
                self.affinity_streak += 1;
                return Some(id);
            }
        }
        self.affinity_streak = 0;
        // with nothing queued there is no one to be fair to, so the slot still
        // runs — the streak only ever costs a turn when someone is waiting.
        self.queue.pop_front().or_else(|| self.next.take())
    }

    /// a cross-worker (or non-worker) wake: ordinary fifo.
    fn push(&mut self, id: TaskId) {
        self.queue.push_back(id);
    }

    /// a same-worker wake: run this one next, displacing any previous occupant to
    /// the back of the fifo so the slot cannot be monopolized.
    fn push_next(&mut self, id: TaskId) {
        if let Some(previous) = self.next.replace(id) {
            self.queue.push_back(previous);
        }
    }

    fn is_empty(&self) -> bool {
        self.next.is_none() && self.queue.is_empty()
    }
}

/// a queue of fresh, stealable tasks: a worker's `local` queue or the injector.
/// the length is mirrored in an atomic so an idle worker can tell an empty queue
/// from a full one without taking its lock. before the mirror, every idle probe
/// locked every worker's queue in turn, so the cost of looking for work grew
/// with the pool and a 32-worker pool spent most of its time in those locks.
///
/// the mirror is updated under the queue lock, so it is exact whenever the lock
/// is free. a reader that sees zero may miss a push in flight; that push wakes a
/// worker itself (see `wake_searcher`), so a stale zero costs nothing.
#[derive(Default)]
struct StealQueue {
    queue: Mutex<VecDeque<TaskId>>,
    len: AtomicUsize,
}

/// the most tasks one steal moves, besides the one the thief runs.
const STEAL_MAX: usize = 32;

impl StealQueue {
    fn lock(&self) -> MutexGuard<'_, VecDeque<TaskId>> {
        self.queue.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn is_empty(&self) -> bool {
        self.len.load(AtomicOrdering::SeqCst) == 0
    }

    fn push(&self, id: TaskId) {
        let mut q = self.lock();
        q.push_back(id);
        self.len.store(q.len(), AtomicOrdering::SeqCst);
    }

    fn pop(&self) -> Option<TaskId> {
        if self.is_empty() {
            return None;
        }
        let mut q = self.lock();
        let id = q.pop_front();
        self.len.store(q.len(), AtomicOrdering::SeqCst);
        id
    }

    /// pop the front task, unless another thread holds the queue. idle
    /// workers all look at the injector, and when they blocked on its lock
    /// they spent their time in the kernel instead of finding work elsewhere.
    /// a miss loses nothing: the queue's length still reads non-zero, so the
    /// caller looks again before it parks.
    fn try_pop(&self) -> Option<TaskId> {
        if self.is_empty() {
            return None;
        }
        let mut q = self.queue.try_lock().ok()?;
        let id = q.pop_front();
        self.len.store(q.len(), AtomicOrdering::SeqCst);
        id
    }

    /// take the front task to run and move up to half of the rest onto
    /// `into`, so a thief that found a backlog does not have to come back for
    /// each task. like `try_pop`, a thief skips a queue another thread holds.
    fn steal_half(&self, into: &StealQueue) -> Option<TaskId> {
        if self.is_empty() {
            return None;
        }
        let mut batch = [0; STEAL_MAX];
        let (id, taken) = {
            let mut q = self.queue.try_lock().ok()?;
            let id = q.pop_front()?;
            let taken = (q.len() / 2).min(STEAL_MAX);
            for (slot, stolen) in batch.iter_mut().zip(q.drain(..taken)) {
                *slot = stolen;
            }
            self.len.store(q.len(), AtomicOrdering::SeqCst);
            (id, taken)
        };
        if taken > 0 {
            let mut q = into.lock();
            q.extend(&batch[..taken]);
            into.len.store(q.len(), AtomicOrdering::SeqCst);
        }
        Some(id)
    }
}

/// aligned so that two workers never share a cache line: peers lock a
/// worker's queues to steal from it, and without the alignment each of those
/// locks evicted the neighbouring worker's queues as well.
#[repr(align(64))]
struct Worker {
    /// fresh, never-resumed tasks. stealable by peers because a fresh coroutine
    /// carries no live stack (see the pinning note).
    local: StealQueue,
    /// woken started tasks pinned to this worker. only this worker drains it, so
    /// a suspended coroutine only ever resumes on the thread that first ran it.
    /// `next` is the single task a same-worker wake asks to run first — the other
    /// half of a handoff, still cache-hot. two things keep that preference from
    /// starving the fifo behind it: the slot holds exactly ONE task, so a second
    /// same-worker wake displaces the first to the BACK of the fifo instead of
    /// stacking, and `pop` gives the fifo a turn after a bounded run of slot
    /// hits, which is what actually bounds a ping-ponging pair (it empties the
    /// slot before refilling it, so displacement alone never fires for it). one
    /// mutex covers both so the displace-and-push is a single atomic step.
    pinned: Mutex<PinnedQueue>,
    /// the length of `pinned`'s fifo, mirrored so an idle peer can pass over a
    /// worker with nothing to steal without taking its lock.
    pinned_fifo: AtomicUsize,
    /// preempted started tasks pinned to this worker, drained at the *lowest*
    /// priority (after pinned, local, injector, and stealing). a compute task that
    /// overran its quantum lands here so it yields to every other ready task on the
    /// worker; without this it would sit at the front of `pinned` and simply be
    /// re-selected, right back into the loop that got it preempted. also pinned to
    /// the owner, so a suspended coroutine still never crosses a worker.
    deferred: Mutex<VecDeque<TaskId>>,
    /// free stacks and task ids for this worker's own use (see
    /// `with_own_cache`).
    cache: OwnerOnly<WorkerCache>,
    /// whether this worker is currently parked in its poller (`netpoll::
    /// idle_wait`). each worker parks on its own spot, so a wake can target
    /// exactly the worker that is able to run the task — crucially, a *pinned*
    /// wake nudges only the task's owner instead of a pool-wide notify that also
    /// wakes idle peers which have nothing to run. the flag lets `wake_worker`
    /// skip the notify entirely when the target is busy running tasks, the
    /// common case on a hot pipeline. the Dekker pairing is in `park`: it
    /// publishes the flag before its final work re-check, and enqueuers publish
    /// their work before reading the flag, so at least one side always sees the
    /// other. a notify that lands before the wait starts is kept by the poller
    /// (a pending eventfd, or the fallback's flag), so it is never lost.
    parked: AtomicBool,
    /// set by `wake_searcher` when it wakes this worker to look for fresh work.
    /// the waker has already counted the worker in `Scheduler::searching`, so
    /// the worker takes over that count when it sees the flag (see
    /// `worker_loop`) instead of adding its own.
    search_token: AtomicBool,
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
    /// how many tasks this worker has resumed. only the worker writes it; a
    /// peer that sees it stand still knows the worker is inside one long task.
    resumes: AtomicU64,
}

/// the whole green runtime: the per-task sync blocks, the per-worker queues
/// and park spots, and the injector for off-worker spawns.
struct Scheduler {
    /// the lock-free per-task scheduling words, indexed by slab task id (see
    /// the scheduling-word section).
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

    /// the slab id of the green task currently running on this worker, or `None`
    /// between tasks and on any non-worker thread. installed around `resume` in
    /// `run_task`. `channel.rs` reads it (via `current_task`) to tell whether it
    /// is running inside a green task — if so it parks by suspending the
    /// coroutine; if not (the main thread, or the os-thread backend) it keeps the
    /// old condvar-wait behavior. this is the seam that lets one channel serve
    /// both kinds of waiter.
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
// 2. a cell whose *address* has to exist as a value — the lazily initialized
//    handle behind the pool slot, the `RefCell` maps `TLS_GLOBALS` and
//    `MY_MUTATOR` — is touched only inside a `#[inline(never)]` function that
//    returns before anything can suspend. the optimizer may fold identical
//    address computations within one function; it may not fold them across
//    a call it cannot inline.
//
// what a frame that spans a park holds is then only values. of those, a task
// id and the tls map pointer belong to the task and stay right across a
// migration; a worker index belongs to the thread and must be re-read (call
// the accessor again) after any park.
// ---------------------------------------------------------------------------

/// the slab id of the green task running on this worker thread, or `None` when
/// no green task is active here (the main thread, a non-worker thread, or the
/// os-thread backend — where nothing ever sets this). `channel.rs` uses it to
/// choose between suspending the coroutine (green) and condvar-waiting (os).
///
/// a task id is task-owned: reading it once at the top of a block site and
/// carrying it across a park is right, and every block site does exactly that.
#[inline]
pub(crate) fn current_task() -> Option<TaskId> {
    CURRENT_TASK.with(|c| c.get())
}

/// the index of the worker this os thread is, or `None` off the pool. a
/// worker index is thread-owned: it must not be carried across a park, and a
/// caller that needs it after one calls this again.
#[inline]
fn current_worker() -> Option<usize> {
    CURRENT_WORKER.with(|c| c.get())
}

/// the index of the worker this os thread is, for the netpoller, which keys
/// its per-worker pollers by it. thread-owned, like `current_worker`.
#[inline]
pub(crate) fn worker_index() -> Option<usize> {
    current_worker()
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

/// suspend the green task `id` back to its worker. must be called only from
/// inside that task (every caller has just gone through a `current_task()`
/// check, and passes the id it got from it), and only *after* releasing any
/// channel lock — `suspend` returns control to the worker, which may run other
/// pith code that touches the same channel.
///
/// returns when the task is resumed by a later `wake`.
///
/// the id is a parameter rather than a thread-local read for a reason worth
/// keeping. five call sites park inside a `loop`, and a thread-local read
/// inside a loop is exactly the shape a compiler may hoist above it: the
/// second iteration would then suspend through whatever the *first* iteration
/// loaded. that is harmless only while a task always resumes on the thread it
/// parked on. the slab id, by contrast, is a plain index that means the same
/// thing on every thread, and the yielder is read out of the task's own sync
/// block — so this path holds no thread-derived value at all. see the accessor
/// block above `current_task` for the rule this is one instance of.
pub(crate) fn park_current(id: TaskId) {
    // SAFETY: the caller is running inside task `id`, which means this worker
    // holds the Running claim on its block (see the claim protocol on
    // `SyncBlock`), so reading the cell is exclusive. the pointer was published
    // by the coroutine body on its first resume and names the `Yielder` living
    // on that coroutine's own stack, which is the stack we are executing on.
    let yielder = match sync_block(id) {
        Some(block) => unsafe { *block.yielder.get() },
        None => ptr::null(),
    };
    // unreachable: a task cannot reach a block site before its body has run and
    // published the yielder. panic rather than dereference null — `run_task`
    // catches it, marks the task done and keeps the worker alive, where a null
    // deref would take the process with it.
    assert!(
        !yielder.is_null(),
        "park_current on a task with no running coroutine (id {id})"
    );
    // SAFETY: as above — the `Yielder` is valid for as long as this coroutine is
    // on-CPU, which is exactly the window in which pith code can reach this
    // call. the suspend is a stack switch back to the worker; nothing retains
    // the pointer across it.
    unsafe { &*yielder }.suspend(());
}

/// record the running coroutine's `Yielder` in its own sync block, so
/// `park_current` can find it later. called once, at the top of a task body on
/// its first resume — before the task can possibly reach a block site.
///
/// this is a task-entry read of `CURRENT_TASK`, which is the one place the
/// `CURRENT_*` cells may be consulted: `run_task` has just installed them and
/// no suspension has happened yet.
fn publish_yielder(yielder: *const TaskYielder) {
    let Some(id) = current_task() else {
        return;
    };
    if let Some(block) = sync_block(id) {
        // SAFETY: this worker holds the Running claim on the block for the whole
        // resume (see the claim protocol on `SyncBlock`), so the write is
        // exclusive. the yield arm of `run_task` releases it to the next
        // claimant through the transition CAS.
        unsafe { *block.yielder.get() = yielder }
    }
}

/// wake a parked green task: move it back onto its owner worker's queue so it
/// resumes and re-checks the channel condition it blocked on. called by
/// `channel.rs` from a send/recv/close while holding the channel lock; it takes
/// no scheduler lock itself beyond the owner's queue mutex (the state flip is a
/// CAS on the task's scheduling word), so the nesting stays block-site -> queue.
/// safe to call for a task in any state — it acts only when the task is actually
/// parked, and defers via the wake-pending flag if the task is still mid-suspend,
/// so a wake is never lost and never double-enqueued.
pub(crate) fn wake(id: TaskId) {
    let Some(block) = sync_block(id) else {
        return;
    };
    let Some(owner) = claim_parked(block) else {
        return;
    };
    // a parked task always ran, so `owner` is set; fall back to the injector
    // only defensively.
    match owner {
        Some(w) => enqueue_woken(w, id),
        None => enqueue_fresh(id),
    }
}

/// the Parked -> Ready claim both wake paths make, deciding in the same CAS
/// which worker will resume the task. returns the worker to enqueue it on
/// (`Some(None)` only in the defensive never-pinned case), or `None` when the
/// task was not parked: still `Running` (the pending flag is set so the yield
/// arm re-enqueues it), already `Ready`, or `Done`.
///
/// with migration on (the default, read once into the scheduler), a worker
/// that wakes a task pinned to a different worker takes it: the
/// owner bits are rewritten in the claiming CAS, so from the moment the task
/// is `Ready` every reader of the word — the enqueue below, the resume that
/// follows, the next wake — agrees on the new home. a pair of tasks that talk
/// to each other converge onto one worker after the first exchange, and stay
/// there: a wake from the same worker moves nothing. a waker that is not a
/// worker (`main`, a blocking-pool thread) has nowhere to take the task and
/// leaves the owner alone. with `PITH_GREEN_MIGRATE=0` the task always goes
/// back to its owner.
///
/// this is only sound because nothing on the resumable path derives a value
/// from the thread the task parked on — see the accessor block above
/// `current_task`, and the pinning section of the module doc.
#[inline]
fn claim_parked(block: &SyncBlock) -> Option<Option<usize>> {
    let sched = scheduler();
    let mut word = block.word.load(AtomicOrdering::Acquire);
    loop {
        match word & S_STATE_MASK {
            // parked with its coroutine put back: claim it, naming its home.
            S_PARKED => {
                let owner = word_owner(word);
                let home = if sched.migrate {
                    match (owner, current_worker()) {
                        (Some(o), Some(w)) if w != o => Some(w),
                        _ => owner,
                    }
                } else {
                    owner
                };
                let next = home.map_or(S_READY, |w| owner_bits(w) | S_READY);
                match block.word.compare_exchange_weak(
                    word,
                    next,
                    AtomicOrdering::AcqRel,
                    AtomicOrdering::Acquire,
                ) {
                    Ok(_) => {
                        if sched.wake_stats {
                            note_wake(owner, home);
                        }
                        return Some(home);
                    }
                    Err(current) => word = current,
                }
            }
            // still resuming/suspending: it has not yet recorded itself as
            // Parked, so set the pending flag; the yield arm in `run_task` will
            // see it and re-enqueue instead of parking.
            S_RUNNING => {
                match block.word.compare_exchange_weak(
                    word,
                    word | S_WAKE_PENDING,
                    AtomicOrdering::AcqRel,
                    AtomicOrdering::Acquire,
                ) {
                    Ok(_) => return None,
                    Err(current) => word = current,
                }
            }
            // already queued to re-check, or finished: nothing to do.
            _ => return None,
        }
    }
}

/// profiling: categorize a claimed wake by where the waker sits relative to
/// the task's owner worker, and count a migration when the claim moved the
/// task. read-only; the caller has checked that a stats switch is on.
#[cold]
#[inline(never)]
fn note_wake(owner: Option<usize>, home: Option<usize>) {
    let caller = current_worker();
    let (green, perf) = match (caller, owner) {
        (Some(w), Some(o)) if w == o => (&WAKE_SAME, &crate::PERF_GREEN_WAKES_SAME),
        (Some(_), Some(_)) => (&WAKE_CROSS, &crate::PERF_GREEN_WAKES_CROSS),
        // waker is not a worker: main or a blocking-pool thread. either way
        // it must cross into a worker's queue.
        _ => (&WAKE_REACTOR, &crate::PERF_GREEN_WAKES_REACTOR),
    };
    // the green-stats counters are unconditional here (their own dump is
    // registered when the scheduler is built); the perf counters go through
    // the recording hook so their dump is registered even when nothing else
    // in the program counted first.
    green.fetch_add(1, AtomicOrdering::Relaxed);
    if crate::perf_stats_enabled() {
        crate::perf_record1(perf, 1);
    }
    if home != owner {
        WAKE_MIGRATED.fetch_add(1, AtomicOrdering::Relaxed);
        if crate::perf_stats_enabled() {
            crate::perf_record1(&crate::PERF_GREEN_MIGRATIONS, 1);
        }
    }
}

/// wake a parked green task and, if it is truly parked, run `fetch` and hand it
/// the result: a channel send that wakes a parked receiver dequeues on the
/// receiver's behalf, so the receiver resumes with the value in hand instead of
/// re-locking the channel and re-trying the ring — go-style direct handoff, but
/// keeping the ring as the ordering source so fifo holds. `fetch` runs only
/// after the task is claimed (the Parked -> Ready CAS), which is what makes
/// handing over safe: a task that is still `Running` might complete its own
/// attempt, so it gets a plain pending-flag wake and the value stays in the
/// ring. a `fetch` that comes back empty (a racing consumer drained the ring)
/// still wakes the task valueless — it re-checks and re-parks, the usual
/// tolerance.
pub(crate) fn wake_with<F: FnOnce() -> Option<i64>>(id: TaskId, fetch: F) {
    let Some(block) = sync_block(id) else {
        return;
    };
    let Some(owner) = claim_parked(block) else {
        return;
    };
    // the claim above made this thread the only one touching the handoff cell
    // until the task is enqueued (below) and resumed: a Parked task is in no
    // queue, and every other waker now reads Ready and no-ops. see the field
    // doc on `SyncBlock::handoff` for the publication argument.
    // SAFETY: exclusive by the claim, as documented on the field.
    unsafe { *block.handoff.get() = fetch() };
    match owner {
        Some(w) => enqueue_woken(w, id),
        None => enqueue_fresh(id),
    }
}

/// take (and clear) the value a waker handed to task `id`, if any. the first
/// thing a resumed channel block site checks.
///
/// the id is a parameter for the same reason `park_current` takes one: the one
/// caller reads it *before* parking and passes it back in afterwards, so the
/// resumed path never re-derives its identity from a thread-local.
pub(crate) fn take_handoff(id: TaskId) -> Option<i64> {
    let block = sync_block(id)?;
    // SAFETY: only the running task itself reads its handoff cell, and a
    // claiming waker only writes it while the task is Parked — disjoint by the
    // state machine (see `SyncBlock::handoff`).
    unsafe { (*block.handoff.get()).take() }
}

/// the `tls` map of the green task currently running on this thread, or `None`
/// when no green task is active here. the runtime's `threadlocal` FFI consults
/// this so that, under the green backend, each task reads and writes its own
/// thread-local globals rather than sharing the worker OS thread's map.
///
/// the returned pointer is task-owned: it targets the `Box` the task's slab
/// entry holds, which `run_task` keeps alive for the whole `resume` and which
/// only the running task ever dereferences — so it stays right across a park
/// even when the task resumes on another thread, and the `threadlocal`
/// initializer thunk (pith code, which can park) may run between a read of
/// this and the map insert that follows it.
#[inline]
pub(crate) fn current_task_tls() -> Option<*mut TlsMap> {
    let ptr = CURRENT_TLS.with(|c| c.get());
    if ptr.is_null() {
        None
    } else {
        Some(ptr)
    }
}

// contended-acquire and hot-path counters for the same `PITH_GREEN_STATS`
// lens as the wake breakdown above. a `try_lock` miss on a pinned queue is
// one contended acquire; the channel-wake and park counters size the hot
// paths those locks sit on.
pub static C_PINNED: AtomicU64 = AtomicU64::new(0);
pub static C_CH_WAKE: AtomicU64 = AtomicU64::new(0);
static C_PARKS: AtomicU64 = AtomicU64::new(0);

fn dump_contention() {
    eprintln!(
        "[green-stats] contended: pinned={} | channel-wakes={} worker-parks={}",
        C_PINNED.load(AtomicOrdering::Relaxed),
        C_CH_WAKE.load(AtomicOrdering::Relaxed),
        C_PARKS.load(AtomicOrdering::Relaxed)
    );
}

/// count one channel wake slow-path (stats-gated; see the counter block above).
pub fn note_channel_wake() {
    if stats_on() {
        C_CH_WAKE.fetch_add(1, AtomicOrdering::Relaxed);
    }
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

/// start the preemption monitor (sysmon) thread. it is the only thing that ever
/// sets `PITH_PREEMPT_REQUESTED`, and it starts only here alongside the green
/// workers — so under the os-thread backend it never runs and the flag stays 0.
fn start_monitor() {
    if let Err(err) = std::thread::Builder::new()
        .name("pith-green-sysmon".to_string())
        .spawn(monitor_loop)
    {
        runtime_fatal!("could not start the green preemption monitor: {err}")
    }
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
        // a pending cycle-gc stop keeps the flag up regardless: the stop path
        // raised it as an accelerator (every safe-point routes through the
        // collector gate), and a quiet monitor pass must not lower it while
        // the rendezvous is still waiting on some mutator.
        PITH_PREEMPT_REQUESTED.store(
            u8::from(any_overrun || crate::cycle::stop_requested()),
            AtomicOrdering::Relaxed,
        );
    }
}

/// push a *fresh* (never-resumed) task onto a stealable queue and wake one idle
/// worker. spawns from a worker go to that worker's local queue (cache-friendly,
/// keeps a task's children near it); spawns from elsewhere go to the injector.
/// only fresh tasks travel this path, so anything a peer steals from a `local`
/// queue is stack-empty and safe to move across threads.
fn enqueue_fresh(id: TaskId) {
    let sched = scheduler();
    match current_worker() {
        Some(i) => sched.workers[i].local.push(id),
        None => sched.injector.push(id),
    }
    note_notify();
    wake_searcher();
}

/// make sure some worker is looking for the fresh work just queued. if a worker
/// is already searching it will find the task, so nothing is woken; otherwise
/// one parked worker is woken, already counted as a searcher.
///
/// this used to wake every worker on every spawn. one of them took the task and
/// the rest re-scanned the pool and parked again, so on 32 workers a spawn cost
/// about 28 park/wake round trips and a fan-out ran 100x slower than on one
/// worker. claiming the single search slot with a CAS is what keeps a burst of
/// spawns from waking the pool; `stop_searching` passes the slot on when the
/// burst is bigger than one worker.
///
/// the pusher stores its task before it reads `searching` here, and a searcher
/// that gives up decrements `searching` before its last look at the queues
/// (`worker_loop`), so one side always sees the other.
fn wake_searcher() {
    let sched = scheduler();
    if sched
        .searching
        .compare_exchange(0, 1, AtomicOrdering::SeqCst, AtomicOrdering::SeqCst)
        .is_err()
    {
        return;
    }
    let n = sched.workers.len();
    let start = steal_start(n);
    for k in 0..n {
        let i = (start + k) % n;
        let worker = &sched.workers[i];
        if !worker.parked.load(AtomicOrdering::SeqCst) {
            continue;
        }
        // a token left on a worker by an earlier wake that it has not consumed
        // yet means that worker already holds a search slot; try another.
        if worker.search_token.swap(true, AtomicOrdering::SeqCst) {
            continue;
        }
        wake_worker(i);
        return;
    }
    // nobody is parked: every worker is running a task or about to park, and
    // each looks at the stealable queues before it parks. give the slot back.
    sched.searching.fetch_sub(1, AtomicOrdering::SeqCst);
}

/// a searcher found a task. drop its search slot, and if it was the last
/// searcher and fresh work is still queued, wake another worker to search for
/// it. that hand-off is how a large fan-out spreads across the pool.
fn stop_searching() {
    let sched = scheduler();
    if sched.searching.fetch_sub(1, AtomicOrdering::SeqCst) == 1 && any_stealable() {
        wake_searcher();
    }
}

/// is there fresh work in the injector or in any worker's local queue? reads
/// only the queues' atomic lengths.
fn any_stealable() -> bool {
    let sched = scheduler();
    !sched.injector.is_empty() || sched.workers.iter().any(|w| !w.local.is_empty())
}

/// re-enqueue a *woken started* task onto its owner worker's pinned queue and
/// wake the pool. this is the only path a task that has already resumed takes
/// back into scheduling, and it always targets the owner recorded in the
/// task's word — which is the worker that first ran it, or, with migration
/// on, the worker whose wake just claimed it (see `claim_parked`).
fn enqueue_woken(owner: usize, id: TaskId) {
    let sched = scheduler();
    {
        let m = &sched.workers[owner].pinned;
        let mut pinned = match m.try_lock() {
            Ok(g) => g,
            Err(_) => {
                if stats_on() {
                    C_PINNED.fetch_add(1, AtomicOrdering::Relaxed);
                }
                m.lock().unwrap_or_else(|p| p.into_inner())
            }
        };
        // wake affinity: when the waker is the owner worker itself, the woken
        // task is usually the other half of a handoff the current task just
        // completed — ask to run it next, while its data is hot, instead of
        // behind every earlier wake. it goes in the single `next` slot, so the
        // second such wake displaces the first to the back of the fifo: a
        // ping-ponging pair yields the slot on every exchange and cannot starve
        // the tasks queued behind it (preemption cannot rescue that shape —
        // each half re-parks long before it overruns a quantum). cross-worker
        // wakes are plain fifo.
        if current_worker() == Some(owner) {
            pinned.push_next(id);
        } else {
            pinned.push(id);
        }
        sched.workers[owner].pinned_fifo.store(pinned.queue.len(), AtomicOrdering::Relaxed);
    }
    // nudge *only* the owner. a peer steals from a pinned queue only once the
    // owner has been stuck in one task, so waking one here would mostly find
    // nothing to take. if the owner is busy the notify is
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

/// nudge one worker's park spot, its poller. a worker that is not parked needs
/// nothing: it re-runs find_work on its own.
fn wake_worker(index: usize) {
    let worker = &scheduler().workers[index];
    // `parked` is published before park's final work re-check, and our caller
    // enqueued before this load, so a worker mid-park either sees the task or
    // is visible here.
    if !worker.parked.load(AtomicOrdering::SeqCst) {
        return;
    }
    crate::netpoll::notify(index);
}

/// nudge every worker. only the cycle collector's stop-the-world uses it, since
/// that needs every parked worker to reach its gate. fresh work wakes a single
/// searcher instead (`wake_searcher`).
pub(crate) fn wake_pool() {
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
/// steal. only fresh, never-resumed tasks travel those queues: a fresh task
/// has no `owner` yet and sits `Ready`. if either were false here, a started
/// task's coroutine would be about to resume from a queue that is not its
/// owner's — outside the claim protocol that hands a suspended coroutine
/// between threads (a wake's Parked -> Ready CAS, see `claim_parked`). cheap,
/// and compiled out in release.
///
/// history worth keeping: re-homing a parked task to the worker that wakes it
/// (now the default, see `claim_parked`) was first tried before the runtime's own rust
/// frames stopped holding thread-local addresses across a suspend, and it
/// silently dropped channel messages. the coroutine stack was never the
/// problem and neither was the pith compiler (a `threadlocal` global lowers
/// to a call, and `CURRENT_TLS` is per-task); see the accessor block above
/// `current_task` for what was, and `pith_green_cross_thread_probe` with
/// `tests/cross_thread_resume.rs` for the check that keeps it fixed against
/// the shipped archive.
#[cfg(debug_assertions)]
fn debug_assert_stealable(id: TaskId) {
    if let Some(block) = sync_block(id) {
        let word = block.word.load(AtomicOrdering::Acquire);
        debug_assert!(
            word_owner(word).is_none(),
            "stole/ran-fresh a task already pinned to a worker (id {id})"
        );
        debug_assert!(
            word & S_STATE_MASK == S_READY,
            "a stealable task must be Ready, never mid-run (id {id})"
        );
    }
}
#[cfg(not(debug_assertions))]
#[inline(always)]
fn debug_assert_stealable(_id: TaskId) {}

/// find the next ready task for worker `index`: its pinned queue first (resume
/// suspended work it owns), then its own local queue, then the injector, then a
/// steal from a random peer's local queue, and only then its deferred queue.
/// with `deferred_first` the deferred queue goes first instead; the worker loop
/// asks for that every `POLL_EVERY` tasks so a preempted task waits a bounded
/// number of turns. returns `None` if the whole pool is momentarily empty.
fn find_work(index: usize, deferred_first: bool) -> Option<TaskId> {
    let sched = scheduler();

    if deferred_first {
        if let Some(id) = pop_deferred(index) {
            return Some(id);
        }
    }

    // pinned first: these are woken tasks this worker must resume itself.
    {
        let worker = &sched.workers[index];
        let mut pinned = worker.pinned.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(id) = pinned.pop() {
            worker.pinned_fifo.store(pinned.queue.len(), AtomicOrdering::Relaxed);
            return Some(id);
        }
    }

    // own local queue (stealable): pop into a local so the queue lock is released
    // before the debug invariant check locks the slab.
    let own = &sched.workers[index].local;
    if let Some(id) = own.pop() {
        debug_assert_stealable(id);
        return Some(id);
    }

    // one at a time from the injector, unlike a peer steal below. the
    // injector holds what `main` spawned, often a handful of tasks that talk
    // to each other; taking half of them packed them onto one worker and made
    // the channel fan-out benchmark about twice as slow.
    if let Some(id) = sched.injector.try_pop() {
        debug_assert_stealable(id);
        return Some(id);
    }

    // steal from a peer. start at a pseudo-random offset so workers do not all
    // hammer worker 0. an empty victim is skipped on its atomic length, so the
    // scan takes a lock only where there is something to take, and a hit
    // takes half the victim's backlog along with it.
    let n = sched.workers.len();
    let start = steal_start(n);
    for k in 0..n {
        let victim = (start + k) % n;
        if victim == index {
            continue;
        }
        if let Some(id) = sched.workers[victim].local.steal_half(own) {
            debug_assert_stealable(id);
            return Some(id);
        }
    }

    // then a started task waiting behind a peer that is busy with another.
    for k in 0..n {
        let victim = (start + k) % n;
        if victim != index {
            if let Some(id) = steal_pinned(victim, index) {
                return Some(id);
            }
        }
    }

    // deferred last: only resume a preempted compute task once there is genuinely
    // nothing else — no owned woken work, no fresh work anywhere to run or steal.
    // this is what lets the other tasks on this worker make progress before the
    // hog runs again.
    pop_deferred(index)
}

/// how long a thief watches a peer before taking a task off its pinned fifo.
/// a peer that resumes anything in this window is getting through its queue,
/// and taking from it would split a pipeline that runs best on one worker.
const PIN_STEAL_WAIT: Duration = Duration::from_micros(20);

/// take the oldest task off `victim`'s pinned fifo and make worker `thief` its
/// owner, if the victim has been inside one task for `PIN_STEAL_WAIT`. the
/// affinity slot is left alone: it holds the other half of a handoff the
/// victim resumes next.
///
/// a task in a pinned queue is `Ready` and held by that queue alone (a waker
/// that reads `Ready` changes nothing), so popping it under the victim's lock
/// gives this thread the only right to rewrite its owner. that is the same
/// hand-over a migrating wake makes in `claim_parked`.
fn steal_pinned(victim: usize, thief: usize) -> Option<TaskId> {
    let worker = &scheduler().workers[victim];
    if worker.pinned_fifo.load(AtomicOrdering::Relaxed) == 0 {
        return None;
    }
    let before = worker.resumes.load(AtomicOrdering::Relaxed);
    let deadline = Instant::now() + PIN_STEAL_WAIT;
    while Instant::now() < deadline {
        std::hint::spin_loop();
        if worker.resumes.load(AtomicOrdering::Relaxed) != before {
            return None;
        }
    }
    if worker.running_task.load(AtomicOrdering::Relaxed) == 0 {
        return None;
    }
    let id = {
        let mut pinned = worker.pinned.lock().unwrap_or_else(|p| p.into_inner());
        let id = pinned.queue.pop_front()?;
        worker.pinned_fifo.store(pinned.queue.len(), AtomicOrdering::Relaxed);
        id
    };
    if let Some(block) = sync_block(id) {
        let mut word = block.word.load(AtomicOrdering::Acquire);
        while word & S_STATE_MASK == S_READY {
            let next = (word & !S_OWNER_MASK) | owner_bits(thief);
            match block.word.compare_exchange_weak(
                word,
                next,
                AtomicOrdering::AcqRel,
                AtomicOrdering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => word = current,
            }
        }
    }
    Some(id)
}

/// take the oldest preempted task off worker `index`'s deferred queue.
fn pop_deferred(index: usize) -> Option<TaskId> {
    scheduler().workers[index]
        .deferred
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .pop_front()
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

/// park an idle worker until work likely exists. it waits in its poller
/// (`netpoll::idle_wait`), so a readiness on one of its sockets, a socket or
/// sleep deadline, or a notify from `wake_worker` ends the wait. the timeout
/// is a backstop, so a missed wakeup costs latency, never a hang.
///
/// the timeout is the caller's: `worker_loop` starts at 1ms and doubles it
/// on each consecutive timed-out park (capped), so a busy pool keeps the
/// tight bound while an idle one stops ticking the clock ~1000 times a
/// second per worker. before the backoff, an idle two-worker process made
/// ~2,000 failing futex calls per second doing nothing. returns whether
/// the wait timed out (true) or was cut short by a notify (false).
fn park(index: usize, timeout: Duration) -> bool {
    let worker = &scheduler().workers[index];
    // publish the flag BEFORE the final work re-check. an enqueuer publishes
    // its work before reading the flag (wake_worker's fast path), so either it
    // sees us parked and pays the notify, or our re-check below sees its task
    // and we never sleep. flipping this order would let both sides miss each
    // other and cost a full park timeout per task.
    worker.parked.store(true, AtomicOrdering::SeqCst);
    // one last check: if work appeared, don't sleep.
    if has_any_work(index) {
        worker.parked.store(false, AtomicOrdering::SeqCst);
        return false;
    }
    // profiling: mark ourselves parked so a concurrent wake can be attributed as
    // a real futex wake vs one absorbed by a busy pool.
    if stats_on() {
        PARKED_WORKERS.fetch_add(1, AtomicOrdering::Relaxed);
    }
    if stats_on() {
        C_PARKS.fetch_add(1, AtomicOrdering::Relaxed);
    }
    let timed_out = crate::netpoll::idle_wait(index, timeout);
    worker.parked.store(false, AtomicOrdering::SeqCst);
    if stats_on() {
        PARKED_WORKERS.fetch_sub(1, AtomicOrdering::Relaxed);
    }
    timed_out
}

/// how long a worker keeps looking for work before it parks. this is the cost
/// of a *split* pipeline: when the two halves of a request/response pair pin to
/// different workers, each message hands off across threads, and without this
/// the receiving worker parks and must be futex-woken every single time. a
/// two-task ping-pong that lands split measured ~19us per round against ~0.2us
/// colocated — a 90x cliff that a bounded spin closes, because the partner's
/// reply is on its way already.
///
/// it is only paid when the worker found work recently (`idle_timeout` still at
/// its floor). an idle pool spins once on the way down and then backs off, so
/// this does not undo the idle-syscall work: a worker that is genuinely out of
/// work parks as before.
const PARK_SPIN: Duration = Duration::from_micros(10);

/// how many tasks a busy worker runs between non-blocking polls of its
/// sockets and serves its deferred queue first. go checks its global queue on
/// the same kind of count (61); this only bounds how long a ready socket's
/// task, or a preempted one, can wait behind queued work.
const POLL_EVERY: u32 = 61;

/// look for work for up to `PARK_SPIN` before parking. probes are spaced by a
/// short pause burst rather than run back-to-back: `has_any_work` takes the
/// queue locks, and hammering them would slow down the very peer this worker is
/// waiting on.
fn spin_for_work(index: usize) -> bool {
    let deadline = Instant::now() + PARK_SPIN;
    loop {
        for _ in 0..64 {
            std::hint::spin_loop();
        }
        if has_any_work(index) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
    }
}

/// is there any work *this* worker could actually take? used only to avoid
/// parking on a false-empty; the authoritative pop is still `find_work`, and it
/// must consider exactly the same sources: this worker's own pinned queue, its
/// own local queue, the injector, and any peer's *local* (stealable) queue.
///
/// it must NOT count a *peer's pinned* queue. `find_work` can steal from one,
/// but only from a peer stuck in one long task; counting the queue here kept
/// every idle worker spinning beside a busy pipeline instead of parking. on
/// the channel fan-out benchmark that took 20 times the CPU for the same
/// wall time. an idle worker still looks at peers' pinned queues on each
/// pass through `find_work` before it parks.
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
    // the injector and any worker's local queue are stealable (own or a
    // peer's). their atomic lengths answer this without a lock.
    if any_stealable() {
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
fn worker_loop(index: usize, cycle_slot: Option<Arc<crate::cycle::MutatorSlot>>) {
    set_current_worker(Some(index));
    CACHE_OWNER.with(|c| c.set(Some(index)));
    // a worker moves reference counts, so it is a mutator the cycle collector
    // must be able to stop. the slot was registered by the spawning thread
    // (see `ensure_workers_started`); adopting it makes it this thread's own.
    crate::cycle::adopt_mutator_slot(cycle_slot);
    // the park timeout backs off while this worker keeps finding nothing:
    // the tight bound only earns its keep when a wake is imminent, and an
    // idle pool paying it forever is pure syscall noise (measured: ~1,000
    // failing futex waits per second per worker, and the runtime's own
    // futex traffic scaled linearly with worker count on an idle pool).
    // any work at all resets it, so a busy pool never sees the long tail.
    let mut idle_timeout = PARK_TIMEOUT_MIN;
    // whether this worker holds one of the `searching` slots. it takes one
    // when it runs out of work, or inherits one from the `wake_searcher` that
    // woke it, and gives it up when it finds a task or parks.
    let mut searching = false;
    let worker = &scheduler().workers[index];
    let mut since_poll: u32 = 0;
    // set on the same count as the socket poll: the next `find_work` serves
    // one preempted task ahead of everything else.
    let mut deferred_turn = false;
    loop {
        if worker.search_token.swap(false, AtomicOrdering::SeqCst) {
            if searching {
                // already counted: the slot the waker took is one too many.
                scheduler().searching.fetch_sub(1, AtomicOrdering::SeqCst);
            }
            searching = true;
        }
        match find_work(index, std::mem::take(&mut deferred_turn)) {
            Some(id) => {
                idle_timeout = PARK_TIMEOUT_MIN;
                if searching {
                    searching = false;
                    stop_searching();
                }
                // between tasks is the worker's stop point: the previous
                // coroutine is off-CPU and the next has not started, so a
                // worker parked here holds no pith frame at all.
                crate::cycle::mutator_gate();
                run_task(id);
                // a worker that always has work never parks, so it never
                // waits in its poller: look at its sockets every so often, or
                // the tasks whose data has arrived would wait behind all of
                // its queued work. the same count gives the deferred queue a
                // turn, or a preempted task would wait for the worker to run
                // dry, which a server under steady load may not do for
                // hundreds of milliseconds.
                since_poll += 1;
                if since_poll >= POLL_EVERY {
                    since_poll = 0;
                    deferred_turn = true;
                    crate::netpoll::poll_ready(index);
                }
            }
            None => {
                // the idle path passes this gate once per park timeout, so an
                // idle worker answers a stop request within that bound. the
                // collector raises the preemption flag and notifies the pool
                // when it wants the world stopped, which cuts a long park
                // short — the bound below is the fallback, not the mechanism.
                crate::cycle::mutator_gate();
                // out of queued work: pick up whatever this worker's sockets
                // have ready before searching or parking.
                since_poll = 0;
                if crate::netpoll::poll_ready(index) {
                    continue;
                }
                // an idle worker is about to look at the stealable queues
                // until it parks, so it counts as a searcher: a spawn in that
                // window need not wake anyone else.
                if !searching {
                    searching = true;
                    scheduler().searching.fetch_add(1, AtomicOrdering::SeqCst);
                }
                // a worker still at the floor timeout was running a moment ago,
                // so a handoff aimed at it is plausibly already in flight —
                // worth a bounded look before paying a park/futex round trip.
                if idle_timeout == PARK_TIMEOUT_MIN && spin_for_work(index) {
                    continue;
                }
                // give the slot up before the last look at the queues, so a
                // spawn racing with this either sees no searcher and wakes a
                // worker, or lands before the look below and is seen by it.
                searching = false;
                scheduler().searching.fetch_sub(1, AtomicOrdering::SeqCst);
                if any_stealable() {
                    continue;
                }
                if park(index, idle_timeout) {
                    idle_timeout = (idle_timeout * 2).min(PARK_TIMEOUT_MAX);
                } else {
                    idle_timeout = PARK_TIMEOUT_MIN;
                }
            }
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

/// record a task's completion and unblock everyone awaiting it.
///
/// both `run_task` completion paths come here, a clean return and the panic
/// boundary, so an awaiter gets the same defined outcome either way. once
/// `done` is set under the join lock, a later `green_await` sees it and
/// returns without registering, so draining `awaiters` here loses no one.
fn finish_task(id: TaskId, result: i64) {
    let Some(block) = sync_block(id) else {
        return;
    };
    // from here on every wake reads Done and no-ops. a plain store is enough:
    // only this worker finishes the task, and a racing flag CAS either lands
    // first (and is overwritten) or fails, reloads and sees Done.
    block.word.store(S_DONE, AtomicOrdering::Release);

    // the body has returned, so the closure that held its captured arguments
    // and the task's threadlocal values can go. the emitter hands the closure
    // to `spawn` without releasing it, so the task owns its one count.
    //
    // SAFETY: this worker still holds the claim it took to resume the task.
    // nothing else touches these cells until the task is retired below or by
    // its awaiter, which waits for `done`.
    let (closure, tls_values) =
        unsafe { (std::mem::take(&mut *block.closure.get()), (*block.tls.get()).take_values()) };
    if closure != 0 {
        unsafe { crate::pith_closure_release(closure) };
    }
    crate::runtime_core::release_finished_tls(tls_values);

    let retired = {
        let mut join = lock_join(&block.join);
        join.done = true;
        join.result = result;
        if join.blocked > 0 {
            block.joined.notify_all();
        }
        // `wake` takes only the scheduling word and a run-queue lock, never a
        // join lock, so waking under this one cannot deadlock.
        for waiter in join.awaiters.drain(..) {
            wake(waiter);
        }
        if join.detached {
            join.retire();
        }
        !join.live
    };
    if retired {
        free_id(id);
    }
}

/// resume one task's coroutine to its next suspension point. the coroutine is
/// taken out of the slab (so we don't hold the slab lock across arbitrary pith
/// code, which may itself spawn), resumed, then either dropped (returned) or put
/// back (parked).
fn run_task(id: TaskId) {
    // claim the task on its scheduling word: Ready -> Running, pinning it to
    // this worker on first resume (whoever runs it first owns it forever, and
    // the owner rides in the word so a waker can read it lock-free). finding
    // it not Ready — someone else already claimed a double-enqueued id, or it
    // is gone — means nothing to do, the same tolerance the old slab-guarded
    // state check had.
    let Some(block) = sync_block(id) else {
        return;
    };
    let mut word = block.word.load(AtomicOrdering::Acquire);
    loop {
        if word & S_STATE_MASK != S_READY {
            return;
        }
        let mut next = (word & S_OWNER_MASK) | S_RUNNING;
        if next & S_OWNER_MASK == 0 {
            if let Some(w) = current_worker() {
                next |= owner_bits(w);
            }
        }
        match block.word.compare_exchange_weak(
            word,
            next,
            AtomicOrdering::AcqRel,
            AtomicOrdering::Acquire,
        ) {
            Ok(_) => break,
            Err(current) => word = current,
        }
    }

    // the claim above hands this worker the block's cells (see the claim
    // protocol on `SyncBlock`): take the coroutine out, no lock needed.
    //
    // SAFETY: exclusive by the Ready -> Running claim; the previous holder's
    // writes are visible through the word's CAS pairing.
    let Some(mut coro) = (unsafe { (*block.coro.get()).take() }) else {
        // a claimed task with no coroutine should be impossible; skip
        // defensively rather than panic on a corrupt slot.
        return;
    };

    // install this task's identity for the resume: its id (so a blocking
    // primitive knows which task to park) and its threadlocal map, restored
    // afterward. the map lives in the block, which never moves, and only this
    // task touches it, on this thread, while it runs.
    let (prev_task, prev_tls) = swap_task_identity(Some(id), block.tls.get());

    // publish this task as the worker's running task and stamp the epoch, so the
    // monitor thread can detect a quantum overrun and set the preempt flag. only
    // the monitor reads these, and only via atomics, so this never races the
    // coroutine. cleared right after the resume returns.
    let worker_index = current_worker();
    if let Some(w) = worker_index {
        let worker = &scheduler().workers[w];
        worker
            .running_since
            .store(PREEMPT_EPOCH.load(AtomicOrdering::Relaxed), AtomicOrdering::Relaxed);
        worker
            .running_task
            .store(id + 1, AtomicOrdering::Relaxed);
        let resumes = worker.resumes.load(AtomicOrdering::Relaxed);
        worker.resumes.store(resumes + 1, AtomicOrdering::Relaxed);
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

    swap_task_identity(prev_task, prev_tls);

    match outcome {
        Ok(CoroutineResult::Return(result)) => {
            // clean return: record the result, flip done, wake awaiters of both
            // kinds, and hand the stack back to the pool. see `finish_task` for
            // the join-lock / wake ordering.
            recycle_stack(coro);
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
            // worker itself is unharmed and goes on to run the next task. the
            // unwound coroutine is complete, so its stack is reusable too.
            recycle_stack(coro);
            finish_task(id, 0);
        }
        Ok(CoroutineResult::Yield(())) => {
            // the task suspended: either it parked on a blocking op (a channel, a
            // P2b primitive, or an await), or it was preempted at a safe-point.
            //
            // put the coroutine back BEFORE the word transition below makes the
            // task claimable: once the word reads Parked a waker may re-enqueue
            // the task, and its next resume — always on this same worker, by
            // pinning — must find the coroutine present. the transition CAS
            // releases this write to the next claimant (see the claim protocol
            // on `SyncBlock`). the yielder cell needs no write here: the body
            // published it on its first resume and the `Yielder` lives on the
            // coroutine's own stack, so the pointer is good for the coroutine's
            // whole life.
            //
            // SAFETY: we still hold the claim from Ready -> Running.
            unsafe {
                *block.coro.get() = Some(coro);
            }
            // then decide how to re-enqueue on the scheduling word:
            //   - the preempt flag: overran its quantum -> `deferred` (lowest
            //     priority), so it yields to every other ready task on the worker.
            //   - the wake flag: a block-site wake landed while we were still
            //     Running (the wake/park race) -> `pinned`, resume promptly.
            //   - neither: a plain block -> Parked; the block site re-enqueues us.
            // a wake can land between the load and the CAS — the retry sees its
            // flag, so it is never lost.
            let mut word = block.word.load(AtomicOrdering::Acquire);
            let requeue = loop {
                debug_assert_eq!(
                    word & S_STATE_MASK,
                    S_RUNNING,
                    "a yielded task must still be Running (id {id})"
                );
                let (next, requeue) = if word & S_PREEMPT_PENDING != 0 {
                    ((word & S_OWNER_MASK) | S_READY, Requeue::Preempted(word_owner(word)))
                } else if word & S_WAKE_PENDING != 0 {
                    ((word & S_OWNER_MASK) | S_READY, Requeue::Woken(word_owner(word)))
                } else {
                    ((word & S_OWNER_MASK) | S_PARKED, Requeue::None)
                };
                match block.word.compare_exchange_weak(
                    word,
                    next,
                    AtomicOrdering::AcqRel,
                    AtomicOrdering::Acquire,
                ) {
                    Ok(_) => break requeue,
                    Err(current) => word = current,
                }
            };
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

    // the coroutine body runs the pith closure exactly as the os-thread backend
    // does: look up the function pointer, transmute to the pith calling
    // convention, call it with the closure handle. it runs on the coroutine's
    // own stack instead of a fresh OS-thread stack; the pith code is identical,
    // so refcount retain/release stays balanced across the boundary.
    let coro = TaskCoroutine::with_stack(
        take_stack(),
        move |yielder: &TaskYielder, _input: ()| -> i64 {
            // publish this coroutine's yielder into the task's own sync block so
            // pith code running on this stack can suspend the task (via
            // park_current). this runs once, on the first resume — the body's
            // top does not run again after a suspend — and the pointer stays
            // valid for the whole life of the body, so once is enough.
            publish_yielder(yielder as *const TaskYielder);
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
    let generation = scheduler().sync.ensure(id).arm(SendCoroutine(coro), closure_handle);
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

// ---------------------------------------------------------------------------
// the cross-thread resume probe.
//
// a task parked on one os thread and resumed on another must see the resuming
// thread's identity — its worker index — and must still find its own task id
// and tls map, which the resuming thread's `run_task` installs. that is the
// resume a migration performs, and this probe forces it by hand: the calling
// thread poses as worker 0 and runs a task until it parks; a second os thread,
// posing as the last worker, claims the task and resumes it; the task body
// reports what it saw. `tests/cross_thread_resume.rs` drives it.
//
// it lives in library code rather than in the test module for a reason that
// is the whole point of the check. what makes a stale read possible is the
// code shape of the *shipped* runtime — the tls model `libpith_runtime.a`
// is compiled with (see the accessor block above `current_task`). a `cargo
// test` binary is an executable in its own right, always gets the local-exec
// model whatever the workspace is configured with, and so can never observe
// the hazard however its body is written: the archive built with the
// dynamic model failed this probe (worker `none` after the resume, identity
// lost) while the identical body passed inside the test binary. the probe's
// body is therefore compiled here, and the integration test runs it a
// second time through a c harness linked against the archive, which is how
// every pith program links it.
// ---------------------------------------------------------------------------

/// pack the probe's readings: the pool's worker count (bits 24..32), the
/// worker seen before the park (bits 16..24), the worker seen after (bits
/// 8..16; 0xff for "none"), and whether the task id and tls map seen after
/// the park are still the task's own (bit 0). `-1` means the probe could not
/// run: the pool has fewer than two workers, so there is no second identity
/// to pose as.
#[doc(hidden)]
pub fn encode_probe(workers: usize, before: Option<usize>, after: Option<usize>, own: bool) -> i64 {
    let n = (workers.min(0xff)) as i64;
    let b = before.map_or(0xff, |w| w as i64);
    let a = after.map_or(0xff, |w| w as i64);
    (n << 24) | (b << 16) | (a << 8) | i64::from(own)
}

/// run the cross-thread resume scenario and report what the task body saw,
/// encoded by `encode_probe`. the expected value is
/// `encode_probe(n, Some(0), Some(n - 1), true)` for a pool of `n` workers.
///
/// exported unmangled so a harness linked against the archive can call it.
#[doc(hidden)]
#[no_mangle]
pub extern "C" fn pith_green_cross_thread_probe() -> i64 {
    // build the scheduler without starting its workers: the two identities
    // below are posed by this thread and a plain spawned thread, and a real
    // worker draining queues would race the hand-off.
    let n = scheduler().workers.len();
    if n < 2 {
        return -1;
    }
    let first = 0;
    let second = n - 1;
    set_current_worker(Some(first));

    let coro = TaskCoroutine::with_stack(
        take_stack(),
        move |yielder: &TaskYielder, _input: ()| -> i64 {
            publish_yielder(yielder as *const TaskYielder);
            let Some(id) = current_task() else {
                return encode_probe(n, None, None, false);
            };
            let tls = current_task_tls();
            let before = current_worker();
            park_current(id);
            // every read from here on is on whichever thread resumed us.
            let after = current_worker();
            let own = current_task() == Some(id) && current_task_tls() == tls;
            encode_probe(n, before, after, own)
        },
    );
    let id = alloc_id();
    let generation = scheduler().sync.ensure(id).arm(SendCoroutine(coro), 0);

    // first resume, on this thread: the body parks. claim it the way a waker
    // does (Parked -> Ready) but hand it straight to the thread below instead
    // of enqueueing it, so a real worker 0 — if the process has started the
    // pool — cannot take it first.
    run_task(id);
    set_current_worker(None);
    let Some(block) = sync_block(id) else {
        return encode_probe(n, None, None, false);
    };
    let word = block.word.load(AtomicOrdering::Acquire);
    if word & S_STATE_MASK != S_PARKED
        || block
            .word
            .compare_exchange(
                word,
                (word & S_OWNER_MASK) | S_READY,
                AtomicOrdering::AcqRel,
                AtomicOrdering::Acquire,
            )
            .is_err()
    {
        return encode_probe(n, None, None, false);
    }

    let resumed = std::thread::Builder::new()
        .name("pith-probe-resumer".to_string())
        .spawn(move || {
            set_current_worker(Some(second));
            run_task(id);
            set_current_worker(None);
            match sync_block(id) {
                Some(block) if lock_join(&block.join).done => lock_join(&block.join).result,
                _ => encode_probe(n, None, None, false),
            }
        });
    let result = match resumed {
        Ok(handle) => handle.join().unwrap_or_else(|_| encode_probe(n, None, None, false)),
        Err(_) => encode_probe(n, None, None, false),
    };
    // the task is done and nobody awaits it: give its block back.
    let _ = unsafe { green_await(make_handle(id, generation)) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // a pair of tasks handing off to each other on one worker refills the
    // affinity slot on every exchange, which is exactly the shape that used to
    // leapfrog the fifo forever: `pop` empties the slot before `push_next` puts
    // the partner back, so the displace-on-occupied rule never fires. the fifo
    // has to get a turn anyway.
    #[test]
    fn a_ping_ponging_pair_cannot_starve_the_fifo() {
        let mut q = PinnedQueue::default();
        q.push(99); // a third task, waiting behind the pair
        let mut partner = 1;
        for exchange in 0..(MAX_AFFINITY_STREAK as usize * 2) {
            q.push_next(partner);
            if q.pop() == Some(99) {
                // the fifo got its turn; the pair is still queued and running.
                assert!(exchange > 0, "the slot should win at least once first");
                return;
            }
            partner = if partner == 1 { 2 } else { 1 };
        }
        panic!("the fifo never ran in {} exchanges", MAX_AFFINITY_STREAK * 2);
    }

    // the streak only costs a turn when someone is actually waiting: with an
    // empty fifo the slot keeps running, however long the pair goes on.
    #[test]
    fn the_streak_costs_nothing_with_an_empty_fifo() {
        let mut q = PinnedQueue::default();
        for _ in 0..(MAX_AFFINITY_STREAK * 4) {
            q.push_next(7);
            assert_eq!(q.pop(), Some(7));
        }
        assert!(q.is_empty());
    }

    // a second same-worker wake while the slot is still full displaces the first
    // to the back rather than dropping it.
    #[test]
    fn a_displaced_slot_task_goes_to_the_back_of_the_fifo() {
        let mut q = PinnedQueue::default();
        q.push(50);
        q.push_next(10);
        q.push_next(20);
        assert_eq!(q.pop(), Some(20));
        assert_eq!(q.pop(), Some(50));
        assert_eq!(q.pop(), Some(10));
        assert!(q.is_empty());
    }

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
    // stamped at the current epoch has not overrun, nor one stamped one epoch
    // ago, since a single tick says nothing about how long it ran; a full
    // quantum later it has.
    #[test]
    fn task_overrun_triggers_after_the_quantum() {
        assert!(!task_overrun(5, 5));
        assert!(!task_overrun(5, 6));
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
        set_current_worker(Some(0));

        // a task body that, on its first resume, simulates the monitor observing
        // time pass (bumps the epoch past the quantum) and then hits a safe-point;
        // after it is resumed the second time it runs to completion.
        let coro = TaskCoroutine::with_stack(
            corosensei::stack::DefaultStack::new(STACK_SIZE).expect("stack"),
            move |yielder: &TaskYielder, _input: ()| -> i64 {
                publish_yielder(yielder as *const TaskYielder);
                PREEMPT_EPOCH.fetch_add(QUANTUM_EPOCHS + 1, AtomicOrdering::Relaxed);
                pith_green_maybe_yield();
                99
            },
        );

        let id = push_ready_task(coro);

        // force the flag on so the safe-point takes its slow path.
        PITH_PREEMPT_REQUESTED.store(1, AtomicOrdering::Relaxed);

        // first resume: the body overruns and parks at the safe-point. run_task's
        // yield path must see the preempt flag and re-enqueue onto owner 0's
        // lowest-priority deferred queue.
        run_task(id);
        {
            let word = sync_block(id).unwrap().word.load(AtomicOrdering::Acquire);
            assert_eq!(word_owner(word), Some(0), "task should pin to worker 0");
            assert_eq!(word & S_STATE_MASK, S_READY, "should be re-enqueued, not parked");
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
        assert_eq!(completion(id), Some(99));

        PITH_PREEMPT_REQUESTED.store(0, AtomicOrdering::Relaxed);
        set_current_worker(None);
    }

    // arm a Ready task wrapping `coro` the way `green_spawn` does, without
    // enqueueing it, and return its id. shared by the tests that drive
    // `run_task` by hand.
    fn push_ready_task(coro: TaskCoroutine) -> TaskId {
        spawn_unqueued(coro).0
    }

    fn spawn_unqueued(coro: TaskCoroutine) -> (TaskId, i64) {
        let id = alloc_id();
        let generation = scheduler().sync.ensure(id).arm(SendCoroutine(coro), 0);
        (id, make_handle(id, generation))
    }

    // the result a finished task recorded, or `None` while it has not finished.
    fn completion(id: TaskId) -> Option<i64> {
        let join = lock_join(&sync_block(id).unwrap().join);
        join.done.then_some(join.result)
    }

    // a task whose body returns `value` straight away.
    fn returning(value: i64) -> TaskCoroutine {
        TaskCoroutine::with_stack(
            corosensei::stack::DefaultStack::new(STACK_SIZE).expect("stack"),
            move |yielder: &TaskYielder, _input: ()| -> i64 {
                publish_yielder(yielder as *const TaskYielder);
                value
            },
        )
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
        set_current_worker(Some(0));

        let coro = TaskCoroutine::with_stack(
            corosensei::stack::DefaultStack::new(STACK_SIZE).expect("stack"),
            move |yielder: &TaskYielder, _input: ()| -> i64 {
                publish_yielder(yielder as *const TaskYielder);
                panic!("boom from a green task");
            },
        );
        let id = push_ready_task(coro);

        // must return, not unwind. without the boundary this call propagates the
        // panic and the test aborts.
        run_task(id);

        // the task is recorded done with a zero result, so an awaiter unblocks
        // with a defined outcome rather than hanging, and its coroutine is
        // dropped (never re-enqueued).
        assert_eq!(completion(id), Some(0), "a failed task reports a zero result");
        let block = sync_block(id).unwrap();
        assert_eq!(
            block.word.load(AtomicOrdering::Acquire) & S_STATE_MASK,
            S_DONE,
            "the scheduling word must retire so late wakes no-op"
        );
        // SAFETY: the task is Done, so nothing else touches its cells.
        assert!(
            unsafe { (*block.coro.get()).is_none() },
            "dead coroutine must not be put back"
        );
        // the worker (this thread) survived: a fresh task still runs to completion.
        let id2 = push_ready_task(returning(7));
        run_task(id2);
        assert_eq!(completion(id2), Some(7), "worker still runs tasks after a panic");

        set_current_worker(None);
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

    // awaiting a finished task returns its result and retires it: the handle
    // stops resolving, and the id comes back at a new generation, so the old
    // handle can never name the task that reuses the block.
    #[test]
    fn await_retires_the_task_and_its_handle() {
        let (id, handle) = spawn_unqueued(returning(5));
        run_task(id);
        assert_eq!(green_is_done(handle), 1);
        assert_eq!(unsafe { green_await(handle) }, 5);
        assert_eq!(green_is_done(handle), 0, "a retired handle names no task");
        assert_eq!(unsafe { green_await(handle) }, 0, "await consumes the handle");

        let join = lock_join(&sync_block(id).unwrap().join);
        assert!(!join.live);
        assert_ne!(Some((id, join.generation)), split_handle(handle));
    }

    // a handle of another generation finds nothing and never blocks.
    #[test]
    fn a_stale_generation_resolves_to_no_task() {
        let (id, handle) = spawn_unqueued(returning(1));
        let (_, generation) = split_handle(handle).unwrap();
        let stale = make_handle(id, generation.wrapping_add(1) & GEN_MASK);
        assert_eq!(green_is_done(stale), 0);
        assert_eq!(unsafe { green_await(stale) }, 0);
        green_detach(stale);
        run_task(id);
        assert_eq!(unsafe { green_await(handle) }, 1, "the real handle still works");
    }

    // detaching a running task retires it when it finishes.
    #[test]
    fn a_detached_task_retires_when_it_finishes() {
        let (id, handle) = spawn_unqueued(returning(3));
        green_detach(handle);
        assert!(lock_join(&sync_block(id).unwrap().join).live, "still running");
        run_task(id);
        assert!(!lock_join(&sync_block(id).unwrap().join).live);
        assert_eq!(unsafe { green_await(handle) }, 0);
    }

    // detaching a finished task retires it at once.
    #[test]
    fn detaching_a_finished_task_retires_it_at_once() {
        let (id, handle) = spawn_unqueued(returning(3));
        run_task(id);
        green_detach(handle);
        assert!(!lock_join(&sync_block(id).unwrap().join).live);
        green_detach(handle);
    }

    // the scheduling-word owner packing: 0 means unpinned, worker indices
    // round-trip through the owner bits, and the owner bits never collide with
    // the state or flag bits.
    #[test]
    fn scheduling_word_owner_round_trips() {
        assert_eq!(word_owner(S_READY), None);
        assert_eq!(word_owner(S_PARKED | owner_bits(0)), Some(0));
        assert_eq!(word_owner(S_RUNNING | S_WAKE_PENDING | owner_bits(7)), Some(7));
        assert_eq!(
            owner_bits(3) & (S_STATE_MASK | S_WAKE_PENDING | S_PREEMPT_PENDING),
            0
        );
    }

    // a wake against an id no task has ever had must be a harmless no-op — the
    // reactor can probe ids far beyond any real task (see netpoll.rs).
    #[test]
    fn wake_on_never_used_id_is_noop() {
        wake(ARENA_CHUNK * ARENA_SPINE + 5); // beyond the spine entirely
        wake(ARENA_CHUNK * (ARENA_SPINE - 1)); // in range, chunk never allocated
    }

    /// set in the child half of the arena-bound case below.
    const ARENA_BOUND_CHILD: &str = "PITH_GREEN_ARENA_BOUND";

    // arming a sync block past the end of the spine has to stop the process with
    // a diagnostic. it used to `panic!`, which on this path is far worse than a
    // crash: `ensure` runs on the spawn path, inside whatever task called spawn,
    // and `run_task` catches every panic a task raises — so the spawning task
    // would die with `join.done` never set and its awaiters would block forever,
    // with nothing on stderr to say why.
    //
    // `ensure` exits, so the case runs this test binary again as a child and the
    // parent asserts on how the child died.
    #[test]
    fn arming_past_the_arena_spine_stops_the_process() {
        if std::env::var(ARENA_BOUND_CHILD).is_ok() {
            scheduler().sync.ensure(ARENA_CHUNK * ARENA_SPINE);
            unreachable!("ensure past the spine returned instead of stopping the process");
        }

        let exe = std::env::current_exe().expect("test binary path");
        let output = std::process::Command::new(exe)
            .args([
                "concurrency::green::tests::arming_past_the_arena_spine_stops_the_process",
                "--exact",
                "--nocapture",
            ])
            .env(ARENA_BOUND_CHILD, "1")
            .output()
            .expect("re-run the test binary");
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        assert_eq!(
            output.status.code(),
            Some(1),
            "expected a clean exit(1), got {:?}. stderr:\n{stderr}",
            output.status
        );
        assert!(
            stderr.contains("exceeds the sync arena bound"),
            "the diagnostic should name the bound. stderr:\n{stderr}"
        );
        assert!(
            !stderr.contains("panicked"),
            "the trap must not surface as a rust panic. stderr:\n{stderr}"
        );
    }

    // the full park -> wake -> resume cycle on the lock-free scheduling word: a
    // task that parks with no pending flag reads Parked and sits in no queue;
    // wake() claims it (Parked -> Ready) and enqueues it on its owner's pinned
    // queue exactly once; a second wake while Ready is a no-op; the resumed
    // task runs to completion and retires to Done.
    #[test]
    fn parked_task_wakes_once_onto_owner_pinned_queue() {
        let _guard = PREEMPT_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_current_worker(Some(0));
        // start from an empty pinned queue so the assertions below see only
        // this test's enqueues.
        while scheduler().workers[0]
            .pinned
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .pop()
            .is_some()
        {}

        let coro = TaskCoroutine::with_stack(
            corosensei::stack::DefaultStack::new(STACK_SIZE).expect("stack"),
            move |yielder: &TaskYielder, _input: ()| -> i64 {
                publish_yielder(yielder as *const TaskYielder);
                park_current(current_task().expect("test body runs inside a task"));
                21
            },
        );
        let id = push_ready_task(coro);

        // first resume: the body parks with no pending flag.
        run_task(id);
        let block = sync_block(id).unwrap();
        assert_eq!(
            block.word.load(AtomicOrdering::Acquire) & S_STATE_MASK,
            S_PARKED
        );
        assert!(scheduler().workers[0]
            .pinned
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty());

        // wake claims Parked -> Ready and enqueues on the owner's pinned queue.
        wake(id);
        let word = block.word.load(AtomicOrdering::Acquire);
        assert_eq!(word & S_STATE_MASK, S_READY);
        assert_eq!(word_owner(word), Some(0), "task stays pinned to its owner");
        // a second wake while Ready must not enqueue again.
        wake(id);
        {
            let mut pinned = scheduler().workers[0]
                .pinned
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            assert_eq!(pinned.pop(), Some(id));
            assert!(pinned.is_empty(), "a Ready task must not be double-enqueued");
        }

        run_task(id);
        assert_eq!(completion(id), Some(21));
        assert_eq!(
            block.word.load(AtomicOrdering::Acquire) & S_STATE_MASK,
            S_DONE
        );

        set_current_worker(None);
    }

    // a task woken onto a busy owner's pinned fifo can be taken by an idle
    // peer, which becomes its owner. an owner that is not running anything is
    // left alone, since it is about to drain the queue itself.
    #[test]
    fn idle_peer_steals_from_a_busy_owners_pinned_queue() {
        let _guard = PREEMPT_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        if scheduler().workers.len() < 2 {
            return;
        }
        set_current_worker(Some(0));
        let owner = &scheduler().workers[0];
        while owner.pinned.lock().unwrap_or_else(|p| p.into_inner()).pop().is_some() {}

        let coro = TaskCoroutine::with_stack(
            corosensei::stack::DefaultStack::new(STACK_SIZE).expect("stack"),
            move |yielder: &TaskYielder, _input: ()| -> i64 {
                publish_yielder(yielder as *const TaskYielder);
                park_current(current_task().expect("test body runs inside a task"));
                34
            },
        );
        let id = push_ready_task(coro);
        run_task(id);

        // a wake from outside the pool keeps the owner and goes on the fifo.
        set_current_worker(None);
        wake(id);
        let block = sync_block(id).unwrap();
        assert_eq!(word_owner(block.word.load(AtomicOrdering::Acquire)), Some(0));
        assert_eq!(owner.pinned_fifo.load(AtomicOrdering::Relaxed), 1);

        owner.running_task.store(0, AtomicOrdering::Relaxed);
        assert_eq!(steal_pinned(0, 1), None, "an idle owner drains its own queue");

        owner.running_task.store(usize::MAX, AtomicOrdering::Relaxed);
        assert_eq!(steal_pinned(0, 1), Some(id));
        owner.running_task.store(0, AtomicOrdering::Relaxed);
        assert_eq!(owner.pinned_fifo.load(AtomicOrdering::Relaxed), 0);
        let word = block.word.load(AtomicOrdering::Acquire);
        assert_eq!(word & S_STATE_MASK, S_READY);
        assert_eq!(word_owner(word), Some(1), "the thief owns the task now");

        set_current_worker(Some(1));
        run_task(id);
        assert_eq!(completion(id), Some(34));
        set_current_worker(None);
    }
}
