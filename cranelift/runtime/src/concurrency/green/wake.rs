//! parking a task, waking it, and putting tasks on run queues.

use super::*;

/// suspend the green task `id` back to its worker. must be called only from
/// inside that task (every caller has just gone through a `current_task()`
/// check, and passes the id it got from it), and only *after* releasing its
/// block-site lock: `suspend` returns control to the worker, which may run
/// other pith code that takes the same lock.
///
/// returns when the task is resumed by a later `wake`.
///
/// the id is a parameter rather than a thread-local read because several call
/// sites park inside a loop, and a compiler may hoist a thread-local read out
/// of a loop: a later iteration would then suspend through a value read on the
/// thread an earlier iteration ran on. the id means the same thing on every
/// thread, and the yielder is read from the task's own block, so this path
/// holds no thread-derived value at all (see the accessor notes above
/// `current_task`).
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
    // published the yielder. panic rather than dereference null: `run_task`
    // catches it, marks the task done and keeps the worker alive, where a null
    // deref would take the process with it.
    assert!(
        !yielder.is_null(),
        "park_current on a task with no running coroutine (id {id})"
    );
    // SAFETY: as above. the `Yielder` is valid for as long as this coroutine is
    // on-CPU, which is exactly the window in which pith code can reach this
    // call. the suspend is a stack switch back to the worker; nothing retains
    // the pointer across it.
    unsafe { &*yielder }.suspend(());
}

/// record the running coroutine's `Yielder` in its own sync block, so
/// `park_current` can find it later. called once, at the top of a task body on
/// its first resume, before the task can possibly reach a block site.
///
/// this is a task-entry read of `CURRENT_TASK`, which is the one place the
/// `CURRENT_*` cells may be consulted: `run_task` has just installed them and
/// no suspension has happened yet.
pub(super) fn publish_yielder(yielder: *const TaskYielder) {
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
/// resumes and re-checks the condition it blocked on. block sites call it
/// while holding their own lock (a channel send/recv/close, a mutex unlock, a
/// task finishing); it takes no scheduler lock itself beyond the owner's queue
/// mutex (the state flip is a CAS on the task's scheduling word), so the
/// nesting stays block-site -> queue. safe to call for a task in any state: it
/// acts only when the task is actually parked, and defers via the wake-pending
/// flag if the task is still mid-suspend, so a wake is never lost and never
/// double-enqueued.
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
/// is `Ready` every reader of the word (the enqueue below, the resume that
/// follows, the next wake) agrees on the new home. a pair of tasks that talk
/// to each other converge onto one worker after the first exchange, and stay
/// there: a wake from the same worker moves nothing. a waker that is not a
/// worker (`main`, a blocking-pool thread) has nowhere to take the task and
/// leaves the owner alone. with `PITH_GREEN_MIGRATE=0` the task always goes
/// back to its owner.
///
/// this is only sound because nothing on the resumable path derives a value
/// from the thread the task parked on. see the accessor block above
/// `current_task`, and the placement section of the module doc.
#[inline]
pub(super) fn claim_parked(block: &SyncBlock) -> Option<Option<usize>> {
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

/// wake a parked green task and, if it is truly parked, run `fetch` and hand it
/// the result: a channel send that wakes a parked receiver dequeues on the
/// receiver's behalf, so the receiver resumes with the value in hand instead of
/// re-locking the channel and re-trying the ring. this is go-style direct
/// handoff, but the ring stays the ordering source so fifo holds. `fetch` runs only
/// after the task is claimed (the Parked -> Ready CAS), which is what makes
/// handing over safe: a task that is still `Running` might complete its own
/// attempt, so it gets a plain pending-flag wake and the value stays in the
/// ring. a `fetch` that comes back empty (a racing consumer drained the ring)
/// still wakes the task valueless; it re-checks and re-parks, the usual
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
    // claiming waker only writes it while the task is Parked. the state
    // machine keeps the two disjoint (see `SyncBlock::handoff`).
    unsafe { (*block.handoff.get()).take() }
}

/// push a *fresh* (never-resumed) task onto a stealable queue and wake one idle
/// worker. spawns from a worker go to that worker's local queue (cache-friendly,
/// keeps a task's children near it); spawns from elsewhere go to the injector.
/// only fresh tasks travel this path, so anything a peer steals from a `local`
/// queue is stack-empty and safe to move across threads.
pub(super) fn enqueue_fresh(id: TaskId) {
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
/// claiming the single search slot with a CAS is what keeps a burst of spawns
/// from waking the whole pool, where all but one woken worker would find
/// nothing and park again. `stop_searching` passes the slot on when the burst
/// is bigger than one worker can take.
///
/// the pusher stores its task before it reads `searching` here, and a searcher
/// that gives up decrements `searching` before its last look at the queues
/// (`worker_loop`), so one side always sees the other.
pub(super) fn wake_searcher() {
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
pub(super) fn stop_searching() {
    let sched = scheduler();
    if sched.searching.fetch_sub(1, AtomicOrdering::SeqCst) == 1 && any_stealable() {
        wake_searcher();
    }
}

/// is there fresh work in the injector or in any worker's local queue? reads
/// only the queues' atomic lengths.
pub(super) fn any_stealable() -> bool {
    let sched = scheduler();
    !sched.injector.is_empty() || sched.workers.iter().any(|w| !w.local.is_empty())
}

/// re-enqueue a *woken started* task onto its owner worker's pinned queue and
/// wake the pool. this is the only path a task that has already resumed takes
/// back into scheduling, and it always targets the owner recorded in the
/// task's word: the worker that first ran it, or a worker that has since
/// taken it over (a migrating wake in `claim_parked`, or `steal_pinned`).
pub(super) fn enqueue_woken(owner: usize, id: TaskId) {
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
        // completed, so ask to run it next, while its data is hot, instead of
        // behind every earlier wake. it goes in the single `next` slot, so the
        // second such wake displaces the first to the back of the fifo: a
        // ping-ponging pair yields the slot on every exchange and cannot starve
        // the tasks queued behind it (preemption cannot rescue that shape:
        // each half re-parks long before it overruns a quantum). cross-worker
        // wakes are plain fifo.
        if current_worker() == Some(owner) {
            pinned.push_next(id);
        } else {
            pinned.push(id);
        }
        sched.workers[owner].pinned_fifo.store(pinned.queue.len(), AtomicOrdering::Relaxed);
    }
    // nudge only the owner. a peer steals from a pinned queue only once the
    // owner has been stuck in one task, so waking a peer here would mostly
    // find nothing to take, and a pipeline whose tasks share one worker leaves
    // the others parked. if the owner is busy the notify is a cheap no-op.
    note_notify();
    wake_worker(owner);
}

/// re-enqueue a *preempted* started task onto its owner worker's `deferred`
/// queue, the lowest-priority spot. like `enqueue_woken` it targets the owner
/// and nudges only the owner; no peer ever takes from a deferred queue. the
/// difference is priority: `find_work` drains this queue last (or on the
/// periodic deferred turn), so a preempted compute task cannot crowd out other
/// ready work. that is what makes preemption relieve starvation rather than
/// re-select the same hog.
pub(super) fn enqueue_preempted(owner: usize, id: TaskId) {
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
pub(super) fn wake_worker(index: usize) {
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

