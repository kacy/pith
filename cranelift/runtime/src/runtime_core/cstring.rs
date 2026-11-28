//! Refcounted 16-byte-header C strings: allocation, retain/release,
//! concatenation, comparison, and fast scalar-to-string formatters.

use super::{count_release, pith_alloc, pith_layout};
use crate::string;
use std::alloc::Layout;
use std::sync::atomic::{AtomicU32, Ordering};

pub(crate) fn pith_strdup_string(text: &str) -> *mut i8 {
    // the length is already known, so allocate the headered cstring directly.
    // routing through pith_strdup would probe 16 bytes in front of the rust
    // string's buffer looking for a header no rust allocation has: a read of
    // memory this process does not own whenever the buffer starts its heap
    // block. pith_alloc_cstring writes the nul terminator itself.
    pith_copy_bytes_to_cstring(text.as_bytes())
}

// --- refcounted c strings ---
//
// generated code passes strings around as bare `*const i8`, so ownership
// has to live behind the pointer. every heap cstring carries a 16-byte
// header in front of its data:
//
//   [magic: u32][refcount: u32, atomic][data_len: u64][bytes...][NUL]
//
// static literals (strref data baked into the binary) have no header. the
// magic + alignment check below turns retain/release into no-ops for them,
// so the compiler can emit retain/release for every string register without
// caring where the string came from.

pub const CSTRING_MAGIC: u32 = 0x50435352; // "PCSR"
/// The magic of the header the compiler writes in front of a string literal:
/// [magic: u32][0: u32][data_len: u64], laid out like a heap header so the
/// length sits at the same offset. A literal is never freed or counted, so it
/// carries a magic `cstring_base` rejects, and only `cstring_header_len`
/// accepts it. It differs from the heap magic in the low bit alone, which
/// lets the codegen test for either with one compare.
pub const CSTRING_STATIC_MAGIC: u32 = CSTRING_MAGIC | 1;
pub(super) const CSTRING_HEADER_SIZE: usize = 16;
pub(super) const CSTRING_ALIGN: usize = 8;

/// One shared empty string, laid out like a compiled literal so that every
/// header probe finds the literal magic and leaves it alone. Without a header
/// the probe read the 16 bytes in front of it, which belong to whatever static
/// the linker put there: harmless only while those bytes did not happen to
/// spell a heap magic, at which point a retain would have written into rodata.
pub(super) static CSTRING_EMPTY: StaticChar = StaticChar {
    magic: CSTRING_STATIC_MAGIC,
    zero: 0,
    len: 0,
    bytes: [0; 8],
};

/// The allocation size for a string of `data_len` bytes: the header, the
/// data and its NUL, rounded up to a size class. The size is a function of
/// the length alone, so a free recomputes it from the header, and an append
/// that stays inside the class writes in place (`pith_cstring_append_many`).
/// Up to 136 bytes the classes are 16k + 8, the usable sizes of glibc's
/// smallest chunks, so a short string wastes nothing the allocator would not
/// already round away. Past that there are four classes per power of two,
/// which wastes at most a quarter and keeps repeated appends amortized.
pub(super) fn cstring_alloc_size_checked(data_len: usize) -> Option<usize> {
    let total = CSTRING_HEADER_SIZE.checked_add(data_len)?.checked_add(1)?;
    let size = if total <= 136 {
        (total + 7) / 16 * 16 + 8
    } else {
        let top = usize::BITS - 1 - (total - 1).leading_zeros();
        let step = 1usize << (top - 2);
        total.checked_next_multiple_of(step)?
    };
    if size > (isize::MAX as usize) - (CSTRING_ALIGN - 1) {
        return None;
    }
    Some(size)
}

pub(crate) fn cstring_len_valid(data_len: usize) -> bool {
    cstring_alloc_size_checked(data_len).is_some()
}

pub(super) fn cstring_alloc_size(data_len: usize) -> usize {
    match cstring_alloc_size_checked(data_len) {
        Some(size) => size,
        None => {
            eprintln!("pith runtime error: invalid allocation layout");
            // panic-guard: requested string size exceeds the maximum valid allocation layout.
            std::process::exit(1);
        }
    }
}

fn cstring_layout(data_len: usize) -> Layout {
    pith_layout(cstring_alloc_size(data_len), CSTRING_ALIGN)
}

/// The length a heap cstring's header records.
#[inline(always)]
unsafe fn cstring_data_len(base: *const u8) -> usize {
    // SAFETY: Caller guarantees `base` points to a valid 16-byte cstring header.
    unsafe { (base.add(8) as *const u64).read() as usize }
}

