use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Global statistics for debugging
pub static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static LIVE_OBJECTS: AtomicUsize = AtomicUsize::new(0);
// count operations by kind: the retains and releases of each counted kind
// the strings do not already cover, the struct releases that took the
// sole-owner path, and weak references. indexed by the RC_* constants.
pub const RC_STRUCT_RETAIN: usize = 0;
pub const RC_STRUCT_RELEASE: usize = 1;
pub const RC_STRUCT_SOLE_RELEASE: usize = 2;
pub const RC_WEAK_RETAIN: usize = 3;
pub const RC_WEAK_RELEASE: usize = 4;
pub const RC_LIST_RETAIN: usize = 5;
pub const RC_LIST_RELEASE: usize = 6;
pub const RC_MAP_RETAIN: usize = 7;
pub const RC_MAP_RELEASE: usize = 8;
pub const RC_SET_RETAIN: usize = 9;
pub const RC_SET_RELEASE: usize = 10;
pub const RC_CLOSURE_RETAIN: usize = 11;
pub const RC_CLOSURE_RELEASE: usize = 12;
pub const RC_BYTES_RETAIN: usize = 13;
pub const RC_BYTES_RELEASE: usize = 14;
const RC_OP_NAMES: [&str; 15] = [
    "struct retain",
    "struct release",
    "struct sole-owner release",
    "weak retain",
    "weak release",
    "list retain",
    "list release",
    "map retain",
    "map release",
    "set retain",
    "set release",
    "closure retain",
    "closure release",
    "bytes retain",
    "bytes release",
];
pub static PERF_RC_OPS: [AtomicUsize; 15] = [const { AtomicUsize::new(0) }; 15];
// struct allocations by field count, the last bucket holding 16 and up. an
// optional shell has two fields and a result box three, so the small buckets
// say how much of the struct traffic is wrappers rather than user types.
pub static PERF_STRUCT_FIELDS: [AtomicUsize; 17] = [const { AtomicUsize::new(0) }; 17];
pub static PERF_STRING_ALLOCS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_STRING_ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_CSTRING_ALLOCS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_CSTRING_FREES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_CSTRING_RETAINS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_CSTRING_RETAINS_PUSH: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_CASCADE_RELEASES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_CSTRING_RELEASES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_STRUCT_ALLOCS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_STRUCT_FREES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_BYTES_ALLOCS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_BYTES_FREES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_BYTES_ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_BYTE_BUFFER_NEWS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_BYTE_BUFFER_FREES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_BYTE_BUFFER_WRITES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_BYTE_BUFFER_WRITE_BYTES: AtomicUsize = AtomicUsize::new(0);
// Channels created, channels successfully closed, the bytes those channels
// asked the allocator for, and the bodies given back. RETAINED_BYTES counts
// every byte ever requested and only grows; FREED_BYTES counts what
// retirement handed back, so the difference is what the process still holds.
// A retired channel leaves its permanent handle stub behind (see
// `docs/channel_ownership.md`), which is why the two never meet.
pub static PERF_CHANNEL_NEWS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_CHANNEL_CLOSES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_CHANNEL_RETAINED_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_CHANNEL_FREES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_CHANNEL_FREED_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_SIGNAL_WAITS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_SIGNAL_DELIVERIES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_SOCK_READS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_SOCK_READ_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_SOCK_WRITES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_SOCK_WRITE_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_REACTOR_WAITS: AtomicUsize = AtomicUsize::new(0);
// green wakes of parked tasks, by where the waker sat relative to the task's
// worker: the same worker (a userspace handoff), a different worker (a
// cross-thread enqueue and, if that worker is parked, a futex), or no worker
// at all (main or a blocking-pool thread). MIGRATIONS counts the cross-worker
// wakes that moved the task to the waker's worker (`PITH_GREEN_MIGRATE=1`).
pub static PERF_GREEN_WAKES_SAME: AtomicUsize = AtomicUsize::new(0);
pub static PERF_GREEN_WAKES_CROSS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_GREEN_WAKES_REACTOR: AtomicUsize = AtomicUsize::new(0);
pub static PERF_GREEN_MIGRATIONS: AtomicUsize = AtomicUsize::new(0);
// preemption safe-points. CHECKS counts the safe-points that took the slow path
// — the monitor had set the request flag, so the task called into the runtime to
// find out whether it was the one overrunning — and PREEMPTIONS the calls that
// ended in a park. the gap between them is the cost of the flag being global:
// one task overrunning wakes every running task at its next back-edge.
pub static PERF_GREEN_PREEMPT_CHECKS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_GREEN_PREEMPTIONS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_NEWS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_FREES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_PUSHES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_GETS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_GET_VALUE_CALLS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_GET_VALUE_CHECKED_CALLS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_GET_VALUE_UNCHECKED_CALLS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_GET_BYTES_CALLS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_GET_ELEM8: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_GET_ELEM_OTHER: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_SETS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_INSERTS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_LIST_REMOVES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_INSERTS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_STRING_INSERTS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_GETS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_STRING_GETS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_CONTAINS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_STRING_CONTAINS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_REMOVES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_STRING_REMOVES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_FAST_INSERTS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_FAST_GETS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_FAST_CONTAINS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_FAST_REMOVES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_FALLBACK_INSERTS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_FALLBACK_GETS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_FALLBACK_CONTAINS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_MAP_INT_FALLBACK_REMOVES: AtomicUsize = AtomicUsize::new(0);

