//! task system for spawn/await
//!
//! the default backend runs one OS thread per task. the tasks live in a shared
//! generational slotmap (`Slab`) so a spawn/await/detach cycle *reclaims* its
//! slot instead of leaving a dead shell behind forever. the reclamation shape —
//! a free list of reusable indices plus a per-slot generation stamped into the
//! pith-facing handle — mirrors the green backend (`green.rs`), so a stale handle
//! to a reused slot no longer resolves and the handle encoding stays identical
//! across the two backends (generation 0 is exactly the old `id + 1`).
//!
//! ## when a slot is reclaimed
//!
//! - **await** joins the thread, reads the result, and reclaims the slot: await
//!   consumes the handle (Rust-style), so it is never observed again.
//! - **detach** promises no one will await the task. if the body has already
//!   finished we reclaim right away; if it is still running we flag it
//!   `reclaim_on_done` and the completing thread reclaims the slot as it exits.
//!   this matches green's `green_detach` policy exactly, so the two backends
//!   invalidate a detached handle at the same observable point — while the task
//!   runs the handle stays valid (e.g. `is_done` still answers), and it is
//!   reclaimed once the body completes.
//!
//! ## where the on-completion reclaim happens
//!
//! green runs its reclaim from the worker's `finish_task`. the os-thread backend
//! has no worker, so the task's own OS thread performs the same step as it exits:
//! after it records the result and wakes awaiters, it calls back into the slab to
//! either reclaim itself (if it was detached) or mark itself `finished` so a
//! later detach can reclaim immediately. that callback is why `os_thread_spawn`
//! reserves the slot up front — the thread needs its own `(index, generation)`.
//!
//! ## why dropping a slot is memory-safe here
//!
//! a task's body runs on its own OS thread; the slab slot holds only a
//! `JoinHandle` and a clone of the `Arc<shared>` join channel. dropping the slot
//! never touches a running body. dropping the `JoinHandle` merely detaches the
//! thread (it runs to completion and the OS reclaims it), and the `Arc` clone is
//! refcounted — the thread keeps its own clone. so reclaiming a slot is safe
//! whether or not the body has finished; the `reclaim_on_done` deferral above is
//! about *when the handle should stop resolving*, matching green, not about
//! memory safety.

use crate::handle_registry::{self, HandleKind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

static TASKS: std::sync::OnceLock<Mutex<Slab>> = std::sync::OnceLock::new();

fn tasks() -> &'static Mutex<Slab> {
    TASKS.get_or_init(|| Mutex::new(Slab::new()))
}

fn lock_tasks() -> MutexGuard<'static, Slab> {
    tasks()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock_shared(lock: &Mutex<TaskShared>) -> MutexGuard<'_, TaskShared> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_shared<'a>(
    cvar: &Condvar,
    state: MutexGuard<'a, TaskShared>,
) -> MutexGuard<'a, TaskShared> {
    cvar.wait(state)
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct TaskState {
    handle: Option<JoinHandle<()>>,
    shared: Arc<(Mutex<TaskShared>, Condvar)>,
    /// set by `detach` on a task whose body is still running: no one will await
    /// it, so the completing thread reclaims its slot the moment it finishes
    /// rather than leaking it for the life of the process. mirrors the green
    /// backend's `reclaim_on_done`.
    reclaim_on_done: bool,
    /// set by the completing thread (under the slab lock) once its body has
    /// returned. it is the os-thread analogue of green's `state == Done`: a later
    /// `detach` reads it to decide whether to reclaim immediately (body already
    /// finished) or defer via `reclaim_on_done` (body still running).
    finished: bool,
}

struct TaskShared {
    done: bool,
    result: i64,
    /// awaiters currently blocked in `cvar.wait`. completion only signals the
    /// condvar when this is non-zero — a detached or not-yet-awaited task then
    /// skips the notify entirely, and a later awaiter sees `done` before it
    /// ever waits.
    waiters: u32,
}

// ---------------------------------------------------------------------------
// generational slotmap
//
// the same shape as the green backend's `Slab`: `entries` grows on demand and
// `free` holds the indices of reclaimed slots ready for the next spawn. without
// reclamation this vector grew by one entry per task ever spawned and never
// shrank — an unbounded leak on a long-running server that fans out one task per
// connection. reusing a slot bumps its generation so a stale handle can never
// alias the task that takes its place.
// ---------------------------------------------------------------------------

/// index into the task slab. the pith-facing handle is not the raw index — it
/// packs the index with the slot's generation (see `make_handle`).
type TaskId = usize;