/// Borrow a heap cstring's atomic refcount for the duration of `f`.
///
/// # Safety
/// `base` must point to a live 16-byte heap cstring header that outlives `f`.
#[inline(always)]
unsafe fn with_cstring_refcount<R>(base: *mut u8, f: impl FnOnce(&AtomicU32) -> R) -> R {
    // SAFETY: Caller guarantees `base` is an 8-aligned valid cstring header,
    // so `base + 4` is a 4-aligned `AtomicU32` refcount slot.
    let rc = unsafe { &*(base.add(4) as *const AtomicU32) };
    f(rc)
}

/// Borrow a bounded prefix of a heap cstring's bytes for diagnostic logging.
///
/// # Safety
/// `base` must be the live header corresponding to `s`.
#[inline(always)]
unsafe fn with_cstring_preview<R>(
    base: *const u8,
    s: *const i8,
    max_len: usize,
    f: impl FnOnce(&[u8]) -> R,
) -> R {
    // SAFETY: Caller guarantees `base` is the valid header for `s`.
    let data_len = unsafe { cstring_data_len(base) };
    // SAFETY: `s` has `data_len` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(s as *const u8, data_len.min(max_len)) };
    f(bytes)
}

pub(super) fn preview_lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[inline(always)]
fn acquire_last_u32(rc: &AtomicU32) {
    let _ = rc;
    acquire_last_count!(rc);
}

/// Debug: under PITH_CSTRING_NOFREE + PITH_LEAK_SCAN, every allocation is
/// remembered and the exit hook prints the ones whose count never reached
/// zero, which is the definitive leak list.
pub(super) fn leak_scan_registry() -> Option<&'static std::sync::Mutex<Vec<usize>>> {
    static REG: std::sync::OnceLock<Option<std::sync::Mutex<Vec<usize>>>> = std::sync::OnceLock::new();
    REG.get_or_init(|| {
        if std::env::var("PITH_LEAK_SCAN").is_ok() {
            Some(std::sync::Mutex::new(Vec::new()))
        } else {
            None
        }
    })
    .as_ref()
}

pub fn report_leaked_cstrings() {
    let Some(reg) = leak_scan_registry() else {
        return;
    };
    let ptrs = crate::sync::lock(&reg);
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for &p in ptrs.iter() {
        let s = p as *const i8;
        // SAFETY: `p` came from `alloc_cstring` under `PITH_CSTRING_NOFREE` + `PITH_LEAK_SCAN`.
        let Some(base) = (unsafe { cstring_base(s) }) else {
            continue;
        };
        // SAFETY: `base` was just validated and is never freed under leak scan.
        let rc = unsafe { with_cstring_refcount(base, |r| r.load(Ordering::Relaxed)) };
        if rc == 0 {
            continue;
        }
        // SAFETY: `base` is the validated header for `s`.
        let content = unsafe { with_cstring_preview(base, s, 40, preview_lossy) };
        *counts.entry(format!("rc={} {:?}", rc, content)).or_insert(0) += 1;
    }
    let mut rows: Vec<(usize, String)> = counts.into_iter().map(|(k, v)| (v, k)).collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    eprintln!("leaked cstrings ({} rows):", rows.len());
    for (n, k) in rows.iter().take(40) {
        eprintln!("  {} x {}", n, k);
    }
}

// the switches that add work to a string allocation, retain or release.
// with none of them on, those paths call nothing they do not need; the copies
// that count, sample and debug live out of line.
const CSTRING_HOOKS: u8 = crate::switches::PERF_STATS
    | crate::switches::ALLOC_SITES
    | crate::switches::CSTRING_DEBUG
    | crate::switches::QUARANTINE;

/// Allocate a heap cstring with `data_len` data bytes, refcount 1, and the
/// trailing NUL already written. The caller fills bytes 0..data_len.
pub(crate) unsafe fn pith_alloc_cstring(data_len: usize) -> *mut i8 {
    alloc_cstring_raw(data_len)
}

#[inline(always)]
fn alloc_cstring_raw(data_len: usize) -> *mut i8 {
    if crate::switches::any(CSTRING_HOOKS) {
        return alloc_cstring_hooked(data_len);
    }
    alloc_cstring::<false>(data_len)
}

#[cold]
#[inline(never)]
fn alloc_cstring_hooked(data_len: usize) -> *mut i8 {
    alloc_cstring::<true>(data_len)
}

#[inline(always)]
fn alloc_cstring<const HOOKED: bool>(data_len: usize) -> *mut i8 {
    if HOOKED {
        crate::alloc_sites::record("string");
        crate::perf_count(&crate::PERF_CSTRING_ALLOCS, 1);
    }
    let layout = cstring_layout(data_len);
    // SAFETY: `layout` has non-zero size `>= CSTRING_HEADER_SIZE + data_len + 1` and
    // alignment `CSTRING_ALIGN` (8); initialize header fields and trailing NUL.
    let data = unsafe {
        let base = pith_alloc(layout);
        (base as *mut u32).write(CSTRING_MAGIC);
        (base.add(4) as *mut u32).write(1);
        (base.add(8) as *mut u64).write(data_len as u64);
        let data = base.add(CSTRING_HEADER_SIZE) as *mut i8;
        *data.add(data_len) = 0;
        data
    };
    if !HOOKED {
        return data;
    }
    if let Some(reg) = leak_scan_registry() {
        crate::sync::lock(&reg)
            .push(data as usize);
    }
    data
}

