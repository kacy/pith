use crate::string;
use std::alloc::{alloc, Layout};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicU64, AtomicUsize, Ordering};

pub(crate) fn pith_strdup_string(text: &str) -> *mut i8 {
    // the length is already known, so allocate the headered cstring directly.
    // routing through pith_strdup would probe 16 bytes in front of the rust
    // string's buffer looking for a header no rust allocation has: a read of
    // memory this process does not own whenever the buffer starts its heap
    // block. pith_alloc_cstring writes the nul terminator itself.
    pith_copy_bytes_to_cstring(text.as_bytes())
}

#[no_mangle]
pub unsafe extern "C" fn pith_runtime_error(code: i64) -> i64 {
    let message = match code {
        1 => "division by zero",
        2 => "integer division overflow",
        3 => "allocation failed",
        4 => "invalid allocation layout",
        5 => "call to a function the compiler could not resolve — a compiler bug, not a problem in your program",
        6 => "value() on none",
        7 => "integer overflow",
        _ => "runtime error",
    };
    // a failed allocation and a compiler bug stop the program wherever they
    // happen. the rest are program errors, which an isolated call (an http
    // handler, a grpc method) survives: see `failure`.
    if matches!(code, 3..=5) {
        eprintln!("pith runtime error: {message}");
        // panic-guard: the compiler's runtime-error trap for conditions no caller can recover from.
        std::process::exit(1);
    }
    crate::failure::fail_static(message)
}

pub(crate) fn pith_layout(size: usize, align: usize) -> Layout {
    match Layout::from_size_align(size, align) {
        Ok(layout) => layout,
        Err(_) => {
            eprintln!("pith runtime error: invalid allocation layout");
            // panic-guard: an invalid allocation layout is a compiler bug with no way to continue.
            std::process::exit(1);
        }
    }
}