static PERF_STATS_REGISTERED: AtomicBool = AtomicBool::new(false);

/// Whether `PITH_PERF_STATS` asked for the counters.
#[inline(always)]
pub fn perf_stats_enabled() -> bool {
    crate::switches::any(crate::switches::PERF_STATS)
}

/// Count one operation of the kind `op` names (an RC_* constant).
#[inline(always)]
pub fn perf_rc_op(op: usize) {
    count_rc_op(crate::switches::get(), op);
}

/// `perf_rc_op` for a caller that already holds a copy of the switches.
#[inline(always)]
pub fn count_rc_op(switches: u8, op: usize) {
    if switches & crate::switches::PERF_STATS != 0 {
        perf_record1(&PERF_RC_OPS[op], 1);
    }
}

#[inline(always)]
pub fn perf_count(counter: &AtomicUsize, delta: usize) {
    if perf_stats_enabled() {
        counter.fetch_add(delta, Ordering::Relaxed);
    }
}

/// Record one, two, or three counters off the hot path.
///
/// These are `#[cold]` and never inlined on purpose. Folding the counter
/// updates into the caller costs far more than the updates themselves: the
/// address materialisation and the atomic add need callee-saved registers, so
/// every hooked entry point — including ones that are otherwise leaf functions
/// — was pushing registers and building a stack frame on every call, whether
/// or not stats were on. Behind a call, the caller keeps its frameless fast
/// path and pays only the enabled-check.
#[cold]
#[inline(never)]
pub fn perf_record1(a: &AtomicUsize, da: usize) {
    ensure_perf_stats_registered_slow();
    a.fetch_add(da, Ordering::Relaxed);
}

#[cold]
#[inline(never)]
pub fn perf_record2(a: &AtomicUsize, da: usize, b: &AtomicUsize, db: usize) {
    ensure_perf_stats_registered_slow();
    a.fetch_add(da, Ordering::Relaxed);
    b.fetch_add(db, Ordering::Relaxed);
}

#[cold]
#[inline(never)]
pub fn perf_record3(
    a: &AtomicUsize,
    da: usize,
    b: &AtomicUsize,
    db: usize,
    c: &AtomicUsize,
    dc: usize,
) {
    ensure_perf_stats_registered_slow();
    a.fetch_add(da, Ordering::Relaxed);
    b.fetch_add(db, Ordering::Relaxed);
    c.fetch_add(dc, Ordering::Relaxed);
}

/// Record one group of perf counters, but only when `PITH_PERF_STATS` asked
/// for them.
///
/// The runtime's counter hooks sit on its hottest entry points, and each one
/// used to pay for its own `perf_stats_enabled()` load: the registration hook
/// plus one per counter, none of which LLVM can merge because they read a
/// relaxed atomic. Grouping them behind a single check leaves the disabled
/// path — every run that is not explicitly profiling — costing one relaxed
/// load and a perfectly predicted branch for the whole group. That matters
/// because of the volumes involved: type-checking the self-hosted compiler
/// indexes lists over 185 million times in about a second, so a couple of
/// redundant loads per index is a measurable slice of the run.
#[macro_export]
macro_rules! perf_stats {
    ($a:ident += $da:expr $(,)?) => {
        if $crate::perf_stats_enabled() {
            $crate::perf_record1(&$crate::$a, $da);
        }
    };
    ($a:ident += $da:expr, $b:ident += $db:expr $(,)?) => {
        if $crate::perf_stats_enabled() {
            $crate::perf_record2(&$crate::$a, $da, &$crate::$b, $db);
        }
    };
    ($a:ident += $da:expr, $b:ident += $db:expr, $c:ident += $dc:expr $(,)?) => {
        if $crate::perf_stats_enabled() {
            $crate::perf_record3(&$crate::$a, $da, &$crate::$b, $db, &$crate::$c, $dc);
        }
    };
}