/// low 31 bits — the generation lives in bits 32..62 of the handle, leaving the
/// sign bit clear so a handle is always a positive `i64` (0 stays "no task").
const GEN_MASK: u32 = 0x7fff_ffff;

/// pack a slab index and its generation into the pith-facing task handle. the
/// low 32 bits hold `index + 1` (so index 0 still yields a nonzero handle and 0
/// stays reserved for "no task"); the generation sits above. for a slot's first
/// use the generation is 0, so the handle is exactly `index + 1` — identical to
/// the old encoding and to the green backend. the two backends never coexist in
/// one process (they are chosen by the `PITH_GREEN` flag), so a shared encoding
/// is not required, but keeping it identical avoids any surprise.
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

/// one slab entry: a task plus the generation stamp that makes its handle unique
/// across slot reuse. reclaiming a task drops the `TaskState` (dropping its
/// `JoinHandle` and its clone of the join `Arc`) and bumps `generation`, so any
/// handle still naming this slot no longer resolves.
struct Slot {
    task: Option<TaskState>,
    generation: u32,
}

/// the task slab: a generational slotmap. see the module-level note on why
/// reclamation exists and why it is safe to drop an os-thread slot at any time.
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
    fn insert(&mut self, task: TaskState) -> (TaskId, u32) {
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

    /// resolve a handle-checked task: the slot must both exist and still carry
    /// `generation`, or this returns `None` (a stale handle to a reused slot).
    fn get_checked(&mut self, id: TaskId, generation: u32) -> Option<&mut TaskState> {
        let slot = self.entries.get_mut(id)?;
        if slot.generation != generation {
            return None;
        }
        slot.task.as_mut()
    }

    /// drop a slot: empty it, bump the generation (so the old handle no longer
    /// resolves), and queue the index for reuse. returns the pith-facing handle
    /// that named the slot so the caller can unregister it. the single place that
    /// actually frees a slot — `reclaim`/`detach`/`mark_finished_and_maybe_reclaim`
    /// all funnel through it after their own generation checks.
    fn free_slot(&mut self, id: TaskId) -> i64 {
        let slot = &mut self.entries[id];
        let handle = make_handle(id, slot.generation);
        slot.task = None;
        slot.generation = slot.generation.wrapping_add(1) & GEN_MASK;
        self.free.push(id);
        handle
    }

    /// reclaim a slot, but only if it still carries `expected_gen`. the caller
    /// (await) has already joined the thread, so the body is finished. the
    /// generation gate means that when several callers race to reclaim the same
    /// handle, exactly the first succeeds and the rest no-op. returns `true` when
    /// it reclaimed.
    fn reclaim(&mut self, id: TaskId, expected_gen: u32) -> bool {
        match self.entries.get(id) {
            Some(slot) if slot.generation == expected_gen && slot.task.is_some() => {
                self.free_slot(id);
                true
            }
            _ => false,
        }
    }

    /// the task body has returned. reclaim the slot now if the task was detached
    /// while it ran (no one will await it), returning the handle to unregister;
    /// otherwise record that the body `finished` so a later detach can reclaim
    /// immediately. only acts on the slot that still carries `generation`. mirrors
    /// green's `reclaim_if_detached`.
    fn mark_finished_and_maybe_reclaim(&mut self, id: TaskId, generation: u32) -> Option<i64> {
        let reclaim_now = {
            let task = self.get_checked(id, generation)?;
            if task.reclaim_on_done {
                true
            } else {
                task.finished = true;
                false
            }
        };
        reclaim_now.then(|| self.free_slot(id))
    }

    /// detach a task: reclaim it immediately if its body has already finished,
    /// otherwise flag it so the completing thread reclaims it on the way out (see
    /// `mark_finished_and_maybe_reclaim`). only acts on the slot that still carries
    /// `generation`. returns the handle to unregister when it reclaimed right now,
    /// else `None`. mirrors green's `detach`.
    fn detach(&mut self, id: TaskId, generation: u32) -> Option<i64> {
        let reclaim_now = {
            let task = self.get_checked(id, generation)?;
            if task.finished {
                true
            } else {
                task.reclaim_on_done = true;
                false
            }
        };
        reclaim_now.then(|| self.free_slot(id))
    }
}

/// Codegen-facing spawn entrypoint. Routes through the scheduler seam so the
/// task backend (os threads today, green threads later) is chosen behind the
/// `PITH_GREEN` flag without the compiler ever caring which one runs.
#[no_mangle]
pub unsafe extern "C" fn pith_spawn(closure_handle: i64) -> i64 {
    crate::concurrency::scheduler::spawn(closure_handle)
}

