use crate::collections::list::{pith_list_new, pith_list_push_value};
use crate::handle_registry::{self, HandleKind};
use crate::string;
use std::alloc::{alloc, Layout};

pub(crate) fn pith_strdup_string(text: &str) -> *mut i8 {
    // the length is already known, so allocate the headered cstring directly.
    // routing through pith_strdup would probe 16 bytes in front of the rust
    // string's buffer looking for a header no rust allocation has — a read of
    // memory this process does not own whenever the buffer starts its heap
    // block. pith_alloc_cstring writes the nul terminator itself.
    unsafe {
        let result = pith_alloc_cstring(text.len());
        std::ptr::copy_nonoverlapping(text.as_ptr() as *const i8, result, text.len());
        result
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_runtime_error(code: i64) -> i64 {
    let message = match code {
        1 => "division by zero",
        2 => "integer division overflow",
        3 => "allocation failed",
        4 => "invalid allocation layout",
        5 => "call to a function the compiler could not resolve — a compiler bug, not a problem in your program",
        _ => "runtime error",
    };
    eprintln!("pith runtime error: {message}");
    // panic-guard: the compiler's runtime-error trap; the program cannot continue past any of these.
    std::process::exit(1);
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
    let ptr = alloc(layout);
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

pub(crate) const CSTRING_MAGIC: u32 = 0x50435352; // "PCSR"
const CSTRING_HEADER_SIZE: usize = 16;
const CSTRING_ALIGN: usize = 8;

/// One shared empty string. It has no header, so retain/release skip it,
/// which is exactly right for a value that is never freed.
static CSTRING_EMPTY: &[u8] = b"\0";

fn cstring_layout(data_len: usize) -> Layout {
    pith_layout(CSTRING_HEADER_SIZE + data_len + 1, CSTRING_ALIGN)
}

/// Allocate a heap cstring with `data_len` data bytes, refcount 1, and the
/// trailing NUL already written. The caller fills bytes 0..data_len.
/// Debug: under PITH_CSTRING_NOFREE + PITH_LEAK_SCAN, every allocation is
/// remembered and the exit hook prints the ones whose count never reached
/// zero — the definitive leak list.
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
    let ptrs = reg.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for &p in ptrs.iter() {
        unsafe {
            let s = p as *const i8;
            let Some(base) = cstring_base(s as i64 as *const i8 as *const i8) else {
                continue;
            };
            let rc = cstring_refcount(base).load(std::sync::atomic::Ordering::Relaxed);
            if rc == 0 {
                continue;
            }
            let data_len = (base.add(8) as *const u64).read() as usize;
            let bytes = std::slice::from_raw_parts(s as *const u8, data_len.min(40));
            let content = String::from_utf8_lossy(bytes).into_owned();
            *counts.entry(format!("rc={} {:?}", rc, content)).or_insert(0) += 1;
        }
    }
    let mut rows: Vec<(usize, String)> = counts.into_iter().map(|(k, v)| (v, k)).collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    eprintln!("leaked cstrings ({} rows):", rows.len());
    for (n, k) in rows.iter().take(40) {
        eprintln!("  {} x {}", n, k);
    }
}

pub(crate) unsafe fn pith_alloc_cstring(data_len: usize) -> *mut i8 {
    crate::perf_count(&crate::PERF_CSTRING_ALLOCS, 1);
    let base = pith_alloc(cstring_layout(data_len));
    (base as *mut u32).write(CSTRING_MAGIC);
    (base.add(4) as *mut u32).write(1);
    (base.add(8) as *mut u64).write(data_len as u64);
    let data = base.add(CSTRING_HEADER_SIZE) as *mut i8;
    *data.add(data_len) = 0;
    if let Some(reg) = leak_scan_registry() {
        reg.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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
    let addr = s as usize;
    if addr < CSTRING_HEADER_SIZE || addr % CSTRING_ALIGN != 0 {
        return None;
    }
    let base = (s as *const u8).sub(CSTRING_HEADER_SIZE) as *mut u8;
    if (base as *const u32).read() != CSTRING_MAGIC {
        return None;
    }
    Some(base)
}

unsafe fn cstring_refcount(base: *mut u8) -> &'static std::sync::atomic::AtomicU32 {
    &*(base.add(4) as *const std::sync::atomic::AtomicU32)
}

/// Bump the refcount of a heap cstring. No-op for literals and null.
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_retain(s: *const i8) {
    if let Some(base) = cstring_base(s) {
        crate::perf_count(&crate::PERF_CSTRING_RETAINS, 1);
        let prev = cstring_refcount(base).fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        cstring_watch(base, s, "retain", prev as i64, 1);
    }
}

/// Debug: PITH_CSTRING_WATCH=<content> logs every rc operation on strings
/// with that exact content, with the count before the op.
#[inline(always)]
unsafe fn cstring_watch(base: *const u8, s: *const i8, op: &str, prev: i64, delta: i64) {
    let Some(needle) = debug_watch_needle() else {
        return;
    };
    let data_len = (base.add(8) as *const u64).read() as usize;
    let bytes = std::slice::from_raw_parts(s as *const u8, data_len.min(64));
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

/// Drop one count; free the allocation when the last count dies. The magic
/// is scrubbed before the free so a stale pointer fails the header check
/// instead of double-freeing. No-op for literals and null.
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_release(s: *const i8) {
    if let Some(base) = cstring_base(s) {
        crate::perf_count(&crate::PERF_CSTRING_RELEASES, 1);
        let prev = cstring_refcount(base).fetch_sub(1, std::sync::atomic::Ordering::Release);
        cstring_watch(base, s, "release", prev as i64, -1);
        if prev == 0 {
            // an over-release means the emitter's ownership accounting is
            // wrong somewhere; report it loudly in debug runs instead of
            // corrupting the heap.
            if cstring_debug_no_free() {
                let data_len = (base.add(8) as *const u64).read() as usize;
                let bytes = std::slice::from_raw_parts(s as *const u8, data_len.min(60));
                eprintln!(
                    "pith debug: cstring over-release: {:?}",
                    String::from_utf8_lossy(bytes)
                );
            }
            cstring_refcount(base).store(0, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        if prev == 1 {
            std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
            if let Some(needle) = debug_trace_needle() {
                let data_len = (base.add(8) as *const u64).read() as usize;
                let bytes = std::slice::from_raw_parts(s as *const u8, data_len.min(40));
                let content = String::from_utf8_lossy(bytes);
                if content == needle {
                    eprintln!("pith trace: final release of {:?} at {:p}", content, s);
                }
            }
            if cstring_debug_no_free() {
                // scrub instead of freeing: a read-after-release shows up
                // as '!' runs in program output instead of silent reuse
                let data_len = (base.add(8) as *const u64).read() as usize;
                if debug_scrub_log() {
                    let bytes = std::slice::from_raw_parts(s as *const u8, data_len.min(24));
                    eprintln!("scrub {:p} len={} {:?}", s, data_len, String::from_utf8_lossy(bytes));
                }
                std::ptr::write_bytes(s as *mut u8, b'!', data_len);
                return;
            }
            let data_len = (base.add(8) as *const u64).read() as usize;
            (base as *mut u32).write(0);
            crate::perf_count(&crate::PERF_CSTRING_FREES, 1);
            std::alloc::dealloc(base, cstring_layout(data_len));
        }
    }
}

/// Debug switch: keep freed cstrings alive and report over-releases, so an
/// accounting bug in emitted code shows up as a message, not corruption.
#[inline(always)]
fn debug_watch_needle() -> Option<&'static str> {
    static NEEDLE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    NEEDLE
        .get_or_init(|| std::env::var("PITH_CSTRING_WATCH").ok())
        .as_deref()
}

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

fn cstring_debug_no_free() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var("PITH_CSTRING_NOFREE").is_ok())
}

/// Test-only view of a heap cstring's refcount (None for literals/null).
#[cfg(test)]
pub(crate) unsafe fn cstring_refcount_for_tests(s: *const i8) -> Option<u32> {
    cstring_base(s).map(|base| cstring_refcount(base).load(std::sync::atomic::Ordering::Relaxed))
}

/// O(1) length for heap cstrings via the header; None for literals.
pub(crate) unsafe fn cstring_header_len(s: *const i8) -> Option<i64> {
    cstring_base(s).map(|base| (base.add(8) as *const u64).read() as i64)
}

/// Debug: release with an emission-site id; prints the site when the traced
/// string takes its final release.
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_release_traced(s: *const i8, site: i64) {
    if let Some(base) = cstring_base(s) {
        let rc = cstring_refcount(base).load(std::sync::atomic::Ordering::Relaxed);
        if rc == 1 {
            if let Some(needle) = debug_trace_needle() {
                let data_len = (base.add(8) as *const u64).read() as usize;
                let bytes = std::slice::from_raw_parts(s as *const u8, data_len.min(40));
                let content = String::from_utf8_lossy(bytes);
                if content == needle {
                    eprintln!("pith trace: site {} finally releases {:?}", site, content);
                }
            }
        }
    }
    pith_cstring_release(s);
}

/// Debug: retain with an emission-site id, counted per site when
/// PITH_RETAIN_SITES is set.
#[no_mangle]
pub unsafe extern "C" fn pith_cstring_retain_traced(s: *const i8, site: i64) {
    if let Some(base) = cstring_base(s) {
        if debug_watch_needle().is_some() {
            let data_len = (base.add(8) as *const u64).read() as usize;
            let bytes = std::slice::from_raw_parts(s as *const u8, data_len.min(64));
            if Some(String::from_utf8_lossy(bytes).into_owned().as_str()) == debug_watch_needle() {
                eprintln!("watch retain SITE {}", site);
            }
        }
    }
    pith_cstring_retain(s);
}

pub(crate) unsafe fn pith_copy_bytes_to_cstring(bytes: &[u8]) -> *mut i8 {
    let ptr = pith_alloc_cstring(bytes.len());
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
    ptr
}

pub(crate) unsafe fn pith_cstring_empty() -> *mut i8 {
    CSTRING_EMPTY.as_ptr() as *mut i8
}

const PITH_CLOSURE_ENV_SLOTS: usize = 16;

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
// the closure took a WEAK reference, never a strong count — that is what
// lets a closure refer back to the state that owns it without forming a
// strong cycle. dropping it releases only the weak count.
const CLOSURE_TAG_WEAK: u8 = 8;

// closures validate the same way strings and structs do: a magic word
// at the front of the allocation, read without a lock. `#[repr(C)]`
// pins `magic` to offset 0 so a stale or garbage handle is rejected by
// reading the first four bytes rather than by consulting a global set.
const CLOSURE_MAGIC: u32 = 0x50434c53; // "PCLS"

#[repr(C)]
struct PithClosure {
    magic: u32,
    func_ptr: i64,
    ref_count: std::sync::atomic::AtomicI64,
    env: [i64; PITH_CLOSURE_ENV_SLOTS],
    env_tags: [u8; PITH_CLOSURE_ENV_SLOTS],
    // bit 0: this closure sits in the cycle collector's suspect buffer.
    // appended at the end on purpose — the layout is #[repr(C)] and nothing
    // outside the runtime reads past env_tags, so the early offsets the
    // codegen could bake stay where they were.
    cycle_flags: std::sync::atomic::AtomicU8,
}

/// Validate a closure handle by its magic word. Returns the typed
/// pointer only when the handle is non-null, aligned, and still carries
/// the magic — the lock-free equivalent of the old registry lookup.
/// Same practical-guard contract as the string and struct magic words:
/// a stale pointer whose magic was scrubbed on free reads as invalid.
unsafe fn closure_base<'a>(handle: i64) -> Option<&'a PithClosure> {
    if handle == 0 || (handle as u64) % 8 != 0 {
        return None;
    }
    let ptr = handle as *const PithClosure;
    if (ptr as *const u32).read() != CLOSURE_MAGIC {
        return None;
    }
    Some(&*ptr)
}

unsafe fn release_captured_value(value: i64, tag: u8) {
    match tag {
        CLOSURE_TAG_STRING => pith_cstring_release(value as *const i8),
        CLOSURE_TAG_LIST => crate::collections::list::pith_list_release_handle(value),
        CLOSURE_TAG_MAP => crate::collections::map::pith_map_release_handle(value),
        CLOSURE_TAG_SET => crate::collections::set::pith_set_release_handle(value),
        CLOSURE_TAG_BYTES => crate::bytes::pith_bytes_release(value),
        CLOSURE_TAG_STRUCT => pith_struct_release(value),
        CLOSURE_TAG_CLOSURE => pith_closure_release(value),
        CLOSURE_TAG_WEAK => pith_struct_weak_release(value),
        _ => {}
    }
}

unsafe fn pith_closure_mut<'a>(handle: i64) -> Option<&'a mut PithClosure> {
    closure_base(handle)?;
    Some(&mut *(handle as *mut PithClosure))
}

unsafe fn pith_closure_ref<'a>(handle: i64) -> Option<&'a PithClosure> {
    closure_base(handle)
}

#[no_mangle]
pub extern "C" fn pith_closure_new(func_ptr: i64) -> i64 {
    let ptr = Box::into_raw(Box::new(PithClosure {
        magic: CLOSURE_MAGIC,
        func_ptr,
        ref_count: std::sync::atomic::AtomicI64::new(1),
        env: [0; PITH_CLOSURE_ENV_SLOTS],
        env_tags: [0; PITH_CLOSURE_ENV_SLOTS],
        cycle_flags: std::sync::atomic::AtomicU8::new(0),
    }));
    ptr as i64
}

/// One more owner of this closure.
///
/// # Safety
/// handle must be a valid closure handle or garbage (registry-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_closure_retain(handle: i64) {
    if let Some(closure) = pith_closure_ref(handle) {
        closure
            .ref_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Drop one owner; the last release drops the counts the closure took
/// on its captured values, then frees the closure. this is the fix for
/// the closure-environment leak — closures used to allocate with no
/// release path at all.
///
/// # Safety
/// handle must be a valid closure handle or garbage (registry-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_closure_release(handle: i64) {
    let Some(closure) = pith_closure_ref(handle) else {
        return;
    };
    // cycle-gc suspect hook, before our count is given up: while we still
    // hold it the closure cannot die under the hook, which is what makes
    // marking it safe against a concurrent final release (see
    // `maybe_suspect_struct` for the full argument).
    if crate::cycle::cycle_gc_enabled()
        && closure.ref_count.load(std::sync::atomic::Ordering::Relaxed) > 1
    {
        maybe_suspect_closure(closure, handle);
    }
    let prev = closure
        .ref_count
        .fetch_sub(1, std::sync::atomic::Ordering::Release);
    if prev > 1 {
        return;
    }
    if prev <= 0 {
        // over-release: restore and leave it alone
        closure
            .ref_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
    for slot in 0..PITH_CLOSURE_ENV_SLOTS {
        release_captured_value(closure.env[slot], closure.env_tags[slot]);
    }
    let buffered = closure
        .cycle_flags
        .load(std::sync::atomic::Ordering::Relaxed)
        & 1
        != 0;
    // scrub the magic so a stale handle fails closure_base before we
    // hand the box back to the allocator.
    (handle as *mut u32).write(0);
    // a closure that dies while the suspect buffer points at it keeps its
    // shell: the captures above are already released, so deferring only the
    // box drop to the graveyard leaks the shell until the collector frees it,
    // and the scrubbed magic keeps the buffered handle from validating.
    if buffered && crate::cycle::cycle_gc_enabled() {
        crate::cycle::graveyard_defer(handle as usize, crate::cycle::CYCLE_KIND_CLOSURE);
        return;
    }
    drop(Box::from_raw(handle as *mut PithClosure));
}

/// Mark a closure as a cycle suspect and hand it to the buffer. Outlined and
/// cold for the same reason the perf recorders are: the release fast path
/// must stay frameless when the flag is off.
#[cold]
#[inline(never)]
unsafe fn maybe_suspect_closure(closure: &PithClosure, handle: i64) {
    if closure
        .cycle_flags
        .fetch_or(1, std::sync::atomic::Ordering::Relaxed)
        & 1
        != 0
    {
        return; // already buffered
    }
    crate::cycle::cycle_suspect(handle as usize, crate::cycle::CYCLE_KIND_CLOSURE);
}

/// Drop a closure's buffered mark (overflow, or a collector drain). Magic-
/// checked, so a closure that already died and was freed is a no-op.
pub(crate) unsafe fn cycle_clear_closure_buffered(handle: i64) {
    if let Some(closure) = pith_closure_ref(handle) {
        closure
            .cycle_flags
            .fetch_and(!1, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
pub(crate) unsafe fn closure_buffered_for_tests(handle: i64) -> bool {
    pith_closure_ref(handle).is_some_and(|closure| {
        closure
            .cycle_flags
            .load(std::sync::atomic::Ordering::Relaxed)
            & 1
            != 0
    })
}

#[no_mangle]
pub unsafe extern "C" fn pith_closure_get_fn(handle: i64) -> i64 {
    if let Some(closure) = pith_closure_ref(handle) {
        closure.func_ptr
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_closure_set_env(handle: i64, slot: i64, value: i64) {
    if slot < 0 || (slot as usize) >= PITH_CLOSURE_ENV_SLOTS {
        return;
    }
    if let Some(closure) = pith_closure_mut(handle) {
        closure.env[slot as usize] = value;
    }
}

/// Store a captured value that carries a reference count, tagging the
/// slot so the closure releases that count when it is freed. the caller
/// has already retained the value into the closure.
///
/// # Safety
/// handle must be a valid closure handle or garbage (registry-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_closure_set_env_rc(handle: i64, slot: i64, value: i64, tag: i64) {
    if slot < 0 || (slot as usize) >= PITH_CLOSURE_ENV_SLOTS {
        return;
    }
    if let Some(closure) = pith_closure_mut(handle) {
        closure.env[slot as usize] = value;
        closure.env_tags[slot as usize] = tag as u8;
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_closure_get_env(handle: i64, slot: i64) -> i64 {
    if slot < 0 || (slot as usize) >= PITH_CLOSURE_ENV_SLOTS {
        return 0;
    }
    if let Some(closure) = pith_closure_ref(handle) {
        closure.env[slot as usize]
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_print(s: string::PithString) {
    if s.ptr.is_null() || s.len == 0 {
        println!();
        return;
    }

    let slice = std::slice::from_raw_parts(s.ptr, s.len as usize);
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
        let base = (s as *const u8).sub(CSTRING_HEADER_SIZE);
        (base.add(4) as *const u32).read()
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
        (
            (base.add(STRUCT_OFF_STRONG) as *const u32).read(),
            (base.add(STRUCT_OFF_WEAK) as *const u32).read(),
        )
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

            // the target is alive: a weak read returns the pointer
            assert_eq!(pith_struct_weak_load(s), s);

            // drop the only strong owner: value dies, header stays (weak > 0)
            pith_struct_release(s);
            assert_eq!(pith_struct_weak_load(s), 0); // dead -> none

            // dropping the last weak frees the header
            pith_struct_weak_release(s);
        }
    }

    // structs travel between tasks, and tasks run on different worker threads,
    // so the last strong owner can die on a thread the weak reader never runs
    // on. the dead flag is an atomic for exactly this shape; the join gives the
    // load a happens-after edge, so it must observe the death.
    #[test]
    fn weak_load_sees_a_release_from_another_thread() {
        unsafe {
            let s = pith_struct_alloc(1);
            pith_struct_weak_retain(s);
            assert_eq!(pith_struct_weak_load(s), s);

            std::thread::spawn(move || pith_struct_release(s))
                .join()
                .expect("releasing thread panicked");

            assert_eq!(pith_struct_weak_load(s), 0);
            pith_struct_weak_release(s);
        }
    }

    #[test]
    fn weak_load_on_a_dead_or_invalid_pointer_is_none() {
        unsafe {
            assert_eq!(pith_struct_weak_load(0), 0);
            assert_eq!(pith_struct_weak_load(12345), 0);
            let s = pith_struct_alloc(1);
            pith_struct_release(s); // fully freed, no weak refs
            assert_eq!(pith_struct_weak_load(s), 0); // magic scrubbed -> none
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
            pith_closure_set_env(handle, PITH_CLOSURE_ENV_SLOTS as i64, 99);
            assert_eq!(pith_closure_get_fn(handle), 77);
            assert_eq!(pith_closure_get_env(handle, 0), 42);
            assert_eq!(pith_closure_get_env(handle, -1), 0);
            assert_eq!(
                pith_closure_get_env(handle, PITH_CLOSURE_ENV_SLOTS as i64),
                0
            );
        }
    }
}

#[no_mangle]
pub extern "C" fn pith_print_int(n: i64) {
    println!("{}", n);
}

#[no_mangle]
pub unsafe extern "C" fn pith_concat_cstr(a: *const i8, b: *const i8) -> *mut i8 {
    if a.is_null() {
        return if b.is_null() {
            std::ptr::null_mut()
        } else {
            pith_strdup(b)
        };
    }
    if b.is_null() {
        return pith_strdup(a);
    }

    let len_a = string::pith_cstring_len(a) as usize;
    let len_b = string::pith_cstring_len(b) as usize;
    let result = pith_alloc_cstring(len_a + len_b);

    std::ptr::copy_nonoverlapping(a, result, len_a);
    std::ptr::copy_nonoverlapping(b, result.add(len_a), len_b);
    result
}

#[no_mangle]
pub unsafe extern "C" fn pith_strdup(ptr: *const i8) -> *mut i8 {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }

    let len = string::pith_cstring_len(ptr) as usize;
    let result = pith_alloc_cstring(len);
    std::ptr::copy_nonoverlapping(ptr, result, len);

    result
}

#[no_mangle]
pub unsafe extern "C" fn pith_print_cstr(ptr: *const i8) {
    if ptr.is_null() {
        println!();
        return;
    }

    let len = string::pith_cstring_len(ptr) as usize;
    let slice = std::slice::from_raw_parts(ptr as *const u8, len);
    if let Ok(str_ref) = std::str::from_utf8(slice) {
        println!("{}", str_ref);
    } else {
        println!();
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_print_err(ptr: *const i8) {
    if ptr.is_null() {
        eprintln!();
        return;
    }

    let len = string::pith_cstring_len(ptr) as usize;
    let slice = std::slice::from_raw_parts(ptr as *const u8, len);
    if let Ok(str_ref) = std::str::from_utf8(slice) {
        eprintln!("{}", str_ref);
    } else {
        eprintln!();
    }
}

/// Report an error escaping `main` and exit nonzero. The emitter calls this on
/// main's error-return paths, because the entry glue pins main's process exit
/// code to 0 — without this, a program that failed out of main looked like it
/// succeeded. `is_string` says whether `err` is a string handle; a typed error
/// gets a generic line rather than a read through a non-string pointer.
#[no_mangle]
pub unsafe extern "C" fn pith_main_error_exit(err: i64, is_string: i64) -> i64 {
    if is_string != 0 && err != 0 {
        let ptr = err as *const i8;
        let len = string::pith_cstring_len(ptr) as usize;
        let slice = std::slice::from_raw_parts(ptr as *const u8, len);
        if let Ok(msg) = std::str::from_utf8(slice) {
            eprintln!("error: {}", msg);
        } else {
            eprintln!("error: main returned an error");
        }
    } else {
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

    let mut pa = a;
    let mut pb = b;
    loop {
        let ca = *pa;
        let cb = *pb;

        if ca != cb {
            return 0;
        }

        if ca == 0 {
            return 1;
        }

        pa = pa.add(1);
        pb = pb.add(1);
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_ord_cstr(s: *const i8) -> i64 {
    if s.is_null() || *s == 0 {
        return 0;
    }
    *s as i64
}

#[no_mangle]
pub unsafe extern "C" fn pith_chr_cstr(n: i64) -> *mut i8 {
    let ptr = pith_alloc_cstring(1);
    *ptr = (n as u8) as i8;
    ptr
}

// one verdict for the whole process. a built-in assertion ends the process at
// the point it fails, so it never needs to consult this; std.testing's checks
// report and carry on, and this is how one of them that failed still reaches
// the exit code. the test runner forks per test, so "the process" is the child
// running a single test and the flag cannot leak into the next one.
static TEST_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[no_mangle]
pub extern "C" fn pith_assert(cond: i64) {
    if cond == 0 {
        TEST_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
        eprintln!("assertion failed");
    }
}

/// Set or clear the process verdict. `std.testing` records a failure through
/// here so a check it counted still fails the run; clearing is for the tally's
/// own tests, which provoke failures deliberately.
#[no_mangle]
pub extern "C" fn pith_test_set_failed(failed: i64) {
    TEST_FAILED.store(failed != 0, std::sync::atomic::Ordering::Relaxed);
}

// --- test value equality ----------------------------------------------------
//
// assert_eq compares by value. bytes and lists are heap handles, so comparing
// the raw handles would report two equal-content collections as different; these
// walk the contents instead. a handle that does not resolve falls back to
// identity so a malformed value still gives a definite answer.

#[no_mangle]
pub unsafe extern "C" fn pith_list_eq_scalar(a: i64, b: i64) -> i64 {
    let (Some(x), Some(y)) = (
        crate::collections::list::list_ref_from_handle(a),
        crate::collections::list::list_ref_from_handle(b),
    ) else {
        return (a == b) as i64;
    };
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
pub unsafe extern "C" fn pith_list_eq_string(a: i64, b: i64) -> i64 {
    let (Some(x), Some(y)) = (
        crate::collections::list::list_ref_from_handle(a),
        crate::collections::list::list_ref_from_handle(b),
    ) else {
        return (a == b) as i64;
    };
    if x.len() != y.len() {
        return 0;
    }
    for i in 0..x.len() {
        let pa = x.get_value(i).unwrap_or(0) as *const i8;
        let pb = y.get_value(i).unwrap_or(0) as *const i8;
        if pith_cstring_eq(pa, pb) == 0 {
            return 0;
        }
    }
    1
}

unsafe fn bytes_hex(handle: i64) -> String {
    if let Some(b) = crate::bytes::pith_bytes_ref(handle) {
        let mut s = String::from("0x");
        for byte in &b.data {
            s.push_str(&format!("{:02x}", byte));
        }
        s
    } else {
        format!("<bytes {}>", handle)
    }
}

unsafe fn list_display(handle: i64, string_elems: bool) -> String {
    let Some(list) = crate::collections::list::list_ref_from_handle(handle) else {
        return format!("<list {}>", handle);
    };
    let mut s = String::from("[");
    for i in 0..list.len() {
        if i > 0 {
            s.push_str(", ");
        }
        let v = list.get_value(i).unwrap_or(0);
        if string_elems {
            s.push_str(&format!("\"{}\"", cstr_to_display(v as *const i8)));
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
    match kind {
        1 => f64::from_bits(v as u64).to_string(),
        2 => format!("\"{}\"", cstr_to_display(v as *const i8)),
        3 => bytes_hex(v),
        4 => if v != 0 { "true".to_string() } else { "false".to_string() },
        5 => list_display(v, false),
        6 => list_display(v, true),
        _ => v.to_string(),
    }
}

/// Report an assert_eq failure with both sides decoded per `kind`.
///
/// # Safety
/// `a`/`b` must be valid values of the given `kind` for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_assert_eq_fail(a: i64, b: i64, kind: i64) {
    TEST_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
    eprintln!("assertion failed: {} != {}", assert_operand(a, kind), assert_operand(b, kind));
}

/// Report an assert_ne failure. Both sides are shown even though they compared
/// equal: which of two supposedly different values the pair collapsed to is the
/// first thing anyone reading the failure wants to know.
///
/// # Safety
/// `a`/`b` must be valid values of the given `kind` for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_assert_ne_fail(a: i64, b: i64, kind: i64) {
    TEST_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
    eprintln!("assertion failed: {} == {}", assert_operand(a, kind), assert_operand(b, kind));
}

// --- test runner ------------------------------------------------------------
//
// each `test "..."` block compiles to a `__test_i` function; the generated main
// is a pure dispatcher that forks once per test and records the outcome. running
// each test in its own child means a failing assert's exit(1), or even a crash,
// ends only that test — the others still run, and the parent reports every one.

use std::sync::atomic::{AtomicUsize, Ordering};

static TEST_PASS: AtomicUsize = AtomicUsize::new(0);
static TEST_FAIL: AtomicUsize = AtomicUsize::new(0);
static TEST_FILTERED: AtomicUsize = AtomicUsize::new(0);
static TEST_SKIPPED: AtomicUsize = AtomicUsize::new(0);

// the name of the test running in the current child, so skip_test can print a
// full result line. set once at the top of each child.
static CURRENT_TEST: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

// a child that skipped exits with this code; the parent tells it apart from a
// pass (0) or a failure (anything else).
const TEST_SKIP_CODE: i32 = 42;

/// Record the name of the test about to run in this child.
///
/// # Safety
/// `name` must point to a valid NUL-terminated string for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_test_enter(name: *const i8) -> i64 {
    if let Ok(mut current) = CURRENT_TEST.lock() {
        *current = cstr_to_display(name);
    }
    0
}

/// Skip the current test with a reason: print the result line and exit the child
/// with the skip code, which the parent tallies as skipped rather than run.
///
/// # Safety
/// `reason` must point to a valid NUL-terminated string for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_test_skip(reason: *const i8) -> i64 {
    use std::io::Write;
    let name = CURRENT_TEST.lock().map(|n| n.clone()).unwrap_or_default();
    println!("  {} ... skipped ({})", name, cstr_to_display(reason));
    let _ = std::io::stdout().flush();
    // panic-guard: a skipped test exits with the skip code the harness reads.
    std::process::exit(TEST_SKIP_CODE);
}

/// Whether a test should run under the current `PITH_TEST_FILTER`. With no
/// filter every test runs; otherwise only names containing the substring do,
/// and the rest are tallied as filtered out.
///
/// # Safety
/// `name` must point to a valid NUL-terminated string for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_test_should_run(name: *const i8) -> i64 {
    let filter = match std::env::var("PITH_TEST_FILTER") {
        Ok(f) if !f.is_empty() => f,
        _ => return 1,
    };
    if cstr_to_display(name).contains(&filter) {
        1
    } else {
        TEST_FILTERED.fetch_add(1, Ordering::Relaxed);
        0
    }
}

/// Flush pending output and fork. Returns the child pid to the parent and 0 to
/// the child, matching the C fork contract. Flushing first keeps the child from
/// inheriting (and re-emitting) the parent's buffered stdout.
#[no_mangle]
pub extern "C" fn pith_test_fork() -> i64 {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    unsafe { libc::fork() as i64 }
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
    if TEST_FAILED.load(std::sync::atomic::Ordering::Relaxed) {
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
    let mut status: i32 = 0;
    libc::waitpid(pid as i32, &mut status, 0);
    let name_str = cstr_to_display(name);
    if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == TEST_SKIP_CODE {
        // the child already printed its own "... skipped (reason)" line
        TEST_SKIPPED.fetch_add(1, Ordering::Relaxed);
        0
    } else if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
        TEST_PASS.fetch_add(1, Ordering::Relaxed);
        println!("  {} ... ok", name_str);
        0
    } else {
        TEST_FAIL.fetch_add(1, Ordering::Relaxed);
        if libc::WIFSIGNALED(status) {
            println!("  {} ... FAILED (killed by signal {})", name_str, libc::WTERMSIG(status));
        } else {
            println!("  {} ... FAILED", name_str);
        }
        1
    }
}

/// Print the final tally and return the process exit code (non-zero on any
/// failure).
#[no_mangle]
pub extern "C" fn pith_test_summary() -> i64 {
    let passed = TEST_PASS.load(Ordering::Relaxed);
    let failed = TEST_FAIL.load(Ordering::Relaxed);
    let filtered = TEST_FILTERED.load(Ordering::Relaxed);
    let skipped = TEST_SKIPPED.load(Ordering::Relaxed);
    println!();
    let mut line = format!("{} passed, {} failed", passed, failed);
    if skipped > 0 {
        line.push_str(&format!(", {} skipped", skipped));
    }
    if filtered > 0 {
        line.push_str(&format!(", {} filtered out", filtered));
    }
    println!("{}", line);
    if failed > 0 {
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

#[no_mangle]
pub extern "C" fn pith_bit_shl(a: i64, b: i64) -> i64 {
    a << b
}

#[no_mangle]
pub extern "C" fn pith_bit_shr(a: i64, b: i64) -> i64 {
    ((a as u64) >> b) as i64
}

#[no_mangle]
pub extern "C" fn pith_uint(n: i64) -> i64 {
    n
}

#[no_mangle]
pub extern "C" fn pith_int8(n: i64) -> i64 {
    (n as i8) as i64
}

#[no_mangle]
pub extern "C" fn pith_int16(n: i64) -> i64 {
    (n as i16) as i64
}

#[no_mangle]
pub extern "C" fn pith_int32(n: i64) -> i64 {
    (n as i32) as i64
}

#[no_mangle]
pub extern "C" fn pith_int64(n: i64) -> i64 {
    n
}

#[no_mangle]
pub extern "C" fn pith_uint8(n: i64) -> i64 {
    (n as u8) as i64
}

#[no_mangle]
pub extern "C" fn pith_uint16(n: i64) -> i64 {
    (n as u16) as i64
}

#[no_mangle]
pub extern "C" fn pith_uint32(n: i64) -> i64 {
    (n as u32) as i64
}

#[no_mangle]
pub extern "C" fn pith_uint64(n: i64) -> i64 {
    n
}

#[no_mangle]
pub extern "C" fn pith_abs(n: i64) -> i64 {
    n.abs()
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

#[no_mangle]
pub extern "C" fn pith_log(n: f64) -> f64 {
    n.ln()
}

#[no_mangle]
pub extern "C" fn pith_log10(n: f64) -> f64 {
    n.log10()
}

#[no_mangle]
pub extern "C" fn pith_log2(n: f64) -> f64 {
    n.log2()
}

#[no_mangle]
pub extern "C" fn pith_exp(n: f64) -> f64 {
    n.exp()
}

#[no_mangle]
pub extern "C" fn pith_abs_float(n: f64) -> f64 {
    n.abs()
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
    let mut pa = a;
    let mut pb = b;
    loop {
        let ca = *pa as u8;
        let cb = *pb as u8;
        if ca != cb {
            return if ca < cb { -1 } else { 1 };
        }
        if ca == 0 {
            return 0;
        }
        pa = pa.add(1);
        pb = pb.add(1);
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_lt(a: *const i8, b: *const i8) -> i64 {
    if pith_cstring_compare(a, b) < 0 {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_gt(a: *const i8, b: *const i8) -> i64 {
    if pith_cstring_compare(a, b) > 0 {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_lte(a: *const i8, b: *const i8) -> i64 {
    if pith_cstring_compare(a, b) <= 0 {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_cstring_gte(a: *const i8, b: *const i8) -> i64 {
    if pith_cstring_compare(a, b) >= 0 {
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_int_to_cstr(n: i64) -> *mut i8 {
    let s = n.to_string();
    let len = s.len();
    pith_copy_bytes_to_cstring(&s.as_bytes()[..len])
}

#[no_mangle]
pub unsafe extern "C" fn pith_uint_to_cstr(n: i64) -> *mut i8 {
    let s = (n as u64).to_string();
    let len = s.len();
    pith_copy_bytes_to_cstring(&s.as_bytes()[..len])
}

#[no_mangle]
pub extern "C" fn pith_float_to_cstr(n: f64) -> *mut i8 {
    let s = if n == n.floor() && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        let formatted = format!("{:.6}", n);
        formatted
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    };
    let len = s.len();
    unsafe { pith_copy_bytes_to_cstring(&s.as_bytes()[..len]) }
}

#[no_mangle]
pub extern "C" fn pith_bool_to_cstr(b: i64) -> *mut i8 {
    let s = if b != 0 { "true" } else { "false" };
    let len = s.len();
    unsafe { pith_copy_bytes_to_cstring(&s.as_bytes()[..len]) }
}

// --- container display -------------------------------------------------
//
// interpolating a list or map renders it through these. the emitter passes
// an element-kind code because the runtime cannot tell an int list from a
// float list (both store 8-byte values): 0=int, 1=float, 2=bool, 3=string.

pub(crate) unsafe fn display_value_for_map(out: &mut String, raw: i64, kind: i64) {
    display_value(out, raw, kind)
}

unsafe fn display_value(out: &mut String, raw: i64, kind: i64) {
    match kind {
        1 => {
            let f = f64::from_bits(raw as u64);
            let ptr = pith_float_to_cstr(f);
            out.push_str(&cstr_to_display(ptr));
        }
        2 => out.push_str(if raw != 0 { "true" } else { "false" }),
        3 => out.push_str(&cstr_to_display(raw as *const i8)),
        _ => out.push_str(&raw.to_string()),
    }
}

unsafe fn cstr_to_display(ptr: *const i8) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let len = crate::string::pith_cstring_len(ptr) as usize;
    let bytes = std::slice::from_raw_parts(ptr as *const u8, len);
    String::from_utf8_lossy(bytes).into_owned()
}

/// Render a list as "[a, b, c]" for interpolation. `sorted` is set when the
/// source is a set: hash iteration order is not part of the language, so a
/// displayed set sorts its elements.
#[no_mangle]
pub unsafe extern "C" fn pith_display_list(handle: i64, elem_kind: i64, sorted: i64) -> *mut i8 {
    let mut values: Vec<i64> = Vec::new();
    if let Some(list) = crate::collections::list::list_ref_from_handle(handle) {
        for i in 0..list.len() {
            if let Some(raw) = list.get_value(i) {
                values.push(raw);
            }
        }
    }
    if sorted != 0 {
        match elem_kind {
            1 => values.sort_by(|a, b| {
                f64::from_bits(*a as u64)
                    .partial_cmp(&f64::from_bits(*b as u64))
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            3 => values.sort_by_key(|v| cstr_to_display(*v as *const i8)),
            _ => values.sort(),
        }
    }
    let mut out = String::from("[");
    for (i, raw) in values.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        display_value(&mut out, *raw, elem_kind);
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
        std::alloc::dealloc(ptr as *mut u8, Layout::new::<u8>());
    }
}

#[no_mangle]
pub extern "C" fn pith_int_to_float(n: i64) -> f64 {
    n as f64
}

#[no_mangle]
pub extern "C" fn pith_float_to_int(n: f64) -> i64 {
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
// it. Entries are not reclaimed at thread exit — bounded by the number of
// threadlocal globals, the same acceptable one-time leak as the struct pool.
thread_local! {
    static TLS_GLOBALS: std::cell::RefCell<std::collections::HashMap<i64, i64>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Under the green backend, the map backing the currently-running task's
/// `threadlocal` globals, or `None` on the default os-thread path (and on any
/// green worker moment with no task active). Returning `None` means the caller
/// falls back to the per-OS-thread `TLS_GLOBALS`, so the os-thread path is
/// untouched: one worker OS thread per task there means per-OS-thread already is
/// per-task. Under green, one worker runs many tasks, so we must route to the
/// task's own map instead of sharing the worker's.
fn green_task_tls() -> Option<*mut std::collections::HashMap<i64, i64>> {
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
    if let Some(map) = green_task_tls() {
        // SAFETY: `map` points into the `Box<HashMap>` owned by the green task
        // currently running on this worker thread. `green` installs it around
        // `resume` and clears it after, so it is valid for the whole synchronous
        // duration of this pith call, and only this worker thread touches it. we
        // take only short-lived borrows: the read borrow ends before we call the
        // thunk, and the write borrow is created fresh afterward. that mirrors
        // the `TLS_GLOBALS` path — the thunk may itself touch other threadlocal
        // slots on the same map without a live borrow being held across it.
        if let Some(v) = (*map).get(&slot).copied() {
            return v;
        }
        // panic-guard: calling a compiler-emitted threadlocal initializer thunk.
        let thunk: extern "C" fn() -> i64 = std::mem::transmute(init_thunk as usize);
        let value = thunk();
        (*map).insert(slot, value);
        return value;
    }

    if let Some(v) = TLS_GLOBALS.with(|t| t.borrow().get(&slot).copied()) {
        return v;
    }
    // panic-guard: calling a compiler-emitted threadlocal initializer thunk.
    let thunk: extern "C" fn() -> i64 = std::mem::transmute(init_thunk as usize);
    let value = thunk();
    TLS_GLOBALS.with(|t| {
        // another access on this thread during the thunk can't have run (single
        // thread), so a plain insert is correct.
        t.borrow_mut().insert(slot, value);
    });
    value
}

/// Set this thread's value for a threadlocal global (a reassignment). The old
/// value's refcount is dropped by the compiler-emitted release before this call,
/// the same as a plain global assignment.
#[no_mangle]
pub unsafe extern "C" fn pith_tls_set(slot: i64, value: i64) {
    if let Some(map) = green_task_tls() {
        // SAFETY: same as `pith_tls_get_or_init` — the pointer targets the
        // running task's own map, valid and unaliased for this synchronous call
        // on this worker thread. a single short-lived write borrow.
        (*map).insert(slot, value);
        return;
    }
    TLS_GLOBALS.with(|t| {
        t.borrow_mut().insert(slot, value);
    });
}

// Small structs are allocated and freed constantly — the result box built for
// every `T!` return is the worst offender — and the malloc/free round-trip for
// each one shows up plainly in profiles. These keep a per-thread freelist of
// dead blocks, bucketed by total byte size, so a fresh struct of a common size
// reuses a recycled block instead of hitting the global allocator. A reused
// block is re-zeroed, so it is indistinguishable from an alloc_zeroed one.
//
// Per-thread means no locking and no cross-thread races: a block freed on
// another thread simply lands in that thread's pool. Blocks retained at thread
// exit leak, but the pool is capped, so the leak is bounded and one-time.
//
// The pool is disabled when PITH_STRUCT_FREELIST=0 so `make memcheck` runs see
// every real free — valgrind can only track use-after-free if the block is
// actually handed back to the allocator.
const STRUCT_POOL_MAX_TOTAL: usize = 256; // total bytes incl the 32-byte header
const STRUCT_POOL_CAP: usize = 512; // max retained blocks per size bucket

// 0 = not probed yet, 1 = disabled, 2 = enabled. this check runs on every
// struct alloc and free, so the settled path must be one relaxed load and a
// predictable branch, same as the perf-stats gate.
static STRUCT_POOL_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

#[cold]
fn struct_pool_probe() -> bool {
    let enabled = !matches!(
        std::env::var("PITH_STRUCT_FREELIST").as_deref(),
        Ok("0") | Ok("off") | Ok("false")
    );
    STRUCT_POOL_STATE.store(
        if enabled { 2 } else { 1 },
        std::sync::atomic::Ordering::Relaxed,
    );
    enabled
}

#[inline(always)]
fn struct_pool_enabled() -> bool {
    match STRUCT_POOL_STATE.load(std::sync::atomic::Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => struct_pool_probe(),
    }
}

thread_local! {
    // indexed by total_size / 8; each entry is a stack of reusable blocks.
    static STRUCT_POOL: std::cell::RefCell<Vec<Vec<*mut u8>>> =
        std::cell::RefCell::new(Vec::new());
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
unsafe fn struct_pool_take(total: usize) -> *mut u8 {
    if !struct_pool_enabled() {
        return std::ptr::null_mut();
    }
    let Some(bucket) = struct_pool_bucket(total) else {
        return std::ptr::null_mut();
    };
    let block = STRUCT_POOL.with(|pool| {
        pool.borrow_mut()
            .get_mut(bucket)
            .and_then(|stack| stack.pop())
    });
    match block {
        Some(ptr) => {
            std::ptr::write_bytes(ptr, 0, total);
            ptr
        }
        None => std::ptr::null_mut(),
    }
}

/// Hand a dead block back to the per-thread pool. Returns false (caller should
/// dealloc) when the block is too large or the bucket is full.
unsafe fn struct_pool_return(base: *mut u8, total: usize) -> bool {
    if !struct_pool_enabled() {
        return false;
    }
    let Some(bucket) = struct_pool_bucket(total) else {
        return false;
    };
    STRUCT_POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        if pool.len() <= bucket {
            pool.resize(bucket + 1, Vec::new());
        }
        let stack = &mut pool[bucket];
        if stack.len() >= STRUCT_POOL_CAP {
            return false;
        }
        stack.push(base);
        true
    })
}

#[no_mangle]
pub unsafe extern "C" fn pith_struct_alloc(num_fields: i64) -> i64 {
    use std::alloc::alloc_zeroed;

    crate::perf_stats!(PERF_STRUCT_ALLOCS += 1);
    let size = (num_fields.max(0) as usize) * 8;
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
    let total = size + STRUCT_HEADER;
    let base = struct_pool_take(total);
    let base = if base.is_null() {
        let allocated = alloc_zeroed(pith_layout(total, 8));
        if allocated.is_null() {
            eprintln!("pith runtime error: allocation failed");
            // panic-guard: out of memory.
            std::process::exit(1);
        }
        allocated
    } else {
        base
    };
    (base as *mut u32).write(STRUCT_MAGIC);
    (base.add(4) as *mut u32).write(1); // strong
    (base.add(8) as *mut u32).write(1); // weak: the implicit unit for strong refs
    (base.add(16) as *mut u64).write(size as u64);
    base.add(STRUCT_HEADER) as i64
}

pub(crate) const STRUCT_MAGIC: u32 = 0x50535452; // "PSTR"
pub(crate) const STRUCT_HEADER: usize = 32;

// header field offsets within the base allocation
const STRUCT_OFF_STRONG: usize = 4;
const STRUCT_OFF_WEAK: usize = 8;
const STRUCT_OFF_DEAD: usize = 12;
const STRUCT_OFF_SIZE: usize = 16;
const STRUCT_OFF_DTOR: usize = 24;

unsafe fn struct_strong(base: *mut u8) -> &'static std::sync::atomic::AtomicU32 {
    &*(base.add(STRUCT_OFF_STRONG) as *const std::sync::atomic::AtomicU32)
}

unsafe fn struct_weak(base: *mut u8) -> &'static std::sync::atomic::AtomicU32 {
    &*(base.add(STRUCT_OFF_WEAK) as *const std::sync::atomic::AtomicU32)
}

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
pub(crate) const STRUCT_BUFFERED_BIT: u32 = 2;

unsafe fn struct_dead(base: *mut u8) -> &'static std::sync::atomic::AtomicU32 {
    &*(base.add(STRUCT_OFF_DEAD) as *const std::sync::atomic::AtomicU32)
}

// drop one unit of weak; free the allocation when it reaches zero. this is the
// single place a struct header is deallocated, so the strong path and every
// weak reference funnel their final drop through here.
unsafe fn struct_weak_drop(base: *mut u8) {
    let weak = struct_weak(base);
    if weak.fetch_sub(1, std::sync::atomic::Ordering::Release) != 1 {
        return;
    }
    std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
    let size = (base.add(STRUCT_OFF_SIZE) as *const u64).read() as usize;
    // scrub the magic so any stale pointer fails the check
    (base as *mut u32).write(0);
    let total = size + STRUCT_HEADER;
    // keep the block on the per-thread freelist when it fits; otherwise free it.
    if !struct_pool_return(base, total) {
        std::alloc::dealloc(base, pith_layout(total, 8));
    }
}

/// Build a pith `T?` optional value in the on-heap tuple layout the codegen
/// expects: field 0 is `is_some` (0 or 1), field 1 is the payload. Used by
/// runtime accessors that want to hand back "not present" without collapsing
/// it to a zero/empty sentinel.
pub(crate) fn optional_tuple(is_some: bool, value: i64) -> i64 {
    unsafe {
        let tuple = pith_struct_alloc(2);
        if tuple == 0 {
            return 0;
        }
        let ptr = tuple as *mut i64;
        *ptr = if is_some { 1 } else { 0 };
        *ptr.add(1) = value;
        tuple
    }
}

unsafe fn struct_base(ptr: i64) -> Option<*mut u8> {
    if ptr == 0 || (ptr as u64) % 8 != 0 {
        return None;
    }
    let base = (ptr as usize).checked_sub(STRUCT_HEADER)? as *mut u8;
    if (base as *const u32).read() != STRUCT_MAGIC {
        return None;
    }
    Some(base)
}

/// Attach a compiled destructor to a struct: called with the struct
/// pointer at its final release, before the memory is freed.
///
/// # Safety
/// ptr must be a pith_struct_alloc result; dtor a compiled fn(i64).
#[no_mangle]
pub unsafe extern "C" fn pith_struct_set_dtor(ptr: i64, dtor: i64) {
    if let Some(base) = struct_base(ptr) {
        (base.add(STRUCT_OFF_DTOR) as *mut u64).write(dtor as u64);
    }
}

/// One more owner of this struct.
///
/// # Safety
/// ptr must be a pith_struct_alloc result or garbage (magic-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_struct_retain(ptr: i64) {
    if let Some(base) = struct_base(ptr) {
        struct_strong(base).fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Drop one owner; the last strong release runs the destructor (releasing the
/// struct's rc fields), marks the value dead, and drops the implicit weak unit
/// — which frees the allocation unless a weak reference is still holding it.
///
/// # Safety
/// ptr must be a pith_struct_alloc result or garbage (magic-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_struct_release(ptr: i64) {
    let Some(base) = struct_base(ptr) else {
        return;
    };
    let strong = struct_strong(base);
    // cycle-gc suspect hook, placed before our count is given up rather than
    // in the prev > 1 arm below. the ordering is load-bearing: while we still
    // hold a strong count the header cannot be freed, so the buffered bit and
    // the buffer's weak retain land on live memory. after the fetch_sub a
    // concurrent owner can finish the whole death path — including the weak
    // drop that frees the header — under the hook's feet. if other owners
    // release between this load and our decrement, the struct simply dies
    // buffered, which the death path below already supports.
    if crate::cycle::cycle_gc_enabled() && strong.load(std::sync::atomic::Ordering::Relaxed) > 1 {
        maybe_suspect_struct(ptr, base);
    }
    let prev = strong.fetch_sub(1, std::sync::atomic::Ordering::Release);
    if prev > 1 {
        return;
    }
    if prev == 0 {
        // over-release: restore and leave the object alone
        strong.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
    let dtor = (base.add(STRUCT_OFF_DTOR) as *const u64).read();
    if dtor != 0 {
        // panic-guard: calling a compiler-emitted struct destructor from its header slot.
        let f: unsafe extern "C" fn(i64) = std::mem::transmute(dtor as usize);
        f(ptr);
    }
    crate::perf_count(&crate::PERF_STRUCT_FREES, 1);
    // the value is now dead; a weak read after this returns none. the release
    // ordering publishes the destructor's writes to any thread whose weak load
    // sees the flag. fetch_or rather than a store so a concurrently-set
    // buffered bit survives. then drop the implicit weak unit, which frees the
    // header if no weak refs remain — for a buffered struct the suspect
    // buffer's weak count keeps the header alive past this point.
    struct_dead(base).fetch_or(STRUCT_DEAD_BIT, std::sync::atomic::Ordering::Release);
    struct_weak_drop(base);
}

/// Mark a struct as a cycle suspect: set the buffered bit, take the weak
/// count the buffer owns, and hand the pointer to the buffer. Only the
/// thread that flips the bit pushes, so many racing releases buffer once.
/// The caller still holds a strong count, so the header is alive throughout.
#[cold]
#[inline(never)]
unsafe fn maybe_suspect_struct(ptr: i64, base: *mut u8) {
    if struct_dead(base).fetch_or(STRUCT_BUFFERED_BIT, std::sync::atomic::Ordering::Relaxed)
        & STRUCT_BUFFERED_BIT
        != 0
    {
        return; // already buffered
    }
    struct_weak(base).fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    crate::cycle::cycle_suspect(ptr as usize, crate::cycle::CYCLE_KIND_STRUCT);
}

/// Drop a struct's buffered mark (overflow, or a collector drain). The
/// caller drops the buffer's weak count separately; magic-checked so a
/// header that is already gone is a no-op.
pub(crate) unsafe fn cycle_clear_struct_buffered(ptr: i64) {
    if let Some(base) = struct_base(ptr) {
        struct_dead(base).fetch_and(!STRUCT_BUFFERED_BIT, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
pub(crate) unsafe fn struct_dead_word_for_tests(ptr: i64) -> u32 {
    struct_base(ptr)
        .map(|base| struct_dead(base).load(std::sync::atomic::Ordering::Relaxed))
        .unwrap_or(u32::MAX)
}

#[cfg(test)]
pub(crate) unsafe fn struct_weak_count_for_tests(ptr: i64) -> u32 {
    struct_base(ptr)
        .map(|base| struct_weak(base).load(std::sync::atomic::Ordering::Relaxed))
        .unwrap_or(u32::MAX)
}

/// One more weak (non-owning) reference to this struct. Does not affect the
/// strong count, so it never keeps the value alive — only the header.
///
/// # Safety
/// ptr must be a pith_struct_alloc result or garbage (magic-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_struct_weak_retain(ptr: i64) {
    if let Some(base) = struct_base(ptr) {
        struct_weak(base).fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Drop one weak reference; frees the header if this was the last hold on it.
///
/// # Safety
/// ptr must be a pith_struct_alloc result or garbage (magic-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_struct_weak_release(ptr: i64) {
    if let Some(base) = struct_base(ptr) {
        struct_weak_drop(base);
    }
}

/// Read a weak reference: returns the struct pointer if the target is still
/// alive, or 0 if it has been released. Never dereferences dead field data —
/// the header stays valid while any weak reference holds it, and the dead flag
/// is set the instant the last strong owner goes away.
///
/// # Safety
/// ptr must be a pith_struct_alloc result, 0, or garbage (magic-checked).
#[no_mangle]
pub unsafe extern "C" fn pith_struct_weak_load(ptr: i64) -> i64 {
    let Some(base) = struct_base(ptr) else {
        return 0;
    };
    // acquire pairs with the release fetch_or in `pith_struct_release`: a load
    // that sees the target dead also sees its destructor's effects. test the
    // death bit specifically — a live struct sitting in the cycle collector's
    // suspect buffer has bit 1 set and must still read as alive.
    if struct_dead(base).load(std::sync::atomic::Ordering::Acquire) & STRUCT_DEAD_BIT != 0 {
        return 0;
    }
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn pith_args_to_list() -> i64 {
    let list = pith_list_new(8, 1);

    for arg in std::env::args() {
        let arg_len = arg.len();
        let arg_ptr = pith_copy_bytes_to_cstring(&arg.as_bytes()[..arg_len]);
        pith_list_push_value(list, arg_ptr as i64);
        pith_cstring_release(arg_ptr as *const i8);
    }

    list.ptr as i64
}