pub(crate) unsafe fn pith_alloc(layout: Layout) -> *mut u8 {
    // SAFETY: Caller guarantees `layout` has non-zero size.
    let ptr = unsafe { alloc(layout) };
    if ptr.is_null() {
        eprintln!("pith runtime error: allocation failed");
        // panic-guard: out of memory.
        std::process::exit(1);
    }
    ptr
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
const CSTRING_HEADER_SIZE: usize = 16;
const CSTRING_ALIGN: usize = 8;

/// One shared empty string, laid out like a compiled literal so that every
/// header probe finds the literal magic and leaves it alone. Without a header
/// the probe read the 16 bytes in front of it, which belong to whatever static
/// the linker put there: harmless only while those bytes did not happen to
/// spell a heap magic, at which point a retain would have written into rodata.
static CSTRING_EMPTY: StaticChar = StaticChar {
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
fn cstring_alloc_size_checked(data_len: usize) -> Option<usize> {
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

fn cstring_alloc_size(data_len: usize) -> usize {
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

fn preview_lossy(bytes: &[u8]) -> String {
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
fn leak_scan_registry() -> Option<&'static std::sync::Mutex<Vec<usize>>> {
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

/// Drop one count and return the count before, like `fetch_sub(1, Release)`.
/// A count of one belongs to the caller alone: no other owner is left to
/// retain or release concurrently, and the acquire load orders every earlier
/// owner's release before it. So the last release, the common case for a
/// temporary, is a load and a plain store instead of a locked
/// read-modify-write.
#[inline(always)]
pub(crate) fn count_release(count: &AtomicU32) -> u32 {
    use std::sync::atomic::Ordering;
    if count.load(Ordering::Acquire) == 1 {
        count.store(0, Ordering::Relaxed);
        return 1;
    }
    count.fetch_sub(1, Ordering::Release)
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
fn cstring_debug_no_free() -> bool {
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

pub const PITH_CLOSURE_ENV_SLOTS: usize = 16;

// release-kind tags for captured environment slots, so the closure can
// drop the counts it took on its captures when it is itself freed. 0
// means the slot holds a plain value (int/bool/float) with nothing to
// release. the numbers match the emitter's rc kinds.
const CLOSURE_TAG_STRING: u8 = 1;
const CLOSURE_TAG_LIST: u8 = 2;
const CLOSURE_TAG_MAP: u8 = 3;
const CLOSURE_TAG_SET: u8 = 4;
const CLOSURE_TAG_BYTES: u8 = 5;
const CLOSURE_TAG_STRUCT: u8 = 6;
const CLOSURE_TAG_CLOSURE: u8 = 7;
// a captured `weak` binding: the slot holds the bare target pointer and
// the closure took a WEAK reference, never a strong count. that is what
// lets a closure refer back to the state that owns it without forming a
// strong cycle. dropping it releases only the weak count.
const CLOSURE_TAG_WEAK: u8 = 8;
// a captured channel handle: the closure holds one of the channel's owner
// counts, released like any other captured heap value.
const CLOSURE_TAG_CHANNEL: u8 = 9;
// a captured Mutex, WaitGroup, Semaphore or AtomicInt handle: one owner
// count, like a channel's.
const CLOSURE_TAG_SYNC: u8 = 10;

// closures validate the same way strings and structs do: a magic word
// at the front of the allocation, read without a lock. `#[repr(C)]`
// pins `magic` to offset 0 so a stale or garbage handle is rejected by
// reading the first four bytes rather than by consulting a global set.
pub const CLOSURE_MAGIC: u32 = 0x50434c53; // "PCLS"

// hard cap on capture slot indices, inline block included. codegen
// bounds real closures far below this (a few hundred captures at the
// pathological extreme), so a slot at or past the cap can only come
// from a corrupted or hostile caller and is ignored exactly like a
// negative slot, the defensive posture every slot had before the
// spill existed. without the cap, `set_env` with an absurd slot
// (say 1<<40) attempted a multi-terabyte vec resize and aborted.
const PITH_CLOSURE_MAX_ENV_SLOTS: usize = PITH_CLOSURE_ENV_SLOTS + 4096;

/// One captured value together with its release-kind tag. Storing the
/// pair as a unit is what keeps the spill's values and tags from ever
/// falling out of lockstep: there is no second vec to forget to grow.
#[derive(Clone, Copy)]
struct Capture {
    value: i64,
    tag: u8,
}

/// Captures beyond the inline slots. Closures capturing more than
/// PITH_CLOSURE_ENV_SLOTS values are rare, so the common case pays
/// nothing: the option stays None and every read of an inline slot
/// never touches this. Allocated on the first spill store and grown to
/// the highest slot written. Before this existed, the 17th capture was
/// silently DROPPED: `set_env` bounds-checked and returned, `get_env`
/// answered 0, and the program computed a wrong answer with no
/// diagnostic anywhere. The VALUES here are released by the same paths
/// that release the inline slots (pith_closure_release /
/// cycle_closure_release_env); dropping this frees only the storage.
struct ClosureOverflow {
    captures: Vec<Capture>,
}

/// Where a capture slot lives: the inline array or the heap spill,
/// with the index already rebased into that storage. The 16-slot
/// boundary test, the rebase past it, and the upper cap exist only in
/// `classify`; every accessor matches on the result instead of
/// re-deriving the arithmetic.
enum EnvSlot {
    Inline(usize),
    Spill(usize),
}

impl EnvSlot {
    /// `None` for a slot that is negative or absurdly large; callers
    /// ignore those stores and answer 0 for those loads, exactly as
    /// every out-of-range slot behaved before the spill existed.
    fn classify(slot: i64) -> Option<EnvSlot> {
        if slot < 0 {
            return None;
        }
        let slot = slot as usize;
        if slot < PITH_CLOSURE_ENV_SLOTS {
            Some(EnvSlot::Inline(slot))
        } else if slot < PITH_CLOSURE_MAX_ENV_SLOTS {
            Some(EnvSlot::Spill(slot - PITH_CLOSURE_ENV_SLOTS))
        } else {
            None
        }
    }
}

/// Where codegen's inline read of a capture finds the magic word and the
/// inline env slots, and the alignment `closure_base` requires of a handle.
pub const CLOSURE_MAGIC_OFFSET: i32 = std::mem::offset_of!(PithClosure, magic) as i32;
pub const CLOSURE_ENV_OFFSET: i32 = std::mem::offset_of!(PithClosure, env) as i32;
pub const CLOSURE_HANDLE_ALIGN: i64 = 8;
/// The layout codegen writes when it builds a closure in a stack slot of its
/// own frame (the ir's `closure_stack`). The env is left as it is: the closure
/// reads only the slots its construction writes. `overflow` is None as a null
/// pointer, which Rust guarantees for an `Option<Box<_>>`.
pub const CLOSURE_SIZE: u32 = std::mem::size_of::<PithClosure>() as u32;
pub const CLOSURE_FUNC_PTR_OFFSET: i32 = std::mem::offset_of!(PithClosure, func_ptr) as i32;
pub const CLOSURE_REF_COUNT_OFFSET: i32 = std::mem::offset_of!(PithClosure, ref_count) as i32;
pub const CLOSURE_ENV_TAGS_OFFSET: i32 = std::mem::offset_of!(PithClosure, env_tags) as i32;
pub const CLOSURE_CYCLE_FLAGS_OFFSET: i32 = std::mem::offset_of!(PithClosure, cycle_flags) as i32;
pub const CLOSURE_OVERFLOW_OFFSET: i32 = std::mem::offset_of!(PithClosure, overflow) as i32;
const _: () = assert!(PITH_CLOSURE_ENV_SLOTS == 16);

/// The bit in `cycle_flags` that says the closure sits in the cycle
/// collector's suspect buffer.
const CLOSURE_BUFFERED_FLAG: u8 = 1;

#[repr(C)]
struct PithClosure {
    magic: u32,
    func_ptr: i64,
    ref_count: std::sync::atomic::AtomicI64,
    env: [i64; PITH_CLOSURE_ENV_SLOTS],
    env_tags: [u8; PITH_CLOSURE_ENV_SLOTS],
    // bit 0: this closure sits in the cycle collector's suspect buffer.
    // bit 7: shared (`share::CONTAINER_SHARED_FLAG`). codegen zeroes this
    // byte when it builds a closure on the stack, through
    // CLOSURE_CYCLE_FLAGS_OFFSET.
    cycle_flags: std::sync::atomic::AtomicU8,
    // spill storage for capture slots >= PITH_CLOSURE_ENV_SLOTS; None for
    // the overwhelmingly common closure that fits inline. the stakes here
    // are HIGHER than the inline env's stale-read argument: a set_env past
    // the inline block can grow the vec and move its buffer, so a racing
    // get_env would read through a freed allocation, not merely observe an
    // old value. it is still safe, for a structural reason rather than an
    // atomicity one: codegen emits every set_env during construction,
    // straight after pith_closure_new and before the handle escapes to any
    // other task, so no concurrent reader can exist while the vec may
    // still move; after construction the spill is read-only like the
    // inline env. the growth path debug_asserts the refcount is still the
    // single count from pith_closure_new, which catches a set_env after
    // the closure escaped. codegen writes None here (a null pointer)
    // through CLOSURE_OVERFLOW_OFFSET when it builds a stack closure.
    overflow: Option<Box<ClosureOverflow>>,
}

impl PithClosure {
    /// The spill capture for `idx` (already rebased past the inline
    /// slots), growing the overflow storage to reach it. Construction
    /// time only; see the contract on the `overflow` field.
    fn spill_capture(&mut self, idx: usize) -> &mut Capture {
        // growing the vec can move its buffer, which is only safe while
        // no other task can reach this closure; the single count from
        // pith_closure_new is a cheap proxy for "has not escaped yet".
        debug_assert_eq!(
            self.ref_count.load(Ordering::Relaxed),
            1,
            "closure spill grew after construction"
        );
        let overflow = self
            .overflow
            .get_or_insert_with(|| Box::new(ClosureOverflow { captures: Vec::new() }));
        if overflow.captures.len() <= idx {
            overflow.captures.resize(idx + 1, Capture { value: 0, tag: 0 });
        }
        &mut overflow.captures[idx]
    }

    /// Every capture in walk order (inline slots 0..PITH_CLOSURE_ENV_SLOTS
    /// first, then the spill in index order) as (value, tag) pairs. the
    /// three capture walkers (the release walk, the cycle child scan, and
    /// the cycle env release) all iterate through here, so none of them
    /// can disagree about the order or forget the spill.
    fn captures(&self) -> impl Iterator<Item = (i64, u8)> + '_ {
        self.env
            .iter()
            .copied()
            .zip(self.env_tags.iter().copied())
            .chain(self.overflow.iter().flat_map(|overflow| {
                overflow.captures.iter().map(|c| (c.value, c.tag))
            }))
    }

    /// Drop the count each counted capture holds. This is the release walk
    /// `captures` describes, written out for the hot path: a closure is
    /// released once per construction, and walking all sixteen inline slots
    /// through the chained iterator, with a call per slot, was most of the
    /// cost of building and dropping a small closure. A slot whose tag is 0
    /// holds a plain value, and the sixteen tags are read as one word, so a
    /// closure that captured only plain values skips the walk.
    fn release_captures(&self) {
        if u128::from_ne_bytes(self.env_tags) != 0 {
            for (value, tag) in self.env.iter().zip(self.env_tags.iter()) {
                if *tag != 0 {
                    // SAFETY: a nonzero tag marks a slot that holds one count
                    // on a value of that kind, taken when it was captured.
                    unsafe { release_captured_value(*value, *tag) };
                }
            }
        }
        if let Some(overflow) = &self.overflow {
            for capture in &overflow.captures {
                if capture.tag != 0 {
                    // SAFETY: as above, for a spilled slot.
                    unsafe { release_captured_value(capture.value, capture.tag) };
                }
            }
        }
    }

    /// Copy every capture out in walk order and blank the slots so a later
    /// shell free (or cyclic back-edge release) can never release them a
    /// second time.
    fn take_captures(&mut self) -> ([Capture; PITH_CLOSURE_ENV_SLOTS], Vec<Capture>) {
        let mut inline = [Capture { value: 0, tag: 0 }; PITH_CLOSURE_ENV_SLOTS];
        for ((dst, value), tag) in inline
            .iter_mut()
            .zip(self.env.iter_mut())
            .zip(self.env_tags.iter_mut())
        {
            *dst = Capture {
                value: *value,
                tag: *tag,
            };
            *value = 0;
            *tag = 0;
        }
        let spill = match &mut self.overflow {
            Some(overflow) => {
                let mut out = Vec::with_capacity(overflow.captures.len());
                for c in &mut overflow.captures {
                    out.push(*c);
                    c.value = 0;
                    c.tag = 0;
                }
                out
            }
            None => Vec::new(),
        };
        (inline, spill)
    }
}

/// Validate a closure handle by its magic word for `op` and borrow it for the
/// duration of `f`. This is the lock-free equivalent of the old registry lookup:
/// a stale pointer whose magic was scrubbed on free reads as invalid, and the
/// borrow cannot escape `f`.
#[inline(always)]
unsafe fn with_closure_for<R>(
    handle: i64,
    op: crate::quarantine::Op,
    f: impl FnOnce(&PithClosure) -> R,
) -> Option<R> {
    if handle == 0 || (handle as u64) % 8 != 0 {
        return None;
    }
    let ptr = handle as *const PithClosure;
    // SAFETY: Non-null 8-aligned pointer; magic word sits at offset 0 of `PithClosure`.
    let magic = unsafe { std::ptr::addr_of!((*ptr).magic).read() };
    if magic != CLOSURE_MAGIC {
        // SAFETY: `ptr` is only used as a lookup key in quarantine reporting.
        unsafe { crate::quarantine::miss(magic, std::ptr::addr_of!((*ptr).magic), op) };
        return None;
    }
    // SAFETY: Magic word matched `CLOSURE_MAGIC`; caller guarantees handle stays live for `f`.
    let closure = unsafe { &*ptr };
    Some(f(closure))
}

/// Validate a closure handle and borrow it immutably for the duration of `f`.
#[inline(always)]
unsafe fn with_closure<R>(handle: i64, f: impl FnOnce(&PithClosure) -> R) -> Option<R> {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { with_closure_for(handle, crate::quarantine::Op::Use, f) }
}

/// Validate a closure handle and borrow it mutably for the duration of `f`.
#[inline(always)]
unsafe fn with_closure_mut<R>(handle: i64, f: impl FnOnce(&mut PithClosure) -> R) -> Option<R> {
    if handle == 0 || (handle as u64) % 8 != 0 {
        return None;
    }
    let ptr = handle as *mut PithClosure;
    // SAFETY: Non-null 8-aligned pointer; magic word sits at offset 0 of `PithClosure`.
    let magic = unsafe { std::ptr::addr_of!((*ptr).magic).read() };
    if magic != CLOSURE_MAGIC {
        // SAFETY: `ptr` is only used as a lookup key in quarantine reporting.
        unsafe { crate::quarantine::miss(magic, std::ptr::addr_of!((*ptr).magic), crate::quarantine::Op::Use) };
        return None;
    }
    // SAFETY: Magic word matched `CLOSURE_MAGIC`; caller guarantees exclusive access for `f`.
    let closure = unsafe { &mut *ptr };
    Some(f(closure))
}

unsafe fn release_captured_value(value: i64, tag: u8) {
    // SAFETY: Caller guarantees `value` holds one refcount of the kind named by `tag`.
    match tag {
        CLOSURE_TAG_STRING => unsafe { pith_cstring_release(value as *const i8) },
        CLOSURE_TAG_LIST => unsafe { crate::collections::list::pith_list_release_handle(value) },
        CLOSURE_TAG_MAP => unsafe { crate::collections::map::pith_map_release_handle(value) },
        CLOSURE_TAG_SET => unsafe { crate::collections::set::pith_set_release_handle(value) },
        CLOSURE_TAG_BYTES => unsafe { crate::bytes::pith_bytes_release(value) },
        CLOSURE_TAG_STRUCT => unsafe { pith_struct_release(value) },
        CLOSURE_TAG_CLOSURE => unsafe { pith_closure_release(value) },
        CLOSURE_TAG_WEAK => unsafe { pith_struct_weak_release(value) },
        CLOSURE_TAG_CHANNEL => unsafe { crate::concurrency::channel::pith_channel_release(value) },
        CLOSURE_TAG_SYNC => unsafe { crate::concurrency::sync_handle::pith_sync_release(value) },
        _ => {}
    }
}

#[no_mangle]
pub extern "C" fn pith_closure_new(func_ptr: i64) -> i64 {
    crate::alloc_sites::record("closure");
    // a closure is built and dropped once per construction, and a benchmark
    // that builds two per iteration spends about 56ns in each against a 7ns
    // call. almost none of that is the work; it is the allocator round-trip.
    // so a dead closure's block goes on a per-thread stack and the next
    // construction writes over it, the same trade the struct pool makes and
    // behind the same switch, so `make memcheck` still sees every free.
    let pooled = closure_pool_take();
    if !pooled.is_null() {
        let ptr = pooled as *mut PithClosure;
        // SAFETY: `pooled` is an unowned recycled `PithClosure` allocation.
        unsafe { ptr.write(fresh_closure(func_ptr)) };
        return ptr as i64;
    }
    Box::into_raw(Box::new(fresh_closure(func_ptr))) as i64
}

fn fresh_closure(func_ptr: i64) -> PithClosure {
    PithClosure {
        magic: CLOSURE_MAGIC,
        func_ptr,
        ref_count: std::sync::atomic::AtomicI64::new(1),
        env: [0; PITH_CLOSURE_ENV_SLOTS],
        env_tags: [0; PITH_CLOSURE_ENV_SLOTS],
        cycle_flags: std::sync::atomic::AtomicU8::new(0),
        overflow: None,
    }
}

// A dead closure's block, kept for the next construction. Fixed size, so a
// single stack rather than the struct pool's size buckets, and a stack of its
// own inside the shared pool slot so the two can never hand each other a
// block laid out for the other. Honours the same switch as the struct pool:
// with it off every block goes back to the allocator, which is what lets
// valgrind see a use after free.
const CLOSURE_POOL_CAP: usize = 512;

fn closure_pool_take() -> *mut u8 {
    if !struct_pool_enabled() {
        return std::ptr::null_mut();
    }
    let slot = pool_slot();
    if slot.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `slot` is non-null and exclusively owned by this thread while active.
    unsafe { (*slot).closures.pop() }.unwrap_or(std::ptr::null_mut())
}

/// Recycle a dead closure shell, or hand it back to the allocator. The
/// overflow spill owns its buffer either way, so it is dropped first;
/// everything else is plain data the next construction overwrites.
unsafe fn closure_free_shell(handle: i64) {
    let ptr = handle as *mut PithClosure;
    // SAFETY: Sole owner after magic scrub; drop only the `overflow` spill storage.
    unsafe { std::ptr::drop_in_place(std::ptr::addr_of_mut!((*ptr).overflow)) };
    let size = std::mem::size_of::<PithClosure>();
    // SAFETY: `ptr..ptr+size` is writable and magic sits at offset 0.
    if unsafe { crate::quarantine::bury(ptr as *mut u8, size, ptr as *mut u32, crate::quarantine::Kind::Closure) } {
        return;
    }
    if struct_pool_enabled() {
        let slot = pool_slot();
        // SAFETY: `slot` is non-null and exclusively owned by this thread while active.
        if !slot.is_null() && unsafe { (*slot).try_push_closure(ptr as *mut u8) } {
            return;
        }
    }
    // SAFETY: `ptr` was originally allocated via `Box::new(PithClosure { .. })`.
    unsafe { std::alloc::dealloc(ptr as *mut u8, std::alloc::Layout::new::<PithClosure>()) };
}

/// One more owner of this closure.
///
/// # Safety
/// handle must be a valid closure handle or garbage (magic-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_closure_retain(handle: i64) {
    crate::perf::perf_rc_op(crate::perf::RC_CLOSURE_RETAIN);
    // SAFETY: `handle` is validated by `with_closure_for` before bumping its refcount.
    unsafe { with_closure_for(handle, crate::quarantine::Op::Retain, |c| c.ref_count.fetch_add(1, Ordering::Relaxed)) };
}

enum ClosureReleaseAction {
    Keep,
    Retire { buffered: bool },
}

#[inline(always)]
fn closure_release_step(closure: &PithClosure) -> ClosureReleaseAction {
    // cycle-gc suspect hook, before our count is given up: while we still
    // hold it the closure cannot die under the hook, which is what makes
    // marking it safe against a concurrent final release (see
    // `maybe_suspect_struct` for the full argument).
    let gc = crate::cycle::cycle_gc_enabled();
    if gc && closure.ref_count.load(Ordering::Relaxed) > 1 {
        maybe_suspect_closure(closure);
    }
    // a count of one is ours alone: no other owner is left to retain or
    // release concurrently. so the last release of a closure that never
    // escaped (the common case: built, called, dropped) is a load and a plain
    // store, not a locked read-modify-write, which would first wait for the
    // construction's stores to drain. the acquire load pairs with the release
    // decrements of the owners before us. with the cycle collector on, the
    // suspect buffer can reach the closure without a count, so it keeps the
    // locked path.
    let prev = if !gc && closure.ref_count.load(Ordering::Acquire) == 1 {
        closure.ref_count.store(0, Ordering::Relaxed);
        1
    } else {
        closure.ref_count.fetch_sub(1, Ordering::Release)
    };
    if prev > 1 {
        return ClosureReleaseAction::Keep;
    }
    if prev <= 0 {
        crate::quarantine::over_release(crate::quarantine::Kind::Closure);
        // over-release: restore and leave it alone
        closure.ref_count.fetch_add(1, Ordering::Relaxed);
        return ClosureReleaseAction::Keep;
    }
    acquire_last_count!(closure.ref_count);
    closure.release_captures();
    let buffered = closure.cycle_flags.load(Ordering::Relaxed) & CLOSURE_BUFFERED_FLAG != 0;
    ClosureReleaseAction::Retire { buffered }
}

/// Drop one owner; the last release drops the counts the closure took
/// on its captured values, then frees the closure. Closures once
/// allocated with no release path at all, and leaked their environments.
///
/// # Safety
/// handle must be a valid closure handle or garbage (magic-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_closure_release(handle: i64) {
    crate::perf::perf_rc_op(crate::perf::RC_CLOSURE_RELEASE);
    // SAFETY: `handle` is validated by `with_closure_for` before decrementing its refcount.
    let action = unsafe { with_closure_for(handle, crate::quarantine::Op::Release, closure_release_step) };
    if let Some(ClosureReleaseAction::Retire { buffered }) = action {
        // SAFETY: Last release; `&PithClosure` borrow has ended so scrubbing and freeing cannot alias.
        unsafe { retire_closure_shell(handle, buffered) };
    }
}

/// The end of a closure whose captures are already released: scrub the
/// magic so a stale handle fails `with_closure_for`, then free the shell. A
/// closure that dies while the suspect buffer points at it parks its shell
/// in the graveyard instead, so the buffered entry never dangles; the
/// scrubbed magic keeps that handle from validating, and the next collection
/// frees the shell.
#[inline(always)]
unsafe fn retire_closure_shell(handle: i64, buffered: bool) {
    // SAFETY: Sole owner after last release; no `&PithClosure` borrow is live.
    unsafe { (handle as *mut u32).write(0) };
    if buffered && crate::cycle::cycle_gc_enabled() {
        crate::cycle::graveyard_defer(handle as usize, crate::cycle::CYCLE_KIND_CLOSURE);
        return;
    }
    // SAFETY: Sole owner and not deferred to graveyard; recycle or deallocate shell.
    unsafe { closure_free_shell(handle) };
}

/// Mark a closure as a cycle suspect and hand it to the buffer. Outlined and
/// cold for the same reason the perf recorders are: the release fast path
/// must stay frameless when the flag is off.
#[cold]
#[inline(never)]
fn maybe_suspect_closure(closure: &PithClosure) {
    if closure
        .cycle_flags
        .fetch_or(CLOSURE_BUFFERED_FLAG, Ordering::Relaxed)
        & CLOSURE_BUFFERED_FLAG
        != 0
    {
        return; // already buffered
    }
    // SAFETY: Caller holds a strong count on this live `PithClosure` and just set its
    // buffered bit; its address is the closure handle.
    let handle = closure as *const PithClosure as usize;
    unsafe { crate::cycle::cycle_suspect(handle, crate::cycle::CYCLE_KIND_CLOSURE) };
}

/// Drop a closure's buffered mark (overflow, or a collector drain). Magic-
/// checked, so a closure that already died and was freed is a no-op.
pub(crate) unsafe fn cycle_clear_closure_buffered(handle: i64) {
    // SAFETY: Preconditions forwarded from cycle collector.
    unsafe { with_closure(handle, |c| c.cycle_flags.fetch_and(!CLOSURE_BUFFERED_FLAG, Ordering::Relaxed)) };
}

// --- collector-facing closure accessors -------------------------------------
//
// the collection pass runs with every mutator stopped, so these read and
// rewrite the count with plain relaxed operations; each one magic-checks the
// handle so an edge into something that was never (or is no longer) a closure
// degrades to a None or a no-op instead of a wild read.

/// The closure's current reference count, or `None` when the handle no longer
/// validates (the closure died and its magic was scrubbed).
pub(crate) unsafe fn cycle_closure_strong_count(handle: i64) -> Option<i64> {
    // SAFETY: Preconditions forwarded from cycle collector.
    unsafe { with_closure(handle, |c| c.ref_count.load(Ordering::Relaxed)) }
}

/// Add `delta` to the closure's reference count. The collector's teardown
/// guard: a huge bias keeps releases from other dying objects from ever
/// reaching this closure's death path while the collector owns it.
pub(crate) unsafe fn cycle_closure_guard_strong(handle: i64, delta: i64) {
    // SAFETY: Preconditions forwarded from cycle collector.
    unsafe { with_closure(handle, |c| c.ref_count.fetch_add(delta, Ordering::Relaxed)) };
}

fn mark_closure_shared_impl(closure: &PithClosure) -> bool {
    closure
        .cycle_flags
        .fetch_or(crate::share::CONTAINER_SHARED_FLAG, Ordering::Release)
        & crate::share::CONTAINER_SHARED_FLAG
        == 0
}

/// Mark the closure shared (see `share.rs`); true when this call set the mark.
pub(crate) unsafe fn closure_mark_shared(handle: i64) -> bool {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { with_closure(handle, mark_closure_shared_impl) }.unwrap_or(false)
}

fn walk_closure_children(closure: &PithClosure, f: &mut dyn FnMut(i64, u8)) {
    // a closure over plain values only (every spawn of `f(i, n)`, say) has no
    // tagged slot: two word reads answer that without walking all sixteen
    if closure.overflow.is_none() && closure.env_tags.iter().all(|&tag| tag == 0) {
        return;
    }
    for (value, tag) in closure.captures() {
        if value != 0 && tag != 0 {
            f(value, tag);
        }
    }
}

/// Report every captured value that carries a count, as (value, env tag).
/// Weak captures (tag 8) are reported too, and the caller filters them: they
/// hold no strong count and are not edges of the ownership graph.
pub(crate) unsafe fn cycle_closure_children(handle: i64, f: &mut dyn FnMut(i64, u8)) {
    // SAFETY: Preconditions forwarded from cycle collector / share walk.
    unsafe { with_closure(handle, |c| walk_closure_children(c, f)) };
}

/// The destruction body of a garbage closure: drop the counts it took on its
/// captures, exactly as the last release would, then blank the slots so the
/// shell free below can never release them a second time.
pub(crate) unsafe fn cycle_closure_release_env(handle: i64) {
    // SAFETY: Collector runs with mutators stopped; blank slots while holding `&mut`,
    // then drop the borrow before releasing captures so cyclic back-edges cannot alias it.
    let Some((inline, spill)) = (unsafe { with_closure_mut(handle, PithClosure::take_captures) }) else {
        return;
    };
    for capture in inline.into_iter().chain(spill) {
        if capture.tag != 0 {
            // SAFETY: Non-zero tag marks a captured value holding one refcount.
            unsafe { release_captured_value(capture.value, capture.tag) };
        }
    }
}

/// Free a garbage closure's shell: the tail of `pith_closure_release` minus
/// the environment release, which `cycle_closure_release_env` already ran.
/// A shell whose buffered bit was set during teardown (a release hook fired
/// on the collector thread) parks in the graveyard.
pub(crate) unsafe fn cycle_closure_free_dead(handle: i64) {
    // SAFETY: Preconditions forwarded from cycle collector.
    let Some(buffered) = (unsafe {
        with_closure(handle, |c| c.cycle_flags.load(Ordering::Relaxed) & CLOSURE_BUFFERED_FLAG != 0)
    }) else {
        return;
    };
    // SAFETY: Collector owns this dead shell and the shared borrow above has ended.
    unsafe { retire_closure_shell(handle, buffered) };
}

/// Drop a closure shell the graveyard parked. The magic is already scrubbed
/// and the captures already released, so only the box itself remains.
pub(crate) unsafe fn cycle_drop_closure_shell(ptr: usize) {
    let closure = ptr as *mut PithClosure;
    // SAFETY: Graveyard owns this boxed shell; magic sits at offset 0.
    unsafe { crate::quarantine::free_box(closure, closure as *mut u32, crate::quarantine::Kind::Closure) };
}

#[cfg(test)]
pub(crate) unsafe fn closure_buffered_for_tests(handle: i64) -> bool {
    // SAFETY: Preconditions forwarded from test caller.
    unsafe { with_closure(handle, |c| c.cycle_flags.load(Ordering::Relaxed) & CLOSURE_BUFFERED_FLAG != 0) }
        .unwrap_or(false)
}

#[no_mangle]
pub unsafe extern "C" fn pith_closure_get_fn(handle: i64) -> i64 {
    // SAFETY: `handle` is validated by `with_closure` before reading `func_ptr`.
    unsafe { with_closure(handle, |c| c.func_ptr) }.unwrap_or(0)
}

#[inline(always)]
fn closure_set_env_impl(closure: &mut PithClosure, slot: EnvSlot, value: i64) {
    match slot {
        EnvSlot::Inline(idx) => closure.env[idx] = value,
        EnvSlot::Spill(idx) => closure.spill_capture(idx).value = value,
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_closure_set_env(handle: i64, slot: i64, value: i64) {
    let Some(slot) = EnvSlot::classify(slot) else {
        return;
    };
    // SAFETY: `handle` is validated by `with_closure_mut`; construction-time exclusive access.
    unsafe { with_closure_mut(handle, |c| closure_set_env_impl(c, slot, value)) };
}

#[inline(always)]
fn closure_set_env_rc_impl(closure: &mut PithClosure, slot: EnvSlot, value: i64, tag: u8) {
    match slot {
        EnvSlot::Inline(idx) => {
            closure.env[idx] = value;
            closure.env_tags[idx] = tag;
        }
        EnvSlot::Spill(idx) => {
            let capture = closure.spill_capture(idx);
            capture.value = value;
            capture.tag = tag;
        }
    }
}

/// Store a captured value that carries a reference count, tagging the
/// slot so the closure releases that count when it is freed. the caller
/// has already retained the value into the closure.
///
/// # Safety
/// handle must be a valid closure handle or garbage (magic-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_closure_set_env_rc(handle: i64, slot: i64, value: i64, tag: i64) {
    let Some(slot) = EnvSlot::classify(slot) else {
        return;
    };
    // SAFETY: `handle` is validated by `with_closure_mut`; construction-time exclusive access.
    unsafe { with_closure_mut(handle, |c| closure_set_env_rc_impl(c, slot, value, tag as u8)) };
}

#[inline(always)]
fn closure_get_env_impl(closure: &PithClosure, slot: EnvSlot) -> i64 {
    match slot {
        EnvSlot::Inline(idx) => closure.env[idx],
        // an unwritten (or never-spilled) slot answers 0, like an
        // unwritten inline slot
        EnvSlot::Spill(idx) => closure
            .overflow
            .as_ref()
            .and_then(|overflow| overflow.captures.get(idx))
            .map_or(0, |capture| capture.value),
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_closure_get_env(handle: i64, slot: i64) -> i64 {
    let Some(slot) = EnvSlot::classify(slot) else {
        return 0;
    };
    // SAFETY: `handle` is validated by `with_closure` before reading its env.
    unsafe { with_closure(handle, |c| closure_get_env_impl(c, slot)) }.unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn pith_print(s: string::PithString) {
    if s.ptr.is_null() || s.len <= 0 {
        println!();
        return;
    }

    // SAFETY: Caller guarantees `s.ptr` is valid for `s.len` readable bytes.
    let slice = unsafe { std::slice::from_raw_parts(s.ptr, s.len as usize) };
    if let Ok(str_ref) = std::str::from_utf8(slice) {
        println!("{}", str_ref);
    } else {
        println!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn heap_refcount(s: *const i8) -> u32 {
        unsafe {
            let base = (s as *const u8).sub(CSTRING_HEADER_SIZE);
            (base.add(4) as *const u32).read()
        }
    }

    unsafe fn text(s: *const i8) -> String {
        let len = string::pith_cstring_len(s) as usize;
        let bytes = unsafe { std::slice::from_raw_parts(s as *const u8, len) };
        String::from_utf8_lossy(bytes).into_owned()
    }

    unsafe fn append(base: *const i8, parts: &[*const i8]) -> *mut i8 {
        let mut p = [std::ptr::null(); APPEND_MANY_MAX];
        p[..parts.len()].copy_from_slice(parts);
        unsafe {
            pith_cstring_append_many(parts.len() as i64, base, p[0], p[1], p[2], p[3], p[4], p[5], p[6])
        }
    }

    #[test]
    fn size_classes_cover_the_length_and_grow_geometrically() {
        assert_eq!(cstring_alloc_size(0), 24);
        assert_eq!(cstring_alloc_size(7), 24);
        assert_eq!(cstring_alloc_size(8), 40);
        assert_eq!(cstring_alloc_size(119), 136);
        assert_eq!(cstring_alloc_size(120), 160);
        for len in 0..5000 {
            let size = cstring_alloc_size(len);
            assert!(size >= CSTRING_HEADER_SIZE + len + 1);
            if len > 200 {
                assert!(size * 4 <= (CSTRING_HEADER_SIZE + len + 1) * 5);
            }
        }
        assert_eq!(cstring_alloc_size_checked(usize::MAX), None);
        assert_eq!(cstring_alloc_size_checked(usize::MAX - 1), None);
        assert_eq!(cstring_alloc_size_checked(usize::MAX - CSTRING_HEADER_SIZE), None);
        assert_eq!(cstring_alloc_size_checked(isize::MAX as usize), None);
        assert_eq!(cstring_alloc_size_checked((isize::MAX as usize) - CSTRING_HEADER_SIZE), None);
    }

    #[test]
    fn a_sole_owner_appends_in_place() {
        unsafe {
            let s = pith_copy_bytes_to_cstring(b"ab");
            let tail = pith_copy_bytes_to_cstring(b"cd");
            let grown = append(s, &[tail, std::ptr::null()]);
            assert_eq!(grown, s, "still inside the size class");
            assert_eq!(text(grown), "abcd");
            assert_eq!(heap_refcount(grown), 1);
            // past the class: a realloc, same contents, still one count
            let mut acc = grown;
            for _ in 0..100 {
                acc = append(acc, &[tail]);
            }
            assert_eq!(text(acc).len(), 4 + 200);
            assert!(text(acc).ends_with("cdcd"));
            assert_eq!(heap_refcount(acc), 1);
            pith_cstring_release(acc);
            pith_cstring_release(tail);
        }
    }

    #[test]
    fn a_shared_or_literal_base_is_copied() {
        unsafe {
            let s = pith_copy_bytes_to_cstring(b"ab");
            pith_cstring_retain(s); // a second owner
            let tail = pith_copy_bytes_to_cstring(b"!");
            let fresh = append(s, &[tail]);
            assert_ne!(fresh, s as *mut i8);
            assert_eq!(text(s), "ab", "the other owner's value is untouched");
            assert_eq!(text(fresh), "ab!");
            assert_eq!(heap_refcount(s), 1, "the caller's count moved to the result");
            pith_cstring_release(s);
            pith_cstring_release(fresh);

            let literal = b"lit\0".as_ptr() as *const i8;
            let from_literal = append(literal, &[tail]);
            assert_eq!(text(from_literal), "lit!");
            pith_cstring_release(from_literal);
            pith_cstring_release(tail);
        }
    }

    #[test]
    fn the_shared_empty_string_carries_a_literal_header() {
        unsafe {
            let empty = pith_cstring_empty();
            assert_eq!(cstring_header_len(empty), Some(0));
            assert!(cstring_base(empty).is_none(), "never counted or freed");
            pith_cstring_retain(empty);
            pith_cstring_release(empty);
            assert_eq!(crate::string::pith_cstring_len(empty), 0);
        }
    }

    #[test]
    fn appending_a_string_to_itself_copies() {
        unsafe {
            let s = pith_copy_bytes_to_cstring(b"xy");
            let doubled = append(s, &[s]);
            assert_eq!(text(doubled), "xyxy");
            pith_cstring_release(doubled);
        }
    }

    #[test]
    fn heap_cstrings_carry_a_refcount() {
        unsafe {
            let s = pith_copy_bytes_to_cstring(b"hello");
            assert_eq!(heap_refcount(s), 1);
            pith_cstring_retain(s);
            assert_eq!(heap_refcount(s), 2);
            pith_cstring_release(s);
            assert_eq!(heap_refcount(s), 1);
            pith_cstring_release(s); // frees; magic is scrubbed first
        }
    }

    #[test]
    fn integers_format_in_decimal() {
        unsafe {
            for n in [0, 7, -7, 10, 99, -100, 1234567890, i64::MAX, i64::MIN] {
                let s = pith_int_to_cstr(n);
                let text = std::ffi::CStr::from_ptr(s).to_str().unwrap().to_owned();
                assert_eq!(text, n.to_string());
                assert_eq!(cstring_header_len(s), Some(text.len() as i64));
                pith_cstring_release(s);
            }
            for n in [0u64, 42, u64::MAX] {
                let s = pith_uint_to_cstr(n as i64);
                assert_eq!(std::ffi::CStr::from_ptr(s).to_str().unwrap(), n.to_string());
                pith_cstring_release(s);
            }
        }
    }

    #[test]
    fn released_cstring_fails_the_header_check() {
        unsafe {
            let s = pith_copy_bytes_to_cstring(b"x");
            pith_cstring_release(s);
            // stale pointer: magic scrubbed, so this must be a no-op,
            // not a double free
            pith_cstring_release(s);
            pith_cstring_retain(s);
        }
    }

    #[test]
    fn literals_and_null_are_no_ops() {
        unsafe {
            let lit = b"static literal\0".as_ptr() as *const i8;
            pith_cstring_retain(lit);
            pith_cstring_release(lit);
            pith_cstring_release(lit);
            pith_cstring_retain(std::ptr::null());
            pith_cstring_release(std::ptr::null());
            // the shared empty string is header-less by design
            let empty = pith_cstring_empty();
            pith_cstring_release(empty);
            assert_eq!(*empty, 0);
        }
    }

    #[test]
    fn concat_and_strdup_produce_owned_cstrings() {
        unsafe {
            let a = pith_copy_bytes_to_cstring(b"ab");
            let b = pith_copy_bytes_to_cstring(b"cd");
            let joined = pith_concat_cstr(a, b);
            assert_eq!(heap_refcount(joined), 1);
            assert_eq!(crate::string::pith_cstring_len(joined), 4);
            let dup = pith_strdup(joined);
            assert_eq!(heap_refcount(dup), 1);
            pith_cstring_release(joined);
            pith_cstring_release(dup);
            pith_cstring_release(a);
            pith_cstring_release(b);
        }
    }

    unsafe fn struct_counts(ptr: i64) -> (u32, u32) {
        let base = (ptr as usize - STRUCT_HEADER) as *const u8;
        unsafe {
            (
                (base.add(STRUCT_OFF_STRONG) as *const u32).read(),
                (base.add(STRUCT_OFF_WEAK) as *const u32).read(),
            )
        }
    }

    #[test]
    fn struct_without_weak_refs_frees_on_last_strong_release() {
        unsafe {
            let s = pith_struct_alloc(2);
            assert_eq!(struct_counts(s), (1, 1)); // strong 1, implicit weak 1
            pith_struct_retain(s);
            assert_eq!(struct_counts(s).0, 2);
            pith_struct_release(s);
            assert_eq!(struct_counts(s).0, 1);
            pith_struct_release(s); // frees; magic scrubbed. no leak, no crash.
        }
    }

    #[test]
    fn weak_ref_keeps_the_header_alive_past_the_strong_release() {
        unsafe {
            let s = pith_struct_alloc(1);
            pith_struct_weak_retain(s);
            assert_eq!(struct_counts(s), (1, 2)); // implicit + one weak

            // the target is alive: an upgrade returns the pointer with a
            // count of its own
            assert_eq!(pith_struct_weak_upgrade(s), s);
            assert_eq!(struct_counts(s), (2, 2));
            pith_struct_release(s);

            // drop the only strong owner: value dies, header stays (weak > 0)
            pith_struct_release(s);
            assert_eq!(pith_struct_weak_upgrade(s), 0); // dead -> none

            // dropping the last weak frees the header
            pith_struct_weak_release(s);
        }
    }

    // structs travel between tasks, and tasks run on different worker threads,
    // so the last strong owner can die on a thread the weak reader never runs
    // on. the dead flag is an atomic for exactly this shape; the join gives the
    // load a happens-after edge, so it must observe the death.
    #[test]
    fn weak_upgrade_sees_a_release_from_another_thread() {
        unsafe {
            let s = pith_struct_alloc(1);
            pith_struct_weak_retain(s);
            assert_eq!(pith_struct_weak_upgrade(s), s);
            pith_struct_release(s);

            std::thread::spawn(move || pith_struct_release(s))
                .join()
                .expect("releasing thread panicked");

            assert_eq!(pith_struct_weak_upgrade(s), 0);
            pith_struct_weak_release(s);
        }
    }

    // a weak field store or read on a holder nothing has marked shared skips
    // the field lock, since only the task that built the holder can reach it.
    // on a shared holder both wait for the lock, which is what keeps a store
    // in another task from freeing the old target's header between a read's
    // load and its upgrade.
    #[test]
    fn weak_field_calls_lock_only_a_shared_holder() {
        unsafe {
            let holder = pith_struct_alloc(1);
            let target = pith_struct_alloc(1);
            for shared in [false, true] {
                if shared {
                    struct_mark_shared(holder);
                }
                let guard = WeakFieldGuard::lock(holder);
                let (done, finished) = std::sync::mpsc::channel();
                let worker = std::thread::spawn(move || {
                    pith_struct_weak_retain(target);
                    pith_struct_weak_store_field(holder, 0, target);
                    done.send(pith_struct_weak_load_field(holder, 0)).expect("send");
                });
                let wait = std::time::Duration::from_millis(if shared { 50 } else { 10_000 });
                let early = finished.recv_timeout(wait);
                assert_eq!(early.is_ok(), !shared, "shared: {shared}");
                drop(guard);
                let read = early.or_else(|_| finished.recv()).expect("worker result");
                worker.join().expect("worker panicked");
                assert_eq!(read, target);
                // the upgrade's count, and the field's weak count on the
                // target stored before
                assert_eq!(struct_counts(target), (2, 2));
                pith_struct_release(read);
            }
            pith_struct_weak_store_field(holder, 0, 0);
            assert_eq!(struct_counts(target), (1, 1));
            pith_struct_release(target);
            pith_struct_release(holder);
        }
    }

    /// a compiled literal's header gives its length, and retain and release
    /// leave it alone.
    #[test]
    fn a_literal_header_gives_the_length_and_is_never_counted() {
        let mut words = [0u64; 4];
        let text = b"literal\0";
        unsafe {
            let base = words.as_mut_ptr() as *mut u8;
            (base as *mut u32).write(CSTRING_STATIC_MAGIC);
            (base.add(8) as *mut u64).write(7);
            std::ptr::copy_nonoverlapping(text.as_ptr(), base.add(16), text.len());
            let s = base.add(16) as *const i8;
            assert_eq!(cstring_header_len(s), Some(7));
            assert_eq!(crate::string::pith_cstring_len(s), 7);
            assert!(cstring_base(s).is_none());
            pith_cstring_retain(s);
            pith_cstring_release(s);
            assert_eq!((base.add(4) as *const u32).read(), 0);
            assert_eq!((base as *const u32).read(), CSTRING_STATIC_MAGIC);
        }
    }

    #[test]
    fn a_join_writes_its_int_parts_in_decimal() {
        unsafe {
            let a = crate::pith_copy_bytes_to_cstring(b"id=");
            let b = crate::pith_copy_bytes_to_cstring(b";");
            let null = std::ptr::null();
            let int = |n: i64| n as *const i8;
            let ints = |bits: i64| bits << JOIN_INT_SHIFT;
            for n in [0, 7, 10, 99, 100, -1, -10, 1234567890123, i64::MAX, i64::MIN] {
                assert_eq!(int_text_len(n), n.to_string().len());
                let joined = pith_concat_cstr_many(3 | ints(0b010), a, int(n), b, null, null, null, null, null);
                assert_eq!(text(joined), format!("id={};", n));
                assert_eq!(cstring_header_len(joined), Some(n.to_string().len() as i64 + 4));
                pith_cstring_release(joined);
            }
            // an int part of 0 is text, where a null string part is empty
            let zero = pith_concat_cstr_many(1 | ints(0b1), int(0), null, null, null, null, null, null, null);
            assert_eq!(text(zero), "0");
            // a bit past the count is ignored
            let one = pith_concat_cstr_many(1 | ints(0b10), a, int(5), null, null, null, null, null, null);
            assert_eq!(text(one), "id=");

            // appended in place, and through the copy a shared base takes
            let s = pith_copy_bytes_to_cstring(b"n");
            let grown = pith_cstring_append_many(2 | ints(0b01), s, int(-42), b, null, null, null, null, null);
            assert_eq!(grown as *const i8, s as *const i8, "still inside the size class");
            assert_eq!(text(grown), "n-42;");
            pith_cstring_retain(grown);
            let copy = pith_cstring_append_many(2 | ints(0b10), grown, b, int(8), null, null, null, null, null);
            assert_ne!(copy as *const i8, grown as *const i8);
            assert_eq!(text(copy), "n-42;;8");
            assert_eq!(text(grown), "n-42;");
            for s in [zero, one, copy, grown, a, b] {
                pith_cstring_release(s);
            }
        }
    }

    #[test]
    fn concat_many_joins_its_parts_like_a_chain_of_concats() {
        unsafe {
            let a = crate::pith_copy_bytes_to_cstring(b"ab");
            let b = crate::pith_copy_bytes_to_cstring(b"");
            let c = crate::pith_copy_bytes_to_cstring(b"cde");
            let null = std::ptr::null();
            let joined = pith_concat_cstr_many(4, a, null, b, c, a, a, a, a);
            assert_eq!(std::ffi::CStr::from_ptr(joined).to_bytes(), b"abcde");
            assert_eq!(cstring_header_len(joined), Some(5));
            assert_eq!(heap_refcount(joined), 1);
            let all = pith_concat_cstr_many(8, a, a, a, a, a, a, a, c);
            assert_eq!(std::ffi::CStr::from_ptr(all).to_bytes(), b"abababababababcde");
            assert!(pith_concat_cstr_many(2, null, null, a, a, a, a, a, a).is_null());
            // the parts keep their own counts
            assert_eq!(heap_refcount(a), 1);
            for s in [joined, all, a, b, c] {
                pith_cstring_release(s);
            }
        }
    }

    #[test]
    fn chr_hands_out_a_static_one_byte_string() {
        unsafe {
            for n in [0i64, 10, 34, 65, 127, 200, 255, 256 + 65] {
                let s = pith_chr_cstr(n);
                assert_eq!(cstring_header_len(s), Some(1));
                assert_eq!(*s as u8, n as u8);
                assert_eq!(*s.add(1), 0);
                assert!(cstring_base(s).is_none());
                pith_cstring_retain(s);
                pith_cstring_release(s);
                pith_cstring_release(s);
                assert_eq!(*s as u8, n as u8);
            }
            assert_eq!(pith_chr_cstr(65), pith_chr_cstr(65));
        }
    }

    #[test]
    fn count_release_returns_the_count_before() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let count = AtomicU32::new(3);
        assert_eq!(count_release(&count), 3);
        assert_eq!(count_release(&count), 2);
        assert_eq!(count_release(&count), 1);
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }

    static DTOR_RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    unsafe extern "C" fn counting_dtor(_ptr: i64) {
        DTOR_RUNS.fetch_add(1, Ordering::SeqCst);
    }

    /// a destructor that keeps a weak reference to the value it tears down.
    unsafe extern "C" fn weak_taking_dtor(ptr: i64) {
        unsafe { pith_struct_weak_retain(ptr) };
        WEAK_TAKEN.store(ptr, Ordering::SeqCst);
    }

    static WEAK_TAKEN: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

    /// a destructor that tries to upgrade a weak reference to its own value.
    unsafe extern "C" fn self_upgrading_dtor(ptr: i64) {
        SELF_UPGRADE.store(unsafe { pith_struct_weak_upgrade(ptr) }, Ordering::SeqCst);
    }

    static SELF_UPGRADE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-1);

    #[test]
    fn an_upgrade_during_the_destructor_reads_none() {
        unsafe {
            let s = pith_struct_alloc(1);
            pith_struct_weak_retain(s);
            pith_struct_set_dtor(s, self_upgrading_dtor as usize as i64);
            pith_struct_release(s);
            assert_eq!(SELF_UPGRADE.load(Ordering::SeqCst), 0, "a dying value is not revived");
            assert_eq!(pith_struct_weak_upgrade(s), 0);
            pith_struct_weak_release(s);
        }
    }

    #[test]
    fn a_retain_of_a_dead_struct_is_refused() {
        unsafe {
            let s = pith_struct_alloc(1);
            pith_struct_weak_retain(s);
            pith_struct_release(s); // dead; the weak reference keeps the header
            let counts = struct_counts(s);
            pith_struct_retain(s);
            assert_eq!(struct_counts(s), counts, "no count taken on a dead value");
            // the stray retain's release lands on the over-release guard
            pith_struct_release(s);
            assert_eq!(struct_counts(s), counts);
            pith_struct_weak_release(s);
        }
    }

    #[test]
    fn a_sole_owner_release_runs_the_destructor_once() {
        unsafe {
            let before = DTOR_RUNS.load(Ordering::SeqCst);
            let s = pith_struct_alloc(2);
            pith_struct_set_dtor(s, counting_dtor as usize as i64);
            pith_struct_release(s);
            assert_eq!(DTOR_RUNS.load(Ordering::SeqCst), before + 1);
            assert_eq!(pith_struct_strong_count(s), 0); // freed: magic scrubbed
        }
    }

    #[test]
    fn a_weak_reference_taken_in_the_destructor_sees_the_value_dead() {
        unsafe {
            let s = pith_struct_alloc(1);
            pith_struct_set_dtor(s, weak_taking_dtor as usize as i64);
            pith_struct_release(s);
            assert_eq!(WEAK_TAKEN.load(Ordering::SeqCst), s);
            // the header outlives the value for the weak reference
            assert_eq!(struct_counts(s).1, 1);
            assert_eq!(pith_struct_weak_upgrade(s), 0);
            pith_struct_weak_release(s);
        }
    }

    #[test]
    fn strong_count_follows_retain_and_release() {
        unsafe {
            assert_eq!(pith_struct_strong_count(0), 0);
            assert_eq!(pith_struct_strong_count(12345), 0);
            let s = pith_struct_alloc(3);
            assert_eq!(pith_struct_strong_count(s), 1);
            pith_struct_retain(s);
            assert_eq!(pith_struct_strong_count(s), 2);
            pith_struct_release(s);
            assert_eq!(pith_struct_strong_count(s), 1);
            pith_struct_release(s); // fully freed
            assert_eq!(pith_struct_strong_count(s), 0); // magic scrubbed
        }
    }

    #[test]
    fn weak_upgrade_on_a_dead_or_invalid_pointer_is_none() {
        unsafe {
            assert_eq!(pith_struct_weak_upgrade(0), 0);
            assert_eq!(pith_struct_weak_upgrade(12345), 0);
            let s = pith_struct_alloc(1);
            pith_struct_release(s); // fully freed, no weak refs
            assert_eq!(pith_struct_weak_upgrade(s), 0); // magic scrubbed -> none
        }
    }

    #[test]
    fn weak_ops_on_invalid_pointers_are_no_ops() {
        unsafe {
            pith_struct_weak_retain(0);
            pith_struct_weak_release(0);
            pith_struct_weak_retain(12345);
            pith_struct_weak_release(12345);
        }
    }

    #[test]
    fn invalid_closure_handles_return_safe_defaults() {
        unsafe {
            assert_eq!(pith_closure_get_fn(12345), 0);
            assert_eq!(pith_closure_get_env(12345, 0), 0);
            pith_closure_set_env(12345, 0, 99);
        }
    }

    #[test]
    fn closure_env_access_requires_valid_slot() {
        let handle = pith_closure_new(77);
        unsafe {
            pith_closure_set_env(handle, 0, 42);
            pith_closure_set_env(handle, -1, 99);
            assert_eq!(pith_closure_get_fn(handle), 77);
            assert_eq!(pith_closure_get_env(handle, 0), 42);
            assert_eq!(pith_closure_get_env(handle, -1), 0);
        }
    }

    // slots past the inline block spill to the heap extension instead of
    // being silently dropped. the old behavior ignored the store and
    // answered 0, which made a 17-capture closure compute a wrong answer
    // with no diagnostic. an unwritten spill slot still answers 0.
    #[test]
    fn closure_env_slots_past_the_inline_block_spill() {
        let handle = pith_closure_new(77);
        let first_spill = PITH_CLOSURE_ENV_SLOTS as i64;
        unsafe {
            // the boundary pair: the last inline slot and the first spill
            // slot are adjacent indices in different storage, so a rebase
            // off by one would alias them or lose one of the writes.
            pith_closure_set_env(handle, first_spill - 1, 41);
            pith_closure_set_env(handle, first_spill, 99);
            pith_closure_set_env(handle, first_spill + 5, 123);
            assert_eq!(pith_closure_get_env(handle, first_spill - 1), 41);
            assert_eq!(pith_closure_get_env(handle, first_spill), 99);
            assert_eq!(pith_closure_get_env(handle, first_spill + 5), 123);
            // the gap between written spill slots reads as 0, as does a
            // slot past everything written
            assert_eq!(pith_closure_get_env(handle, first_spill + 1), 0);
            assert_eq!(pith_closure_get_env(handle, first_spill + 100), 0);
            // a slot at or past the hard cap is ignored like a negative
            // one. before the cap, this store attempted a terabyte-scale
            // resize and aborted the process
            pith_closure_set_env(handle, PITH_CLOSURE_MAX_ENV_SLOTS as i64, 7);
            pith_closure_set_env(handle, 1 << 40, 7);
            assert_eq!(pith_closure_get_env(handle, PITH_CLOSURE_MAX_ENV_SLOTS as i64), 0);
            assert_eq!(pith_closure_get_env(handle, 1 << 40), 0);
            pith_closure_release(handle);
        }
    }

    // a spilled slot's tag must drive the same release the inline tags
    // do: the closure below captures a struct past the inline block, and
    // the last closure release has to drop the count the caller moved
    // into that spill slot.
    #[test]
    fn spilled_rc_capture_is_released_with_the_closure() {
        unsafe {
            let s = pith_struct_alloc(1);
            pith_struct_retain(s); // the count the closure will own
            assert_eq!(struct_counts(s).0, 2);
            let handle = pith_closure_new(77);
            pith_closure_set_env_rc(
                handle,
                PITH_CLOSURE_ENV_SLOTS as i64 + 3,
                s,
                CLOSURE_TAG_STRUCT as i64,
            );
            pith_closure_release(handle);
            assert_eq!(struct_counts(s).0, 1, "spilled capture was not released");
            pith_struct_release(s);
        }
    }
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

fn print_utf8_bytes(bytes: &[u8]) {
    if let Ok(str_ref) = std::str::from_utf8(bytes) {
        println!("{}", str_ref);
    } else {
        println!();
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_print_cstr(ptr: *const i8) {
    // SAFETY: Caller guarantees `ptr` is null or a valid NUL-terminated C string.
    if unsafe { crate::ffi_util::with_cstr_bytes(ptr, print_utf8_bytes) }.is_none() {
        println!();
    }
}

fn eprint_utf8_bytes(bytes: &[u8]) {
    if let Ok(str_ref) = std::str::from_utf8(bytes) {
        eprintln!("{}", str_ref);
    } else {
        eprintln!();
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_print_err(ptr: *const i8) {
    // SAFETY: Caller guarantees `ptr` is null or a valid NUL-terminated C string.
    if unsafe { crate::ffi_util::with_cstr_bytes(ptr, eprint_utf8_bytes) }.is_none() {
        eprintln!();
    }
}

fn report_main_error_msg(msg: &str) {
    eprintln!("error: {}", msg);
}

/// Report an error escaping `main` and exit nonzero. The emitter calls this on
/// main's error-return paths, because the entry glue pins main's process exit
/// code to 0. Without this, a program that failed out of main looked like it
/// succeeded. `is_string` says whether `err` is a string handle; a typed error
/// gets a generic line rather than a read through a non-string pointer.
#[no_mangle]
pub unsafe extern "C" fn pith_main_error_exit(err: i64, is_string: i64) -> i64 {
    let printed = if is_string != 0 && err != 0 {
        // SAFETY: When `is_string != 0` and `err != 0`, emitter passes a valid C string handle.
        unsafe { crate::ffi_util::with_cstr_str(err as *const i8, report_main_error_msg) }.is_some()
    } else {
        false
    };
    if !printed {
        eprintln!("error: main returned an error");
    }
    // panic-guard: main returned an error; this is the process exit status for it.
    std::process::exit(1);
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
    first as i64
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
struct StaticChar {
    magic: u32,
    zero: u32,
    len: u64,
    bytes: [u8; 8],
}

/// One static string per byte value, so `chr` hands out a string instead of
/// allocating one. http code builds CR, LF and quotes with it per request.
static STATIC_CHARS: [StaticChar; 256] = {
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

// one verdict for the whole process. a built-in assertion ends the process at
// the point it fails, so it never needs to consult this; std.testing's checks
// report and carry on, and this is how one of them that failed still reaches
// the exit code. the test runner forks per test, so "the process" is the child
// running a single test and the flag cannot leak into the next one.
static TEST_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[no_mangle]
pub extern "C" fn pith_assert(cond: i64) {
    if cond == 0 {
        TEST_FAILED.store(true, Ordering::Relaxed);
        let message = "assertion failed";
        record_failure_detail(message);
        eprintln!("{}", message);
    }
}

/// Set or clear the process verdict. `std.testing` records a failure through
/// here so a check it counted still fails the run; clearing is for the tally's
/// own tests, which provoke failures deliberately.
#[no_mangle]
pub extern "C" fn pith_test_set_failed(failed: i64) {
    TEST_FAILED.store(failed != 0, Ordering::Relaxed);
}

// --- test value equality ----------------------------------------------------
//
// assert_eq compares by value. bytes and lists are heap handles, so comparing
// the raw handles would report two equal-content collections as different; these
// walk the contents instead. a handle that does not resolve falls back to
// identity so a malformed value still gives a definite answer.

fn list_eq_scalar_impl(
    x: &crate::collections::list::ListImpl,
    y: &crate::collections::list::ListImpl,
) -> i64 {
    if x.len() != y.len() {
        return 0;
    }
    for i in 0..x.len() {
        if x.get_value(i) != y.get_value(i) {
            return 0;
        }
    }
    1
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_eq_scalar(a: i64, b: i64) -> i64 {
    // SAFETY: `a` and `b` are validated by `with_list_ref2_handle`; shared borrows permit `a == b`.
    unsafe { crate::collections::list::with_list_ref2_handle(a, b, list_eq_scalar_impl) }
        .unwrap_or((a == b) as i64)
}

unsafe fn list_eq_string_impl(
    x: &crate::collections::list::ListImpl,
    y: &crate::collections::list::ListImpl,
) -> i64 {
    if x.len() != y.len() {
        return 0;
    }
    for i in 0..x.len() {
        let pa = x.get_value(i).unwrap_or(0) as *const i8;
        let pb = y.get_value(i).unwrap_or(0) as *const i8;
        // SAFETY: Caller guarantees both lists hold valid C string handles (or null).
        if unsafe { pith_cstring_eq(pa, pb) } == 0 {
            return 0;
        }
    }
    1
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_eq_string(a: i64, b: i64) -> i64 {
    // SAFETY: `a` and `b` are validated by `with_list_ref2_handle`; elements are string handles.
    unsafe { crate::collections::list::with_list_ref2_handle(a, b, |x, y| list_eq_string_impl(x, y)) }
        .unwrap_or((a == b) as i64)
}

fn format_bytes_hex(data: &[u8]) -> String {
    let mut s = String::from("0x");
    for byte in data {
        s.push_str(&format!("{:02x}", byte));
    }
    s
}

unsafe fn bytes_hex(handle: i64) -> String {
    // SAFETY: `handle` is validated by `with_bytes` before borrowing its slice.
    unsafe { crate::bytes::with_bytes(handle, format_bytes_hex) }
        .unwrap_or_else(|| format!("<bytes {}>", handle))
}

fn list_values_copy(list: &crate::collections::list::ListImpl) -> Vec<i64> {
    (0..list.len()).map(|i| list.get_value(i).unwrap_or(0)).collect()
}

unsafe fn list_display(handle: i64, string_elems: bool) -> String {
    // SAFETY: `handle` is validated by `with_list_ref_handle` before reading elements.
    let Some(values) = (unsafe { crate::collections::list::with_list_ref_handle(handle, list_values_copy) }) else {
        return format!("<list {}>", handle);
    };
    let mut s = String::from("[");
    for (i, v) in values.into_iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        if string_elems {
            // SAFETY: Caller guarantees elements are valid C string handles when `string_elems`.
            let text = unsafe { cstr_to_display(v as *const i8) };
            s.push_str(&format!("\"{}\"", text));
        } else {
            s.push_str(&v.to_string());
        }
    }
    s.push(']');
    s
}

/// One side of an assertion failure, decoded per `kind`
/// (0 int, 1 float, 2 string, 3 bytes, 4 bool, 5 int-list, 6 string-list).
unsafe fn assert_operand(v: i64, kind: i64) -> String {
    // SAFETY: Caller guarantees `v` is a valid value of the given `kind`.
    match kind {
        1 => f64::from_bits(v as u64).to_string(),
        2 => format!("\"{}\"", unsafe { cstr_to_display(v as *const i8) }),
        3 => unsafe { bytes_hex(v) },
        4 => if v != 0 { "true".to_string() } else { "false".to_string() },
        5 => unsafe { list_display(v, false) },
        6 => unsafe { list_display(v, true) },
        _ => v.to_string(),
    }
}

/// Report an assert_eq failure with both sides decoded per `kind`.
///
/// # Safety
/// `a`/`b` must be valid values of the given `kind` for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_assert_eq_fail(a: i64, b: i64, kind: i64) {
    TEST_FAILED.store(true, Ordering::Relaxed);
    // SAFETY: Preconditions forwarded from caller.
    let (lhs, rhs) = unsafe { (assert_operand(a, kind), assert_operand(b, kind)) };
    let message = format!("assertion failed: {} != {}", lhs, rhs);
    record_failure_detail(&message);
    eprintln!("{}", message);
}

/// Report an assert_ne failure. Both sides are shown even though they compared
/// equal: which of two supposedly different values the pair collapsed to is the
/// first thing anyone reading the failure wants to know.
///
/// # Safety
/// `a`/`b` must be valid values of the given `kind` for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_assert_ne_fail(a: i64, b: i64, kind: i64) {
    TEST_FAILED.store(true, Ordering::Relaxed);
    // SAFETY: Preconditions forwarded from caller.
    let (lhs, rhs) = unsafe { (assert_operand(a, kind), assert_operand(b, kind)) };
    let message = format!("assertion failed: {} == {}", lhs, rhs);
    record_failure_detail(&message);
    eprintln!("{}", message);
}

// --- test runner ------------------------------------------------------------
//
// each `test "..."` block compiles to a `__test_i` function; the generated main
// is a pure dispatcher that forks once per test and records the outcome. running
// each test in its own child means a failing assert's exit(1), or even a crash,
// ends only that test. the others still run, and the parent reports every one.
//
// a run reports in one of two shapes. the default is the prose a person reads in
// a terminal. with PITH_TEST_JSON set (`pith test --json`) every result is one
// json object on a line of its own instead, because a run is a stream produced
// across process boundaries: the child prints its own rows and its own skip, the
// parent prints the verdict once the child is gone, and a test that crashes
// still leaves every record written before it. one json document could not be
// closed by a process that died, and buffering the whole run to close it would
// throw away the streaming the fork-per-test design is built on.

static TEST_PASS: AtomicUsize = AtomicUsize::new(0);
static TEST_FAIL: AtomicUsize = AtomicUsize::new(0);
static TEST_FILTERED: AtomicUsize = AtomicUsize::new(0);
static TEST_SKIPPED: AtomicUsize = AtomicUsize::new(0);

// the name of the test running in the current child, so skip_test can print a
// full result line. set once at the top of each child.
static CURRENT_TEST: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

// the tags of the test being considered, comma-joined, and the file the build
// was made from. both are set in the parent before it forks, so a child sees
// them without being told again.
static CURRENT_TAGS: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
static TEST_FILE: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

// every tag any test in this build carries. a run that selects or excludes a
// tag no test has is a mistake in the invocation. a typo that silently runs
// nothing is the failure mode a tagged suite invites, so the summary names it
// and the run fails.
static SEEN_TAGS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

// when the current test started, stamped before the fork so the child measures
// the same span the parent does.
static TEST_START: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

// where the assertion about to fail sits, recorded by the emitted code on the
// failing branch. only a test build emits it, so an ordinary program pays
// nothing for it.
static FAIL_SITE: std::sync::Mutex<Option<(String, i64, i64)>> = std::sync::Mutex::new(None);

// the pipe carrying one failure's detail from the child back to the parent.
// the message and its position are known where the assertion failed, and the
// record is written where the exit status is known; a pipe is the shortest
// path between the two that does not touch the filesystem. the read end is
// non-blocking and the child is already gone when it is read, so nothing here
// can wedge the run.
static DETAIL_WRITE_FD: AtomicI32 = AtomicI32::new(-1);
static DETAIL_READ_FD: AtomicI32 = AtomicI32::new(-1);
static DETAIL_WRITTEN: AtomicBool = AtomicBool::new(false);
const DETAIL_MAX: usize = 4096;

// a child that skipped exits with this code; the parent tells it apart from a
// pass (0) or a failure (anything else).
const TEST_SKIP_CODE: i32 = 42;

fn locked_string(cell: &std::sync::Mutex<String>) -> String {
    cell.lock().map(|value| value.clone()).unwrap_or_default()
}

/// Whether this run reports json records rather than prose.
fn test_json_mode() -> bool {
    match std::env::var("PITH_TEST_JSON") {
        Ok(value) => !value.is_empty() && value != "0",
        Err(_) => false,
    }
}

fn json_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn json_string(text: &str) -> String {
    format!("\"{}\"", json_escape(text))
}

fn json_string_array(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|item| json_string(item)).collect();
    format!("[{}]", quoted.join(","))
}

fn json_message(message: Option<&str>) -> String {
    match message {
        Some(text) => json_string(text),
        None => "null".to_string(),
    }
}

fn json_position(site: &Option<(String, i64, i64)>) -> String {
    match site {
        Some((file, line, col)) => format!(
            "{{\"file\":{},\"line\":{},\"col\":{}}}",
            json_string(file),
            line,
            col
        ),
        None => "null".to_string(),
    }
}

/// The tags in a comma-joined list, empties dropped.
fn tag_list(tags: &str) -> Vec<String> {
    tags.split(',')
        .map(|tag| tag.trim())
        .filter(|tag| !tag.is_empty())
        .map(|tag| tag.to_string())
        .collect()
}

fn env_tag_list(name: &str) -> Vec<String> {
    match std::env::var(name) {
        Ok(value) => tag_list(&value),
        Err(_) => Vec::new(),
    }
}

fn note_seen_tags(tags: &[String]) {
    if let Ok(mut seen) = SEEN_TAGS.lock() {
        for tag in tags {
            if !seen.contains(tag) {
                seen.push(tag.clone());
            }
        }
    }
}

/// Whether the tags a test carries pass the tags this run selected and
/// excluded. With nothing selected every test qualifies; with a selection a
/// test needs one of the selected tags; an excluded tag removes it either way,
/// so an exclusion beats a selection when a test carries both.
fn tags_admit(own: &[String]) -> bool {
    let selected = env_tag_list("PITH_TEST_TAGS");
    let excluded = env_tag_list("PITH_TEST_EXCLUDE_TAGS");
    if !selected.is_empty() && !own.iter().any(|tag| selected.contains(tag)) {
        return false;
    }
    !own.iter().any(|tag| excluded.contains(tag))
}

/// The tags this run named that no test in the build carries.
fn unmatched_tags() -> Vec<String> {
    let seen = SEEN_TAGS.lock().map(|tags| tags.clone()).unwrap_or_default();
    let mut named = env_tag_list("PITH_TEST_TAGS");
    for tag in env_tag_list("PITH_TEST_EXCLUDE_TAGS") {
        if !named.contains(&tag) {
            named.push(tag);
        }
    }
    named.retain(|tag| !seen.contains(tag));
    named
}

fn elapsed_ms() -> u64 {
    let started = TEST_START.lock().map(|start| *start).unwrap_or(None);
    match started {
        Some(instant) => instant.elapsed().as_millis() as u64,
        None => 0,
    }
}

/// One test's result as a json object on a line of its own.
fn print_test_record(
    name: &str,
    outcome: &str,
    duration_ms: u64,
    message: Option<&str>,
    site: &Option<(String, i64, i64)>,
) {
    println!(
        "{{\"type\":\"test\",\"name\":{},\"file\":{},\"tags\":{},\"outcome\":\"{}\",\"duration_ms\":{},\"message\":{},\"position\":{}}}",
        json_string(name),
        json_string(&locked_string(&TEST_FILE)),
        json_string_array(&tag_list(&locked_string(&CURRENT_TAGS))),
        outcome,
        duration_ms,
        json_message(message),
        json_position(site)
    );
}

/// One check or table row as a json object. A row belongs to the test that
/// holds it, which is why it names that test and carries its file and tags: a
/// reader grouping records by test needs no second lookup, and `name` alone
/// would not say which test a row labeled `[3] gamma` came from. Outside a
/// test (`std.testing` in a plain `main()`), the test name is empty.
fn print_case_record(name: &str, ok: bool, detail: &str) {
    let outcome = if ok { "passed" } else { "failed" };
    let message = if ok { None } else { Some(detail) };
    println!(
        "{{\"type\":\"case\",\"name\":{},\"test\":{},\"file\":{},\"tags\":{},\"outcome\":\"{}\",\"message\":{}}}",
        json_string(name),
        json_string(&locked_string(&CURRENT_TEST)),
        json_string(&locked_string(&TEST_FILE)),
        json_string_array(&tag_list(&locked_string(&CURRENT_TAGS))),
        outcome,
        json_message(message)
    );
}

/// Send one failure's message and position to the parent. The first failure
/// wins: a child stops at its first built-in assertion anyway, and a run of
/// recorded checks is best summarised by the one that failed first.
fn record_failure_detail(message: &str) {
    if DETAIL_WRITTEN.swap(true, Ordering::Relaxed) {
        return;
    }
    let fd = DETAIL_WRITE_FD.load(Ordering::Relaxed);
    if fd < 0 {
        return;
    }
    let site = FAIL_SITE.lock().map(|site| site.clone()).unwrap_or(None);
    let (file, line, col) = site.unwrap_or((String::new(), 0, 0));
    // file, line and col first, message last: the message is whatever the
    // assertion printed and may hold anything, including newlines, and reading
    // it as the remainder needs no escaping on either side.
    let mut payload = format!("{}\n{}\n{}\n{}", file, line, col, message);
    if payload.len() > DETAIL_MAX {
        let mut cut = DETAIL_MAX;
        while cut > 0 && !payload.is_char_boundary(cut) {
            cut -= 1;
        }
        payload.truncate(cut);
    }
    unsafe {
        libc::write(fd, payload.as_ptr() as *const libc::c_void, payload.len());
    }
}

/// Parent side: the failure detail the child sent, if it sent one. Always
/// called once per test, pass or fail, because it is also what closes the read
/// end; a suite of a few hundred tests would otherwise run out of descriptors.
fn read_failure_detail() -> Option<(String, Option<(String, i64, i64)>)> {
    let fd = DETAIL_READ_FD.swap(-1, Ordering::Relaxed);
    if fd < 0 {
        return None;
    }
    let mut buf = [0u8; DETAIL_MAX];
    let mut text: Vec<u8> = Vec::new();
    loop {
        let read = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if read <= 0 {
            break;
        }
        text.extend_from_slice(&buf[..read as usize]);
        if text.len() >= DETAIL_MAX {
            break;
        }
    }
    unsafe {
        libc::close(fd);
    }
    if text.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&text).to_string();
    let mut parts = text.splitn(4, '\n');
    let file = parts.next().unwrap_or("").to_string();
    let line: i64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let col: i64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let message = parts.next().unwrap_or("").to_string();
    let site = if file.is_empty() {
        None
    } else {
        Some((file, line, col))
    };
    Some((message, site))
}

/// Record the name of the test about to run in this child.
///
/// # Safety
/// `name` must point to a valid NUL-terminated string for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_test_enter(name: *const i8) -> i64 {
    // SAFETY: Caller guarantees `name` is a valid NUL-terminated string.
    let name = unsafe { cstr_to_display(name) };
    if let Ok(mut current) = CURRENT_TEST.lock() {
        *current = name;
    }
    0
}

/// Record the source file this test build was made from. The generated main
/// calls it once, before any test runs: the binary has no other way to know
/// what it was built from, and every record names the file.
///
/// # Safety
/// `file` must point to a valid NUL-terminated string for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_test_set_file(file: *const i8) -> i64 {
    // SAFETY: Caller guarantees `file` is a valid NUL-terminated string.
    let file = unsafe { cstr_to_display(file) };
    if let Ok(mut current) = TEST_FILE.lock() {
        *current = file;
    }
    0
}

/// Record where the assertion about to fail sits, so its record carries a
/// position and not only a message.
///
/// # Safety
/// `file` must point to a valid NUL-terminated string for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_test_fail_site(file: *const i8, line: i64, col: i64) {
    // SAFETY: Caller guarantees `file` is a valid NUL-terminated string.
    let file = unsafe { cstr_to_display(file) };
    if let Ok(mut site) = FAIL_SITE.lock() {
        *site = Some((file, line, col));
    }
}

/// Report one check or one table row. `std.testing` counts its own results and
/// calls this to print them, so the prose and the json shapes of a row are
/// decided in the one place the test's own result is decided.
///
/// # Safety
/// `name` and `detail` must point to valid NUL-terminated strings for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_test_report_case(name: *const i8, ok: i64, detail: *const i8) {
    // SAFETY: Caller guarantees `name` and `detail` are valid NUL-terminated strings.
    let (name, detail) = unsafe { (cstr_to_display(name), cstr_to_display(detail)) };
    if ok == 0 {
        record_failure_detail(&format!("{} -- {}", name, detail));
    }
    if test_json_mode() {
        print_case_record(&name, ok != 0, &detail);
        return;
    }
    if ok != 0 {
        println!("  ok   {}", name);
    } else {
        println!("  FAIL {} -- {}", name, detail);
    }
}

/// Skip the current test with a reason: print the result line and exit the child
/// with the skip code, which the parent tallies as skipped rather than run.
///
/// # Safety
/// `reason` must point to a valid NUL-terminated string for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_test_skip(reason: *const i8) -> i64 {
    use std::io::Write;
    let name = locked_string(&CURRENT_TEST);
    // SAFETY: Caller guarantees `reason` is a valid NUL-terminated string.
    let reason = unsafe { cstr_to_display(reason) };
    if test_json_mode() {
        print_test_record(&name, "skipped", elapsed_ms(), Some(&reason), &None);
    } else {
        println!("  {} ... skipped ({})", name, reason);
    }
    let _ = std::io::stdout().flush();
    // panic-guard: a skipped test exits with the skip code the harness reads.
    std::process::exit(TEST_SKIP_CODE);
}

/// The name of the test running in this child, or the empty string outside one.
/// `std.testing` builds a table row's identity on top of it, which is what lets
/// a row name itself in the output and be reachable by `--filter`.
#[no_mangle]
pub extern "C" fn pith_test_current_name() -> *const i8 {
    let name = locked_string(&CURRENT_TEST);
    pith_strdup_string(&name)
}

/// The separator between a test's name and the label of one case inside it.
/// A filter naming a case begins with the test's name and this, which is how
/// the parent knows to run a test whose own name does not match.
pub const TEST_CASE_SEPARATOR: &str = " / ";

fn filtered_out(name: &str) -> i64 {
    TEST_FILTERED.fetch_add(1, Ordering::Relaxed);
    if test_json_mode() {
        print_test_record(name, "filtered", 0, None, &None);
    }
    0
}

/// Whether a test should run under this invocation's tags and
/// `PITH_TEST_FILTER`. With nothing selected every test runs; otherwise only
/// names containing the substring do, and the rest are tallied as filtered out.
///
/// A filter can also name one case inside a table-driven test, as `"the test
/// name / a case label"`. The test's own name does not contain that, so
/// matching on the substring alone would filter the test out and the case
/// would never get the chance to select itself. A filter that starts with this
/// test's name and the case separator is aimed inside it, so the test runs and
/// `std.testing`'s `each` applies the same filter to each row's identity.
///
/// Tags and the filter compose as one conjunction: the tags say which tests
/// this run is about, the filter picks a name out of those. Neither widens the
/// other, so `--tag slow --filter parse` is the slow tests whose name contains
/// parse and nothing else.
///
/// # Safety
/// `name` and `tags` must point to valid NUL-terminated strings for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_test_should_run(name: *const i8, tags: *const i8) -> i64 {
    // SAFETY: Caller guarantees `name` and `tags` are valid NUL-terminated strings.
    let (name, tags) = unsafe { (cstr_to_display(name), cstr_to_display(tags)) };
    if let Ok(mut current) = CURRENT_TAGS.lock() {
        *current = tags.clone();
    }
    let own = tag_list(&tags);
    note_seen_tags(&own);
    if !tags_admit(&own) {
        return filtered_out(&name);
    }
    let filter = match std::env::var("PITH_TEST_FILTER") {
        Ok(f) if !f.is_empty() => f,
        _ => return 1,
    };
    let aimed_inside =
        !name.is_empty() && filter.starts_with(&format!("{}{}", name, TEST_CASE_SEPARATOR));
    if name.contains(&filter) || aimed_inside {
        1
    } else {
        filtered_out(&name)
    }
}

/// Flush pending output and fork. Returns the child pid to the parent and 0 to
/// the child, matching the C fork contract. Flushing first keeps the child from
/// inheriting (and re-emitting) the parent's buffered stdout. The clock starts
/// and the failure-detail pipe opens here, before the fork, so both sides of it
/// measure and report the same test.
#[no_mangle]
pub extern "C" fn pith_test_fork() -> i64 {
    use std::io::Write;
    if let Ok(mut start) = TEST_START.lock() {
        *start = Some(std::time::Instant::now());
    }
    let mut fds: [libc::c_int; 2] = [-1, -1];
    let piped = unsafe { libc::pipe(fds.as_mut_ptr()) } == 0;
    if piped {
        unsafe {
            // a subprocess a test starts must not inherit the write end, or the
            // parent's read would wait on a descriptor nobody is going to close
            libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(fds[1], libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK);
        }
    }
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    let pid = unsafe { libc::fork() as i64 };
    if !piped {
        return pid;
    }
    if pid < 0 {
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
        DETAIL_READ_FD.store(-1, Ordering::Relaxed);
        return -1;
    }
    if pid == 0 {
        unsafe {
            libc::close(fds[0]);
        }
        DETAIL_READ_FD.store(-1, Ordering::Relaxed);
        DETAIL_WRITTEN.store(false, Ordering::Relaxed);
        DETAIL_WRITE_FD.store(fds[1], Ordering::Relaxed);
    } else {
        unsafe {
            libc::close(fds[1]);
        }
        DETAIL_READ_FD.store(fds[0], Ordering::Relaxed);
    }
    pid
}

/// Called at the end of a test in the child process: flush and exit with the
/// process verdict. A built-in assertion that failed has already exited(1)
/// before reaching here; a `std.testing` check that failed only recorded it,
/// and this is where that recording becomes a failed test rather than a line
/// of output nobody counts.
#[no_mangle]
pub extern "C" fn pith_test_child_ok() -> i64 {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    if TEST_FAILED.load(Ordering::Relaxed) {
        // panic-guard: a failing test child exits non-zero for the harness.
        std::process::exit(1);
    }
    // panic-guard: a passing test child exits zero for the harness.
    std::process::exit(0);
}

/// Parent side: wait for a test's child, print its result line, and tally it. A
/// clean exit(0) passes; any non-zero exit or a killing signal fails.
///
/// # Safety
/// `name` must point to a valid NUL-terminated string for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_test_record(name: *const i8, pid: i64) -> i64 {
    // SAFETY: Caller guarantees `name` is a valid NUL-terminated string.
    let name_str = unsafe { cstr_to_display(name) };
    let duration = elapsed_ms();
    let json = test_json_mode();
    if pid <= 0 {
        let _ = read_failure_detail();
        TEST_FAIL.fetch_add(1, Ordering::Relaxed);
        if json {
            print_test_record(&name_str, "failed", duration, Some("fork failed"), &None);
        } else {
            println!("  {} ... FAILED (fork failed)", name_str);
        }
        return 1;
    }
    let mut status: i32 = 0;
    // SAFETY: `status` is a valid out-pointer for `waitpid`.
    let waited = loop {
        let rc = unsafe { libc::waitpid(pid as i32, &mut status, 0) };
        if rc < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        break rc;
    };
    // read unconditionally: this is also what closes the read end of the pipe
    let detail = read_failure_detail();
    if waited < 0 {
        TEST_FAIL.fetch_add(1, Ordering::Relaxed);
        if json {
            print_test_record(&name_str, "failed", duration, Some("waitpid failed"), &None);
        } else {
            println!("  {} ... FAILED (waitpid failed)", name_str);
        }
        return 1;
    }
    if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == TEST_SKIP_CODE {
        // the child already reported its own skip, in whichever shape this run
        // is printing
        TEST_SKIPPED.fetch_add(1, Ordering::Relaxed);
        0
    } else if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
        TEST_PASS.fetch_add(1, Ordering::Relaxed);
        if json {
            print_test_record(&name_str, "passed", duration, None, &None);
        } else {
            println!("  {} ... ok", name_str);
        }
        0
    } else {
        TEST_FAIL.fetch_add(1, Ordering::Relaxed);
        let signalled = libc::WIFSIGNALED(status);
        if json {
            let (message, site) = match detail {
                Some((message, site)) => (message, site),
                None => {
                    let message = if signalled {
                        format!("killed by signal {}", libc::WTERMSIG(status))
                    } else {
                        format!("exited with status {}", libc::WEXITSTATUS(status))
                    };
                    (message, None)
                }
            };
            print_test_record(&name_str, "failed", duration, Some(&message), &site);
        } else if signalled {
            println!(
                "  {} ... FAILED (killed by signal {})",
                name_str,
                libc::WTERMSIG(status)
            );
        } else {
            println!("  {} ... FAILED", name_str);
        }
        1
    }
}

/// Print the final tally and return the process exit code (non-zero on any
/// failure, and on a tag this run named that no test carries).
#[no_mangle]
pub extern "C" fn pith_test_summary() -> i64 {
    let passed = TEST_PASS.load(Ordering::Relaxed);
    let failed = TEST_FAIL.load(Ordering::Relaxed);
    let filtered = TEST_FILTERED.load(Ordering::Relaxed);
    let skipped = TEST_SKIPPED.load(Ordering::Relaxed);
    let unmatched = unmatched_tags();
    if test_json_mode() {
        println!(
            "{{\"type\":\"summary\",\"passed\":{},\"failed\":{},\"skipped\":{},\"filtered_out\":{},\"unmatched_tags\":{}}}",
            passed,
            failed,
            skipped,
            filtered,
            json_string_array(&unmatched)
        );
    } else {
        println!();
        let mut line = format!("{} passed, {} failed", passed, failed);
        if skipped > 0 {
            line.push_str(&format!(", {} skipped", skipped));
        }
        if filtered > 0 {
            line.push_str(&format!(", {} filtered out", filtered));
        }
        println!("{}", line);
        for tag in &unmatched {
            println!("no test carries the tag \"{}\"", tag);
        }
    }
    if failed > 0 || !unmatched.is_empty() {
        1
    } else {
        0
    }
}

#[no_mangle]
pub extern "C" fn pith_bit_and(a: i64, b: i64) -> i64 {
    a & b
}

#[no_mangle]
pub extern "C" fn pith_bit_or(a: i64, b: i64) -> i64 {
    a | b
}

#[no_mangle]
pub extern "C" fn pith_bit_xor(a: i64, b: i64) -> i64 {
    a ^ b
}

#[no_mangle]
pub extern "C" fn pith_bit_not(a: i64) -> i64 {
    !a
}

// the error an integer that does not fit raises, the same one the code
// generator's overflow trap reports (overflow_trap.rs).
fn integer_overflow() -> ! {
    crate::failure::fail("integer overflow".to_string())
}

// a shift count outside 0..63 stops the program rather than being taken mod
// 64. codegen lowers calls to these to the same check and one shift
// instruction; these are the fallback with the same result.
#[no_mangle]
pub extern "C" fn pith_bit_shl(a: i64, b: i64) -> i64 {
    if !(0..64).contains(&b) {
        integer_overflow();
    }
    a << b
}

#[no_mangle]
pub extern "C" fn pith_bit_shr(a: i64, b: i64) -> i64 {
    if !(0..64).contains(&b) {
        integer_overflow();
    }
    ((a as u64) >> b) as i64
}

// the rotates work on the low 32 or all 64 bits of `a`, with the amount taken
// mod the width; a 32-bit result is zero-extended. codegen lowers calls to
// these to one rotate instruction, and these are the fallback with the same
// result.
#[no_mangle]
pub extern "C" fn pith_bit_rotl32(a: i64, b: i64) -> i64 {
    (a as u32).rotate_left(b as u32 & 31) as i64
}

#[no_mangle]
pub extern "C" fn pith_bit_rotr32(a: i64, b: i64) -> i64 {
    (a as u32).rotate_right(b as u32 & 31) as i64
}

#[no_mangle]
pub extern "C" fn pith_bit_rotl64(a: i64, b: i64) -> i64 {
    (a as u64).rotate_left(b as u32 & 63) as i64
}

#[no_mangle]
pub extern "C" fn pith_bit_rotr64(a: i64, b: i64) -> i64 {
    (a as u64).rotate_right(b as u32 & 63) as i64
}

// the conversions from Int. `UInt8(x)` and the rest stop the program when x
// does not fit the target type, and the `wrapping_*` forms keep the low bits
// of the two's complement value. a narrow value is held sign or zero extended
// to 64 bits and an unsigned word as the bits of an i64. codegen inlines all
// of these, and these are the fallback with the same result.
fn fit(n: i64, lo: i64, hi: i64) -> i64 {
    if n < lo || n > hi {
        integer_overflow();
    }
    n
}

#[no_mangle]
pub extern "C" fn pith_uint(n: i64) -> i64 {
    fit(n, 0, i64::MAX)
}

#[no_mangle]
pub extern "C" fn pith_int8(n: i64) -> i64 {
    fit(n, i8::MIN as i64, i8::MAX as i64)
}

#[no_mangle]
pub extern "C" fn pith_int16(n: i64) -> i64 {
    fit(n, i16::MIN as i64, i16::MAX as i64)
}

#[no_mangle]
pub extern "C" fn pith_int32(n: i64) -> i64 {
    fit(n, i32::MIN as i64, i32::MAX as i64)
}

#[no_mangle]
pub extern "C" fn pith_int64(n: i64) -> i64 {
    n
}

#[no_mangle]
pub extern "C" fn pith_uint8(n: i64) -> i64 {
    fit(n, 0, u8::MAX as i64)
}

#[no_mangle]
pub extern "C" fn pith_uint16(n: i64) -> i64 {
    fit(n, 0, u16::MAX as i64)
}

#[no_mangle]
pub extern "C" fn pith_uint32(n: i64) -> i64 {
    fit(n, 0, u32::MAX as i64)
}

#[no_mangle]
pub extern "C" fn pith_uint64(n: i64) -> i64 {
    fit(n, 0, i64::MAX)
}

#[no_mangle]
pub extern "C" fn pith_wrapping_int8(n: i64) -> i64 {
    (n as i8) as i64
}

#[no_mangle]
pub extern "C" fn pith_wrapping_int16(n: i64) -> i64 {
    (n as i16) as i64
}

#[no_mangle]
pub extern "C" fn pith_wrapping_int32(n: i64) -> i64 {
    (n as i32) as i64
}

#[no_mangle]
pub extern "C" fn pith_wrapping_uint8(n: i64) -> i64 {
    (n as u8) as i64
}

#[no_mangle]
pub extern "C" fn pith_wrapping_uint16(n: i64) -> i64 {
    (n as u16) as i64
}

#[no_mangle]
pub extern "C" fn pith_wrapping_uint32(n: i64) -> i64 {
    (n as u32) as i64
}

#[no_mangle]
pub extern "C" fn pith_wrapping_uint64(n: i64) -> i64 {
    n
}

#[no_mangle]
pub extern "C" fn pith_abs(n: i64) -> i64 {
    match n.checked_abs() {
        Some(v) => v,
        None => integer_overflow(),
    }
}

#[no_mangle]
pub extern "C" fn pith_min(a: i64, b: i64) -> i64 {
    if a < b {
        a
    } else {
        b
    }
}

#[no_mangle]
pub extern "C" fn pith_max(a: i64, b: i64) -> i64 {
    if a > b {
        a
    } else {
        b
    }
}

#[no_mangle]
pub extern "C" fn pith_clamp(n: i64, min: i64, max: i64) -> i64 {
    if n < min {
        min
    } else if n > max {
        max
    } else {
        n
    }
}

#[no_mangle]
pub extern "C" fn pith_pow(a: f64, b: f64) -> f64 {
    a.powf(b)
}

#[no_mangle]
pub extern "C" fn pith_sqrt(n: f64) -> f64 {
    n.sqrt()
}

#[no_mangle]
pub extern "C" fn pith_floor(n: f64) -> f64 {
    n.floor()
}

#[no_mangle]
pub extern "C" fn pith_ceil(n: f64) -> f64 {
    n.ceil()
}

#[no_mangle]
pub extern "C" fn pith_round(n: f64) -> f64 {
    n.round()
}

#[no_mangle]
pub extern "C" fn pith_sin(n: f64) -> f64 {
    n.sin()
}

#[no_mangle]
pub extern "C" fn pith_cos(n: f64) -> f64 {
    n.cos()
}

#[no_mangle]
pub extern "C" fn pith_tan(n: f64) -> f64 {
    n.tan()
}

#[no_mangle]
pub extern "C" fn pith_asin(n: f64) -> f64 {
    n.asin()
}

#[no_mangle]
pub extern "C" fn pith_acos(n: f64) -> f64 {
    n.acos()
}

#[no_mangle]
pub extern "C" fn pith_atan(n: f64) -> f64 {
    n.atan()
}

#[no_mangle]
pub extern "C" fn pith_atan2(y: f64, x: f64) -> f64 {
    y.atan2(x)
}

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
fn int_text_len(n: i64) -> usize {
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

fn format_float(n: f64) -> String {
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

// --- container display -------------------------------------------------
//
// interpolating a list or map renders it through these. the emitter passes
// an element-kind code because the runtime cannot tell an int list from a
// float list (both store 8-byte values): 0=int, 1=float, 2=bool, 3=string.

pub(crate) unsafe fn display_value_for_map(out: &mut String, raw: i64, kind: i64) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { display_value(out, raw, kind) }
}

unsafe fn display_value(out: &mut String, raw: i64, kind: i64) {
    match kind {
        1 => out.push_str(&format_float(f64::from_bits(raw as u64))),
        2 => out.push_str(if raw != 0 { "true" } else { "false" }),
        // SAFETY: Caller guarantees `raw` is a valid C string handle when `kind == 3`.
        3 => out.push_str(&unsafe { cstr_to_display(raw as *const i8) }),
        _ => out.push_str(&raw.to_string()),
    }
}

unsafe fn cstr_to_display(ptr: *const i8) -> String {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { crate::ffi_util::with_cstr_bytes(ptr, preview_lossy) }.unwrap_or_default()
}

fn collect_list_values(list: &crate::collections::list::ListImpl) -> Vec<i64> {
    let mut values = Vec::with_capacity(list.len());
    for i in 0..list.len() {
        if let Some(raw) = list.get_value(i) {
            values.push(raw);
        }
    }
    values
}

/// Render a list as "[a, b, c]" for interpolation. `sorted` is set when the
/// source is a set: hash iteration order is not part of the language, so a
/// displayed set sorts its elements.
#[no_mangle]
pub unsafe extern "C" fn pith_display_list(handle: i64, elem_kind: i64, sorted: i64) -> *mut i8 {
    // SAFETY: `handle` is validated by `with_list_ref_handle` before reading elements.
    let mut values = unsafe { crate::collections::list::with_list_ref_handle(handle, collect_list_values) }
        .unwrap_or_default();
    if sorted != 0 {
        match elem_kind {
            1 => values.sort_by(|a, b| {
                f64::from_bits(*a as u64)
                    .partial_cmp(&f64::from_bits(*b as u64))
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            3 => values.sort_by_key(|v| {
                // SAFETY: Caller guarantees elements are valid C strings when `elem_kind == 3`.
                unsafe { cstr_to_display(*v as *const i8) }
            }),
            _ => values.sort(),
        }
    }
    let mut out = String::from("[");
    for (i, raw) in values.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        // SAFETY: Caller guarantees elements match `elem_kind`.
        unsafe { display_value(&mut out, *raw, elem_kind) };
    }
    out.push(']');
    pith_copy_bytes_to_cstring(out.as_bytes())
}

pub use pith_ceil as pith_math_ceil;
pub use pith_floor as pith_math_floor;
pub use pith_pow as pith_math_pow;
pub use pith_round as pith_math_round;
pub use pith_sqrt as pith_math_sqrt;

#[no_mangle]
pub unsafe extern "C" fn pith_free(ptr: *mut i8) {
    use std::alloc::Layout;

    if !ptr.is_null() {
        // SAFETY: Caller guarantees `ptr` was allocated with `Layout::new::<u8>()`.
        unsafe { std::alloc::dealloc(ptr as *mut u8, Layout::new::<u8>()) };
    }
}

#[no_mangle]
pub extern "C" fn pith_int_to_float(n: i64) -> f64 {
    n as f64
}

#[no_mangle]
pub extern "C" fn pith_float_to_int(n: f64) -> i64 {
    // truncates toward zero. NaN and a value outside Int's range stop the
    // program instead of turning into 0 or the nearest end of the range.
    // -2^63 converts exactly, and 2^63 is the first value past the top.
    if n.is_nan() {
        crate::failure::fail("NaN converted to an integer".to_string());
    }
    if !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&n) {
        integer_overflow();
    }
    n as i64
}

// Reinterpret the raw bits, not the numeric value: the bit pattern of a float
// read as an integer and vice versa. `to_float`/`to_int` above convert values
// (5 -> 5.0); these preserve bits, which is what binary wire formats need.
#[no_mangle]
pub extern "C" fn pith_float_from_bits(bits: i64) -> f64 {
    f64::from_bits(bits as u64)
}

#[no_mangle]
pub extern "C" fn pith_float_to_bits(value: f64) -> i64 {
    value.to_bits() as i64
}

#[no_mangle]
pub extern "C" fn pith_second(_a: i64, b: i64) -> i64 {
    b
}

// Thread-local storage for `threadlocal` module globals. Each os thread gets its
// own value per slot; the compiler assigns each threadlocal global a unique i64
// slot and generates a `() -> i64` init thunk that builds the correctly-typed
// initial value. A slot is materialized lazily the first time a thread touches
// it. Each value carries the element tag of its type, so the counts a task's
// or an os thread's values hold are dropped when that task or thread finishes
// (release_finished_tls); the main thread's live for the process.
//
// The compiler numbers the slots densely from zero, so a thread's values live
// in a vector indexed by slot. std.json reads its parser state from
// threadlocals on every byte, and a hash probe per read was a quarter of a
// node-pool parse.
#[derive(Default)]
pub(crate) struct TlsMap {
    values: Vec<Option<(i64, u8)>>,
    /// the innermost failure-isolation frame of the task that owns the map
    /// (see `failure`), or 0. only a green task's map uses it.
    pub(crate) isolation: usize,
}

impl TlsMap {
    pub(crate) const fn new() -> Self {
        TlsMap {
            values: Vec::new(),
            isolation: 0,
        }
    }

    #[inline]
    pub(crate) fn get(&self, slot: &i64) -> Option<i64> {
        self.values.get(*slot as usize)?.map(|(value, _)| value)
    }

    pub(crate) fn insert(&mut self, slot: i64, value: i64, tag: i64) {
        let i = slot as usize;
        if i >= self.values.len() {
            self.values.resize(i + 1, None);
        }
        self.values[i] = Some((value, tag.clamp(0, 255) as u8));
    }

    /// Every value, with its tag, leaving the map empty.
    pub(crate) fn is_empty(&self) -> bool {
        self.values.iter().all(Option::is_none)
    }

    pub(crate) fn take_values(&mut self) -> Vec<(i64, u8)> {
        std::mem::take(&mut self.values).into_iter().flatten().collect()
    }
}

/// Drop the counts a finished task's or thread's threadlocal values held. A
/// destructor this runs sees no task of its own, so a threadlocal it reads
/// resolves to the worker's map, as anything else running there would.
pub(crate) fn release_finished_tls(values: Vec<(i64, u8)>) {
    for (value, tag) in values {
        let tag = crate::collections::list::element_tag_from_code(tag as i32);
        unsafe {
            // the release runs after the task is gone, outside any task, so
            // what it drops is no longer its builder's alone
            crate::share::share_value(value, tag);
            crate::collections::list::release_element(tag, value);
        }
    }
}

/// The os-thread backend's half: a task's thread is about to exit, and its
/// threadlocal map would be dropped with the counts still held.
pub(crate) fn release_thread_tls() {
    release_finished_tls(take_thread_tls());
}

/// the thread-local touch behind its own call, like `tls_globals_get`: no
/// frame that runs pith code may hold the address of `TLS_GLOBALS`
#[inline(never)]
fn take_thread_tls() -> Vec<(i64, u8)> {
    TLS_GLOBALS
        .try_with(|t| t.borrow_mut().take_values())
        .unwrap_or_default()
}

thread_local! {
    static TLS_GLOBALS: std::cell::RefCell<TlsMap> = const { std::cell::RefCell::new(TlsMap::new()) };
}

thread_local! {
    /// The address of this thread's `TLS_GLOBALS` map once the thread has
    /// used it, null before. A plain cell, so a read is one `%fs:`-relative
    /// load: the fast paths below reach the map without the accessor call
    /// and the `RefCell` bookkeeping. It is read afresh on every access and
    /// never kept across pith code, so a task that resumes on another thread
    /// reads that thread's cell.
    static THREAD_TLS: std::cell::Cell<*mut TlsMap> = const { std::cell::Cell::new(std::ptr::null_mut()) };
}

/// The map a threadlocal access goes to right now: the running green task's,
/// or this thread's own once `thread_tls_install` has recorded it, or null.
/// `CURRENT_TLS` is only ever set by the green scheduler, so there is no
/// backend test here.
#[inline(always)]
fn active_tls_map() -> *mut TlsMap {
    match crate::concurrency::green::current_task_tls() {
        Some(map) => map,
        None => THREAD_TLS.with(|c| c.get()),
    }
}

/// Record this thread's map for `active_tls_map`. The address of
/// `TLS_GLOBALS` is taken here, behind a call the optimizer cannot inline,
/// like the other touches of it.
#[inline(never)]
fn thread_tls_install() {
    let map = TLS_GLOBALS.with(|t| t.as_ptr());
    THREAD_TLS.with(|c| c.set(map));
}

/// Under the green backend, the map backing the currently-running task's
/// `threadlocal` globals, or `None` on the default os-thread path (and on any
/// green worker moment with no task active). Returning `None` means the caller
/// falls back to the per-OS-thread `TLS_GLOBALS`, so the os-thread path is
/// untouched: one worker OS thread per task there means per-OS-thread already is
/// per-task. Under green, one worker runs many tasks, so we must route to the
/// task's own map instead of sharing the worker's.
fn green_task_tls() -> Option<*mut TlsMap> {
    use crate::concurrency::scheduler::{backend, Backend};
    if backend() != Backend::Green {
        return None;
    }
    crate::concurrency::green::current_task_tls()
}

/// Read this thread's value for a threadlocal global, initializing it on first
/// access by calling the compiler-generated init thunk. The thunk runs outside
/// the map borrow so it may touch other threadlocal globals without a re-entrant
/// borrow panic.
///
/// # Safety
/// `init_thunk` must be the address of a compiled `extern "C" fn() -> i64`.
#[no_mangle]
pub unsafe extern "C" fn pith_tls_get_or_init(slot: i64, init_thunk: i64) -> i64 {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { pith_tls_get_or_init_tagged(slot, init_thunk, 0) }
}

/// `pith_tls_get_or_init` for a slot whose values carry counts: `tag` is the
/// element tag of the global's type, kept with the value so a finished task's
/// count can be dropped (0 for a plain value).
///
/// # Safety
/// as `pith_tls_get_or_init`.
#[no_mangle]
pub unsafe extern "C" fn pith_tls_get_or_init_tagged(slot: i64, init_thunk: i64, tag: i64) -> i64 {
    // the common read: the slot already holds a value. std.json reads its
    // parser state through here on every byte, so this path is a cell load
    // and a vector index, and everything else is out of line.
    let map = active_tls_map();
    if !map.is_null() {
        // SAFETY: a task's map is owned by the running task and the thread's
        // map by this thread; nothing else holds a borrow of either while
        // pith code runs.
        if let Some(v) = unsafe { (*map).get(&slot) } {
            return v;
        }
    }
    // SAFETY: Preconditions on `init_thunk` forwarded from caller.
    unsafe { tls_get_or_init_slow(slot, init_thunk, tag) }
}

/// The first read of a slot, which runs its initializer, and any read made
/// before this thread's map is recorded.
#[cold]
#[inline(never)]
unsafe fn tls_get_or_init_slow(slot: i64, init_thunk: i64, tag: i64) -> i64 {
    if let Some(map) = green_task_tls() {
        // SAFETY: `map` points into the `Box<TlsMap>` owned by the green task
        // currently running on this worker thread. `green` installs it around
        // `resume` and clears it after, so it is valid for the whole synchronous
        // duration of this pith call, and only this worker thread touches it. we
        // take only short-lived borrows: the read borrow ends before we call the
        // thunk, and the write borrow is created fresh afterward. that mirrors
        // the `TLS_GLOBALS` path: the thunk may itself touch other threadlocal
        // slots on the same map without a live borrow being held across it.
        if let Some(v) = unsafe { (*map).get(&slot) } {
            return v;
        }
        // panic-guard: calling a compiler-emitted threadlocal initializer thunk.
        let thunk: extern "C" fn() -> i64 = unsafe { std::mem::transmute(init_thunk as usize) };
        let value = thunk();
        // SAFETY: Short-lived write borrow after the thunk returns.
        unsafe { (*map).insert(slot, value, tag) };
        return value;
    }

    if let Some(v) = tls_globals_get(slot) {
        return v;
    }
    // panic-guard: calling a compiler-emitted threadlocal initializer thunk.
    let thunk: extern "C" fn() -> i64 = unsafe { std::mem::transmute(init_thunk as usize) };
    let value = thunk();
    // another access on this thread during the thunk can't have run (single
    // thread), so a plain insert is correct.
    tls_globals_set(slot, value, tag);
    thread_tls_install();
    value
}

/// the two touches of the per-os-thread map, each behind a call the optimizer
/// cannot inline. a `RefCell<HashMap>` has to be addressed as a value, and
/// inlined into `pith_tls_get_or_init` that address was computed once and
/// kept in a register across the initializer thunk, which is pith code. the branch
/// is only taken off a green task, where the thunk cannot suspend a
/// coroutine, but the rule that no frame spanning pith code holds a
/// thread-local address is kept without exceptions, so that coupling is not
/// load-bearing. see the accessor block in `concurrency/green.rs`.
#[inline(never)]
fn tls_globals_get(slot: i64) -> Option<i64> {
    TLS_GLOBALS.with(|t| t.borrow().get(&slot))
}

#[inline(never)]
fn tls_globals_set(slot: i64, value: i64, tag: i64) {
    TLS_GLOBALS.with(|t| {
        t.borrow_mut().insert(slot, value, tag);
    });
}

/// Set this thread's value for a threadlocal global (a reassignment). The old
/// value's refcount is dropped by the compiler-emitted release before this call,
/// the same as a plain global assignment.
#[no_mangle]
pub unsafe extern "C" fn pith_tls_set(slot: i64, value: i64) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { pith_tls_set_tagged(slot, value, 0) }
}

/// `pith_tls_set` with the value's element tag (see
/// `pith_tls_get_or_init_tagged`).
///
/// # Safety
/// as `pith_tls_set`.
#[no_mangle]
pub unsafe extern "C" fn pith_tls_set_tagged(slot: i64, value: i64, tag: i64) {
    let map = active_tls_map();
    if !map.is_null() {
        // SAFETY: as in `pith_tls_get_or_init_tagged`: the running task's own
        // map, or this thread's, with no other borrow live.
        unsafe { (*map).insert(slot, value, tag) };
        return;
    }
    tls_globals_set(slot, value, tag);
    thread_tls_install();
}

// Small structs are allocated and freed constantly (the result box built for
// every `T!` return is the worst offender), and the malloc/free round-trip for
// each one shows up plainly in profiles. These keep a per-thread freelist of
// dead blocks, bucketed by total byte size, so a fresh struct of a common size
// reuses a recycled block instead of hitting the global allocator. A reused
// block is re-zeroed, so it is indistinguishable from an alloc_zeroed one.
//
// Per-thread means no locking and no cross-thread races on the fast path: a
// block freed on another thread simply lands in that thread's pool.
//
// The pool is disabled when PITH_STRUCT_FREELIST=0 so `make memcheck` runs see
// every real free: valgrind can only track use-after-free if the block is
// actually handed back to the allocator.
const STRUCT_POOL_MAX_TOTAL: usize = 256; // total bytes incl the 32-byte header
const STRUCT_POOL_CAP: usize = 512; // max retained blocks per size bucket

/// What one pool slot may hold in recycled struct blocks, in bytes.
///
/// The per-bucket cap alone does not state the bound a reader wants: with 32
/// size buckets each allowed 512 blocks, "512" meant about 2.1 MiB per slot.
/// A single hot size still gets its full 512 blocks (256 bytes × 512 is half
/// of this), while a program cycling through many shapes is held to a number
/// stated in the unit that matters. Retention across the process is this
/// times the peak number of concurrently live threads, because slots are
/// adopted, not created, once the peak has passed.
const STRUCT_POOL_MAX_BYTES: usize = 256 * 1024;

// 0 = not probed yet, 1 = disabled, 2 = enabled. this check runs on every
// struct alloc and free, so the settled path must be one relaxed load and a
// predictable branch, same as the perf-stats gate.
static STRUCT_POOL_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

#[cold]
fn struct_pool_probe() -> bool {
    // quarantine never hands a block back, so the pools would only ever be
    // empty; turning them off says so
    let enabled = !crate::quarantine::on()
        && !matches!(
            std::env::var("PITH_STRUCT_FREELIST").as_deref(),
            Ok("0") | Ok("off") | Ok("false")
        );
    STRUCT_POOL_STATE.store(
        if enabled { 2 } else { 1 },
        Ordering::Relaxed,
    );
    enabled
}

#[inline(always)]
fn struct_pool_enabled() -> bool {
    match STRUCT_POOL_STATE.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => struct_pool_probe(),
    }
}

/// The recycled blocks one thread is using: struct blocks by size bucket, and
/// closure shells as a single stack. Both pools live in one slot so a thread
/// adopts once.
///
/// A slot is owned by one thread at a time and is **released at thread exit,
/// never freed**. The blocks stay in it for the next thread to adopt. This is
/// what makes the pool pay on the os-thread backend, where every spawned task
/// is a fresh thread: deallocating the retained blocks in the thread's
/// destructor cost more than the pool had saved (8.1M of 33M instructions in
/// a task-per-thread benchmark, a quarter of the run, with glibc consolidating
/// on every one of the burst frees), and the next task then rebuilt the same
/// pool from empty. An adopted slot arrives pre-grown and pre-filled, so a
/// task's first allocation is a reuse. `channel.rs` and `cycle.rs` keep their
/// per-thread slots the same way for the same reasons.
struct PoolSlot {
    /// indexed by total_size / 8; each entry is a stack of reusable blocks.
    struct_buckets: Vec<Vec<*mut u8>>,
    /// bytes held across every struct bucket, against STRUCT_POOL_MAX_BYTES.
    struct_bytes: usize,
    closures: Vec<*mut u8>,
    owned: std::sync::atomic::AtomicBool,
}

impl PoolSlot {
    #[inline(always)]
    fn try_push_closure(&mut self, ptr: *mut u8) -> bool {
        if self.closures.len() < CLOSURE_POOL_CAP {
            self.closures.push(ptr);
            true
        } else {
            false
        }
    }

    #[inline(always)]
    fn pop_struct(&mut self, bucket: usize, total: usize) -> Option<*mut u8> {
        let ptr = self.struct_buckets.get_mut(bucket)?.pop()?;
        self.struct_bytes -= total;
        Some(ptr)
    }

    #[inline(always)]
    fn push_struct(&mut self, bucket: usize, base: *mut u8, total: usize) -> bool {
        if self.struct_bytes + total > STRUCT_POOL_MAX_BYTES {
            return false;
        }
        if self.struct_buckets.len() <= bucket {
            self.struct_buckets.resize(bucket + 1, Vec::new());
        }
        let stack = &mut self.struct_buckets[bucket];
        if stack.len() >= STRUCT_POOL_CAP {
            return false;
        }
        stack.push(base);
        self.struct_bytes += total;
        true
    }
}

struct PoolRegistry {
    slots: Vec<*mut PoolSlot>,
}
unsafe impl Send for PoolRegistry {}

/// every pool slot ever created. bounded by the peak number of concurrently
/// live threads, not the total ever spawned, because an exited thread's slot
/// is adopted by the next one.
static POOL_REGISTRY: std::sync::Mutex<PoolRegistry> =
    std::sync::Mutex::new(PoolRegistry { slots: Vec::new() });

/// the fast cache holds this after the owning handle has been dropped: a
/// struct freed late in thread teardown, by another thread-local's destructor,
/// must fall back to the allocator rather than push into a slot that another
/// thread may already have adopted. any non-null value that is not a slot
/// would do.
const POOL_SLOT_RELEASED: *mut PoolSlot = 1 as *mut PoolSlot;

/// releases this thread's pool slot at thread exit, leaving its blocks in the
/// registry for the next thread. deliberately not freed: see `PoolSlot`.
struct PoolHandle(*mut PoolSlot);

impl Drop for PoolHandle {
    fn drop(&mut self) {
        // the cache has no drop glue, so it is always reachable here; marking
        // it released turns every later pool access on this thread into an
        // allocator call.
        let _ = POOL_SLOT_FAST.try_with(|c| c.set(POOL_SLOT_RELEASED));
        unsafe {
            (*self.0)
                .owned
                .store(false, Ordering::Release);
        }
    }
}

thread_local! {
    // the fast cache: a const-initialized cell compiles to a direct
    // tls-relative load with no lazy-init machinery on the access path. null
    // until this thread's first pool use adopts a slot.
    static POOL_SLOT_FAST: std::cell::Cell<*mut PoolSlot> =
        const { std::cell::Cell::new(std::ptr::null_mut()) };

    // the owning handle: created once per thread, releases at thread exit.
    // touched only on the first use; every later access goes through the
    // cache above.
    static POOL_SLOT: PoolHandle = PoolHandle(adopt_pool_slot());
}

/// take over a slot an exited thread released, or add one if every slot is
/// owned. the claim runs under the registry lock. a released slot keeps its
/// blocks, which is the point.
fn adopt_pool_slot() -> *mut PoolSlot {
    let mut registry = crate::sync::lock(&POOL_REGISTRY);
    for &slot in registry.slots.iter() {
        let owned = unsafe { &(*slot).owned };
        if owned
            .compare_exchange(
                false,
                true,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            return slot;
        }
    }
    let slot = Box::into_raw(Box::new(PoolSlot {
        struct_buckets: Vec::new(),
        struct_bytes: 0,
        closures: Vec::new(),
        owned: std::sync::atomic::AtomicBool::new(true),
    }));
    registry.slots.push(slot);
    slot
}

/// this thread's pool slot: one tls load on every call after the first. null
/// when the pool cannot be used (before the thread-local machinery is up, or
/// after this thread released its slot), and the caller then goes to the
/// allocator, which is always correct.
///
/// the fast path is a single `%fs:`-relative load of the cached pointer and
/// may be inlined anywhere: the runtime is compiled for the executable it
/// lives in, so the access carries no thread-local base a frame could keep
/// across a park (see the accessor block in `concurrency/green.rs`). the
/// first-use path is different: the lazily initialized handle's address has
/// to exist as a value, and a struct release runs the value's destructor
/// (pith code, which under the green backend can park) right before it
/// returns the block here. so that path stays behind a call the optimizer
/// may not inline, where the address is computed and consumed on whichever
/// thread is running, never hoisted above the destructor.
#[inline]
fn pool_slot() -> *mut PoolSlot {
    let cached = POOL_SLOT_FAST
        .try_with(|c| c.get())
        .unwrap_or(POOL_SLOT_RELEASED);
    if cached == POOL_SLOT_RELEASED {
        return std::ptr::null_mut();
    }
    if !cached.is_null() {
        return cached;
    }
    pool_slot_slow()
}

/// first use on this thread: adopt, then cache. `try_with` because a struct
/// freed during thread teardown, after the handle's turn in the destructor
/// list has passed, must not re-create the thread-local.
#[cold]
#[inline(never)]
fn pool_slot_slow() -> *mut PoolSlot {
    let slot = POOL_SLOT.try_with(|h| h.0).unwrap_or(std::ptr::null_mut());
    if !slot.is_null() {
        let _ = POOL_SLOT_FAST.try_with(|c| c.set(slot));
    }
    slot
}

#[inline]
fn struct_pool_bucket(total: usize) -> Option<usize> {
    if total == 0 || total > STRUCT_POOL_MAX_TOTAL || total % 8 != 0 {
        return None;
    }
    Some(total / 8)
}

/// Reuse a recycled block of exactly `total` bytes, re-zeroed like alloc_zeroed.
/// Returns null on a pool miss (the caller then allocates fresh).
fn struct_pool_take(total: usize) -> *mut u8 {
    if !struct_pool_enabled() {
        return std::ptr::null_mut();
    }
    let Some(bucket) = struct_pool_bucket(total) else {
        return std::ptr::null_mut();
    };
    let slot = pool_slot();
    if slot.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `slot` is non-null and exclusively owned by this thread while active.
    let Some(ptr) = (unsafe { (*slot).pop_struct(bucket, total) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: `ptr` is an unowned recycled struct allocation of exactly `total` bytes.
    unsafe { std::ptr::write_bytes(ptr, 0, total) };
    ptr
}

/// Hand a dead block back to this thread's pool. Returns false (caller should
/// dealloc) when the block is too large, the bucket is full, or the slot has
/// reached its byte budget.
unsafe fn struct_pool_return(base: *mut u8, total: usize) -> bool {
    if !struct_pool_enabled() {
        return false;
    }
    let Some(bucket) = struct_pool_bucket(total) else {
        return false;
    };
    let slot = pool_slot();
    if slot.is_null() {
        return false;
    }
    // SAFETY: `slot` is non-null and exclusively owned by this thread while active.
    unsafe { (*slot).push_struct(bucket, base, total) }
}

#[no_mangle]
pub unsafe extern "C" fn pith_struct_alloc(num_fields: i64) -> i64 {
    use std::alloc::alloc_zeroed;

    if crate::switches::any(crate::switches::PERF_STATS | crate::switches::ALLOC_SITES) {
        struct_alloc_diagnostics(num_fields);
    }
    let Some(size) = (num_fields.max(0) as usize).checked_mul(8) else {
        eprintln!("pith runtime error: invalid allocation layout");
        // panic-guard: struct byte size overflows usize.
        std::process::exit(1);
    };
    if size == 0 {
        return 0;
    }

    // a 32-byte header precedes the fields so field offsets stay where the
    // codegen baked them (offsets are relative to the returned pointer, so the
    // header size is a pure runtime detail):
    //   [magic u32][strong u32][weak u32][dead u32][size u64][dtor u64]
    // this follows Rust's Rc/Weak split. `strong` counts owners; `weak` counts
    // non-owning references plus one implicit unit held collectively by the
    // strong refs. when strong hits 0 the destructor runs, the value is dead,
    // and the implicit weak is dropped; the allocation is freed only when weak
    // also hits 0, so a weak reference can always safely read the header to see
    // whether the target is still alive. dtor is a compiled destructor's
    // address (0 = none).
    let Some(total) = size.checked_add(STRUCT_HEADER) else {
        eprintln!("pith runtime error: invalid allocation layout");
        // panic-guard: struct total allocation size overflows usize.
        std::process::exit(1);
    };
    let base = struct_pool_take(total);
    let base = if base.is_null() {
        let layout = pith_layout(total, 8);
        // SAFETY: `total >= STRUCT_HEADER + 8 > 0`, so `layout` has non-zero size.
        let allocated = unsafe { alloc_zeroed(layout) };
        if allocated.is_null() {
            eprintln!("pith runtime error: allocation failed");
            // panic-guard: out of memory.
            std::process::exit(1);
        }
        allocated
    } else {
        base
    };
    // SAFETY: `base` is a freshly zeroed 8-aligned allocation of `total >= STRUCT_HEADER` bytes.
    let ptr = unsafe {
        (base as *mut u32).write(STRUCT_MAGIC);
        (base.add(STRUCT_OFF_STRONG) as *mut u32).write(1);
        // the implicit weak unit the strong refs hold together
        (base.add(STRUCT_OFF_WEAK) as *mut u32).write(1);
        (base.add(STRUCT_OFF_SIZE) as *mut u64).write(size as u64);
        base.add(STRUCT_HEADER) as i64
    };
    crate::share::note_owner(ptr);
    ptr
}

/// The counters and the allocation census for one struct allocation, kept
/// off the allocation path, which runs once per struct.
#[cold]
#[inline(never)]
fn struct_alloc_diagnostics(num_fields: i64) {
    let bucket = num_fields.clamp(0, 16) as usize;
    if crate::perf_stats_enabled() {
        crate::perf_record2(&crate::PERF_STRUCT_ALLOCS, 1, &crate::perf::PERF_STRUCT_FIELDS[bucket], 1);
    }
    crate::alloc_sites::record(crate::alloc_sites::STRUCT_KINDS[bucket]);
}

pub const STRUCT_MAGIC: u32 = 0x50535452; // "PSTR"
pub const STRUCT_HEADER: usize = 32;
/// The header codegen writes for a box it builds in a stack slot (the ir's
/// `tuple_stack`), the same words `pith_struct_alloc` writes: magic, one strong
/// count, the implicit weak unit, the payload size, and no destructor. The
/// box is never released; its function's cleanup drops what it holds.
pub const STRUCT_STRONG_OFFSET: i32 = STRUCT_OFF_STRONG as i32;
pub const STRUCT_WEAK_OFFSET: i32 = STRUCT_OFF_WEAK as i32;
pub const STRUCT_DEAD_OFFSET: i32 = STRUCT_OFF_DEAD as i32;
pub const STRUCT_SIZE_OFFSET: i32 = STRUCT_OFF_SIZE as i32;
pub const STRUCT_DTOR_OFFSET: i32 = STRUCT_OFF_DTOR as i32;

// header field offsets within the base allocation
const STRUCT_OFF_STRONG: usize = 4;
const STRUCT_OFF_WEAK: usize = 8;
const STRUCT_OFF_DEAD: usize = 12;
const STRUCT_OFF_SIZE: usize = 16;
const STRUCT_OFF_DTOR: usize = 24;

/// Typed view of the 32-byte header sitting immediately before a struct's fields.
/// Atomic fields allow concurrent access through shared `&StructHeader` borrows
/// scoped tightly to [`with_struct_header_for`].
#[repr(C)]
struct StructHeader {
    magic: u32,
    strong: AtomicU32,
    weak: AtomicU32,
    flags: AtomicU32,
    size: u64,
    dtor: AtomicU64,
}

const _: () = {
    assert!(std::mem::size_of::<StructHeader>() == STRUCT_HEADER);
    assert!(std::mem::align_of::<StructHeader>() == 8);
    assert!(std::mem::offset_of!(StructHeader, magic) == 0);
    assert!(std::mem::offset_of!(StructHeader, strong) == STRUCT_OFF_STRONG);
    assert!(std::mem::offset_of!(StructHeader, weak) == STRUCT_OFF_WEAK);
    assert!(std::mem::offset_of!(StructHeader, flags) == STRUCT_OFF_DEAD);
    assert!(std::mem::offset_of!(StructHeader, size) == STRUCT_OFF_SIZE);
    assert!(std::mem::offset_of!(StructHeader, dtor) == STRUCT_OFF_DTOR);
};

// the dead flag must be an atomic like its neighbors: structs cross task (and
// therefore worker-thread) boundaries, so the last strong owner can set it on
// one thread while a weak reference reads it on another. the release store
// pairs with the acquire load in `pith_struct_weak_load`, so a reader that
// observes the value as dead also observes everything the destructor did.
//
// the word is a bitfield now, so every reader and writer must be bit-aware:
// bit 0 is the death flag, bit 1 marks the struct as sitting in the cycle
// collector's suspect buffer. death is fetch_or, not a plain store, so it
// cannot erase a concurrent buffering; the alive test is `& STRUCT_DEAD_BIT`,
// never `!= 0`.
pub(crate) const STRUCT_DEAD_BIT: u32 = 1;
/// the strong count a struct's destructor runs under (see `struct_release`).
/// far above any real count, so `weak_upgrade` reads it as dying: the
/// destructor's own balanced retains and releases move it by a few at most.
pub(crate) const STRUCT_DTOR_GUARD: u32 = 1 << 31;
pub(crate) const STRUCT_BUFFERED_BIT: u32 = 2;
/// Set once the struct can be reached from more than one task (see
/// `share.rs`); until then its counts are updated with plain loads and stores.
pub(crate) const STRUCT_SHARED_BIT: u32 = 4;
/// Where codegen finds a struct's flag word, from the pointer it holds, and
/// the shared mark in it: a field store's write barrier tests the holder's
/// mark inline and calls `pith_share_into` only for a shared one.
pub const STRUCT_FLAGS_FROM_DATA: i32 = STRUCT_OFF_DEAD as i32 - STRUCT_HEADER as i32;
pub const STRUCT_SHARED_MASK: u32 = STRUCT_SHARED_BIT;
/// The dead bit in the same word, which a quarantine build tests before a
/// field read (see `quarantine.rs`).
pub const STRUCT_DEAD_MASK: u32 = STRUCT_DEAD_BIT;

/// Add one to a struct count. A struct only one task can reach needs no
/// locked instruction.
#[inline(always)]
fn count_up(count: &AtomicU32, local: bool) {
    if local {
        count.store(count.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
    } else {
        count.fetch_add(1, Ordering::Relaxed);
    }
}

/// Subtract one from a struct count and return what it held before. The
/// shared form releases, so the owner that takes the count to zero sees
/// every earlier owner's writes once it fences.
#[inline(always)]
fn count_down(count: &AtomicU32, local: bool) -> u32 {
    if local {
        let held = count.load(Ordering::Relaxed);
        count.store(held.wrapping_sub(1), Ordering::Relaxed);
        held
    } else {
        count.fetch_sub(1, Ordering::Release)
    }
}

impl StructHeader {
    /// Whether this struct's counts are plain: local counts are on and nothing
    /// has marked the struct shared. `switches` is the caller's copy of
    /// `switches::get()`, so a count path reads the switch byte once.
    #[inline(always)]
    fn is_local(&self, switches: u8) -> bool {
        switches & crate::switches::LOCAL_COUNTS != 0
            && self.flags.load(Ordering::Relaxed) & STRUCT_SHARED_BIT == 0
    }

    /// Under quarantine, a count operation on a struct whose value already died
    /// is reported, the same as one on a freed struct.
    #[inline(always)]
    fn quarantine_check_alive(&self, op: crate::quarantine::Op) {
        if crate::quarantine::on() && self.flags.load(Ordering::Acquire) & STRUCT_DEAD_BIT != 0 {
            crate::quarantine::dead_struct(op);
        }
    }

    /// Drop one unit of weak and answer whether this dropped the last hold on
    /// the header. The caller frees the header after the `&StructHeader` borrow
    /// ends so magic scrubbing and deallocation never alias a live reference.
    #[inline(always)]
    fn drop_weak_count(&self, ptr: i64, switches: u8) -> bool {
        if self.is_local(switches) {
            crate::share::check_local(switches, ptr, "weak release");
            return count_down(&self.weak, true) == 1;
        }
        // a count of one is ours alone: a new weak reference is made only from a
        // strong or a weak one, and there is neither, so the drop needs no
        // read-modify-write. the acquire load pairs with the release decrements
        // of the holders that went before.
        if self.weak.load(Ordering::Acquire) != 1 {
            if self.weak.fetch_sub(1, Ordering::Release) != 1 {
                return false;
            }
            acquire_last_count!(self.weak);
        }
        true
    }
}

/// Validate `ptr` as a struct handle for `op` and borrow its header for the
/// duration of `f`. A count path that already knows quarantine is off passes
/// `REPORT = false` and skips the freed-magic test.
#[inline(always)]
unsafe fn with_struct_header_for<const REPORT: bool, R>(
    ptr: i64,
    op: crate::quarantine::Op,
    f: impl FnOnce(&StructHeader) -> R,
) -> Option<R> {
    if ptr == 0 || (ptr as u64) % 8 != 0 {
        return None;
    }
    let base = (ptr as usize).checked_sub(STRUCT_HEADER)? as *const StructHeader;
    // SAFETY: `ptr` is non-null, 8-aligned, and `>= STRUCT_HEADER`; read magic at offset 0.
    let magic = unsafe { std::ptr::addr_of!((*base).magic).read() };
    if magic != STRUCT_MAGIC {
        if REPORT {
            // SAFETY: `base` is only used as a lookup key in quarantine reporting.
            unsafe { crate::quarantine::miss(magic, std::ptr::addr_of!((*base).magic), op) };
        }
        return None;
    }
    // SAFETY: Magic matched `STRUCT_MAGIC`; caller keeps the header live for `f`.
    let header = unsafe { &*base };
    Some(f(header))
}

#[inline(always)]
unsafe fn with_struct_header<R>(ptr: i64, f: impl FnOnce(&StructHeader) -> R) -> Option<R> {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { with_struct_header_for::<true, R>(ptr, crate::quarantine::Op::Use, f) }
}

/// Whether the struct at `ptr` is marked shared. False for anything that is
/// not a live struct.
pub(crate) unsafe fn struct_is_shared(ptr: i64) -> bool {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { with_struct_header(ptr, |h| h.flags.load(Ordering::Relaxed) & STRUCT_SHARED_BIT != 0) }
        .unwrap_or(false)
}

fn mark_struct_shared_impl(h: &StructHeader) -> bool {
    h.flags.fetch_or(STRUCT_SHARED_BIT, Ordering::Release) & STRUCT_SHARED_BIT == 0
}

/// Mark the struct at `ptr` shared; true when this call set the mark.
pub(crate) unsafe fn struct_mark_shared(ptr: i64) -> bool {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { with_struct_header(ptr, mark_struct_shared_impl) }.unwrap_or(false)
}

/// `struct_free_header` with quarantine read from `switches`. The quarantine
/// copy stays out of line, so the free path stays small enough to inline.
unsafe fn struct_free_header_for(ptr: i64, switches: u8) {
    if switches & crate::switches::QUARANTINE != 0 {
        // SAFETY: Preconditions forwarded from caller.
        unsafe { struct_free_header_quarantined(ptr) };
    } else {
        // SAFETY: Preconditions forwarded from caller.
        unsafe { struct_free_header::<false>(ptr) };
    }
}

#[cold]
#[inline(never)]
unsafe fn struct_free_header_quarantined(ptr: i64) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { struct_free_header::<true>(ptr) };
}

#[inline(always)]
unsafe fn free_struct_header_if<const HOOKED: bool>(ptr: i64, switches: u8) {
    if HOOKED {
        // SAFETY: Preconditions forwarded from caller.
        unsafe { struct_free_header_for(ptr, switches) };
    } else {
        // SAFETY: Preconditions forwarded from caller.
        unsafe { struct_free_header::<false>(ptr) };
    }
}

/// Scrub and free (or pool) a struct header nothing references any more, or
/// keep it poisoned under quarantine. A release that knows quarantine is off
/// takes the `false` copy and pays nothing for it.
unsafe fn struct_free_header<const QUARANTINE: bool>(ptr: i64) {
    crate::share::forget_owner(ptr);
    let base = (ptr as usize - STRUCT_HEADER) as *mut u8;
    // SAFETY: Sole owner of `base`; read the recorded field byte size.
    let size = unsafe { (base.add(STRUCT_OFF_SIZE) as *const u64).read() as usize };
    let total = size + STRUCT_HEADER;
    if QUARANTINE {
        // SAFETY: Sole owner; poison and mark the full `total`-byte block.
        unsafe { crate::quarantine::bury_now(base, total, base as *mut u32, crate::quarantine::Kind::Struct) };
        return;
    }
    // scrub the magic so any stale pointer fails the check
    // SAFETY: Sole owner and no `&StructHeader` borrow is live.
    unsafe { (base as *mut u32).write(0) };
    // keep the block on the per-thread freelist when it fits; otherwise free it.
    // SAFETY: Sole owner; `base` was allocated with `pith_layout(total, 8)`.
    if !unsafe { struct_pool_return(base, total) } {
        // SAFETY: Matches the allocation layout from `pith_struct_alloc`.
        unsafe { std::alloc::dealloc(base, pith_layout(total, 8)) };
    }
}

/// Build a pith `T?` optional value in the layout the codegen expects: none
/// is the null pointer, and Some is a two-slot struct with 1 in field 0 and
/// the payload in field 1. Used by runtime accessors that want to hand back
/// "not present" without collapsing it to a zero/empty sentinel.
pub(crate) fn optional_tuple(is_some: bool, value: i64) -> i64 {
    if !is_some {
        return 0;
    }
    // SAFETY: `pith_struct_alloc(2)` allocates a fresh 2-field struct.
    let tuple = unsafe { pith_struct_alloc(2) };
    if tuple == 0 {
        return 0;
    }
    // SAFETY: `tuple` points to 2 newly allocated writable `i64` field slots.
    unsafe {
        let ptr = tuple as *mut i64;
        *ptr = 1;
        *ptr.add(1) = value;
    }
    tuple
}

/// An optional getter's answer as a `T?`: see `optional_tuple`.
pub(crate) fn optional_from(found: Option<i64>) -> i64 {
    match found {
        Some(value) => optional_tuple(true, value),
        None => 0,
    }
}

/// An optional getter's answer as two registers, for a compiled caller that
/// unpacks it on the spot and never needs the Some shell: (1, value) for
/// Some and (0, 0) for none.
pub(crate) fn optional_pair(found: Option<i64>) -> crate::ffi_util::ResultPair {
    match found {
        Some(value) => crate::ffi_util::ResultPair {
            is_ok: 1,
            payload: value,
        },
        None => crate::ffi_util::ResultPair {
            is_ok: 0,
            payload: 0,
        },
    }
}

/// The struct check generated code makes under quarantine before it touches
/// a field (see `quarantine::pith_quarantine_check_struct`).
pub(crate) unsafe fn quarantine_check_struct(ptr: i64) {
    // SAFETY: Preconditions forwarded from generated code under quarantine.
    let dead = unsafe { with_struct_header(ptr, |h| h.flags.load(Ordering::Acquire) & STRUCT_DEAD_BIT != 0) };
    if dead == Some(true) {
        crate::quarantine::dead_struct(crate::quarantine::Op::Use);
    }
}

/// Attach a compiled destructor to a struct: called with the struct
/// pointer at its final release, before the memory is freed.
///
/// # Safety
/// ptr must be a pith_struct_alloc result; dtor a compiled fn(i64).
#[no_mangle]
pub unsafe extern "C" fn pith_struct_set_dtor(ptr: i64, dtor: i64) {
    // generated code sets the destructor on a struct it has just allocated,
    // or on an optional shell a runtime call has just returned, so the struct
    // cannot have been freed yet. skipping the quarantine report keeps this
    // path free of a stack frame.
    // SAFETY: `ptr` is validated by `with_struct_header_for` before storing `dtor`.
    unsafe { with_struct_header_for::<false, _>(ptr, crate::quarantine::Op::Use, |h| h.dtor.store(dtor as u64, Ordering::Relaxed)) };
}

/// One more owner of this struct.
///
/// # Safety
/// ptr must be a pith_struct_alloc result or garbage (magic-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_struct_retain(ptr: i64) {
    let switches = crate::switches::get();
    if switches & STRUCT_RETAIN_HOOKS != 0 {
        // SAFETY: Preconditions forwarded from caller.
        return unsafe { struct_retain_hooked(ptr, switches) };
    }
    // SAFETY: Preconditions forwarded from caller.
    unsafe { struct_retain::<false>(ptr, switches) };
}

// the switches that add work to a struct retain. with none of them on, the
// retain calls nothing and needs no stack frame; the copy that counts and
// checks lives out of line, so its calls cost the plain path nothing.
const STRUCT_RETAIN_HOOKS: u8 =
    crate::switches::PERF_STATS | crate::switches::RC_CHECK | crate::switches::QUARANTINE;

#[cold]
#[inline(never)]
unsafe fn struct_retain_hooked(ptr: i64, switches: u8) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { struct_retain::<true>(ptr, switches) };
}

#[inline(always)]
fn struct_retain_impl<const HOOKED: bool>(h: &StructHeader, ptr: i64, switches: u8) {
    if HOOKED {
        h.quarantine_check_alive(crate::quarantine::Op::Retain);
    }
    let flags = h.flags.load(Ordering::Relaxed);
    // a backstop: a value already destroyed is never revived. every weak
    // read takes its count through `weak_upgrade`, so nothing should
    // retain a dead struct; one that does gets no count, and its matching
    // release lands on the over-release guard in `struct_release`.
    if flags & STRUCT_DEAD_BIT != 0 {
        return;
    }
    let local = switches & crate::switches::LOCAL_COUNTS != 0 && flags & STRUCT_SHARED_BIT == 0;
    if HOOKED && local {
        crate::share::check_local(switches, ptr, "retain");
    }
    count_up(&h.strong, local);
}

#[inline(always)]
unsafe fn struct_retain<const HOOKED: bool>(ptr: i64, switches: u8) {
    if HOOKED {
        crate::perf::count_rc_op(switches, crate::perf::RC_STRUCT_RETAIN);
    }
    // SAFETY: `ptr` is validated by `with_struct_header_for` before bumping its count.
    unsafe { with_struct_header_for::<HOOKED, _>(ptr, crate::quarantine::Op::Retain, |h| struct_retain_impl::<HOOKED>(h, ptr, switches)) };
}

/// The number of strong owners this struct has right now; 0 for a pointer
/// that is not a live struct. A caller that holds one count and reads 1 is
/// the sole owner: nobody else can retain a value they have no reference
/// to, so the answer stays true until the caller itself hands the value on.
/// A caller that reads more than 1 may already be stale (another owner may
/// release in the meantime), which is the safe direction for the one use
/// this has: deciding whether a destructor-less box's payload may be
/// dropped explicitly before the box is released.
///
/// # Safety
/// ptr must be a pith_struct_alloc result, 0, or garbage (magic-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_struct_strong_count(ptr: i64) -> i64 {
    // SAFETY: `ptr` is validated by `with_struct_header` before loading its strong count.
    unsafe { with_struct_header(ptr, |h| h.strong.load(Ordering::Acquire) as i64) }.unwrap_or(0)
}

/// Drop one owner; the last strong release runs the destructor (releasing the
/// struct's rc fields), marks the value dead, and drops the implicit weak
/// unit, which frees the allocation unless a weak reference still holds it.
///
/// # Safety
/// ptr must be a pith_struct_alloc result or garbage (magic-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_struct_release(ptr: i64) {
    let switches = crate::switches::get();
    if switches & STRUCT_RELEASE_HOOKS != 0 {
        // SAFETY: Preconditions forwarded from caller.
        return unsafe { struct_release_hooked(ptr, switches) };
    }
    // SAFETY: Preconditions forwarded from caller.
    unsafe { struct_release::<false>(ptr, switches) };
}