/// One OS thread per task: the original, default spawn implementation. Kept
/// as a named backend so the scheduler can pick it explicitly.
pub(crate) unsafe fn os_thread_spawn(closure_handle: i64) -> i64 {
    if closure_handle == 0 {
        return 0;
    }

    let shared = Arc::new((
        Mutex::new(TaskShared {
            done: false,
            result: 0,
            waiters: 0,
        }),
        Condvar::new(),
    ));
    let shared_clone = shared.clone();

    // reserve the slot before spawning so the worker thread knows its own
    // `(index, generation)` for the reclaim-on-completion callback. the handle is
    // filled in right after the thread starts; detach/await cannot run yet (the
    // pith-facing handle has not been returned), so publishing the slot
    // handle-less for that brief window is safe.
    let (index, generation) = lock_tasks().insert(TaskState {
        handle: None,
        shared,
        reclaim_on_done: false,
        finished: false,
    });
    let task_handle = make_handle(index, generation);
    handle_registry::register_id(task_handle, HandleKind::Task);

    // the task thread moves reference counts, so it is a mutator the cycle
    // collector must be able to stop. its slot is created and registered here,
    // on the spawning thread, so a stop-the-world rendezvous can never scan
    // the gap between the thread starting and registering itself; the
    // thread-local owner installed by `adopt_mutator_slot` marks the slot
    // exited as the thread returns.
    let cycle_slot = crate::cycle::mutator_slot_for_spawn();
    let join_handle = std::thread::spawn(move || {
        crate::cycle::adopt_mutator_slot(cycle_slot);
        let func_ptr = crate::pith_closure_get_fn(closure_handle);
        let result = if func_ptr == 0 {
            0
        } else {
            // panic-guard: calling a compiler-emitted closure body through the pith closure abi.
            let func: extern "C" fn(i64) -> i64 = std::mem::transmute(func_ptr as *const ());
            func(closure_handle)
        };
        // the body has run, so release the one closure reference the task owned —
        // otherwise every spawned task leaks its closure environment (the emitter
        // moves the closure into `spawn` and never releases it). mirrors the green
        // backend's release in `finish_task`.
        crate::pith_closure_release(closure_handle);
        // publish completion to awaiters (condvar).
        {
            let (lock, cvar) = &*shared_clone;
            let mut state = lock_shared(lock);
            state.done = true;
            state.result = result;
            if state.waiters > 0 {
                cvar.notify_all();
            }
        }
        // then run the slab-side completion, the os-thread analogue of green's
        // `finish_task` reclaim: reclaim our slot now if we were detached, else
        // record that we finished so a later detach reclaims immediately. done
        // outside the shared lock — the slab lock is a separate, un-nested lock.
        if let Some(handle) = lock_tasks().mark_finished_and_maybe_reclaim(index, generation) {
            handle_registry::unregister_id(handle, HandleKind::Task);
        }
    });

    // store the JoinHandle. the thread may already have finished and marked its
    // slot `finished`, but it can never have *reclaimed* it yet (reclaim needs
    // `reclaim_on_done`, which only detach sets, and detach cannot run before we
    // return the handle), so the slot is still present.
    if let Some(task) = lock_tasks().get_checked(index, generation) {
        task.handle = Some(join_handle);
    } else {
        // unreachable given the argument above; drop the handle (detaching the
        // thread) rather than panic if that reasoning ever failed.
        drop(join_handle);
    }
    task_handle
}

/// Codegen-facing await entrypoint. Routes through the scheduler seam, the
/// mirror of `pith_spawn`.
#[no_mangle]
pub unsafe extern "C" fn pith_await(task_handle: i64) -> i64 {
    crate::concurrency::scheduler::await_task(task_handle)
}