/// Find the header of a heap cstring, or None for null pointers, static
/// literals, and anything else that never came from pith_alloc_cstring.
/// Heap cstring data is always 8-aligned, which rejects most literals
/// before the header word is ever read; an aligned literal gets one read
/// of the 16 bytes in front of it (mapped binary data) and fails the magic
/// compare. Same practical-guard contract as the collection magic words.
pub(crate) unsafe fn cstring_base(s: *const i8) -> Option<*mut u8> {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { cstring_base_for::<true>(s, crate::quarantine::Op::Use) }
}

/// `cstring_base`, naming what the caller does with the string in a
/// quarantine report. A count path that already knows quarantine is off
/// passes `REPORT = false` and skips the freed-magic test.
#[inline(always)]
unsafe fn cstring_base_for<const REPORT: bool>(s: *const i8, op: crate::quarantine::Op) -> Option<*mut u8> {
    let addr = s as usize;
    if addr < CSTRING_HEADER_SIZE || addr % CSTRING_ALIGN != 0 {
        return None;
    }
    // volatile, because for a literal this reads memory outside the literal.
    // once the compiler can see the pointer (a runtime literal, inlined into
    // a caller), a plain read lets it assume the magic matched and write a
    // count into read-only data.
    // SAFETY: `addr >= CSTRING_HEADER_SIZE` and 8-aligned; probe the header magic.
    let (base, magic) = unsafe {
        let base = (s as *const u8).sub(CSTRING_HEADER_SIZE) as *mut u8;
        let magic = std::ptr::read_volatile(base as *const u32);
        (base, magic)
    };
    if magic != CSTRING_MAGIC {
        if REPORT {
            // SAFETY: `base` is only used as a lookup key in quarantine reporting.
            unsafe { crate::quarantine::miss(magic, base as *const u32, op) };
        }
        return None;
    }
    Some(base)
}

/// Bump the refcount of a heap cstring. No-op for literals and null.
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_retain(s: *const i8) {
    if crate::switches::any(CSTRING_HOOKS) {
        // SAFETY: Preconditions forwarded from caller.
        return unsafe { cstring_retain_hooked(s) };
    }
    // SAFETY: `s` is validated by `cstring_base_for` before bumping its refcount.
    if let Some(base) = unsafe { cstring_base_for::<false>(s, crate::quarantine::Op::Retain) } {
        // SAFETY: `base` was just validated as a live heap cstring header.
        unsafe { with_cstring_refcount(base, |rc| rc.fetch_add(1, Ordering::Relaxed)) };
    }
}

#[cold]
#[inline(never)]
unsafe fn cstring_retain_hooked(s: *const i8) {
    // SAFETY: Preconditions forwarded from caller.
    if let Some(base) = unsafe { cstring_base_for::<true>(s, crate::quarantine::Op::Retain) } {
        crate::perf_count(&crate::PERF_CSTRING_RETAINS, 1);
        // SAFETY: `base` was just validated as a live heap cstring header.
        let prev = unsafe { with_cstring_refcount(base, |rc| rc.fetch_add(1, Ordering::Relaxed)) };
        // SAFETY: `base` is the validated header for `s` and remains live.
        unsafe { cstring_watch(base, s, "retain", prev as i64, 1) };
    }
}

fn cstring_watch_log(bytes: &[u8], needle: &str, s: *const i8, op: &str, prev: i64, delta: i64) {
    let content = String::from_utf8_lossy(bytes);
    // a trailing '*' makes the needle a prefix match
    let hit = match needle.strip_suffix('*') {
        Some(prefix) => content.starts_with(prefix),
        None => content == needle,
    };
    if hit {
        eprintln!("watch {:p} {} {} -> {}", s, op, prev, prev + delta);
    }
}

/// Debug: PITH_CSTRING_WATCH=<content> logs every rc operation on strings
/// with that exact content, with the count before the op.
#[inline(always)]
unsafe fn cstring_watch(base: *const u8, s: *const i8, op: &str, prev: i64, delta: i64) {
    let Some(needle) = debug_watch_needle() else {
        return;
    };
    // SAFETY: Caller guarantees `base` is the live header for `s`.
    unsafe { with_cstring_preview(base, s, 64, |b| cstring_watch_log(b, needle, s, op, prev, delta)) };
}

/// Drop one count; free the allocation when the last count dies. The magic
/// is scrubbed before the free so a stale pointer fails the header check
/// instead of double-freeing. No-op for literals and null.
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_release(s: *const i8) {
    if crate::switches::any(CSTRING_HOOKS) {
        // SAFETY: Preconditions forwarded from caller.
        return unsafe { cstring_release_hooked(s) };
    }
    // SAFETY: Preconditions forwarded from caller.
    unsafe { cstring_release::<false>(s) };
}

