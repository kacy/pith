//! `PITH_QUARANTINE=1`: keep every freed heap block allocated, poisoned and
//! marked, so a use after free is reported instead of reading stale or reused
//! memory.
//!
//! A sanitizer only sees the reads the runtime makes, because generated code
//! is not instrumented. A field load, an inlined list read or a closure call
//! in generated code reads freed memory silently, and with the struct and
//! closure pools on it usually reads a block that already belongs to another
//! value. Under quarantine the last release of a struct, list, map, set,
//! bytes, byte buffer, closure, string or channel does not hand the block
//! back: it drops what the value owns, fills the block with `POISON`, writes
//! `FREED_MAGIC` plus the kind into the magic word, and leaks the block. No
//! block is reused, so the pools stay empty.
//!
//! Every runtime check of a magic word reports a freed one (see `miss`), and
//! a compiler that sees the same variable at build time routes generated
//! code's inline reads through checked paths (see `pith_quarantine_check_struct`
//! and the codegen's `quarantine_build`). The report names the kind and the
//! operation and exits with status 1. `PITH_QUARANTINE=trace` also keeps a
//! backtrace of each free and prints it, with the backtrace of the bad use.
//!
//! With the switch off, a free pays one test of the switch byte and a magic
//! mismatch pays one more; nothing else changes.

/// The magic word of a quarantined block, with the kind in the low byte. No
/// live magic shares the upper three bytes.
pub const FREED_MAGIC: u32 = 0xDEAD_5100;
const FREED_MASK: u32 = 0xFFFF_FF00;
/// What a quarantined block is filled with. A pointer read from one is
/// non-canonical, so following it faults instead of reaching live memory.
pub const POISON: u8 = 0xDE;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Kind {
    Struct = 1,
    List = 2,
    Map = 3,
    Set = 4,
    Bytes = 5,
    ByteBuffer = 6,
    Closure = 7,
    String = 8,
    Channel = 9,
}

impl Kind {
    fn from_code(code: u8) -> Option<Kind> {
        Some(match code {
            1 => Kind::Struct,
            2 => Kind::List,
            3 => Kind::Map,
            4 => Kind::Set,
            5 => Kind::Bytes,
            6 => Kind::ByteBuffer,
            7 => Kind::Closure,
            8 => Kind::String,
            9 => Kind::Channel,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Struct => "struct",
            Kind::List => "list",
            Kind::Map => "map",
            Kind::Set => "set",
            Kind::Bytes => "bytes",
            Kind::ByteBuffer => "byte buffer",
            Kind::Closure => "closure",
            Kind::String => "string",
            Kind::Channel => "channel",
        }
    }
}

/// What the caller was doing with the handle it validated.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Use,
    Retain,
    Release,
}

/// Whether `value`, the setting of `PITH_QUARANTINE`, turns the mode on. The
/// compiler reads the same variable when it builds, through this function.
pub fn enabled_by(value: Option<&str>) -> bool {
    matches!(value, Some("1" | "on" | "true" | "yes" | "trace"))
}

#[inline(always)]
pub fn on() -> bool {
    crate::switches::any(crate::switches::QUARANTINE)
}

/// Quarantine a block instead of freeing it: `len` bytes from `block` are
/// poisoned and `magic` gets the freed magic. False, with nothing done, when
/// the mode is off, so a free path reads
/// `if !quarantine::bury(..) { dealloc(..) }`.
///
/// # Safety
/// `block..block+len` must be writable and `magic` must lie inside it.
#[inline(always)]
pub unsafe fn bury(block: *mut u8, len: usize, magic: *mut u32, kind: Kind) -> bool {
    if !on() {
        return false;
    }
    bury_now(block, len, magic, kind);
    true
}

/// `bury` for a boxed value: drop what it owns in place, then quarantine
/// the box's memory. The box is never freed.
///
/// # Safety
/// `ptr` must come from `Box::into_raw` and not be used as a box again;
/// `magic` must point into it.
#[inline(always)]
pub unsafe fn bury_box<T>(ptr: *mut T, magic: *mut u32, kind: Kind) -> bool {
    if !on() {
        return false;
    }
    std::ptr::drop_in_place(ptr);
    bury_now(ptr as *mut u8, std::mem::size_of::<T>(), magic, kind);
    true
}

/// `bury` without the switch test, for a caller that has already made it.
///
/// # Safety
/// As for `bury`.
#[cold]
#[inline(never)]
pub(crate) unsafe fn bury_now(block: *mut u8, len: usize, magic: *mut u32, kind: Kind) {
    std::ptr::write_bytes(block, POISON, len);
    std::ptr::write_volatile(magic, FREED_MAGIC | kind as u32);
    if let Some(sites) = free_sites() {
        let trace = std::backtrace::Backtrace::force_capture();
        lock(sites).insert(magic as usize, trace);
    }
}

/// A validator's magic compare failed on the word `word`, read at `at`. A
/// freed magic there is reported; anything else is left to the caller, which
/// treats the handle as invalid as it always has. The word already in hand
/// is tested first, so a miss reads the switch only when the word looks
/// freed, and a live value whose bytes happen to match is not reported
/// with the mode off.
///
/// # Safety
/// `at` is only used to look up the free site, never read.
#[inline(always)]
pub unsafe fn miss(word: u32, at: *const u32, op: Op) {
    if word & FREED_MASK == FREED_MAGIC && on() {
        // without the hint the compiler tests for a freed magic before the
        // caller's own magic compare, which costs every valid handle
        std::hint::cold_path();
        miss_now(word, at, op);
    }
}

