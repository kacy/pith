//! cooperative preemption. a compute-only task that never blocks would
//! otherwise hold its worker forever and starve every other task there. the
//! backend emits a safe-point at every loop back-edge: one load of
//! `PITH_PREEMPT_REQUESTED` and, only when it is set, a call to
//! `pith_green_maybe_yield`. a monitor thread (`monitor_loop`) sets the flag
//! when a task outruns its quantum. the flag is process-global and the epoch
//! a coarse tick, so the common path is one relaxed byte load and a branch.

use super::*;

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
pub(super) static PREEMPT_EPOCH: AtomicU64 = AtomicU64::new(0);

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
pub(super) const QUANTUM_EPOCHS: u64 = 2;

/// how often the monitor thread wakes to bump the epoch and scan the workers.
pub(super) const MONITOR_INTERVAL: Duration = Duration::from_millis(10);

/// has the task the given worker is running exceeded its quantum as of `now`?
/// factored out so both the monitor and `pith_green_maybe_yield` share one
/// definition of "overrun", and so the epoch arithmetic is unit-testable.
pub(super) fn task_overrun(running_since: u64, now: u64) -> bool {
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
/// 3. overrun → set `preempt_pending` so `run_task`'s yield arm puts the task
///    on its owner's `deferred` queue, then `park_current` to suspend the
///    coroutine back to the scheduler. this reuses the park/wake protocol; no
///    new scheduler path is added.
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

/// start the preemption monitor (sysmon) thread. it is the only thing that ever
/// sets `PITH_PREEMPT_REQUESTED`, and it starts only here alongside the green
/// workers — so under the os-thread backend it never runs and the flag stays 0.
pub(super) fn start_monitor() {
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
pub(super) fn monitor_loop() {
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