#[cold]
#[inline(never)]
unsafe fn cstring_release_hooked(s: *const i8) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { cstring_release::<true>(s) };
}

#[inline(always)]
unsafe fn cstring_release<const HOOKED: bool>(s: *const i8) {
    // SAFETY: `s` is validated by `cstring_base_for` before decrementing its refcount.
    let Some(base) = (unsafe { cstring_base_for::<HOOKED>(s, crate::quarantine::Op::Release) }) else {
        return;
    };
    if HOOKED {
        crate::perf_count(&crate::PERF_CSTRING_RELEASES, 1);
    }
    // SAFETY: `base` was just validated and the caller holds a count on it.
    let prev = unsafe { with_cstring_refcount(base, count_release) };
    if HOOKED {
        // SAFETY: `base` is still valid until deallocated below.
        unsafe { cstring_watch(base, s, "release", prev as i64, -1) };
    }
    if prev == 0 {
        // an over-release means the emitter's ownership accounting is
        // wrong somewhere; report it loudly in debug runs instead of
        // corrupting the heap.
        if HOOKED {
            crate::quarantine::over_release(crate::quarantine::Kind::String);
        }
        if HOOKED && cstring_debug_no_free() {
            // SAFETY: `base` is the validated header for `s` and is kept alive under no-free.
            let content = unsafe { with_cstring_preview(base, s, 60, preview_lossy) };
            eprintln!("pith debug: cstring over-release: {:?}", content);
        }
        // SAFETY: Restore zero refcount on over-release without freeing.
        unsafe { with_cstring_refcount(base, |rc| rc.store(0, Ordering::Relaxed)) };
        return;
    }
    if prev == 1 {
        // SAFETY: Last release; synchronize prior releases before freeing.
        unsafe { with_cstring_refcount(base, acquire_last_u32) };
        let needle = if HOOKED { debug_trace_needle() } else { None };
        if let Some(needle) = needle {
            // SAFETY: Sole owner; `base` is still live before free.
            let content = unsafe { with_cstring_preview(base, s, 40, preview_lossy) };
            if content == needle {
                eprintln!("pith trace: final release of {:?} at {:p}", content, s);
            }
        }
        // SAFETY: Sole owner; read recorded byte length from header.
        let data_len = unsafe { cstring_data_len(base) };
        if HOOKED && cstring_debug_no_free() {
            // scrub instead of freeing: a read-after-release shows up
            // as '!' runs in program output instead of silent reuse
            if debug_scrub_log() {
                // SAFETY: Sole owner; preview bytes before scrubbing.
                let content = unsafe { with_cstring_preview(base, s, 24, preview_lossy) };
                eprintln!("scrub {:p} len={} {:?}", s, data_len, content);
            }
            // SAFETY: Sole owner; `s` has `data_len` writable bytes.
            unsafe { std::ptr::write_bytes(s as *mut u8, b'!', data_len) };
            return;
        }
        if HOOKED {
            crate::perf_count(&crate::PERF_CSTRING_FREES, 1);
            let size = cstring_alloc_size(data_len);
            // SAFETY: Sole owner; `base..base+size` is writable and magic is at `base`.
            if unsafe { crate::quarantine::bury(base, size, base as *mut u32, crate::quarantine::Kind::String) } {
                return;
            }
        }
        let layout = cstring_layout(data_len);
        // SAFETY: Sole owner; scrub magic and deallocate with the layout used to allocate `base`.
        unsafe {
            (base as *mut u32).write(0);
            std::alloc::dealloc(base, layout);
        }
    }
}

/// `PITH_CSTRING_WATCH`: the content whose rc operations `cstring_watch` logs.
#[inline(always)]
fn debug_watch_needle() -> Option<&'static str> {
    static NEEDLE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    NEEDLE
        .get_or_init(|| std::env::var("PITH_CSTRING_WATCH").ok())
        .as_deref()
}

/// `PITH_CSTRING_TRACE`: the content whose final release is reported.
fn debug_trace_needle() -> Option<&'static str> {
    static NEEDLE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    NEEDLE
        .get_or_init(|| std::env::var("PITH_CSTRING_TRACE").ok())
        .as_deref()
}

fn debug_scrub_log() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var("PITH_SCRUB_LOG").is_ok())
}

/// `PITH_CSTRING_NOFREE`: keep freed cstrings alive (scrubbed) and report
/// over-releases, so an accounting bug in emitted code shows up as a
/// message, not corruption.
pub(super) fn cstring_debug_no_free() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var("PITH_CSTRING_NOFREE").is_ok())
}