extern "C" fn pith_perf_dump_stats_at_exit() {
    crate::runtime_core::report_leaked_cstrings();
    dump_perf_stats();
}

#[inline(always)]
pub fn ensure_perf_stats_registered() {
    if !perf_stats_enabled() {
        return;
    }
    ensure_perf_stats_registered_slow();
}

#[cold]
fn ensure_perf_stats_registered_slow() {
    if PERF_STATS_REGISTERED.swap(true, Ordering::Relaxed) {
        return;
    }
    unsafe {
        libc::atexit(pith_perf_dump_stats_at_exit);
    }
}

pub fn dump_perf_stats() {
    if !perf_stats_enabled() {
        return;
    }
    eprintln!("pith perf stats");
    let ops: Vec<String> = RC_OP_NAMES
        .iter()
        .zip(PERF_RC_OPS.iter())
        .map(|(name, count)| (name, count.load(Ordering::Relaxed)))
        .filter(|(_, count)| *count > 0)
        .map(|(name, count)| format!("{name}={count}"))
        .collect();
    eprintln!("  rc ops: {}", ops.join(" "));
    eprintln!(
        "  cstrings: alloc={} free={} live={} retain={} release={}",
        PERF_CSTRING_ALLOCS.load(Ordering::Relaxed),
        PERF_CSTRING_FREES.load(Ordering::Relaxed),
        PERF_CSTRING_ALLOCS.load(Ordering::Relaxed) as i64
            - PERF_CSTRING_FREES.load(Ordering::Relaxed) as i64,
        PERF_CSTRING_RETAINS.load(Ordering::Relaxed),
        PERF_CSTRING_RELEASES.load(Ordering::Relaxed),
    );
    eprintln!(
        "  cstring retains from container pushes: {} cascade releases: {}",
        PERF_CSTRING_RETAINS_PUSH.load(Ordering::Relaxed),
        PERF_LIST_CASCADE_RELEASES.load(Ordering::Relaxed),
    );
    eprintln!(
        "  string allocs: {} bytes={}",
        PERF_STRING_ALLOCS.load(Ordering::Relaxed),
        PERF_STRING_ALLOC_BYTES.load(Ordering::Relaxed)
    );
    eprintln!(
        "  structs: alloc={} free={}",
        PERF_STRUCT_ALLOCS.load(Ordering::Relaxed),
        PERF_STRUCT_FREES.load(Ordering::Relaxed),
    );
    let fields: Vec<String> = PERF_STRUCT_FIELDS
        .iter()
        .enumerate()
        .map(|(n, count)| (n, count.load(Ordering::Relaxed)))
        .filter(|(_, count)| *count > 0)
        .map(|(n, count)| if n == 16 { format!("16+={count}") } else { format!("{n}={count}") })
        .collect();
    eprintln!("  struct allocs by field count: {}", fields.join(" "));
    eprintln!(
        "  bytes allocs: {} frees={} bytes={}",
        PERF_BYTES_ALLOCS.load(Ordering::Relaxed),
        PERF_BYTES_FREES.load(Ordering::Relaxed),
        PERF_BYTES_ALLOC_BYTES.load(Ordering::Relaxed)
    );
    eprintln!(
        "  byte_buffer new: {} frees={} writes={} write_bytes={}",
        PERF_BYTE_BUFFER_NEWS.load(Ordering::Relaxed),
        PERF_BYTE_BUFFER_FREES.load(Ordering::Relaxed),
        PERF_BYTE_BUFFER_WRITES.load(Ordering::Relaxed),
        PERF_BYTE_BUFFER_WRITE_BYTES.load(Ordering::Relaxed)
    );
    eprintln!(
        "  channels: new={} closed={} freed={} retained_bytes={} freed_bytes={}",
        PERF_CHANNEL_NEWS.load(Ordering::Relaxed),
        PERF_CHANNEL_CLOSES.load(Ordering::Relaxed),
        PERF_CHANNEL_FREES.load(Ordering::Relaxed),
        PERF_CHANNEL_RETAINED_BYTES.load(Ordering::Relaxed),
        PERF_CHANNEL_FREED_BYTES.load(Ordering::Relaxed),
    );
    eprintln!(
        "  signals: waits={} deliveries={}",
        PERF_SIGNAL_WAITS.load(Ordering::Relaxed),
        PERF_SIGNAL_DELIVERIES.load(Ordering::Relaxed)
    );
    eprintln!(
        "  sockets: reads={} read_bytes={} writes={} write_bytes={} reactor_waits={}",
        PERF_SOCK_READS.load(Ordering::Relaxed),
        PERF_SOCK_READ_BYTES.load(Ordering::Relaxed),
        PERF_SOCK_WRITES.load(Ordering::Relaxed),
        PERF_SOCK_WRITE_BYTES.load(Ordering::Relaxed),
        PERF_REACTOR_WAITS.load(Ordering::Relaxed)
    );
    eprintln!(
        "  green wakes: same_worker={} cross_worker={} reactor={} migrated={}",
        PERF_GREEN_WAKES_SAME.load(Ordering::Relaxed),
        PERF_GREEN_WAKES_CROSS.load(Ordering::Relaxed),
        PERF_GREEN_WAKES_REACTOR.load(Ordering::Relaxed),
        PERF_GREEN_MIGRATIONS.load(Ordering::Relaxed)
    );
    eprintln!(
        "  green preemption: safepoint_slow_path={} preempted={}",
        PERF_GREEN_PREEMPT_CHECKS.load(Ordering::Relaxed),
        PERF_GREEN_PREEMPTIONS.load(Ordering::Relaxed)
    );
    eprintln!(
        "  lists: new={} free={} live={}",
        PERF_LIST_NEWS.load(Ordering::Relaxed),
        PERF_LIST_FREES.load(Ordering::Relaxed),
        PERF_LIST_NEWS.load(Ordering::Relaxed) as i64
            - PERF_LIST_FREES.load(Ordering::Relaxed) as i64,
    );
    eprintln!(
        "  list ops: push={} get={} get_value={} checked={} unchecked={} get_bytes={} elem8={} elem_other={} set={} insert={} remove={}",
        PERF_LIST_PUSHES.load(Ordering::Relaxed),
        PERF_LIST_GETS.load(Ordering::Relaxed),
        PERF_LIST_GET_VALUE_CALLS.load(Ordering::Relaxed),
        PERF_LIST_GET_VALUE_CHECKED_CALLS.load(Ordering::Relaxed),
        PERF_LIST_GET_VALUE_UNCHECKED_CALLS.load(Ordering::Relaxed),
        PERF_LIST_GET_BYTES_CALLS.load(Ordering::Relaxed),
        PERF_LIST_GET_ELEM8.load(Ordering::Relaxed),
        PERF_LIST_GET_ELEM_OTHER.load(Ordering::Relaxed),
        PERF_LIST_SETS.load(Ordering::Relaxed),
        PERF_LIST_INSERTS.load(Ordering::Relaxed),
        PERF_LIST_REMOVES.load(Ordering::Relaxed)
    );
    eprintln!(
        "  map int ops: insert={} get={} contains={} remove={}",
        PERF_MAP_INT_INSERTS.load(Ordering::Relaxed),
        PERF_MAP_INT_GETS.load(Ordering::Relaxed),
        PERF_MAP_INT_CONTAINS.load(Ordering::Relaxed),
        PERF_MAP_INT_REMOVES.load(Ordering::Relaxed)
    );
    eprintln!(
        "  map int path: fast_insert={} fast_get={} fast_contains={} fast_remove={} fallback_insert={} fallback_get={} fallback_contains={} fallback_remove={}",
        PERF_MAP_INT_FAST_INSERTS.load(Ordering::Relaxed),
        PERF_MAP_INT_FAST_GETS.load(Ordering::Relaxed),
        PERF_MAP_INT_FAST_CONTAINS.load(Ordering::Relaxed),
        PERF_MAP_INT_FAST_REMOVES.load(Ordering::Relaxed),
        PERF_MAP_INT_FALLBACK_INSERTS.load(Ordering::Relaxed),
        PERF_MAP_INT_FALLBACK_GETS.load(Ordering::Relaxed),
        PERF_MAP_INT_FALLBACK_CONTAINS.load(Ordering::Relaxed),
        PERF_MAP_INT_FALLBACK_REMOVES.load(Ordering::Relaxed)
    );
    eprintln!(
        "  map string ops: insert={} get={} contains={} remove={}",
        PERF_MAP_STRING_INSERTS.load(Ordering::Relaxed),
        PERF_MAP_STRING_GETS.load(Ordering::Relaxed),
        PERF_MAP_STRING_CONTAINS.load(Ordering::Relaxed),
        PERF_MAP_STRING_REMOVES.load(Ordering::Relaxed)
    );
}
