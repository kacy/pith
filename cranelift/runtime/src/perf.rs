use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::OnceLock;

/// Global statistics for debugging
pub static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static LIVE_OBJECTS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_RC_ALLOCS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_RC_RETAINS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_RC_RELEASES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_STRING_ALLOCS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_STRING_ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_BYTES_ALLOCS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_BYTES_ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_BYTE_BUFFER_NEWS: AtomicUsize = AtomicUsize::new(0);
pub static PERF_BYTE_BUFFER_WRITES: AtomicUsize = AtomicUsize::new(0);
pub static PERF_BYTE_BUFFER_WRITE_BYTES: AtomicUsize = AtomicUsize::new(0);
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

static PERF_STATS_ENABLED: OnceLock<bool> = OnceLock::new();
static PERF_STATS_REGISTERED: AtomicBool = AtomicBool::new(false);

pub fn perf_stats_enabled() -> bool {
    *PERF_STATS_ENABLED.get_or_init(|| {
        matches!(
            std::env::var("FORGE_PERF_STATS").ok().as_deref(),
            Some("1") | Some("true") | Some("yes")
        )
    })
}

pub fn perf_count(counter: &AtomicUsize, delta: usize) {
    if perf_stats_enabled() {
        counter.fetch_add(delta, Ordering::Relaxed);
    }
}

extern "C" fn forge_perf_dump_stats_at_exit() {
    dump_perf_stats();
}

pub fn ensure_perf_stats_registered() {
    if !perf_stats_enabled() {
        return;
    }
    if PERF_STATS_REGISTERED.swap(true, Ordering::Relaxed) {
        return;
    }
    unsafe {
        libc::atexit(forge_perf_dump_stats_at_exit);
    }
}

pub fn dump_perf_stats() {
    if !perf_stats_enabled() {
        return;
    }
    eprintln!("forge perf stats");
    eprintln!("  rc allocs: {}", PERF_RC_ALLOCS.load(Ordering::Relaxed));
    eprintln!("  rc retains: {}", PERF_RC_RETAINS.load(Ordering::Relaxed));
    eprintln!("  rc releases: {}", PERF_RC_RELEASES.load(Ordering::Relaxed));
    eprintln!(
        "  string allocs: {} bytes={}",
        PERF_STRING_ALLOCS.load(Ordering::Relaxed),
        PERF_STRING_ALLOC_BYTES.load(Ordering::Relaxed)
    );
    eprintln!(
        "  bytes allocs: {} bytes={}",
        PERF_BYTES_ALLOCS.load(Ordering::Relaxed),
        PERF_BYTES_ALLOC_BYTES.load(Ordering::Relaxed)
    );
    eprintln!(
        "  byte_buffer new: {} writes={} write_bytes={}",
        PERF_BYTE_BUFFER_NEWS.load(Ordering::Relaxed),
        PERF_BYTE_BUFFER_WRITES.load(Ordering::Relaxed),
        PERF_BYTE_BUFFER_WRITE_BYTES.load(Ordering::Relaxed)
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