/// Test-only view of a heap cstring's refcount (None for literals/null).
#[cfg(test)]
pub(crate) unsafe fn cstring_refcount_for_tests(s: *const i8) -> Option<u32> {
    // SAFETY: Preconditions forwarded from test caller.
    let base = unsafe { cstring_base(s) }?;
    // SAFETY: `base` was just validated as a live cstring header.
    Some(unsafe { with_cstring_refcount(base, |rc| rc.load(Ordering::Relaxed)) })
}

/// O(1) length from the header of a heap cstring or a compiled literal; None
/// for anything without one. The same practical guard as `cstring_base`.
pub(crate) unsafe fn cstring_header_len(s: *const i8) -> Option<i64> {
    let addr = s as usize;
    if addr < CSTRING_HEADER_SIZE || addr % CSTRING_ALIGN != 0 {
        return None;
    }
    // SAFETY: `addr >= CSTRING_HEADER_SIZE` and 8-aligned; probe the header magic.
    let (base, magic) = unsafe {
        let base = (s as *const u8).sub(CSTRING_HEADER_SIZE);
        let magic = (base as *const u32).read();
        (base, magic)
    };
    if magic != CSTRING_MAGIC && magic != CSTRING_STATIC_MAGIC {
        return None;
    }
    // SAFETY: Magic matched a valid heap or static cstring header.
    Some(unsafe { cstring_data_len(base) } as i64)
}

/// Under quarantine, report `s` if it is a freed heap string. A reader whose
/// header lookup failed calls this before it falls back to counting bytes,
/// which keeps the test off the header path every heap string takes.
#[cold]
#[inline(never)]
pub(crate) unsafe fn cstring_check_freed(s: *const i8) {
    if crate::quarantine::on() {
        // SAFETY: Preconditions forwarded from caller; reports freed heap strings under quarantine.
        unsafe { cstring_base(s) };
    }
}

pub(crate) fn pith_copy_bytes_to_cstring(bytes: &[u8]) -> *mut i8 {
    let ptr = alloc_cstring_raw(bytes.len());
    // SAFETY: `alloc_cstring_raw` returns a fresh writable buffer of at least `bytes.len() + 1` bytes
    // that does not alias `bytes`, with the trailing NUL already written.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len()) };
    ptr
}

pub(crate) fn pith_cstring_empty() -> *mut i8 {
    CSTRING_EMPTY.bytes.as_ptr() as *mut i8
}
#[no_mangle]
pub unsafe extern "C" fn pith_concat_cstr(a: *const i8, b: *const i8) -> *mut i8 {
    if a.is_null() {
        return if b.is_null() {
            std::ptr::null_mut()
        } else {
            // SAFETY: Preconditions forwarded from caller.
            unsafe { pith_strdup(b) }
        };
    }
    if b.is_null() {
        // SAFETY: Preconditions forwarded from caller.
        return unsafe { pith_strdup(a) };
    }

    let len_a = string::pith_cstring_len(a) as usize;
    let len_b = string::pith_cstring_len(b) as usize;
    let result = alloc_cstring_raw(len_a + len_b);
    // SAFETY: `a` and `b` are valid for `len_a` and `len_b` readable bytes, and
    // `result` is a fresh non-overlapping buffer of at least `len_a + len_b + 1` bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(a, result, len_a);
        std::ptr::copy_nonoverlapping(b, result.add(len_a), len_b);
    }
    result
}

/// The most strings one `pith_concat_cstr_many` call joins. The consumer
/// splits a longer chain into calls of at most this many.
pub const CONCAT_MANY_MAX: usize = 8;

/// The first argument of the two join calls holds the part count in its low
/// byte. Above that, bit `i` is set when part `i` is an Int to be written in
/// decimal instead of a string, which is how `"id=" + n.to_string()` joins
/// without first making a string for the number.
pub const JOIN_INT_SHIFT: u32 = 8;

/// Whether part `i` of a join is an Int, going by the call's Int bits.
#[inline(always)]
fn join_part_is_int(ints: u64, i: usize) -> bool {
    ints >> i & 1 != 0
}

/// Fill `lens` with the byte length of each part and return their sum. A
/// null string is empty. Inlined into both join calls so the parts stay in
/// the caller's array: handing them over in a struct made the copy read back
/// words that were still being stored.
#[inline(always)]
fn measure_join_parts(parts: &[*const i8], ints: u64, lens: &mut [usize; CONCAT_MANY_MAX]) -> usize {
    let mut total = 0usize;
    for (i, &part) in parts.iter().enumerate() {
        lens[i] = if join_part_is_int(ints, i) {
            int_text_len(part as i64)
        } else if part.is_null() {
            0
        } else {
            string::pith_cstring_len(part) as usize
        };
        total += lens[i];
    }
    total
}

