//! Background collector thread and explicit collection triggers.

use super::collect::{
    collect, collect_threshold, CYCLE_COLLECTIONS, CYCLE_GRAVEYARD_FREED, CYCLE_ROOTS_EXAMINED,
    CYCLE_WHITES_FREED,
};
use super::stw::{native_bracket, set_on_collector, with_world_stopped};
use super::suspects::{
    lock_graveyard, lock_suspects, CYCLE_GRAVEYARD_DEFERRED, CYCLE_SUSPECTS_BUFFERED,
    CYCLE_SUSPECTS_OVERFLOWED,
};
use super::cycle_gc_enabled;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

pub(super) fn wake_collector_if_needed(len: usize) {
    ensure_collector_thread();
    if len >= collect_threshold() {
        GC_CTRL_CV.notify_all();
    }
}
// --- the collector thread ---------------------------------------------------
//
// a dedicated std thread, spawned lazily on the first suspect (or the first
// forced collection) while the flag is on. it never registers as a mutator,
// so the rendezvous never waits on it. it loops: wait until the suspect
// buffer crosses the threshold or a forced collection is requested, stop the
// world, collect, and on a failed stop back off (10ms doubling to 500ms)
// before retrying.

struct GcControl {
    /// forced collections requested; each `pith_cycle_gc_collect_now` call
    /// takes the next sequence number and waits for it to complete.
    force_pending: u64,
    /// forced requests answered (successfully or not).
    force_done: u64,
    /// whether the attempt that completed `force_done` stopped the world.
    last_force_ok: bool,
}

static GC_CTRL: Mutex<GcControl> = Mutex::new(GcControl {
    force_pending: 0,
    force_done: 0,
    last_force_ok: false,
});
static GC_CTRL_CV: Condvar = Condvar::new();
static COLLECTOR_STARTED: AtomicBool = AtomicBool::new(false);

fn lock_ctrl() -> MutexGuard<'static, GcControl> {
    crate::sync::lock(&GC_CTRL)
}

const COLLECT_BACKOFF_START: Duration = Duration::from_millis(10);
const COLLECT_BACKOFF_MAX: Duration = Duration::from_millis(500);

pub(super) fn ensure_collector_thread() {
    if COLLECTOR_STARTED.load(Ordering::Relaxed) {
        return;
    }
    if COLLECTOR_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("pith-cycle-gc".into())
        .spawn(collector_main);
    if spawned.is_err() {
        // no thread means no collections (cycles leak, the acceptable
        // direction), and a later suspect may try again.
        COLLECTOR_STARTED.store(false, Ordering::SeqCst);
    }
}

fn collector_main() {
    // for its whole life: teardown code must never park this thread on its
    // own stop request, and it must never register as a mutator.
    let _ = set_on_collector(true);
    let mut backoff = COLLECT_BACKOFF_START;
    loop {
        let force_goal = wait_for_work();
        // SAFETY: `with_world_stopped` holds every registered mutator stopped while `collect` runs.
        let stopped = with_world_stopped(|| unsafe { collect() }).is_some();
        finish_forced(force_goal, stopped);
        if stopped {
            backoff = COLLECT_BACKOFF_START;
        } else {
            // some mutator would not stop (or the flag went off). back off,
            // but on the control condvar so a forced request cuts the wait
            // short.
            let ctrl = lock_ctrl();
            if ctrl.force_pending == ctrl.force_done {
                let _ = crate::sync::wait_timeout(&GC_CTRL_CV, ctrl, backoff);
            }
            backoff = (backoff * 2).min(COLLECT_BACKOFF_MAX);
        }
    }
}

/// Block until there is a reason to collect. Returns the forced-request
/// sequence this attempt will answer for (unchanged `force_done` when the
/// trigger was the threshold alone).
fn wait_for_work() -> u64 {
    let mut ctrl = lock_ctrl();
    loop {
        if ctrl.force_pending > ctrl.force_done {
            return ctrl.force_pending;
        }
        if lock_suspects().len() >= collect_threshold() {
            return ctrl.force_pending;
        }
        // the timeout is the threshold poll: pushes only notify on the
        // crossing itself, and a buffer that idles above the threshold
        // afterwards still deserves a pass.
        ctrl = crate::sync::wait_timeout(&GC_CTRL_CV, ctrl, Duration::from_millis(50)).0;
    }
}

/// Answer every forced request the finished attempt covered. Requests that
/// arrived mid-attempt have a later sequence and get their own attempt.
fn finish_forced(goal: u64, stopped: bool) {
    let mut ctrl = lock_ctrl();
    if goal > ctrl.force_done {
        ctrl.force_done = goal;
        ctrl.last_force_ok = stopped;
        GC_CTRL_CV.notify_all();
    }
}

/// Force a collection and wait for the attempt: 1 when it ran, 0 when the
/// stop timed out or the collector is disabled. Callable from pith by its
/// literal name; the wait is a native bracket, so a mutator blocking here
/// counts as stopped for the very collection it asked for.
#[no_mangle]
pub extern "C" fn pith_cycle_gc_collect_now() -> i64 {
    if !cycle_gc_enabled() {
        return 0;
    }
    ensure_collector_thread();
    let mut ctrl = lock_ctrl();
    ctrl.force_pending += 1;
    let my_seq = ctrl.force_pending;
    GC_CTRL_CV.notify_all();
    let _native = native_bracket();
    while ctrl.force_done < my_seq {
        ctrl = crate::sync::wait(&GC_CTRL_CV, ctrl);
    }
    let ok = ctrl.last_force_ok;
    // release the control lock before the bracket exits: the exit may park
    // this thread for a later stop, and the collector needs the lock to
    // answer forced requests; parking while holding it would wedge both.
    drop(ctrl);
    i64::from(ok)
}

/// Print the cycle-gc counters to stderr.
#[no_mangle]
pub extern "C" fn pith_cycle_gc_stats() {
    eprintln!(
        "pith cycle gc: buffered={} overflowed={} graveyard_deferred={} pending_suspects={} pending_graveyard={} collections={} roots_examined={} whites_freed={} graveyard_freed={}",
        CYCLE_SUSPECTS_BUFFERED.load(Ordering::Relaxed),
        CYCLE_SUSPECTS_OVERFLOWED.load(Ordering::Relaxed),
        CYCLE_GRAVEYARD_DEFERRED.load(Ordering::Relaxed),
        lock_suspects().len(),
        lock_graveyard().len(),
        CYCLE_COLLECTIONS.load(Ordering::Relaxed),
        CYCLE_ROOTS_EXAMINED.load(Ordering::Relaxed),
        CYCLE_WHITES_FREED.load(Ordering::Relaxed),
        CYCLE_GRAVEYARD_FREED.load(Ordering::Relaxed),
    );
}
