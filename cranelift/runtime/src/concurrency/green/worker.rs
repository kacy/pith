//! the worker threads: what each one owns, how it finds work, and how it
//! parks when there is none.

use super::*;

/// the park timeout a worker starts from, and the ceiling it backs off to
/// while it keeps finding no work. the minimum is the latency bound that
/// matters: it only covers the sliver where an enqueue's notify races our
/// decision to sleep, and any enqueue that loses that race still wakes us
/// through `wake_worker`. the maximum only bounds how long a worker can
/// sleep through a wake that never arrives. nothing depends on it for
/// correctness, since both the enqueue path and the collector's stop
/// request notify the pool explicitly.
pub(super) const PARK_TIMEOUT_MIN: Duration = Duration::from_millis(1);
pub(super) const PARK_TIMEOUT_MAX: Duration = Duration::from_millis(32);

/// what a worker keeps for itself: free coroutine stacks and free task ids.
/// only the worker's own thread touches it (see `with_own_cache`), so it
/// needs no lock.
#[derive(Default)]
pub(super) struct WorkerCache {
    pub(super) stacks: Vec<corosensei::stack::DefaultStack>,
    pub(super) ids: Vec<TaskId>,
}

/// a value that only one thread ever touches. `Sync` so it can sit in the
/// shared `Worker`; the accessor is what keeps other threads out.
#[derive(Default)]
pub(super) struct OwnerOnly<T>(pub(super) UnsafeCell<T>);

// SAFETY: see `with_own_cache`, the only place the cell is reached.
unsafe impl<T: Send> Sync for OwnerOnly<T> {}

/// run `f` on the calling worker thread's own cache, or return `None` on any
/// other thread. keyed by `CACHE_OWNER`, which only `worker_loop` sets, so a
/// test or probe that poses as a worker with `set_current_worker` uses the
/// shared pools instead of racing the real worker. `f` must not call back in.
pub(super) fn with_own_cache<R>(f: impl FnOnce(&mut WorkerCache) -> R) -> Option<R> {
    let w = CACHE_OWNER.with(|c| c.get())?;
    // SAFETY: worker `w`'s cache is reached only from worker `w`'s own thread
    // (the one that set `CACHE_OWNER`), and `f` does not re-enter, so this is
    // the only reference.
    Some(f(unsafe { &mut *scheduler().workers[w].cache.0.get() }))
}