/// Write every part, in order, into `dst`, whose length equals `sum(lens[..parts.len()])`.
#[inline(always)]
unsafe fn write_join_parts(
    parts: &[*const i8],
    ints: u64,
    lens: &[usize; CONCAT_MANY_MAX],
    mut dst: &mut [std::mem::MaybeUninit<u8>],
) {
    for (i, &part) in parts.iter().enumerate() {
        let len = lens[i];
        let Some((slice, rest)) = dst.split_at_mut_checked(len) else {
            break;
        };
        if join_part_is_int(ints, i) {
            write_int_text(part as i64, slice);
        } else if len > 0 {
            // SAFETY: Caller guarantees `part` has `len` readable bytes non-overlapping with `slice`.
            unsafe {
                std::ptr::copy_nonoverlapping(part as *const u8, slice.as_mut_ptr() as *mut u8, len)
            };
        }
        dst = rest;
    }
}

/// Split a join call's first argument into its part count, capped at `max`,
/// and its Int bits.
fn join_count_and_ints(packed: i64, max: usize) -> (usize, u64) {
    let count = ((packed & 0xff) as usize).min(max);
    let ints = (packed as u64 >> JOIN_INT_SHIFT) & ((1 << count) - 1);
    (count, ints)
}

/// Join the first `count` of up to eight parts into one fresh string, the
/// way a chain of `pith_concat_cstr` calls would, with one allocation and one
/// copy of each part instead of one per step. `count` is packed as
/// `JOIN_INT_SHIFT` describes. A null string part is empty; with every part a
/// null string the result is null, as `pith_concat_cstr(null, null)` is.
/// The consumer turns `a + b + c ...` into this.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn pith_concat_cstr_many(
    count: i64,
    s0: *const i8,
    s1: *const i8,
    s2: *const i8,
    s3: *const i8,
    s4: *const i8,
    s5: *const i8,
    s6: *const i8,
    s7: *const i8,
) -> *mut i8 {
    let (count, ints) = join_count_and_ints(count, CONCAT_MANY_MAX);
    let all = [s0, s1, s2, s3, s4, s5, s6, s7];
    let parts = &all[..count];
    if ints == 0 && parts.iter().all(|p| p.is_null()) {
        return std::ptr::null_mut();
    }
    let mut lens = [0usize; CONCAT_MANY_MAX];
    let total = measure_join_parts(parts, ints, &mut lens);
    let result = alloc_cstring_raw(total);
    // SAFETY: `result` points to `total` freshly allocated writable bytes.
    let dst = unsafe {
        std::slice::from_raw_parts_mut(result as *mut std::mem::MaybeUninit<u8>, total)
    };
    // SAFETY: Caller guarantees non-int, non-null parts are valid non-overlapping C strings.
    unsafe { write_join_parts(parts, ints, &lens, dst) };
    result
}

/// The most parts one `pith_cstring_append_many` call adds to its base.
pub const APPEND_MANY_MAX: usize = CONCAT_MANY_MAX - 1;

/// `base` followed by the first `count` of up to seven parts, for
/// `s = s + a + b ...` where `s` owns `base`. `count` is packed as
/// `JOIN_INT_SHIFT` describes, with bit 0 naming the first part after the
/// base. The call takes over the caller's count on `base`: the result
/// carries it, and `base` must not be released afterwards. When that count
/// is the only one, nothing else can see the string, so the parts are
/// written in place, reallocating only when the new length leaves the size
/// class (see `cstring_alloc_size`). A shared or literal base, or a part
/// that is the base itself, gets a fresh string and the base loses the
/// caller's count. A null part is empty.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn pith_cstring_append_many(
    count: i64,
    base: *const i8,
    s0: *const i8,
    s1: *const i8,
    s2: *const i8,
    s3: *const i8,
    s4: *const i8,
    s5: *const i8,
    s6: *const i8,
) -> *mut i8 {
    let (count, ints) = join_count_and_ints(count, APPEND_MANY_MAX);
    let all = [s0, s1, s2, s3, s4, s5, s6];
    let parts = &all[..count];
    let mut lens = [0usize; CONCAT_MANY_MAX];
    let added = measure_join_parts(parts, ints, &mut lens);
    // a string part that is the base itself would be read while it grows
    let reads_base = parts.iter().enumerate().any(|(i, &part)| part == base && !join_part_is_int(ints, i));
    // SAFETY: `base` is validated by `cstring_base` before checking sole ownership.
    let header = unsafe { cstring_base(base) }.filter(|&h| {
        // SAFETY: `h` was just validated as a live heap cstring header.
        let sole = unsafe { with_cstring_refcount(h, |rc| rc.load(Ordering::Acquire) == 1) };
        sole && !reads_base && !cstring_debug_no_free() && leak_scan_registry().is_none()
    });
    let Some(mut header) = header else {
        let packed = (count + 1) as i64 | (ints << (JOIN_INT_SHIFT + 1)) as i64;
        // SAFETY: Preconditions forwarded from caller.
        let fresh = unsafe { pith_concat_cstr_many(packed, base, s0, s1, s2, s3, s4, s5, s6) };
        // SAFETY: Caller transferred one refcount on `base` to this call.
        unsafe { pith_cstring_release(base) };
        return fresh;
    };
    // SAFETY: `header` is a validated sole-owned heap cstring header.
    let len = unsafe { cstring_data_len(header) };
    let Some(new_len) = len.checked_add(added) else {
        eprintln!("pith runtime error: invalid allocation layout");
        // panic-guard: concatenated string length overflows usize.
        std::process::exit(1);
    };
    let new_size = cstring_alloc_size(new_len);
    if new_size != cstring_alloc_size(len) {
        let old_layout = cstring_layout(len);
        // SAFETY: `header` is sole-owned and was allocated with `old_layout`.
        header = unsafe { std::alloc::realloc(header, old_layout, new_size) };
        if header.is_null() {
            eprintln!("pith runtime error: allocation failed");
            // panic-guard: out of memory.
            std::process::exit(1);
        }
    }
    // SAFETY: `header` is sole-owned with capacity for `CSTRING_HEADER_SIZE + new_len + 1` bytes.
    let (data, tail) = unsafe {
        let data = header.add(CSTRING_HEADER_SIZE) as *mut i8;
        let tail = std::slice::from_raw_parts_mut(
            (data as *mut std::mem::MaybeUninit<u8>).add(len),
            added,
        );
        (data, tail)
    };
    // SAFETY: `reads_base` is false, so no part aliases `tail`, and lengths match `added`.
    unsafe { write_join_parts(parts, ints, &lens, tail) };
    // SAFETY: `header` is sole-owned; write trailing NUL and updated byte length.
    unsafe {
        *data.add(new_len) = 0;
        (header.add(8) as *mut u64).write(new_len as u64);
    }
    data
}

