//! Heap- and stack-allocated closure environments (`PithClosure`).

use super::cstring::pith_cstring_release;
use super::pool::{closure_pool_take, pool_slot, struct_pool_enabled};
use super::struct_rc::{pith_struct_release, pith_struct_weak_release};
use crate::child_kind::{
    BYTES as CLOSURE_TAG_BYTES, CHANNEL as CLOSURE_TAG_CHANNEL, CLOSURE as CLOSURE_TAG_CLOSURE,
    LIST as CLOSURE_TAG_LIST, MAP as CLOSURE_TAG_MAP, SET as CLOSURE_TAG_SET,
    STRING as CLOSURE_TAG_STRING, STRUCT as CLOSURE_TAG_STRUCT, SYNC as CLOSURE_TAG_SYNC,
    WEAK as CLOSURE_TAG_WEAK,
};
use std::sync::atomic::Ordering;

pub const PITH_CLOSURE_ENV_SLOTS: usize = 16;


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
pub(super) const PITH_CLOSURE_MAX_ENV_SLOTS: usize = PITH_CLOSURE_ENV_SLOTS + 4096;

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
pub(super) const CLOSURE_BUFFERED_FLAG: u8 = 1;

#[repr(C)]
pub(super) struct PithClosure {
    magic: u32,
    func_ptr: i64,
    ref_count: std::sync::atomic::AtomicI64,
    // inline capture slots; left uninitialized on allocation (matching
    // `closure_stack` in codegen) so `pith_closure_new` avoids zeroing 128 B
    // on every construction. each slot is written before the closure body
    // reads it, and capture walkers check `env_tags` before touching `env`.
    env: [std::mem::MaybeUninit<i64>; PITH_CLOSURE_ENV_SLOTS],
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