// the switches that add work to a struct release, kept out of line for the
// same reason as the retain's
const STRUCT_RELEASE_HOOKS: u8 = crate::switches::PERF_STATS
    | crate::switches::RC_CHECK
    | crate::switches::CYCLE_GC
    | crate::switches::QUARANTINE;

#[cold]
#[inline(never)]
unsafe fn struct_release_hooked(ptr: i64, switches: u8) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { struct_release::<true>(ptr, switches) };
}

enum StructReleaseStep {
    Keep,
    FreeNow,
    RunDtor(u64),
}

/// Tail of the last strong release after any destructor has run. Returns `true`
/// when the caller must free the header once the `&StructHeader` borrow ends.
#[inline(always)]
unsafe fn struct_release_finish<const HOOKED: bool>(h: &StructHeader, ptr: i64, switches: u8) -> bool {
    if HOOKED {
        crate::perf_count(&crate::PERF_STRUCT_FREES, 1);
    }
    // with only the implicit weak unit left, nothing can read the dead flag,
    // so the header is freed without setting it. a destructor that made a
    // weak reference, or a suspect buffer holding one, keeps the full path.
    if h.weak.load(Ordering::Acquire) == 1 {
        return true;
    }
    // the value is now dead; a weak read after this returns none. the release
    // ordering publishes the destructor's writes to any thread whose weak load
    // sees the flag. fetch_or rather than a store so a concurrently-set
    // buffered bit survives. then drop the implicit weak unit, which frees the
    // header if no weak refs remain. for a buffered struct the suspect
    // buffer's weak count keeps the header alive past this point.
    if HOOKED && crate::quarantine::on() {
        // a weak reference keeps the header, but the fields were released:
        // poison them so a read through a stale pointer cannot pass for data
        // SAFETY: Fields at `ptr..ptr+h.size` sit after `StructHeader` and have no strong owners left.
        unsafe { std::ptr::write_bytes(ptr as *mut u8, crate::quarantine::POISON, h.size as usize) };
    }
    h.flags.fetch_or(STRUCT_DEAD_BIT, Ordering::Release);
    h.drop_weak_count(ptr, switches)
}

