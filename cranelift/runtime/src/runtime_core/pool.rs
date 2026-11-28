//! Per-thread recyclable freelists for small structs and closure shells.

use std::sync::atomic::Ordering;

// A dead closure's block, kept for the next construction. Fixed size, so a
// single stack rather than the struct pool's size buckets, and a stack of its
// own inside the shared pool slot so the two can never hand each other a
// block laid out for the other. Honours the same switch as the struct pool:
// with it off every block goes back to the allocator, which is what lets
// valgrind see a use after free.
pub(super) const CLOSURE_POOL_CAP: usize = 512;

pub(super) fn closure_pool_take() -> *mut u8 {
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
pub(super) const STRUCT_POOL_MAX_TOTAL: usize = 256; // total bytes incl the 32-byte header
pub(super) const STRUCT_POOL_CAP: usize = 512; // max retained blocks per size bucket

/// What one pool slot may hold in recycled struct blocks, in bytes.
///
/// The per-bucket cap alone does not state the bound a reader wants: with 32
/// size buckets each allowed 512 blocks, "512" meant about 2.1 MiB per slot.
/// A single hot size still gets its full 512 blocks (256 bytes × 512 is half
/// of this), while a program cycling through many shapes is held to a number
/// stated in the unit that matters. Retention across the process is this
/// times the peak number of concurrently live threads, because slots are
/// adopted, not created, once the peak has passed.
pub(super) const STRUCT_POOL_MAX_BYTES: usize = 256 * 1024;

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
pub(super) fn struct_pool_enabled() -> bool {
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
pub(super) struct PoolSlot {
    /// indexed by total_size / 8; each entry is a stack of reusable blocks.
    struct_buckets: Vec<Vec<*mut u8>>,
    /// bytes held across every struct bucket, against STRUCT_POOL_MAX_BYTES.
    struct_bytes: usize,
    closures: Vec<*mut u8>,
    owned: std::sync::atomic::AtomicBool,
}

impl PoolSlot {
    #[inline(always)]
    pub(super) fn try_push_closure(&mut self, ptr: *mut u8) -> bool {
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
pub(super) fn pool_slot() -> *mut PoolSlot {
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
pub(super) fn struct_pool_bucket(total: usize) -> Option<usize> {
    if total == 0 || total > STRUCT_POOL_MAX_TOTAL || total % 8 != 0 {
        return None;
    }
    Some(total / 8)
}

/// Reuse a recycled block of exactly `total` bytes, re-zeroed like alloc_zeroed.
/// Returns null on a pool miss (the caller then allocates fresh).
pub(super) fn struct_pool_take(total: usize) -> *mut u8 {
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
pub(super) unsafe fn struct_pool_return(base: *mut u8, total: usize) -> bool {
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