    /// Every tagged (refcounted) capture in walk order (inline slots
    /// 0..PITH_CLOSURE_ENV_SLOTS first, then the spill in index order) as
    /// (value, tag) pairs. Only slots with a nonzero tag are read, because
    /// unwritten inline `env` slots remain uninitialized.
    fn captures(&self) -> impl Iterator<Item = (i64, u8)> + '_ {
        self.env
            .iter()
            .zip(self.env_tags.iter().copied())
            .filter_map(|(value, tag)| {
                if tag != 0 {
                    // SAFETY: a nonzero tag is only written by `pith_closure_set_env_rc`
                    // (or codegen) alongside an initialized `value`.
                    Some((unsafe { value.assume_init() }, tag))
                } else {
                    None
                }
            })
            .chain(self.overflow.iter().flat_map(|overflow| {
                overflow
                    .captures
                    .iter()
                    .filter(|c| c.tag != 0)
                    .map(|c| (c.value, c.tag))
            }))
    }

    /// Copy every tagged capture out in walk order and blank the slots so a later
    /// shell free (or cyclic back-edge release) can never release them a
    /// second time.
    fn take_captures(&mut self) -> ([Capture; PITH_CLOSURE_ENV_SLOTS], Vec<Capture>) {
        let mut inline = [Capture { value: 0, tag: 0 }; PITH_CLOSURE_ENV_SLOTS];
        for ((dst, value), tag) in inline
            .iter_mut()
            .zip(self.env.iter_mut())
            .zip(self.env_tags.iter_mut())
        {
            let t = *tag;
            if t != 0 {
                // SAFETY: a nonzero tag guarantees `value` was initialized.
                let v = unsafe { value.assume_init() };
                *dst = Capture { value: v, tag: t };
                *value = std::mem::MaybeUninit::new(0);
                *tag = 0;
            }
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

pub(super) unsafe fn release_captured_value(value: i64, tag: u8) {
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
        unsafe { init_closure_header(ptr, func_ptr) };
        return ptr as i64;
    }
    let mut boxed = Box::<PithClosure>::new_uninit();
    // SAFETY: `init_closure_header` initializes every field except `env`, whose
    // `MaybeUninit<i64>` elements require no initialization.
    unsafe {
        init_closure_header(boxed.as_mut_ptr(), func_ptr);
        Box::into_raw(boxed.assume_init()) as i64
    }
}

/// Initialize a closure shell's header and tag table without zeroing the 128 B
/// inline `env` array. Matches the invariant codegen already relies on for
/// stack-built closures (`closure_stack`): callers write every inline slot they
/// later read, and release/cycle walks gate on `env_tags` (zeroed here).
#[inline(always)]
unsafe fn init_closure_header(ptr: *mut PithClosure, func_ptr: i64) {
    // SAFETY: Caller guarantees `ptr` points to writable memory for `PithClosure`.
    unsafe {
        std::ptr::addr_of_mut!((*ptr).magic).write(CLOSURE_MAGIC);
        std::ptr::addr_of_mut!((*ptr).func_ptr).write(func_ptr);
        std::ptr::addr_of_mut!((*ptr).ref_count).write(std::sync::atomic::AtomicI64::new(1));
        std::ptr::addr_of_mut!((*ptr).env_tags).write([0; PITH_CLOSURE_ENV_SLOTS]);
        std::ptr::addr_of_mut!((*ptr).cycle_flags).write(std::sync::atomic::AtomicU8::new(0));
        std::ptr::addr_of_mut!((*ptr).overflow).write(None);
    }
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
    Retire,
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
    ClosureReleaseAction::Retire
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
    if let Some(ClosureReleaseAction::Retire) = action {
        // Detach captures and retire the shell (scrubbing magic) BEFORE
        // releasing any captured value: a captured `Drop` method hitting a GC
        // safepoint must never observe this dying closure as a live root, and
        // no `&PithClosure` / `&mut PithClosure` borrow may stay live across
        // arbitrary user destructors.
        let ptr = handle as *mut PithClosure;
        // SAFETY: Sole owner after last release; no `&PithClosure` borrow is live.
        let (buffered, taken) = unsafe {
            let closure = &mut *ptr;
            let buffered = closure.cycle_flags.load(Ordering::Relaxed) & CLOSURE_BUFFERED_FLAG != 0;
            let has_tagged = u128::from_ne_bytes(closure.env_tags) != 0 || closure.overflow.is_some();
            let taken = if has_tagged { Some(closure.take_captures()) } else { None };
            (buffered, taken)
        };
        // SAFETY: Captures have been moved out; safe to scrub magic and recycle/free shell.
        unsafe { retire_closure_shell(handle, buffered) };
        if let Some((inline, spill)) = taken {
            for capture in inline.into_iter().chain(spill) {
                if capture.tag != 0 {
                    // SAFETY: Non-zero tag marks a captured value holding one refcount.
                    unsafe { release_captured_value(capture.value, capture.tag) };
                }
            }
        }
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
        EnvSlot::Inline(idx) => {
            closure.env[idx].write(value);
        }
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
            closure.env[idx].write(value);
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
unsafe fn closure_get_env_impl(closure: &PithClosure, slot: EnvSlot) -> i64 {
    match slot {
        // SAFETY: Caller guarantees inline slots are written during construction
        // before the closure body reads them (same invariant as `closure_stack`).
        EnvSlot::Inline(idx) => unsafe { closure.env[idx].assume_init() },
        // an unwritten (or never-spilled) slot answers 0
        EnvSlot::Spill(idx) => closure
            .overflow
            .as_ref()
            .and_then(|overflow| overflow.captures.get(idx))
            .map_or(0, |capture| capture.value),
    }
}

/// Read a captured slot value.
///
/// # Safety
/// `handle` must be a valid closure handle or garbage (magic-checked), and any
/// inline slot `0..PITH_CLOSURE_ENV_SLOTS` read from a valid handle must have
/// been initialized during closure construction.
#[no_mangle]
pub unsafe extern "C" fn pith_closure_get_env(handle: i64, slot: i64) -> i64 {
    let Some(slot) = EnvSlot::classify(slot) else {
        return 0;
    };
    // SAFETY: `handle` is validated by `with_closure` before reading its env.
    unsafe { with_closure(handle, |c| closure_get_env_impl(c, slot)) }.unwrap_or(0)
}