#[inline(always)]
unsafe fn struct_release_step<const HOOKED: bool>(
    h: &StructHeader,
    ptr: i64,
    switches: u8,
) -> StructReleaseStep {
    if HOOKED {
        h.quarantine_check_alive(crate::quarantine::Op::Release);
    }
    // the sole owner of a struct that no weak reference holds is the only
    // party that can reach it: no other thread can retain, release or
    // upgrade it, so the last release skips the decrement. the acquire
    // loads pair with the release decrements of the owners that went before.
    let sole = h.strong.load(Ordering::Acquire) == 1 && h.weak.load(Ordering::Acquire) == 1;
    if HOOKED && sole {
        crate::perf::count_rc_op(switches, crate::perf::RC_STRUCT_SOLE_RELEASE);
    }
    if !sole {
        // cycle-gc suspect hook, placed before our count is given up rather
        // than in the prev > 1 arm below. the ordering is load-bearing: while
        // we still hold a strong count the header cannot be freed, so the
        // buffered bit and the buffer's weak retain land on live memory.
        // after the fetch_sub a concurrent owner can finish the whole death
        // path, including the weak drop that frees the header, under the
        // hook's feet. if other owners release between this load and our
        // decrement, the struct simply dies buffered, which the death path
        // below already supports.
        if HOOKED && switches & crate::switches::CYCLE_GC != 0 && h.strong.load(Ordering::Relaxed) > 1 {
            // SAFETY: Caller still holds a strong count on `ptr`, matching `h`.
            unsafe { maybe_suspect_struct(ptr, h) };
        }
        let local = h.is_local(switches);
        if HOOKED && local {
            crate::share::check_local(switches, ptr, "release");
        }
        let prev = count_down(&h.strong, local);
        if prev > 1 {
            return StructReleaseStep::Keep;
        }
        if prev == 0 {
            if HOOKED {
                crate::quarantine::over_release(crate::quarantine::Kind::Struct);
            }
            // over-release: restore and leave the object alone
            h.strong.fetch_add(1, Ordering::Relaxed);
            return StructReleaseStep::Keep;
        }
        acquire_last_count!(h.strong);
    }
    let dtor = h.dtor.load(Ordering::Relaxed);
    if dtor != 0 {
        // a destructor may run user code: a `Drop` method that binds `self`
        // or passes it along, which retains and releases the value. with the
        // count sitting at zero, that release would come back through here
        // and run the destructor a second time. so the body runs under a
        // guard count, as the cycle collector's teardown does: every pair
        // inside it balances back to the guard, and no release inside can
        // reach zero. the value is freed below whatever the count reads
        // afterwards, which is sound because the checker refuses a `drop`
        // that lets `self` escape (E318).
        //
        // the guard is `STRUCT_DTOR_GUARD` rather than one, so a weak upgrade
        // racing the destructor can tell it from a live count and refuse it.
        // the release store pairs with the acquire loads in `weak_upgrade`.
        h.strong.store(STRUCT_DTOR_GUARD, Ordering::Release);
        return StructReleaseStep::RunDtor(dtor);
    }
    // SAFETY: Last strong release on `ptr`, matching `h`, with no destructor.
    if unsafe { struct_release_finish::<HOOKED>(h, ptr, switches) } {
        StructReleaseStep::FreeNow
    } else {
        StructReleaseStep::Keep
    }
}

