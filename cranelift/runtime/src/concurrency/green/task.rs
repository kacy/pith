//! per-task state: task ids and handles, the scheduling word, the
//! `SyncBlock` each task lives in, and where ids come from.

use super::*;

/// a task's index into the sync arena.
///
/// the pith-facing task *handle* packs the index together with a generation
/// (see `make_handle`). inside the scheduler a bare index is enough: a task's
/// index stays valid while it is queued or running, and it is only reused
/// after the task is retired. only the handle boundary (`green_await`,
/// `green_is_done`, `green_detach`) needs the generation check.
pub(super) type TaskId = usize;

pub(super) use crate::concurrency::scheduler::{make_handle, split_handle, GEN_MASK};

// ---------------------------------------------------------------------------
// the scheduling word
//
// a wake is the hottest thing the scheduler does (two per channel message at
// saturation), so a task's run state is one AtomicU64 in its `SyncBlock` and
// every transition is a single CAS, with no lock:
//
//   bits 0..2   run state: Ready / Running / Parked / Done
//   bit  2      wake_pending: a wake landed while the task was still Running
//   bit  3      preempt_pending: the task overran its quantum
//   bits 32..   owner worker + 1 (0 = not yet pinned)
//
// the transitions:
//
//   wake         Parked -> Ready (claim + enqueue) | Running -> set wake_pending
//                | Ready / Done -> no-op
//   run_task     Ready -> Running (claim; pins the owner on first resume)
//   yield arm    Running -> Ready (a pending flag was set; re-enqueue)
//                | Running -> Parked (the block site owns the wake)
//   finish_task  -> Done (plain store; every later wake is a no-op)
//
// block reuse and stale wakes: a retired task's block is reused by a later
// spawn, and a wake against a stale id must stay harmless. between retirement
// and re-arming the word reads Done (no-op), and after re-arming it reads
// Ready (no-op). a stale wake that catches the new task genuinely Parked
// wakes it spuriously; it re-checks its block condition and re-parks, which
// every block site tolerates. in practice stale wakes do not occur: a parked
// task's id lives in exactly one waiter list, and every list removes the id
// at wake time.
// ---------------------------------------------------------------------------

/// run states of the scheduling word (bits 0..2).
///
/// - `S_READY`: sitting in a run queue with its coroutine present.
/// - `S_RUNNING`: coroutine taken out and being resumed on some worker.
/// - `S_PARKED`: suspended on a blocking op; coroutine back in its block, the
///   task in no queue, and the block site it parked on owns re-enqueueing it.
/// - `S_DONE`: coroutine returned, or the block holds no task at all (a fresh
///   arena chunk reads `Done`, so a wake against a never-used index no-ops).
pub(crate) const S_STATE_MASK: u64 = 0b11;
pub(crate) const S_READY: u64 = 0;
pub(crate) const S_RUNNING: u64 = 1;
pub(crate) const S_PARKED: u64 = 2;
pub(crate) const S_DONE: u64 = 3;
/// a wake landed while the task was still `Running` (mid-suspend, before the
/// yield arm recorded it as `Parked`): the yield arm re-enqueues instead of
/// parking, closing the wake/park race so a wake in that window is never lost.
pub(crate) const S_WAKE_PENDING: u64 = 1 << 2;
/// set by `pith_green_maybe_yield` right before it parks a task that overran
/// its quantum: the yield arm routes it onto the owner's lowest-priority
/// `deferred` queue instead of the prompt `pinned` queue.
pub(super) const S_PREEMPT_PENDING: u64 = 1 << 3;
pub(super) const S_OWNER_SHIFT: u32 = 32;
pub(super) const S_OWNER_MASK: u64 = !0u64 << S_OWNER_SHIFT;

/// the owner worker packed in a scheduling word, if the task has been pinned.
pub(super) fn word_owner(word: u64) -> Option<usize> {
    match word >> S_OWNER_SHIFT {
        0 => None,
        w => Some(w as usize - 1),
    }
}