/// Join an os-thread task and return its result: the original, default await
/// implementation. Awaiting consumes the handle (Rust-style), so once the result
/// is read the slab slot is reclaimed and the handle unregistered — a second
/// await of the same handle, or an await of a stale/garbage one, safely returns 0.
pub(crate) unsafe fn os_thread_await(task_handle: i64) -> i64 {
    if !handle_registry::is_valid_id(task_handle, HandleKind::Task) {
        return 0;
    }
    let Some((index, generation)) = split_handle(task_handle) else {
        return 0;
    };
    // clone the join arc and take the JoinHandle out under the slab lock. the
    // clone lets us wait for the result without holding the slab lock (running
    // pith code can spawn, which needs the same lock). a stale handle whose slot
    // was reused resolves to `None` here and we return the safe default.
    let taken = {
        let mut t = lock_tasks();
        t.get_checked(index, generation)
            .map(|task| (task.shared.clone(), task.handle.take()))
    };

    let (shared, join_handle) = match taken {
        Some(v) => v,
        None => return 0,
    };
    if let Some(join_handle) = join_handle {
        let _ = join_handle.join();
    }
    let result = {
        let (lock, cvar) = &*shared;
        let mut state = lock_shared(lock);
        while !state.done {
            state.waiters += 1;
            state = wait_shared(cvar, state);
            state.waiters -= 1;
        }
        state.result
    };

    // the task is finished and we have consumed its result (we still hold `shared`
    // for the value), so this handle will not be observed again. reclaim the slot.
    // the reclaim is generation-gated, so if the task was already reclaimed — it
    // was detached, or another awaiter of the same handle beat us — this is a safe
    // no-op and we skip the unregister.
    if lock_tasks().reclaim(index, generation) {
        handle_registry::unregister_id(task_handle, HandleKind::Task);
    }
    result
}

/// Codegen-facing "is this task done?" entrypoint. Routed through the scheduler
/// seam so it consults whichever backend actually owns the task.
#[no_mangle]
pub extern "C" fn pith_task_is_done(task_handle: i64) -> i64 {
    crate::concurrency::scheduler::task_is_done(task_handle)
}

/// Poll an os-thread task's done flag: the original, default implementation. A
/// stale or garbage handle reports "not done" (0) rather than reading a reused
/// slot.
pub(crate) fn os_thread_is_done(task_handle: i64) -> i64 {
    if !handle_registry::is_valid_id(task_handle, HandleKind::Task) {
        return 0;
    }
    let Some((index, generation)) = split_handle(task_handle) else {
        return 0;
    };
    let shared = {
        let mut t = lock_tasks();
        t.get_checked(index, generation)
            .map(|task| task.shared.clone())
    };
    if let Some(shared) = shared {
        let (lock, _) = &*shared;
        let state = lock_shared(lock);
        return if state.done { 1 } else { 0 };
    }
    0
}

/// Codegen-facing detach entrypoint. Routed through the scheduler seam, the
/// mirror of `pith_task_is_done`.
#[no_mangle]
pub extern "C" fn pith_task_detach(task_handle: i64) {
    crate::concurrency::scheduler::task_detach(task_handle)
}