#[inline(always)]
unsafe fn struct_release<const HOOKED: bool>(ptr: i64, switches: u8) {
    if HOOKED {
        crate::perf::count_rc_op(switches, crate::perf::RC_STRUCT_RELEASE);
    }
    // SAFETY: `ptr` is validated by `with_struct_header_for` before decrementing its count.
    let Some(step) = (unsafe { with_struct_header_for::<HOOKED, _>(ptr, crate::quarantine::Op::Release, |h| struct_release_step::<HOOKED>(h, ptr, switches)) }) else {
        return;
    };
    match step {
        StructReleaseStep::Keep => {}
        StructReleaseStep::FreeNow => {
            // SAFETY: Last weak unit was dropped and `&StructHeader` borrow has ended.
            unsafe { free_struct_header_if::<HOOKED>(ptr, switches) };
        }
        StructReleaseStep::RunDtor(dtor) => {
            // panic-guard: calling a compiler-emitted struct destructor from its header slot.
            let f: unsafe extern "C" fn(i64) = unsafe { std::mem::transmute(dtor as usize) };
            // SAFETY: `dtor` is the compiled destructor for `ptr`, running under `STRUCT_DTOR_GUARD`.
            unsafe { f(ptr) };
            let base = (ptr as usize - STRUCT_HEADER) as *const StructHeader;
            // SAFETY: The implicit weak unit keeps the header alive across `f(ptr)`.
            let should_free = unsafe { struct_release_finish::<HOOKED>(&*base, ptr, switches) };
            if should_free {
                // SAFETY: Last weak unit was dropped and `&StructHeader` borrow has ended.
                unsafe { free_struct_header_if::<HOOKED>(ptr, switches) };
            }
        }
    }
}

