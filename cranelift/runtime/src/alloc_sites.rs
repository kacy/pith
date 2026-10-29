//! Where a program allocates. `PITH_ALLOC_SITES=<n>` records, for one heap
//! allocation in every n (`1` records all of them), the compiled function that
//! asked for it, and prints the busiest functions by kind at exit. A struct
//! allocation's kind carries its field count, since an optional shell (two
//! fields) and a result box (three) are the wrappers worth finding. Each
//! sample walks the stack with `std::backtrace`, which costs microseconds, so
//! a large n keeps a long run usable. Build the program with
//! `PITH_KEEP_SYMBOLS=1`, or every site prints as unknown.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

static PERIOD: AtomicU64 = AtomicU64::new(1);
static TICK: AtomicU64 = AtomicU64::new(0);
static SITES: Mutex<Option<HashMap<(&'static str, String), u64>>> = Mutex::new(None);

pub const STRUCT_KINDS: [&str; 17] = [
    "struct/0", "struct/1", "struct/2", "struct/3", "struct/4", "struct/5", "struct/6",
    "struct/7", "struct/8", "struct/9", "struct/10", "struct/11", "struct/12", "struct/13",
    "struct/14", "struct/15", "struct/16+",
];

/// Read `PITH_ALLOC_SITES` and arm the census. Runs once before main, while
/// the switches settle; returns whether the census is on.
pub(crate) fn start() -> bool {
    let period = std::env::var("PITH_ALLOC_SITES")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0);
    if period == 0 {
        return false;
    }
    PERIOD.store(period, Ordering::Relaxed);
    unsafe {
        libc::atexit(report);
    }
    true
}

/// Note one allocation of `kind`.
#[inline(always)]
pub fn record(kind: &'static str) {
    if crate::switches::any(crate::switches::ALLOC_SITES) {
        record_slow(kind);
    }
}

#[cold]
#[inline(never)]
fn record_slow(kind: &'static str) {
    let period = PERIOD.load(Ordering::Relaxed);
    if TICK.fetch_add(1, Ordering::Relaxed) % period != 0 {
        return;
    }
    let trace = std::backtrace::Backtrace::force_capture().to_string();
    let site = first_compiled_frame(&trace);
    let mut sites = SITES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    *sites.get_or_insert_with(HashMap::new).entry((kind, site)).or_insert(0) += period;
}

/// The first frame that belongs to compiled pith code: the runtime's own
/// frames are Rust paths (`a::b`, `<T as U>`), its entry points are named
/// `pith_*`, and the rest of the stack below main is libc and std.
fn first_compiled_frame(trace: &str) -> String {
    for line in trace.lines() {
        let line = line.trim();
        let Some((index, name)) = line.split_once(": ") else {
            continue;
        };
        if index.parse::<u32>().is_err() {
            continue;
        }
        let name = name.trim();
        let skip = name.contains("::")
            || name.starts_with('<')
            || name.starts_with("pith_")
            || name.starts_with("__rust")
            || name.starts_with("__libc")
            || name.starts_with("_start")
            || name == "<unknown>";
        if !skip {
            return name.to_string();
        }
    }
    "<unknown>".to_string()
}

extern "C" fn report() {
    let sites = SITES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(sites) = sites.as_ref() else {
        return;
    };
    let mut rows: Vec<(&(&str, String), &u64)> = sites.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let total: u64 = sites.values().sum();
    eprintln!(
        "pith allocation sites (about {total} allocations, one in {} sampled)",
        PERIOD.load(Ordering::Relaxed)
    );
    for ((kind, site), count) in rows.iter().take(40) {
        eprintln!("  {count:>12}  {kind:<11} {site}");
    }
}