#[no_mangle]
pub unsafe extern "C" fn pith_strdup(ptr: *const i8) -> *mut i8 {
    // SAFETY: Caller guarantees `ptr` is null or a valid NUL-terminated C string.
    unsafe { crate::ffi_util::with_cstr_bytes(ptr, pith_copy_bytes_to_cstring) }
        .unwrap_or(std::ptr::null_mut())
}
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_eq(a: *const i8, b: *const i8) -> i64 {
    if a.is_null() && b.is_null() {
        return 1;
    }
    if a.is_null() || b.is_null() {
        return 0;
    }
    if std::ptr::eq(a, b) {
        return 1;
    }
    // most unequal pairs differ in the first byte, which settles it before
    // either length is taken. an empty string's first byte is its zero
    // terminator, so this holds for empty strings too.
    // SAFETY: Both pointers are non-null valid NUL-terminated C strings, so byte 0 is readable.
    if unsafe { *a != *b } {
        return 0;
    }
    // compare by length, then bytes. a heap string's length is one header
    // read, and a string holding a zero byte compares in full rather than
    // stopping at the zero.
    // SAFETY: Both pointers are non-null valid NUL-terminated C strings.
    unsafe { crate::ffi_util::with_cstr_bytes2(a, b, |sa, sb| (sa == sb) as i64) }.unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn pith_ord_cstr(s: *const i8) -> i64 {
    if s.is_null() {
        return 0;
    }
    // SAFETY: Caller guarantees non-null `s` points to a readable NUL-terminated C string.
    let first = unsafe { *s };
    if first == 0 {
        return 0;
    }
    // a byte, 0 to 255, as `ord(s[i])` reads it (pith_cstring_byte_at).
    // widening the signed `i8` turned bytes from 128 up negative, so the same
    // byte read through a variable and through an index compared unequal.
    first as u8 as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_chr_cstr(n: i64) -> *mut i8 {
    // the string is static and never written: retain and release leave a
    // string with the literal magic alone.
    STATIC_CHARS[(n as u8) as usize].bytes.as_ptr() as *mut i8
}

/// A one-byte string laid out like a compiled literal: the header
/// `CSTRING_STATIC_MAGIC` marks, then the byte and its NUL.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub(super) struct StaticChar {
    magic: u32,
    zero: u32,
    len: u64,
    bytes: [u8; 8],
}

/// One static string per byte value, so `chr` hands out a string instead of
/// allocating one. http code builds CR, LF and quotes with it per request.
pub(super) static STATIC_CHARS: [StaticChar; 256] = {
    let mut table = [StaticChar {
        magic: CSTRING_STATIC_MAGIC,
        zero: 0,
        len: 1,
        bytes: [0; 8],
    }; 256];
    let mut i = 0;
    while i < 256 {
        table[i].bytes[0] = i as u8;
        i += 1;
    }
    table
};
fn compare_bytes(sa: &[u8], sb: &[u8]) -> i64 {
    match sa.cmp(sb) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_compare(a: *const i8, b: *const i8) -> i64 {
    if a.is_null() && b.is_null() {
        return 0;
    }
    if a.is_null() {
        return -1;
    }
    if b.is_null() {
        return 1;
    }
    // compare the full bytes so a zero byte inside a string does not end
    // the compare; a string that is a prefix of the other sorts first
    // SAFETY: Both pointers are non-null valid NUL-terminated C strings.
    unsafe { crate::ffi_util::with_cstr_bytes2(a, b, compare_bytes) }.unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_lt(a: *const i8, b: *const i8) -> i64 {
    // SAFETY: Preconditions forwarded from caller.
    if unsafe { pith_cstring_compare(a, b) } < 0 {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_gt(a: *const i8, b: *const i8) -> i64 {
    // SAFETY: Preconditions forwarded from caller.
    if unsafe { pith_cstring_compare(a, b) } > 0 {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_lte(a: *const i8, b: *const i8) -> i64 {
    // SAFETY: Preconditions forwarded from caller.
    if unsafe { pith_cstring_compare(a, b) } <= 0 {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_gte(a: *const i8, b: *const i8) -> i64 {
    // SAFETY: Preconditions forwarded from caller.
    if unsafe { pith_cstring_compare(a, b) } >= 0 {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_int_to_cstr(n: i64) -> *mut i8 {
    let len = int_text_len(n);
    let ptr = alloc_cstring_raw(len);
    // SAFETY: `ptr` points to `len` freshly allocated writable bytes.
    let dst = unsafe {
        std::slice::from_raw_parts_mut(ptr as *mut std::mem::MaybeUninit<u8>, len)
    };
    write_int_text(n, dst);
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn pith_uint_to_cstr(n: i64) -> *mut i8 {
    let len = uint_text_len(n as u64);
    let ptr = alloc_cstring_raw(len);
    // SAFETY: `ptr` points to `len` freshly allocated writable bytes.
    let dst = unsafe {
        std::slice::from_raw_parts_mut(ptr as *mut std::mem::MaybeUninit<u8>, len)
    };
    write_decimal_into(n as u64, dst);
    ptr
}

/// The number of digits `n` has in decimal.
#[inline]
fn uint_text_len(n: u64) -> usize {
    n.checked_ilog10().map_or(1, |d| d as usize + 1)
}

/// The number of bytes `n` takes in decimal, sign included.
#[inline]
pub(super) fn int_text_len(n: i64) -> usize {
    uint_text_len(n.unsigned_abs()) + usize::from(n < 0)
}

/// Write `n` in decimal into `dst`, whose length is `int_text_len(n)`.
#[inline(always)]
fn write_int_text(n: i64, dst: &mut [std::mem::MaybeUninit<u8>]) {
    write_decimal_into(n.unsigned_abs(), dst);
    if n < 0 {
        if let Some(first) = dst.first_mut() {
            first.write(b'-');
        }
    }
}

/// "00", "01" ... "99", end to end.
const DIGIT_PAIRS: [[u8; 2]; 100] = {
    let mut pairs = [[0u8; 2]; 100];
    let mut i = 0;
    while i < 100 {
        pairs[i] = [b'0' + (i / 10) as u8, b'0' + (i % 10) as u8];
        i += 1;
    }
    pairs
};

/// Write the digits of `n` right-aligned into `dst`. The caller measured the
/// number, so the digits go straight to their place: a number's text costs one
/// allocation and no copy. Two digits per step halve the chain of divisions
/// each digit waits on, which is what a conversion spends most of its time in.
#[inline(always)]
fn write_decimal_into(mut n: u64, mut dst: &mut [std::mem::MaybeUninit<u8>]) {
    while n >= 100 {
        let Some((rest, tail)) = dst.split_last_chunk_mut::<2>() else {
            return;
        };
        let pair = DIGIT_PAIRS[(n % 100) as usize];
        n /= 100;
        tail[0].write(pair[0]);
        tail[1].write(pair[1]);
        dst = rest;
    }
    if n >= 10 {
        if let Some((_, tail)) = dst.split_last_chunk_mut::<2>() {
            let pair = DIGIT_PAIRS[n as usize];
            tail[0].write(pair[0]);
            tail[1].write(pair[1]);
        }
    } else if let Some(last) = dst.last_mut() {
        last.write(b'0' + n as u8);
    }
}

pub(super) fn format_float(n: f64) -> String {
    if n == n.floor() && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        let formatted = format!("{:.6}", n);
        formatted
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

#[no_mangle]
pub extern "C" fn pith_float_to_cstr(n: f64) -> *mut i8 {
    pith_copy_bytes_to_cstring(format_float(n).as_bytes())
}

#[no_mangle]
pub extern "C" fn pith_bool_to_cstr(b: i64) -> *mut i8 {
    let s = if b != 0 { "true" } else { "false" };
    pith_copy_bytes_to_cstring(s.as_bytes())
}