/// Mark a struct as a cycle suspect: set the buffered bit, take the weak
/// count the buffer owns, and hand the pointer to the buffer. Only the
/// thread that flips the bit pushes, so many racing releases buffer once.
/// The caller still holds a strong count, so the header is alive throughout.
#[cold]
#[inline(never)]
unsafe fn maybe_suspect_struct(ptr: i64, h: &StructHeader) {
    if h.flags.fetch_or(STRUCT_BUFFERED_BIT, Ordering::Relaxed) & STRUCT_BUFFERED_BIT != 0 {
        return; // already buffered
    }
    h.weak.fetch_add(1, Ordering::Relaxed);
    // SAFETY: Caller holds a strong count on `ptr`, buffered bit is set, and buffer owns a weak count.
    unsafe { crate::cycle::cycle_suspect(ptr as usize, crate::cycle::CYCLE_KIND_STRUCT) };
}

/// Drop a struct's buffered mark (overflow, or a collector drain). The
/// caller drops the buffer's weak count separately; magic-checked so a
/// header that is already gone is a no-op.
pub(crate) unsafe fn cycle_clear_struct_buffered(ptr: i64) {
    // SAFETY: Preconditions forwarded from cycle collector.
    unsafe { with_struct_header(ptr, |h| h.flags.fetch_and(!STRUCT_BUFFERED_BIT, Ordering::Relaxed)) };
}