/// Detach an os-thread task: a promise that no one will await it, so its slab
/// slot can be reclaimed instead of leaking. if the body has already finished we
/// reclaim right here; otherwise we flag it and the completing thread reclaims it
/// on the way out. matches green's `green_detach` so a detached handle is
/// invalidated at the same point on both backends. a stale or invalid handle is a
/// no-op.
pub(crate) fn os_thread_detach(task_handle: i64) {
    if !handle_registry::is_valid_id(task_handle, HandleKind::Task) {
        return;
    }
    let Some((index, generation)) = split_handle(task_handle) else {
        return;
    };
    if let Some(handle) = lock_tasks().detach(index, generation) {
        handle_registry::unregister_id(handle, HandleKind::Task);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // a slab entry for the slot-logic tests. `finished` seeds the slab-local
    // completion flag; the thread handle is always absent (these tests never run
    // a real thread, only exercise slot bookkeeping).
    fn dummy_task(finished: bool) -> TaskState {
        TaskState {
            handle: None,
            shared: Arc::new((
                Mutex::new(TaskShared {
                    done: finished,
                    result: 0,
                    waiters: 0,
                }),
                Condvar::new(),
            )),
            reclaim_on_done: false,
            finished,
        }
    }

    // the handle encoding: generation 0 is exactly the old `id + 1`, a generation
    // rides above, and the sign bit stays clear so 0 means "no task". identical to
    // the green backend's codec.
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

    // reclaiming a slot frees it; the next insert reuses the index at a bumped
    // generation, so the old handle can never alias the new task.
    #[test]
    fn slab_reclaim_reuses_slot_with_bumped_generation() {
        let mut slab = Slab::new();
        let (id, generation) = slab.insert(dummy_task(true));
        assert_eq!((id, generation), (0, 0));
        assert!(slab.get_checked(id, generation).is_some());

        assert!(slab.reclaim(id, generation));
        assert!(
            slab.get_checked(id, generation).is_none(),
            "stale handle must not resolve"
        );

        let (id2, gen2) = slab.insert(dummy_task(true));
        assert_eq!(id2, id, "freed index is reused");
        assert_ne!(gen2, generation, "generation bumped");

        // reclaiming again with the stale generation is a no-op.
        assert!(!slab.reclaim(id, generation));
    }

    // reclaim only touches the slot that still carries the expected generation.
    #[test]
    fn slab_reclaim_wrong_generation_is_noop() {
        let mut slab = Slab::new();
        let (id, generation) = slab.insert(dummy_task(true));
        assert!(!slab.reclaim(id, generation.wrapping_add(1)));
        assert!(
            slab.get_checked(id, generation).is_some(),
            "slot survives a mismatched reclaim"
        );
    }

    // reclaiming an already-empty slot (a double reclaim, or a garbage index) is a
    // safe no-op rather than a panic or a spurious free-list entry.
    #[test]
    fn slab_reclaim_empty_or_missing_is_noop() {
        let mut slab = Slab::new();
        let (id, generation) = slab.insert(dummy_task(true));
        assert!(slab.reclaim(id, generation));
        // the slot is empty now; reclaiming it again does nothing.
        assert!(!slab.reclaim(id, generation.wrapping_add(1)));
        // an index that was never allocated is also a no-op.
        assert!(!slab.reclaim(999, 0));
    }

    // detaching a still-running task defers: the handle stays valid (so is_done
    // still answers), and the completing thread's `mark_finished` reclaims it.
    #[test]
    fn slab_detach_running_task_reclaims_on_finish() {
        let mut slab = Slab::new();
        let (id, generation) = slab.insert(dummy_task(false));
        assert_eq!(slab.detach(id, generation), None, "running detach defers");
        assert!(
            slab.get_checked(id, generation).is_some(),
            "handle stays valid while the task runs"
        );
        // the body finishing now reclaims it, because it was detached.
        assert_eq!(
            slab.mark_finished_and_maybe_reclaim(id, generation),
            Some(make_handle(id, generation))
        );
        assert!(slab.get_checked(id, generation).is_none());
    }

    // when the body finishes before any detach, `mark_finished` just records
    // `finished` (no reclaim); a later detach then reclaims immediately.
    #[test]
    fn slab_detach_finished_task_reclaims_immediately() {
        let mut slab = Slab::new();
        let (id, generation) = slab.insert(dummy_task(false));
        assert_eq!(
            slab.mark_finished_and_maybe_reclaim(id, generation),
            None,
            "an undetached finish only marks the slot"
        );
        assert!(slab.get_checked(id, generation).is_some());

        assert_eq!(slab.detach(id, generation), Some(make_handle(id, generation)));
        assert!(slab.get_checked(id, generation).is_none());
        // a second detach on the now-stale handle is a no-op.
        assert_eq!(slab.detach(id, generation), None);
    }

    // a finished task nobody detached is left alone by the completion reclaim — it
    // is the await path (not the thread) that reclaims it. mirrors green.
    #[test]
    fn slab_undetached_finish_does_not_reclaim() {
        let mut slab = Slab::new();
        let (id, generation) = slab.insert(dummy_task(false));
        assert_eq!(slab.mark_finished_and_maybe_reclaim(id, generation), None);
        assert!(slab.get_checked(id, generation).is_some());
        // await's reclaim still frees it.
        assert!(slab.reclaim(id, generation));
    }

    // get_checked rejects an out-of-range index instead of panicking, which is
    // what makes an await/is_done/detach on a garbage handle return safely.
    #[test]
    fn slab_get_checked_out_of_range_is_none() {
        let mut slab = Slab::new();
        assert!(slab.get_checked(0, 0).is_none());
        assert!(slab.get_checked(12345, 0).is_none());
    }

    // the FFI entrypoints must survive a garbage or stale handle: an unregistered
    // id fails the registry check and returns the safe default (0 / not-done /
    // no-op) without ever indexing the slab. these never spawn, so they touch no
    // real task — they only prove the guard rejects a bogus i64 without panicking.
    #[test]
    fn ffi_entrypoints_reject_garbage_handles() {
        for &bogus in &[0_i64, -1, i64::MAX, i64::MIN, 999_999_999] {
            // SAFETY: os_thread_await is unsafe only because a valid handle would
            // join a thread; a bogus handle is rejected before any of that.
            assert_eq!(unsafe { os_thread_await(bogus) }, 0);
            assert_eq!(os_thread_is_done(bogus), 0);
            os_thread_detach(bogus); // must not panic
        }
    }
}
