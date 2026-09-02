use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

/// Global statistics for debugging
pub static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static LIVE_OBJECTS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_RC_ALLOCS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_RC_RETAINS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_RC_RELEASES: AtomicUsize = AtomicUsize::new(0);
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

// 0 = not probed yet, 1 = disabled, 2 = enabled. these hooks sit on the
// hottest runtime entry points (list get, byte-buffer write, struct alloc),
// so with stats off the check has to cost one relaxed load and a predictable
// branch — a OnceLock here showed up as a measurable slice of a list index.
static PERF_STATS_STATE: AtomicU8 = AtomicU8::new(0);
static PERF_STATS_REGISTERED: AtomicBool = AtomicBool::new(false);

#[cold]
fn perf_stats_probe() -> bool {
    let enabled = matches!(
        std::env::var("PITH_PERF_STATS").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    );
    PERF_STATS_STATE.store(if enabled { 2 } else { 1 }, Ordering::Relaxed);
    enabled
}

#[inline(always)]
pub fn perf_stats_enabled() -> bool {
    match PERF_STATS_STATE.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => perf_stats_probe(),
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
    eprintln!("  rc allocs: {}", PERF_RC_ALLOCS.load(Ordering::Relaxed));
    eprintln!("  rc retains: {}", PERF_RC_RETAINS.load(Ordering::Relaxed));
    eprintln!(
        "  rc releases: {}",
        PERF_RC_RELEASES.load(Ordering::Relaxed)
    );
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