// --- collector-facing struct accessors --------------------------------------
//
// like the closure accessors above: the collection pass holds the world
// stopped, so these use relaxed reads, and every one magic-checks so a bad
// pointer degrades to None or a no-op.

/// `Some(true)` for a live struct, `Some(false)` for one whose value died
/// but whose header is still held (by a weak count, such as the suspect
/// buffer's for a drained entry), `None` when the pointer is not a struct
/// header at all.
pub(crate) unsafe fn cycle_struct_alive(ptr: i64) -> Option<bool> {
    // SAFETY: Preconditions forwarded from cycle collector.
    unsafe { with_struct_header(ptr, |h| h.flags.load(Ordering::Relaxed) & STRUCT_DEAD_BIT == 0) }
}

fn live_struct_strong_count(h: &StructHeader) -> Option<u32> {
    if h.flags.load(Ordering::Relaxed) & STRUCT_DEAD_BIT != 0 {
        return None;
    }
    Some(h.strong.load(Ordering::Relaxed))
}

/// The struct's current strong count, or `None` for a dead value or a
/// non-struct pointer. The collector must never seed a graph node from a
/// dead value.
pub(crate) unsafe fn cycle_struct_strong_count(ptr: i64) -> Option<u32> {
    // SAFETY: Preconditions forwarded from cycle collector.
    unsafe { with_struct_header(ptr, live_struct_strong_count) }.flatten()
}

