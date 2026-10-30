//! opt-in scheduler profiling, behind `PITH_GREEN_STATS=1`. it counts how
//! wakes break down (same worker, cross worker, from the reactor), how many
//! found a parked worker, and how often the hot locks were contended, and
//! prints the totals at exit. off by default, and each counter is behind one
//! flag check, so a normal run pays nothing.

use super::*;

pub(super) static STATS_ON: OnceLock<bool> = OnceLock::new();
// claimed wakes by where the waker ran (see `note_wake`), and whether a
// notify found a parked worker (see `note_notify`).
pub(super) static WAKE_SAME: AtomicU64 = AtomicU64::new(0);
pub(super) static WAKE_CROSS: AtomicU64 = AtomicU64::new(0);
pub(super) static WAKE_REACTOR: AtomicU64 = AtomicU64::new(0);
pub(super) static WAKE_FUTEX: AtomicU64 = AtomicU64::new(0);
pub(super) static WAKE_ABSORBED: AtomicU64 = AtomicU64::new(0);
pub(super) static WAKE_MIGRATED: AtomicU64 = AtomicU64::new(0);
/// workers inside `idle_wait` right now, kept only while stats are on.
pub(super) static PARKED_WORKERS: AtomicUsize = AtomicUsize::new(0);

pub(super) fn stats_on() -> bool {
    *STATS_ON.get_or_init(|| {
        std::env::var("PITH_GREEN_STATS")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false)
    })
}

pub(super) extern "C" fn dump_stats() {
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

/// profiling: categorize a claimed wake by where the waker sits relative to
/// the task's owner worker, and count a migration when the claim moved the
/// task. read-only; the caller has checked that a stats switch is on.
#[cold]
#[inline(never)]
pub(super) fn note_wake(owner: Option<usize>, home: Option<usize>) {
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

// contended-acquire and hot-path counters for the same `PITH_GREEN_STATS`
// lens as the wake breakdown above. a `try_lock` miss on a pinned queue is
// one contended acquire; the channel-wake and park counters size the hot
// paths those locks sit on.
pub(super) static C_PINNED: AtomicU64 = AtomicU64::new(0);
pub(super) static C_CH_WAKE: AtomicU64 = AtomicU64::new(0);
pub(super) static C_PARKS: AtomicU64 = AtomicU64::new(0);

pub(super) fn dump_contention() {
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

/// profiling helper: record whether a wake actually had to rouse a parked worker
/// (a real futex cost) or was absorbed because the pool was busy. read-only,
/// gated behind `PITH_GREEN_STATS`.
pub(super) fn note_notify() {
    if !stats_on() {
        return;
    }
    if PARKED_WORKERS.load(AtomicOrdering::Relaxed) > 0 {
        WAKE_FUTEX.fetch_add(1, AtomicOrdering::Relaxed);
    } else {
        WAKE_ABSORBED.fetch_add(1, AtomicOrdering::Relaxed);
    }
}

