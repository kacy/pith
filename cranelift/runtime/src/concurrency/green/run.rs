//! resuming a task, and recording its completion.

use super::*;

/// how `run_task` re-enqueues a task that just suspended, carrying the task's
/// owner worker. `Woken` and `Preempted` differ only in queue priority (see the
/// `Yield` arm of `run_task`); `None` means the task parked and its block site
/// owns waking it.
pub(super) enum Requeue {
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
pub(super) fn finish_task(id: TaskId, result: i64) {
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
/// taken out of the task's block, resumed, and then either dropped (the body
/// returned) or put back (the task suspended).
pub(super) fn run_task(id: TaskId) {
    // claim the task on its scheduling word: Ready -> Running, pinning it to
    // this worker on first resume (whoever runs it first owns it until a
    // migrating wake or `steal_pinned` moves it, and the owner rides in the
    // word so a waker can read it lock-free). finding it not Ready (someone
    // else already claimed a double-enqueued id, or it is gone) means there is
    // nothing to do.
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

    // the SAFETY argument for the transmute-and-call lives inside the
    // coroutine body (see green_spawn); here we just drive the coroutine.
    //
    // wrap the resume in `catch_unwind` so a panic raised in pith JIT/FFI code
    // (a trap that surfaces as a Rust panic, an FFI panic) does not unwind
    // through the coroutine into the worker thread and kill it. neither cargo
    // profile sets `panic = "abort"`, so without this a single panicking task
    // would silently retire a worker AND leave its `join.done` unset, hanging
    // every awaiter forever. corosensei re-raises a coroutine panic out of
    // `resume` (it has already unwound the coroutine's own stack, so `coro` is
    // complete and must never be resumed again; the panic arm drops it).
    //
    // AssertUnwindSafe: `coro` (a `SendCoroutine`) is not `UnwindSafe`, but a
    // caught panic here does not observe it in a broken state: we never resume
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
            // coroutine's own stack, so `coro` is complete; we drop it here and
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
            // the task suspended: either it parked on a blocking op (a channel,
            // a mutex or other primitive, or an await), or it was preempted at
            // a safe-point.
            //
            // put the coroutine back BEFORE the word transition below makes the
            // task claimable: once the word reads Parked a waker may re-enqueue
            // the task, and its next resume, on whichever worker claims it,
            // must find the coroutine present. the transition CAS
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
            // a wake can land between the load and the CAS; the retry sees its
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