/// The compiled destructor in the header's dtor slot, or 0 for none (a
/// dtor-less box is a leaf to the collector: no rc children to walk).
pub(crate) unsafe fn cycle_struct_dtor_slot(ptr: i64) -> u64 {
    // SAFETY: Preconditions forwarded from cycle collector / share walk.
    unsafe { with_struct_header(ptr, |h| h.dtor.load(Ordering::Relaxed)) }.unwrap_or(0)
}

/// Add `delta` to the strong count: the collector's teardown guard.
pub(crate) unsafe fn cycle_struct_guard_strong(ptr: i64, delta: u32) {
    // SAFETY: Preconditions forwarded from cycle collector.
    unsafe { with_struct_header(ptr, |h| h.strong.fetch_add(delta, Ordering::Relaxed)) };
}

/// Run a garbage struct's destructor: the same body the last strong release
/// would have run, releasing every field the struct owns.
pub(crate) unsafe fn cycle_struct_run_dtor(ptr: i64) {
    // SAFETY: Preconditions forwarded from cycle collector.
    let Some(dtor) = (unsafe { with_struct_header(ptr, |h| h.dtor.load(Ordering::Relaxed)) }) else {
        return;
    };
    if dtor != 0 {
        // panic-guard: calling a compiler-emitted struct destructor from its header slot.
        let f: unsafe extern "C" fn(i64) = unsafe { std::mem::transmute(dtor as usize) };
        // SAFETY: `dtor` is the compiled destructor for `ptr`, invoked under collector guard.
        unsafe { f(ptr) };
    }
}

fn mark_dead_and_drop_weak(h: &StructHeader, ptr: i64, switches: u8) -> bool {
    crate::perf_count(&crate::PERF_STRUCT_FREES, 1);
    h.flags.fetch_or(STRUCT_DEAD_BIT, Ordering::Release);
    h.drop_weak_count(ptr, switches)
}

/// Free a garbage struct's shell: the tail of `pith_struct_release` minus
/// the destructor, which `cycle_struct_run_dtor` already ran. Marks the
/// value dead (weak reads answer none from here on) and drops the implicit
/// weak unit, so outstanding weak references keep holding the header exactly
/// as they would after a normal death.
pub(crate) unsafe fn cycle_struct_free_dead(ptr: i64) {
    let switches = crate::switches::get();
    // SAFETY: Preconditions forwarded from cycle collector.
    let should_free = unsafe { with_struct_header(ptr, |h| mark_dead_and_drop_weak(h, ptr, switches)) };
    if should_free == Some(true) {
        // SAFETY: Last weak unit was dropped and `&StructHeader` borrow has ended.
        unsafe { struct_free_header_for(ptr, switches) };
    }
}

#[cfg(test)]
pub(crate) unsafe fn struct_dead_word_for_tests(ptr: i64) -> u32 {
    // SAFETY: Preconditions forwarded from test caller.
    unsafe { with_struct_header(ptr, |h| h.flags.load(Ordering::Relaxed)) }.unwrap_or(u32::MAX)
}

#[cfg(test)]
pub(crate) unsafe fn struct_weak_count_for_tests(ptr: i64) -> u32 {
    // SAFETY: Preconditions forwarded from test caller.
    unsafe { with_struct_header(ptr, |h| h.weak.load(Ordering::Relaxed)) }.unwrap_or(u32::MAX)
}

#[inline(always)]
fn struct_weak_retain_impl(h: &StructHeader, ptr: i64, switches: u8) {
    let local = h.is_local(switches);
    if local {
        crate::share::check_local(switches, ptr, "weak retain");
    }
    count_up(&h.weak, local);
}

/// One more weak (non-owning) reference to this struct. Does not affect the
/// strong count, so it keeps only the header alive, never the value.
///
/// # Safety
/// ptr must be a pith_struct_alloc result or garbage (magic-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_struct_weak_retain(ptr: i64) {
    let switches = crate::switches::get();
    crate::perf::count_rc_op(switches, crate::perf::RC_WEAK_RETAIN);
    // SAFETY: `ptr` is validated by `with_struct_header` before bumping its weak count.
    unsafe { with_struct_header(ptr, |h| struct_weak_retain_impl(h, ptr, switches)) };
}

/// Drop one weak reference; frees the header if this was the last hold on it.
///
/// # Safety
/// ptr must be a pith_struct_alloc result or garbage (magic-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_struct_weak_release(ptr: i64) {
    let switches = crate::switches::get();
    crate::perf::count_rc_op(switches, crate::perf::RC_WEAK_RELEASE);
    // SAFETY: `ptr` is validated by `with_struct_header` before dropping its weak count.
    let should_free = unsafe { with_struct_header(ptr, |h| h.drop_weak_count(ptr, switches)) };
    if should_free == Some(true) {
        // SAFETY: Last weak unit was dropped and `&StructHeader` borrow has ended.
        unsafe { struct_free_header_for(ptr, switches) };
    }
}

#[inline(always)]
fn struct_weak_upgrade_impl(h: &StructHeader) -> bool {
    let switches = crate::switches::get();
    if h.is_local(switches) {
        // only the task that built it can reach a local struct, so nothing
        // can release it between this check and the increment.
        let count = h.strong.load(Ordering::Relaxed);
        let live = h.flags.load(Ordering::Relaxed) & STRUCT_DEAD_BIT == 0
            && count != 0
            && count < STRUCT_DTOR_GUARD;
        if live {
            h.strong.store(count + 1, Ordering::Relaxed);
        }
        live
    } else {
        weak_upgrade(&h.strong, &h.flags)
    }
}

/// Upgrade a weak reference: one more strong owner of the struct if it is
/// still alive, returned to the caller, or 0 once its last owner is gone.
/// The count is the caller's to release. Never dereferences dead field data:
/// the header stays valid while any weak reference holds it.
///
/// Taking the count is what makes the read safe on a struct other threads
/// can release. Checking liveness and retaining later left a window in which
/// the last owner could drop it, and the reader then revived a value whose
/// destructor had already run.
///
/// # Safety
/// ptr must be a pith_struct_alloc result, 0, or garbage (magic-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_struct_weak_upgrade(ptr: i64) -> i64 {
    // SAFETY: `ptr` is validated by `with_struct_header` before upgrading.
    let upgraded = unsafe { with_struct_header(ptr, struct_weak_upgrade_impl) }.unwrap_or(false);
    if upgraded {
        ptr
    } else {
        0
    }
}

/// Striped locks that pair a weak field's read with a store into the same
/// field. The field holds a weak count on its target, and that count is all
/// that keeps a dead target's header allocated. A reader that loaded the
/// pointer and a store in another task that dropped the field's weak count
/// before the reader's upgrade left the upgrade reading a freed header. Under
/// the lock the store waits until the reader has its strong count. Only a
/// holder another task can reach takes it (see `weak_holder_is_local`).
static WEAK_FIELD_LOCKS: [AtomicBool; 64] = [const { AtomicBool::new(false) }; 64];

struct WeakFieldGuard(&'static AtomicBool);

impl WeakFieldGuard {
    fn lock(slot: i64) -> Self {
        let lock = &WEAK_FIELD_LOCKS[((slot as u64 >> 3) % 64) as usize];
        let mut spins = 0u32;
        while lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spins += 1;
            if spins < 64 {
                std::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        }
        WeakFieldGuard(lock)
    }
}

impl Drop for WeakFieldGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Whether a weak field of the struct at `holder` can skip
/// `WEAK_FIELD_LOCKS`. It can when nothing has marked the holder shared: only
/// the running task can reach such a holder, so no other task reads or
/// stores its fields. Every route to another task marks a struct, and
/// everything it reaches, before the other task can see it (see `share.rs`),
/// whatever the count switches are set to, and nothing clears the mark. A
/// task that reads the mark clear therefore holds the only path to the
/// holder, and any other task that reaches it reads the mark set and takes
/// the lock. Codegen makes the same test inline (`struct_shared_mark`); this
/// function is the path for a build without fast paths. A pointer that is
/// not a live struct header takes the lock.
#[inline(always)]
unsafe fn weak_holder_is_local(holder: i64, op: &str) -> bool {
    // SAFETY: Caller guarantees `holder` is a live struct.
    let local = unsafe { with_struct_header(holder, |h| h.flags.load(Ordering::Relaxed) & STRUCT_SHARED_BIT == 0) }
        .unwrap_or(false);
    if local {
        crate::share::check_local(crate::switches::get(), holder, op);
    }
    local
}

/// Read a weak field of the struct at `holder`, `offset` bytes into it, and
/// upgrade it: the target with one new strong count, or 0. See
/// `WEAK_FIELD_LOCKS` for why the load and the upgrade are one step.
///
/// # Safety
/// holder must be a live struct with a weak field at offset.
#[no_mangle]
pub unsafe extern "C" fn pith_struct_weak_load_field(holder: i64, offset: i64) -> i64 {
    let slot = holder + offset;
    // SAFETY: Caller guarantees `holder + offset` points to an aligned `AtomicI64` weak field.
    let field = unsafe { &*(slot as *const AtomicI64) };
    // SAFETY: Caller guarantees `holder` is a live struct.
    if unsafe { weak_holder_is_local(holder, "weak field read") } {
        // no other task can store into the field, so the field's weak count
        // keeps the target's header allocated through the upgrade
        // SAFETY: The field's weak count keeps `target`'s header allocated.
        return unsafe { pith_struct_weak_upgrade(field.load(Ordering::Relaxed)) };
    }
    let _guard = WeakFieldGuard::lock(slot);
    let target = field.load(Ordering::Acquire);
    // SAFETY: Guard keeps the field's weak count alive while upgrading `target`.
    unsafe { pith_struct_weak_upgrade(target) }
}

/// Store `target`, on which the caller took a weak count, into the weak field
/// of the struct at `holder`, `offset` bytes into it, and drop the weak count
/// the field held on its old target.
///
/// # Safety
/// holder must be a live struct with a weak field at offset; target a struct
/// or 0.
#[no_mangle]
pub unsafe extern "C" fn pith_struct_weak_store_field(holder: i64, offset: i64, target: i64) {
    let slot = holder + offset;
    // SAFETY: Caller guarantees `holder + offset` points to an aligned `AtomicI64` weak field.
    let field = unsafe { &*(slot as *const AtomicI64) };
    // SAFETY: Caller guarantees `holder` is a live struct.
    let old = if unsafe { weak_holder_is_local(holder, "weak field store") } {
        // no reader in another task can sit between its load and its
        // upgrade, so the swap needs neither the lock nor a locked
        // instruction
        let old = field.load(Ordering::Relaxed);
        field.store(target, Ordering::Relaxed);
        old
    } else {
        let _guard = WeakFieldGuard::lock(slot);
        field.swap(target, Ordering::AcqRel)
    };
    // SAFETY: The field previously owned one weak count on `old`.
    unsafe { pith_struct_weak_release(old) };
}

/// The upgrade itself, on a struct's strong count and flag word: add one to
/// the count only while it is a live one, in a single compare-and-swap, so a
/// last release cannot slip between the check and the increment. 0 means the
/// last owner is gone, `STRUCT_DTOR_GUARD` and above that the destructor is
/// running, and the dead bit that the value is destroyed (the cycle
/// collector frees a value whose count never reached 0). Generic over the
/// atomic so the loom model runs this same function.
#[inline]
pub(crate) fn weak_upgrade<C: crate::sync::Count32>(strong: &C, flags: &C) -> bool {
    // acquire on both: a load that sees the guard or the dead bit also sees
    // the release that set it, and the struct's fields as its last owner
    // left them. test the death bit specifically: a live struct sitting in
    // the cycle collector's suspect buffer has bit 1 set and stays alive.
    if flags.load(Ordering::Acquire) & STRUCT_DEAD_BIT != 0 {
        return false;
    }
    let mut count = strong.load(Ordering::Acquire);
    loop {
        if count == 0 || count >= STRUCT_DTOR_GUARD {
            return false;
        }
        match strong.compare_exchange_weak(count, count + 1, Ordering::Acquire, Ordering::Acquire) {
            Ok(_) => return true,
            Err(current) => count = current,
        }
    }
}

