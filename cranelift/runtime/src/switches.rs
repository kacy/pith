//! The environment switches the runtime checks on its hot paths, read once
//! before main into a single byte. The count, allocation and release paths
//! load that byte and test bits on it, so with every switch at its default a
//! hooked function pays one relaxed load and a predicted branch, and calls no
//! probe.
//!
//! A clear bit is always the safe default: if the constructor never ran, every
//! count stays atomic and every diagnostic stays off.

use std::sync::atomic::{AtomicU8, Ordering};

/// `PITH_PERF_STATS`: count runtime operations and print them at exit.
pub const PERF_STATS: u8 = 1;
/// `PITH_ALLOC_SITES`: sample where the program allocates.
pub const ALLOC_SITES: u8 = 2;
/// `PITH_CYCLE_GC`: track suspects for the cycle collector.
pub const CYCLE_GC: u8 = 4;
/// Keep a struct only one task can reach on plain counts. On unless
/// `PITH_LOCAL_COUNTS=0`, and off with the cycle collector, which reaches
/// structs through its suspect buffer without a count of its own.
pub const LOCAL_COUNTS: u8 = 8;
/// `PITH_RC_CHECK`: abort when a plain count is updated from a task that did
/// not build the struct. Set only together with `LOCAL_COUNTS`.
pub const RC_CHECK: u8 = 16;
/// Any of the string debugging switches: `PITH_CSTRING_WATCH`,
/// `PITH_CSTRING_TRACE`, `PITH_CSTRING_NOFREE`, `PITH_SCRUB_LOG` and
/// `PITH_LEAK_SCAN`. Each keeps its own setting; this bit only tells the
/// string paths that one of them needs looking at.
pub const CSTRING_DEBUG: u8 = 32;
/// `PITH_QUARANTINE`: keep freed blocks poisoned and report a use after free
/// (see `quarantine.rs`).
pub const QUARANTINE: u8 = 64;

static SWITCHES: AtomicU8 = AtomicU8::new(0);

// runs before main, the way a C constructor would, so the first allocation
// already sees the settled byte
#[used]
#[cfg_attr(not(target_os = "macos"), link_section = ".init_array")]
#[cfg_attr(target_os = "macos", link_section = "__DATA,__mod_init_func")]
static SETTLE: extern "C" fn() = settle;

extern "C" fn settle() {
    let mut bits = from_env(|name| std::env::var(name).ok());
    if crate::alloc_sites::start() {
        bits |= ALLOC_SITES;
    }
    SWITCHES.store(bits, Ordering::Relaxed);
}

/// Every switch but `ALLOC_SITES`, whose census also has to be armed, read
/// through `var`.
fn from_env(var: impl Fn(&str) -> Option<String>) -> u8 {
    let is = |name: &str, values: &[&str]| var(name).is_some_and(|v| values.contains(&v.as_str()));
    let mut bits = 0;
    if is("PITH_PERF_STATS", &["1", "true", "yes"]) {
        bits |= PERF_STATS;
    }
    if is("PITH_CYCLE_GC", &["1", "on", "true"]) {
        bits |= CYCLE_GC;
    } else if !is("PITH_LOCAL_COUNTS", &["0", "off", "false"]) {
        bits |= LOCAL_COUNTS;
        if is("PITH_RC_CHECK", &["1", "on", "true"]) {
            bits |= RC_CHECK;
        }
    }
    let cstring_debug = [
        "PITH_CSTRING_WATCH",
        "PITH_CSTRING_TRACE",
        "PITH_CSTRING_NOFREE",
        "PITH_SCRUB_LOG",
        "PITH_LEAK_SCAN",
    ];
    if cstring_debug.iter().any(|name| var(name).is_some()) {
        bits |= CSTRING_DEBUG;
    }
    if crate::quarantine::enabled_by(var("PITH_QUARANTINE").as_deref()) {
        bits |= QUARANTINE;
    }
    bits
}

/// The settled switches. A function that checks several of them loads this
/// once and tests the bits on the copy.
#[inline(always)]
pub fn get() -> u8 {
    SWITCHES.load(Ordering::Relaxed)
}

/// Whether any of the switches in `bits` is on.
#[inline(always)]
pub fn any(bits: u8) -> bool {
    get() & bits != 0
}

/// Turn the cycle collector on or off for a test, with the count mode that
/// goes with it.
#[cfg(test)]
pub(crate) fn set_cycle_gc_for_tests(enabled: bool) {
    let rest = get() & !(CYCLE_GC | LOCAL_COUNTS | RC_CHECK);
    let mode = if enabled { CYCLE_GC } else { LOCAL_COUNTS };
    SWITCHES.store(rest | mode, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(vars: &[(&str, &str)]) -> u8 {
        from_env(|name| vars.iter().find(|(n, _)| *n == name).map(|(_, v)| v.to_string()))
    }

    #[test]
    fn the_defaults_keep_local_counts_and_nothing_else() {
        assert_eq!(bits(&[]), LOCAL_COUNTS);
    }

    #[test]
    fn the_cycle_collector_turns_local_counts_and_their_check_off() {
        assert_eq!(bits(&[("PITH_CYCLE_GC", "1"), ("PITH_RC_CHECK", "1")]), CYCLE_GC);
    }

    #[test]
    fn the_check_needs_local_counts() {
        assert_eq!(bits(&[("PITH_RC_CHECK", "on")]), LOCAL_COUNTS | RC_CHECK);
        assert_eq!(bits(&[("PITH_LOCAL_COUNTS", "0"), ("PITH_RC_CHECK", "on")]), 0);
    }

    #[test]
    fn any_string_debug_switch_sets_its_bit() {
        assert_eq!(bits(&[("PITH_LEAK_SCAN", "")]), LOCAL_COUNTS | CSTRING_DEBUG);
        assert_eq!(bits(&[("PITH_PERF_STATS", "yes")]), LOCAL_COUNTS | PERF_STATS);
        assert_eq!(bits(&[("PITH_PERF_STATS", "0")]), LOCAL_COUNTS);
    }

    #[test]
    fn quarantine_takes_its_own_bit() {
        assert_eq!(bits(&[("PITH_QUARANTINE", "1")]), LOCAL_COUNTS | QUARANTINE);
        assert_eq!(bits(&[("PITH_QUARANTINE", "trace")]), LOCAL_COUNTS | QUARANTINE);
        assert_eq!(bits(&[("PITH_QUARANTINE", "0")]), LOCAL_COUNTS);
    }
}