/// one worker's run queues, its park spot, and what the monitor reads about
/// it. workers are created once and live for the process.
///
/// aligned so that two workers never share a cache line: peers lock a
/// worker's queues to steal from it, and without the alignment each of those
/// locks evicted the neighbouring worker's queues as well.
#[repr(align(64))]
pub(super) struct Worker {
    /// fresh, never-resumed tasks. stealable by peers because a fresh coroutine
    /// carries no live stack (see the placement note in the module doc).
    pub(super) local: StealQueue,
    /// woken started tasks owned by this worker. this worker drains it; a peer
    /// takes from the fifo only once this worker has been stuck in one task for
    /// a while (`steal_pinned`), and then becomes the task's owner.
    /// `next` is the single task a same-worker wake asks to run first: the other
    /// half of a handoff, still cache-hot. two things keep that preference from
    /// starving the fifo behind it: the slot holds exactly ONE task, so a second
    /// same-worker wake displaces the first to the BACK of the fifo instead of
    /// stacking, and `pop` gives the fifo a turn after a bounded run of slot
    /// hits, which is what actually bounds a ping-ponging pair (it empties the
    /// slot before refilling it, so displacement alone never fires for it). one
    /// mutex covers both so the displace-and-push is a single atomic step.
    pub(super) pinned: Mutex<PinnedQueue>,
    /// the length of `pinned`'s fifo, mirrored so an idle peer can pass over a
    /// worker with nothing to steal without taking its lock.
    pub(super) pinned_fifo: AtomicUsize,
    /// preempted started tasks pinned to this worker, drained at the *lowest*
    /// priority (after pinned, local, injector, and stealing). a compute task that
    /// overran its quantum lands here so it yields to every other ready task on the
    /// worker; without this it would sit at the front of `pinned` and simply be
    /// re-selected, right back into the loop that got it preempted. only the
    /// owner drains it; `steal_pinned` never looks here.
    pub(super) deferred: Mutex<VecDeque<TaskId>>,
    /// free stacks and task ids for this worker's own use (see
    /// `with_own_cache`).
    pub(super) cache: OwnerOnly<WorkerCache>,
    /// whether this worker is currently parked in its poller (`netpoll::
    /// idle_wait`). each worker parks on its own spot, so a wake can target
    /// exactly the worker that is able to run the task. crucially, a *pinned*
    /// wake nudges only the task's owner instead of a pool-wide notify that also
    /// wakes idle peers which have nothing to run. the flag lets `wake_worker`
    /// skip the notify entirely when the target is busy running tasks, the
    /// common case on a hot pipeline. the Dekker pairing is in `park`: it
    /// publishes the flag before its final work re-check, and enqueuers publish
    /// their work before reading the flag, so at least one side always sees the
    /// other. a notify that lands before the wait starts is kept by the poller
    /// (a pending eventfd, or the fallback's flag), so it is never lost.
    pub(super) parked: AtomicBool,
    /// set by `wake_searcher` when it wakes this worker to look for fresh work.
    /// the waker has already counted the worker in `Scheduler::searching`, so
    /// the worker takes over that count when it sees the flag (see
    /// `worker_loop`) instead of adding its own.
    pub(super) search_token: AtomicBool,
    /// the id (`id + 1`, 0 = none) of the task this worker is currently
    /// resuming. written right before `resume` and cleared right after, so the
    /// monitor thread can spot a task that has been on-CPU too long. the monitor
    /// only ever reads this (and `running_since`) via atomics and never touches
    /// a queue or coroutine, so it cannot race the worker over shared structure.
    pub(super) running_task: AtomicUsize,
    /// the epoch (`PREEMPT_EPOCH`) at which this worker began resuming its current
    /// task. the monitor and `pith_green_maybe_yield` compare it against the
    /// current epoch to detect a quantum overrun.
    pub(super) running_since: AtomicU64,
    /// how many tasks this worker has resumed. only the worker writes it; a
    /// peer that sees it stand still knows the worker is inside one long task.
    pub(super) resumes: AtomicU64,
}

