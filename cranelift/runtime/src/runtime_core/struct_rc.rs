//! 32-byte-header reference-counted structs, weak references, and weak-field
//! synchronization.

use super::pool::{struct_pool_return, struct_pool_take};
use super::pith_layout;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};

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
pub(super) const STRUCT_OFF_STRONG: usize = 4;
pub(super) const STRUCT_OFF_WEAK: usize = 8;
const STRUCT_OFF_DEAD: usize = 12;
const STRUCT_OFF_SIZE: usize = 16;
const STRUCT_OFF_DTOR: usize = 24;

/// Typed view of the 32-byte header sitting immediately before a struct's fields.
/// Atomic fields allow concurrent access through shared `&StructHeader` borrows
/// scoped tightly to [`with_struct_header_for`].
#[repr(C)]
pub(super) struct StructHeader {
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
pub(super) unsafe fn with_struct_header<R>(ptr: i64, f: impl FnOnce(&StructHeader) -> R) -> Option<R> {
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
    let prev = h.flags.fetch_or(STRUCT_SHARED_BIT, Ordering::Release);
    // Always set STRUCT_SHARED_BIT (even on a dead weak header so cross-thread
    // weak ops stay atomic), but only return `true` to traverse children when
    // the struct was newly marked AND is still alive (not dead/destroying), so
    // `share.rs` never invokes a tracer on freed or poisoned field slots.
    prev & (STRUCT_SHARED_BIT | STRUCT_DEAD_BIT) == 0
        && h.strong.load(Ordering::Acquire) < STRUCT_DTOR_GUARD
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
    // Only a thread-local struct can take the lock-free `sole` shortcut: for a
    // shared struct, `strong` and `weak` are separate AtomicU32 words, so a
    // concurrent `weak_upgrade` (strong 1->2) + `weak_release` (weak 2->1)
    // between the two loads would let this thread free a live struct.
    let local = h.is_local(switches);
    if HOOKED && local {
        crate::share::check_local(switches, ptr, "release");
    }
    let sole = local
        && h.strong.load(Ordering::Relaxed) == 1
        && h.weak.load(Ordering::Relaxed) == 1;
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
/// (or is mid-destructor under `STRUCT_DTOR_GUARD`) while its header is still
/// held, `None` when the pointer is not a struct header at all.
pub(crate) unsafe fn cycle_struct_alive(ptr: i64) -> Option<bool> {
    // SAFETY: Preconditions forwarded from cycle collector.
    unsafe {
        with_struct_header(ptr, |h| {
            if h.flags.load(Ordering::Relaxed) & STRUCT_DEAD_BIT != 0 {
                return false;
            }
            let s = h.strong.load(Ordering::Relaxed);
            s > 0 && s < STRUCT_DTOR_GUARD
        })
    }
}

fn live_struct_strong_count(h: &StructHeader) -> Option<u32> {
    if h.flags.load(Ordering::Relaxed) & STRUCT_DEAD_BIT != 0 {
        return None;
    }
    let s = h.strong.load(Ordering::Relaxed);
    if s == 0 || s >= STRUCT_DTOR_GUARD {
        return None;
    }
    Some(s)
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

pub(super) struct WeakFieldGuard(&'static AtomicBool);

impl WeakFieldGuard {
    pub(super) fn lock(slot: i64) -> Self {
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
pub(super) unsafe fn weak_holder_is_local(holder: i64, op: &str) -> bool {
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