/// Whether the magic word at `at` is `want`, sending a mismatch to `miss`.
/// The validators of the boxed kinds are built on this. A boxed handle
/// rarely fails its check, so the whole miss is out of line and a valid
/// handle's path is the compare alone.
///
/// # Safety
/// `at` must be readable.
#[inline(always)]
pub unsafe fn magic_is(at: *const u32, want: u32, op: Op) -> bool {
    if at.read() == want {
        return true;
    }
    miss_out_of_line(at, op);
    false
}

#[cold]
#[inline(never)]
fn miss_out_of_line(at: *const u32, op: Op) {
    unsafe { miss(at.read(), at, op) }
}

/// The kind a quarantined block's magic word names, or None for any other
/// word.
fn freed_kind(word: u32) -> Option<Kind> {
    if word & FREED_MASK != FREED_MAGIC {
        return None;
    }
    Kind::from_code(word as u8)
}

#[cold]
#[inline(never)]
fn miss_now(word: u32, at: *const u32, op: Op) {
    let Some(kind) = freed_kind(word) else {
        return;
    };
    let what = match op {
        Op::Use => format!("use after free of {}", kind.name()),
        Op::Retain => format!("retain of a freed {} (a dangling reference)", kind.name()),
        Op::Release => format!("release of a freed {} (a double release)", kind.name()),
    };
    report(&what, Some(at as usize));
}

/// A release found the count already at zero, which every kind used to
/// answer by restoring the count and returning. Under quarantine it is
/// reported.
#[inline(always)]
pub fn over_release(kind: Kind) {
    if on() {
        report(
            &format!("release of a {} whose count is already zero (an over-release)", kind.name()),
            None,
        );
    }
}

/// Report a bad use of a struct whose value has died: its last strong count
/// is gone and its fields were released, though a weak reference still holds
/// the header.
#[cold]
#[inline(never)]
pub(crate) fn dead_struct(op: Op) -> ! {
    let what = match op {
        Op::Use => "use after free of struct (its value died; only a weak reference holds it)",
        Op::Retain => "retain of a struct whose value died (a dangling reference)",
        Op::Release => "release of a struct whose value died (a double release)",
    };
    report(what, None)
}

#[cold]
#[inline(never)]
fn report(what: &str, at: Option<usize>) -> ! {
    eprintln!("pith quarantine: {}", what);
    if trace_enabled() {
        if let Some(trace) = at.and_then(|at| free_sites().and_then(|s| lock(s).remove(&at))) {
            eprintln!("freed at:\n{}", trace);
        }
        eprintln!("used at:\n{}", std::backtrace::Backtrace::force_capture());
    }
    // panic-guard: a use after free has been detected; the program's memory can no longer be trusted.
    std::process::exit(1);
}

/// Called by generated code built under quarantine before it reads or writes
/// a struct field, when the header in front of `ptr` is not a live struct's:
/// the magic is not `STRUCT_MAGIC`, or the dead bit is set. A freed or dead
/// struct is reported; any other pointer is left alone.
///
/// # Safety
/// `ptr` must be a pointer generated code is about to read a field through.
#[no_mangle]
pub unsafe extern "C" fn pith_quarantine_check_struct(ptr: i64) {
    crate::runtime_core::quarantine_check_struct(ptr);
}

fn trace_enabled() -> bool {
    static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRACE.get_or_init(|| std::env::var("PITH_QUARANTINE").as_deref() == Ok("trace"))
}

type Sites = std::sync::Mutex<std::collections::HashMap<usize, std::backtrace::Backtrace>>;

fn free_sites() -> Option<&'static Sites> {
    static SITES: std::sync::OnceLock<Sites> = std::sync::OnceLock::new();
    if !trace_enabled() {
        return None;
    }
    Some(SITES.get_or_init(Default::default))
}

fn lock(sites: &Sites) -> std::sync::MutexGuard<'_, std::collections::HashMap<usize, std::backtrace::Backtrace>> {
    sites.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_reads_the_usual_on_spellings_and_trace() {
        for on in ["1", "on", "true", "yes", "trace"] {
            assert!(enabled_by(Some(on)), "{on}");
        }
        for off in ["0", "off", "", "2"] {
            assert!(!enabled_by(Some(off)), "{off}");
        }
        assert!(!enabled_by(None));
    }

    #[test]
    fn a_buried_block_is_poisoned_and_names_its_kind() {
        let mut block = [0u64; 4];
        let base = block.as_mut_ptr() as *mut u8;
        unsafe { bury_now(base, 32, base.add(8) as *mut u32, Kind::Map) };
        let bytes = unsafe { std::slice::from_raw_parts(base, 32) };
        assert!(bytes[..8].iter().chain(&bytes[12..]).all(|&b| b == POISON));
        let word = unsafe { (base.add(8) as *const u32).read() };
        assert_eq!(freed_kind(word), Some(Kind::Map));
    }

    #[test]
    fn only_a_freed_magic_names_a_kind() {
        for kind in [Kind::Struct, Kind::String, Kind::Channel, Kind::ByteBuffer] {
            assert_eq!(freed_kind(FREED_MAGIC | kind as u32), Some(kind));
        }
        let live = [
            crate::runtime_core::STRUCT_MAGIC,
            crate::runtime_core::CSTRING_MAGIC,
            crate::runtime_core::CSTRING_STATIC_MAGIC,
            crate::runtime_core::CLOSURE_MAGIC,
            crate::collections::list::LIST_MAGIC,
            crate::bytes::BYTES_MAGIC,
            0,
            u32::from_ne_bytes([POISON; 4]),
            FREED_MAGIC,
            FREED_MAGIC | 0xff,
        ];
        for word in live {
            assert_eq!(freed_kind(word), None, "{word:#x}");
        }
    }
}