/// debug-only invariant guard for a task pulled from a *stealable* source: a
/// worker's own `local` queue, the global injector, or a peer's `local` on a
/// steal. only fresh, never-resumed tasks travel those queues: a fresh task
/// has no `owner` yet and sits `Ready`. if either were false here, a started
/// task's coroutine would be about to resume from a queue that is not its
/// owner's, outside the claim protocol that hands a suspended coroutine
/// between threads (a wake's Parked -> Ready CAS, see `claim_parked`). cheap,
/// and compiled out in release.
///
/// history worth keeping: re-homing a parked task to the worker that wakes it
/// (now the default, see `claim_parked`) was first tried before the runtime's
/// own rust frames stopped holding thread-local addresses across a suspend, and it
/// silently dropped channel messages. the coroutine stack was never the
/// problem and neither was the pith compiler (a `threadlocal` global lowers
/// to a call, and `CURRENT_TLS` is per-task); see the accessor block above
/// `current_task` for what was, and `pith_green_cross_thread_probe` with
/// `tests/cross_thread_resume.rs` for the check that keeps it fixed against
/// the shipped archive.
#[cfg(debug_assertions)]
pub(super) fn debug_assert_stealable(id: TaskId) {
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
pub(super) fn debug_assert_stealable(_id: TaskId) {}

/// find the next ready task for worker `index`: its pinned queue first (resume
/// suspended work it owns), then its own local queue, then the injector, then a
/// steal from a random peer's local queue, and only then its deferred queue.
/// with `deferred_first` the deferred queue goes first instead; the worker loop
/// asks for that every `POLL_EVERY` tasks so a preempted task waits a bounded
/// number of turns. returns `None` if the whole pool is momentarily empty.
pub(super) fn find_work(index: usize, deferred_first: bool) -> Option<TaskId> {
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

    // own local queue (stealable): pop into a local so the queue lock is
    // released before the debug invariant check reads the task's word.
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

    // deferred last: only resume a preempted compute task once there is
    // nothing else: no owned woken work, no fresh work anywhere to run or steal.
    // this is what lets the other tasks on this worker make progress before the
    // hog runs again.
    pop_deferred(index)
}

/// how long a thief watches a peer before taking a task off its pinned fifo.
/// a peer that resumes anything in this window is getting through its queue,
/// and taking from it would split a pipeline that runs best on one worker.
pub(super) const PIN_STEAL_WAIT: Duration = Duration::from_micros(20);

/// take the oldest task off `victim`'s pinned fifo and make worker `thief` its
/// owner, if the victim has been inside one task for `PIN_STEAL_WAIT`. the
/// affinity slot is left alone: it holds the other half of a handoff the
/// victim resumes next.
///
/// a task in a pinned queue is `Ready` and held by that queue alone (a waker
/// that reads `Ready` changes nothing), so popping it under the victim's lock
/// gives this thread the only right to rewrite its owner. that is the same
/// hand-over a migrating wake makes in `claim_parked`.
pub(super) fn steal_pinned(victim: usize, thief: usize) -> Option<TaskId> {
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
pub(super) fn pop_deferred(index: usize) -> Option<TaskId> {
    scheduler().workers[index]
        .deferred
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .pop_front()
}

/// cheap per-thread rotating start index for steal victim selection. it needs
/// no real randomness, only to keep every worker from probing the same victim
/// first.
pub(super) fn steal_start(n: usize) -> usize {
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
pub(super) fn park(index: usize, timeout: Duration) -> bool {
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
/// colocated, a 90x cliff that a bounded spin closes, because the partner's
/// reply is on its way already.
///
/// it is only paid when the worker found work recently (`idle_timeout` still at
/// its floor). an idle pool spins once on the way down and then backs off, so
/// this does not undo the idle-syscall work: a worker that is genuinely out of
/// work parks as before.
pub(super) const PARK_SPIN: Duration = Duration::from_micros(10);

/// how many tasks a busy worker runs between non-blocking polls of its
/// sockets and serves its deferred queue first. go checks its global queue on
/// the same kind of count (61); this only bounds how long a ready socket's
/// task, or a preempted one, can wait behind queued work.
pub(super) const POLL_EVERY: u32 = 61;

/// look for work for up to `PARK_SPIN` before parking. probes are spaced by a
/// short pause burst rather than run back-to-back: `has_any_work` takes the
/// queue locks, and hammering them would slow down the very peer this worker is
/// waiting on.
pub(super) fn spin_for_work(index: usize) -> bool {
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
/// must consider the same sources: this worker's own pinned and deferred
/// queues, its own local queue, the injector, and any peer's *local*
/// (stealable) queue.
///
/// it must NOT count a *peer's pinned* queue. `find_work` can steal from one,
/// but only from a peer stuck in one long task; counting the queue here kept
/// every idle worker spinning beside a busy pipeline instead of parking. on
/// the channel fan-out benchmark that took 20 times the CPU for the same
/// wall time. an idle worker still looks at peers' pinned queues on each
/// pass through `find_work` before it parks.
pub(super) fn has_any_work(index: usize) -> bool {
    let sched = scheduler();
    // own pinned first: the woken work this worker must resume itself.
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
    // own deferred (preempted) work is reachable only by this worker, so like the
    // pinned queue it must count here or the worker would park on a task it owns.
    !sched.workers[index]
        .deferred
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_empty()
}

/// the worker thread body: take work, run it, park when there is none.
pub(super) fn worker_loop(index: usize, cycle_slot: Option<Arc<crate::cycle::MutatorSlot>>) {
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
                // short. the bound below is the fallback, not the mechanism.
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
                // so a handoff aimed at it is plausibly already in flight and
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