/// the owner bits for a worker index (stored as `worker + 1`, 0 = unpinned).
pub(crate) fn owner_bits(worker: usize) -> u64 {
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
///   enqueue makes the task claimable again. `wake_with` uses that window to
///   deposit a handoff value.
///
/// every hand-over goes through a CAS on `word` (release on publish, acquire
/// on claim), so the cell writes of the previous holder are visible to the
/// next; the run-queue mutex between an enqueue and the owner's dequeue adds
/// its own ordering on top.
#[repr(align(64))]
pub(super) struct SyncBlock {
    pub(super) word: AtomicU64,
    /// the task's coroutine, present exactly while the word reads `Ready` or
    /// `Parked`; taken out by the claiming worker for the duration of a
    /// resume, gone for good once the task is `Done`.
    pub(super) coro: UnsafeCell<Option<SendCoroutine>>,
    /// raw pointer to the coroutine's `Yielder`, captured on the body's first
    /// run (see `green_spawn`) and re-installed before every later resume.
    /// null until the body has run once; only ever dereferenced on the owner
    /// worker while this task is the one running.
    pub(super) yielder: UnsafeCell<*const TaskYielder>,
    /// this task's own `threadlocal` storage. one worker thread runs many
    /// tasks, so a per-thread map would let task B see task A's values. the
    /// block never moves, so the pointer `run_task` installs for the resume
    /// stays valid.
    pub(super) tls: UnsafeCell<TlsMap>,
    /// the closure the task runs. set at spawn and released by `finish_task`
    /// once the body returns, which is when the captured arguments die.
    pub(super) closure: UnsafeCell<i64>,
    /// a value handed to this task by its waker (see `wake_with`): written by
    /// the waker right after it claims the task (the Parked -> Ready CAS),
    /// taken by the task itself once it resumes.
    pub(super) handoff: UnsafeCell<Option<i64>>,
    /// the task's completion, shared with whoever awaits it.
    pub(super) join: Mutex<Join>,
    /// os-thread awaiters wait here for `finish_task` to record the result.
    pub(super) joined: Condvar,
    /// what the sanitizers need to follow the task's stack switches; empty
    /// in a build without one.
    pub(super) fiber: Fiber,
}

// SAFETY: `word` is an atomic; the cells are guarded by the claim protocol
// documented on the type (a single holder at a time, hand-overs ordered by
// the word's CAS pairs and the run-queue mutex). the coroutine cell only ever
// crosses threads under the pinning discipline `SendCoroutine` documents.
unsafe impl Sync for SyncBlock {}

impl SyncBlock {
    /// an inert block: `Done` reads as "no task here", so a wake against an
    /// index whose slot has never held a task is a no-op.
    pub(super) fn new() -> SyncBlock {
        SyncBlock {
            word: AtomicU64::new(S_DONE),
            coro: UnsafeCell::new(None),
            yielder: UnsafeCell::new(ptr::null()),
            tls: UnsafeCell::new(TlsMap::new()),
            closure: UnsafeCell::new(0),
            handoff: UnsafeCell::new(None),
            join: Mutex::new(Join::default()),
            joined: Condvar::new(),
            fiber: Fiber::new(),
        }
    }

    /// arm the block for a freshly spawned task and return the generation its
    /// handle carries. the word reads `Ready` only once the cells are set, and
    /// the caller enqueues the task after that, so nothing sees a half-armed
    /// block. the previous task was retired, so a wake aimed at it reads
    /// `Done` until the store below and no-ops.
    pub(super) fn arm(&self, coro: SendCoroutine, stack: StackBounds, closure: i64) -> u32 {
        // SAFETY: the id came off a free pool, so no task owns this block and
        // no thread touches its cells until the task is enqueued below.
        unsafe {
            *self.coro.get() = Some(coro);
            *self.yielder.get() = ptr::null();
            *self.closure.get() = closure;
            *self.handoff.get() = None;
            debug_assert!((*self.tls.get()).is_empty(), "a retired task left threadlocals");
        }
        self.fiber.arm(stack);
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
/// freed or moved, which is what lets `wake` reach a block with index math
/// and no lock. the spine bounds the live task count at 2M; every live green
/// task also owns a 1 MiB coroutine stack, so real programs exhaust address
/// space long before this bound, and hitting it is a fatal runtime error
/// rather than a silent corruption.
pub(super) const ARENA_CHUNK: usize = 256;
pub(super) const ARENA_SPINE: usize = 8192;

/// the arena of per-task sync blocks, indexed by task id.
pub(super) struct SyncArena {
    /// chunk pointers, null until a task id in that chunk's range first exists.
    pub(super) spine: Box<[AtomicPtr<SyncBlock>]>,
}

impl SyncArena {
    pub(super) fn new() -> SyncArena {
        SyncArena {
            spine: (0..ARENA_SPINE)
                .map(|_| AtomicPtr::new(ptr::null_mut()))
                .collect(),
        }
    }

    /// the sync block for `id`, or `None` when no task with that index has
    /// ever existed. a garbage id (the reactor probing an id far beyond any
    /// real task, say) must read as "no task", never allocate.
    pub(super) fn get(&self, id: TaskId) -> Option<&SyncBlock> {
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
    pub(super) fn ensure(&self, id: TaskId) -> &SyncBlock {
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
pub(super) fn sync_block(id: TaskId) -> Option<&'static SyncBlock> {
    scheduler().sync.get(id)
}


/// where task ids come from. an id names one block of the sync arena for as
/// long as a task holds it; retiring the task returns the id to a pool for the
/// next spawn. a worker keeps a pool of its own (`WorkerCache::ids`), so a
/// spawn and the await that retires the task usually touch no shared state;
/// a thread that is not a worker, or a worker whose pool ran dry or filled
/// up, uses the shared pool here. when both are empty a spawn claims a fresh
/// run of `ARENA_CHUNK` ids.
pub(super) static SHARED_IDS: CacheLine<Mutex<Vec<TaskId>>> = CacheLine(Mutex::new(Vec::new()));
pub(super) static NEXT_ID_CHUNK: AtomicUsize = AtomicUsize::new(0);

/// how many free ids a worker keeps before it hands the rest to the shared
/// pool. a worker that retires more tasks than it spawns (a consumer of
/// detached tasks) would otherwise hold ids its peers need.
pub(super) const WORKER_IDS_MAX: usize = 2 * ARENA_CHUNK;

/// an id for a new task.
pub(super) fn alloc_id() -> TaskId {
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
pub(super) fn free_id(id: TaskId) {
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

pub(super) fn lock_ids() -> MutexGuard<'static, Vec<TaskId>> {
    SHARED_IDS.lock().unwrap_or_else(|p| p.into_inner())
}

/// the coroutine type every task runs: no input/yield, returns the pith result
/// as an `i64`.
pub(super) type TaskCoroutine = Coroutine<(), (), i64>;

/// the yielder handed to a task's coroutine body. a green task suspends itself
/// through this (see `park_current`); `Coroutine<(), (), i64>` yields `()`.
pub(super) type TaskYielder = Yielder<(), ()>;

/// a coroutine wrapped so it can move between the spawning thread and the worker
/// that runs it. corosensei makes `Coroutine` `!Send` because, in general,
/// arbitrary data may live on a *suspended* coroutine's stack and it cannot
/// prove that data is `Send`.
///
/// a task suspends mid-run with live pith stack, so the manual `Send` rests on
/// the hand-over discipline in the placement section of the module doc: a
/// coroutine crosses threads only while it is off-CPU and held by exactly one
/// thread, and nothing it holds is tied to the thread it last ran on.
///
/// - a *fresh* task's coroutine has never resumed; it captures only the
///   closure handle, an `i64`. fresh tasks are stealable.
/// - a *parked* task's coroutine sits in its block, in no queue, and the
///   waker that wins the Parked -> Ready CAS is its only holder until it is
///   enqueued.
/// - a *ready* task popped off a pinned queue by a thief is held by that
///   thief alone (see `steal_pinned`).
///
/// what the stack holds across a park is pith handles and runtime values,
/// none of them derived from the parking thread. the accessor notes above
/// `current_task` are the rule that keeps it so, and
/// `tooling/check_tls_barriers.sh` checks the built archive for it.
///
/// the invariant is enforced by the scheduler's claim protocol, not the type
/// system.
pub(super) struct SendCoroutine(pub(super) TaskCoroutine);

// SAFETY: a coroutine only ever changes threads while off-CPU and held by one
// thread (fresh and stealable, Parked and claimed by its waker through the
// scheduling word's CAS, or Ready and popped off a pinned queue by a thief),
// and nothing on a suspended stack is derived from the thread it last ran on
// (see the type doc and the accessor block above `current_task`).
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
pub(super) struct Join {
    pub(super) generation: u32,
    /// a task holds the block: from spawn until it is retired. a handle to a
    /// block that is not live, or of another generation, finds nothing.
    pub(super) live: bool,
    pub(super) done: bool,
    pub(super) result: i64,
    /// no one will await the task, so `finish_task` retires it on completion.
    pub(super) detached: bool,
    /// green tasks parked in `green_await` on this one.
    pub(super) awaiters: Vec<TaskId>,
    /// os-thread awaiters blocked on `joined`. completion signals the condvar
    /// only when this is non-zero.
    pub(super) blocked: u32,
}

impl Join {
    /// whether `generation` names the task that holds this block.
    pub(super) fn holds(&self, generation: u32) -> bool {
        self.live && self.generation == generation
    }

    /// end the task's hold on its block. its handle stops resolving, and the
    /// caller gives the id back with `free_id` once the join lock is released.
    pub(super) fn retire(&mut self) {
        self.live = false;
        self.generation = self.generation.wrapping_add(1) & GEN_MASK;
    }
}

pub(super) fn lock_join(lock: &Mutex<Join>) -> MutexGuard<'_, Join> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

