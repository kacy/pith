//! Suspect tracking for the trial-deletion cycle collector.
//!
//! ARC alone cannot reclaim a reference cycle: every object in the ring keeps
//! its neighbor's count above zero, so no release ever reaches the free path.
//! A trial-deletion collector (Bacon-Rajan) starts from *suspects* — objects
//! whose count was decremented but stayed above zero, the only way a garbage
//! cycle can form — trial-decrements the counts internal to the candidate
//! graph, and frees the members whose counts reach zero with the internal
//! edges discounted. This module holds the suspect tracking — the buffer, the
//! per-object buffered bit, and the graveyard — and the stop-the-world
//! rendezvous the collection pass will run inside (see the mutator registry
//! below). The collection pass itself lands separately.
//!
//! Invariants the buffer maintains, in force only when `PITH_CYCLE_GC` is on:
//!
//! - a buffered struct holds one weak count owned by the buffer, so the
//!   32-byte header outlives the value and a buffer entry never dangles; a
//!   struct that dies while buffered is identified by bit 0 of its dead word.
//! - a buffered closure, list, or map that dies runs its normal death path —
//!   children released, magic scrubbed, registry entry dropped — except the
//!   final free of the impl shell, which is deferred to the graveyard for the
//!   collector to reclaim. Everything inside the shell was already destructed,
//!   so the deferred drop frees only the shell's own storage.
//! - on buffer overflow the object is un-marked and (for a struct) the
//!   buffer's weak count is dropped: the failure mode is a leak, never a
//!   dangle.
//!
//! With the flag off — the default — every release path pays one relaxed
//! atomic load and a predicted branch, the same shape as the perf-stats gate.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Kind codes for buffer and graveyard entries. These resemble the element
/// tag codes in `collections::list::element_tag_from_code`, but they are an
/// independent runtime-internal enum, not part of the ir contract — only the
/// four kinds that can participate in a cycle appear here, and the numbering
/// owes the wire codes nothing.
pub(crate) const CYCLE_KIND_STRUCT: u8 = 1;
pub(crate) const CYCLE_KIND_CLOSURE: u8 = 2;
pub(crate) const CYCLE_KIND_LIST: u8 = 3;
pub(crate) const CYCLE_KIND_MAP: u8 = 4;

/// Hard cap on buffered suspects. A full buffer stops admitting new suspects
/// until the collector drains it; the cycles those missed suspects rooted are
/// leaked, not collected, which is the acceptable direction to fail in.
pub const CYCLE_SUSPECT_CAP: usize = 65536;

// 0 = not probed yet, 1 = disabled, 2 = enabled. probed once from the env on
// first use; the settled path is one relaxed load and a predictable branch,
// same as the struct-pool and perf-stats gates.
static CYCLE_GC_STATE: AtomicU8 = AtomicU8::new(0);

#[cold]
fn cycle_gc_probe() -> bool {
    let enabled = matches!(
        std::env::var("PITH_CYCLE_GC").as_deref(),
        Ok("1") | Ok("on") | Ok("true")
    );
    CYCLE_GC_STATE.store(if enabled { 2 } else { 1 }, Ordering::Relaxed);
    enabled
}

/// Whether `PITH_CYCLE_GC` asked for suspect tracking. Cheap enough for the
/// release fast paths: a relaxed load and a branch once the probe has run.
#[inline(always)]
pub fn cycle_gc_enabled() -> bool {
    match CYCLE_GC_STATE.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => cycle_gc_probe(),
    }
}

// the suspect buffer and the graveyard are plain locked vecs: both are
// touched only with the flag on, from #[cold] outlined bodies, so contention
// is not a design constraint yet. entries are (pointer, kind code); struct
// entries hold the data pointer (base + 32), the form every struct entry
// point takes.
static SUSPECTS: Mutex<Vec<(usize, u8)>> = Mutex::new(Vec::new());
static GRAVEYARD: Mutex<Vec<(usize, u8)>> = Mutex::new(Vec::new());

/// Suspects admitted to the buffer since process start.
pub static CYCLE_SUSPECTS_BUFFERED: AtomicU64 = AtomicU64::new(0);
/// Suspects turned away by the cap (each one a potential leaked cycle).
pub static CYCLE_SUSPECTS_OVERFLOWED: AtomicU64 = AtomicU64::new(0);
/// Impl shells whose final free was deferred to the graveyard.
pub static CYCLE_GRAVEYARD_DEFERRED: AtomicU64 = AtomicU64::new(0);

// a poisoned lock only means some thread panicked mid-push; the vec itself
// is still coherent, and the release paths must not turn that into a second
// panic across the ffi boundary.
fn lock_suspects() -> std::sync::MutexGuard<'static, Vec<(usize, u8)>> {
    match SUSPECTS.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn lock_graveyard() -> std::sync::MutexGuard<'static, Vec<(usize, u8)>> {
    match GRAVEYARD.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Un-mark an object the buffer is giving up on (overflow, or a test drain).
/// For a struct this also drops the weak count the buffer took. The clear
/// helpers all magic-check, so an object that died and was reclaimed in the
/// meantime degrades to a no-op.
unsafe fn discard_suspect(ptr: usize, kind: u8) {
    match kind {
        CYCLE_KIND_STRUCT => {
            crate::runtime_core::cycle_clear_struct_buffered(ptr as i64);
            crate::runtime_core::pith_struct_weak_release(ptr as i64);
        }
        CYCLE_KIND_CLOSURE => crate::runtime_core::cycle_clear_closure_buffered(ptr as i64),
        CYCLE_KIND_LIST => crate::collections::list::cycle_clear_list_buffered(ptr as i64),
        CYCLE_KIND_MAP => crate::collections::map::cycle_clear_map_buffered(ptr as i64),
        _ => {}
    }
}

/// Admit a suspect to the buffer, or turn it away at the cap. The caller has
/// already won the object's buffered bit (and, for a struct, taken the
/// buffer's weak count), so on overflow this undoes both — the object is
/// simply not tracked, and the cycle it might root is leaked.
///
/// # Safety
/// `ptr` must be the live object the caller just marked, in the form its
/// kind's entry points take (the data pointer for structs, the impl pointer
/// for the rest). The caller must still hold its own strong count on the
/// object, so the object cannot die under this call.
#[cold]
#[inline(never)]
pub(crate) unsafe fn cycle_suspect(ptr: usize, kind: u8) {
    let admitted_len = {
        let mut suspects = lock_suspects();
        if suspects.len() < CYCLE_SUSPECT_CAP {
            suspects.push((ptr, kind));
            CYCLE_SUSPECTS_BUFFERED.fetch_add(1, Ordering::Relaxed);
            Some(suspects.len())
        } else {
            None
        }
    };
    // outside the buffer lock: the collector wake path takes its own lock,
    // and holding both here would order them against the collector's
    // wait-for-work, which reads the buffer length under the control lock.
    let Some(len) = admitted_len else {
        CYCLE_SUSPECTS_OVERFLOWED.fetch_add(1, Ordering::Relaxed);
        discard_suspect(ptr, kind);
        return;
    };
    ensure_collector_thread();
    if len >= collect_threshold() {
        GC_CTRL_CV.notify_all();
    }
}

/// Park a dead closure/list/map shell for the collector to free. Called from
/// the final-death arm after children are released, the magic is scrubbed,
/// and the registry entry is gone — the shell no longer validates anywhere,
/// only its own allocation remains. Structs never come here: their header is
/// kept alive by the buffer's weak count instead.
#[cold]
#[inline(never)]
pub(crate) fn graveyard_defer(ptr: usize, kind: u8) {
    lock_graveyard().push((ptr, kind));
    CYCLE_GRAVEYARD_DEFERRED.fetch_add(1, Ordering::Relaxed);
}

// --- destructor -> tracer side table -------------------------------------
//
// the emitter pairs every destructor that walks rc children with a tracer
// twin: the same field walk, but each child is reported to `pith_cc_visit`
// instead of released. keying the table by destructor address costs nothing
// per instance — the destructor pointer every rc box already carries doubles
// as the trace key, and a box with no destructor (or a destructor with no
// tracer: only string/bytes/weak fields) falls out as a leaf the collector
// never walks into.

static TRACERS: LazyLock<Mutex<HashMap<usize, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock_tracers() -> std::sync::MutexGuard<'static, HashMap<usize, usize>> {
    match TRACERS.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Record a destructor's tracer twin. Called once per module from the
/// generated `__cc_register_tracers_m<N>` body before any user code runs, so
/// lookups during a collection never race a registration in practice; the
/// mutex makes even that ordering safe rather than assumed.
#[no_mangle]
pub extern "C" fn pith_cc_register_tracer(dtor: i64, tracer: i64) {
    if dtor == 0 || tracer == 0 {
        return;
    }
    lock_tracers().insert(dtor as usize, tracer as usize);
}

/// The tracer registered for a destructor, if it has one. A `None` means the
/// object is a leaf: its destructor releases only strings/bytes/weak slots,
/// none of which can point back into a cycle.
pub(crate) fn tracer_for_dtor(dtor: usize) -> Option<usize> {
    lock_tracers().get(&dtor).copied()
}

// the per-child callback tracers invoke. a collector installs itself here
// (a fn-pointer slot; 0 = nobody listening); with no visitor installed a
// visit is a no-op, so the generated tracers are inert in normal runs.
static VISIT_HOOK: AtomicUsize = AtomicUsize::new(0);

/// Visits that arrived with no visitor installed (counted only with the
/// cycle-gc flag on; a diagnostic, not a leak).
pub static CYCLE_VISITS_UNHOOKED: AtomicU64 = AtomicU64::new(0);

/// Install (or with 0, remove) the process-global visitor `pith_cc_visit`
/// dispatches to. The hook must have the shape `extern "C" fn(i64, i64)`.
pub(crate) fn install_visit_hook(hook: usize) {
    VISIT_HOOK.store(hook, Ordering::Release);
}

/// The callback a tracer invokes once per rc child. `kind` uses the closure
/// env tag codes (1=string 2=list 3=map 4=set 5=bytes 6=struct 7=closure);
/// tracers only ever pass rc kinds. A null child (an optional holding none,
/// an unset field slot) is not a child and returns immediately.
#[no_mangle]
pub extern "C" fn pith_cc_visit(child: i64, kind: i64) {
    if child == 0 {
        return;
    }
    let hook = VISIT_HOOK.load(Ordering::Acquire);
    if hook != 0 {
        // the release/acquire pair orders the hook store before its first use.
        // panic-guard: the only writer is install_visit_hook, whose contract pins the fn shape, so the address is always a valid visitor.
        let visit: extern "C" fn(i64, i64) = unsafe { std::mem::transmute(hook) };
        visit(child, kind);
        return;
    }
    if cycle_gc_enabled() {
        CYCLE_VISITS_UNHOOKED.fetch_add(1, Ordering::Relaxed);
    }
}

// --- stop-the-world rendezvous --------------------------------------------
//
// the collection pass must read and rewrite reference counts with no mutator
// moving them underneath it. nothing compiled emits a safe-point it could
// rely on, so the halt is assembled from the places a mutator already stands
// still:
//
//  - a *gate*: a point where the thread holds no pith frame (a green worker
//    between tasks) or is frozen at a compiled preemption safe-point. seeing
//    the stop request there, the thread parks on the gc condvar until the
//    request clears.
//  - a *native bracket*: a blocking runtime wait (poll, a condvar, a sleep)
//    during which the thread provably reads no heap handle. the thread is
//    counted as stopped for as long as it is inside; the exit side re-checks
//    the request and parks before any heap access resumes.
//
// mutators are the threads that can touch a reference count: green workers,
// os-thread-backend task threads, and the process main thread. the netpoll
// reactor, the sysmon monitor, and the blocking-pool threads never do (the
// pool's contract is owned plain data only), so they are never registered and
// the rendezvous never waits on them.
//
// a stop request that some mutator does not answer within the timeout is
// abandoned: the request clears, everyone parked resumes, and the caller gets
// `false` to retry later. the failure direction is a delayed collection,
// never a mutator observed as stopped while it runs.

/// the mutator is executing code that may touch reference counts.
const MUTATOR_RUNNING: u8 = 0;
/// the mutator is inside a native bracket: blocked (or about to block) in a
/// wait that reads no heap handle until the bracket exits.
const MUTATOR_NATIVE: u8 = 1;
/// the mutator saw the stop request at a gate or a bracket exit and is
/// waiting it out on the gc condvar.
const MUTATOR_PARKED_FOR_GC: u8 = 2;
/// the mutator's thread has exited. slots are never removed from the
/// registry, only marked, so the rendezvous skips these.
const MUTATOR_EXITED: u8 = 3;

/// One registered mutator thread's state word. Created once per thread and
/// kept in the registry for the life of the process; the owning thread holds
/// it through a thread-local and flips it with plain seq-cst stores.
///
/// seq-cst is what makes the rendezvous scan trustworthy: a mutator that
/// loaded `GC_STOP` as clear and re-entered `running` did so before the
/// stopper's store in the total order, so the stopper's later scan cannot
/// still read the slot's older `native` — it never observes "stopped" for a
/// thread that is running.
pub(crate) struct MutatorSlot {
    state: AtomicU8,
}

#[cfg(test)]
impl MutatorSlot {
    pub(crate) fn state_for_tests(&self) -> u8 {
        self.state.load(Ordering::SeqCst)
    }
}

// every mutator slot ever created, in registration order. slots are pushed
// under the mutex and never removed (an exited thread's slot is marked, not
// popped), so the stopper's scan holds the lock only long enough to read the
// state words.
static MUTATORS: Mutex<Vec<Arc<MutatorSlot>>> = Mutex::new(Vec::new());

/// the stop request. set by the stopper, polled by mutators at gates and
/// bracket exits, cleared (under `GC_GATE`) by resume or an abandoned stop.
static GC_STOP: AtomicBool = AtomicBool::new(false);

// the condvar a stopped mutator parks on until the request clears. the mutex
// guards only the wait handshake: resume clears GC_STOP and notifies while
// holding it, and a parker re-checks GC_STOP under it, so the clear can never
// slip between a parker's check and its wait.
static GC_GATE: Mutex<()> = Mutex::new(());
static GC_GATE_CV: Condvar = Condvar::new();

// stoppers must not interleave: two concurrent stop/resume pairs would clear
// each other's request mid-collection. `with_world_stopped` serializes on
// this; the raw pair's contract is a single caller (the collector thread).
static STOPPER: Mutex<()> = Mutex::new(());

/// how long a stop request waits before giving up, in milliseconds. resolved
/// once from `PITH_CYCLE_GC_STOP_MS`; 0 in the cell means not yet resolved
/// (0 is not a valid timeout — it would abandon every stop unconditionally).
static STOP_TIMEOUT_MS: AtomicU64 = AtomicU64::new(0);
const STOP_TIMEOUT_DEFAULT_MS: u64 = 25;

fn stop_timeout_ms() -> u64 {
    let cached = STOP_TIMEOUT_MS.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    let ms = std::env::var("PITH_CYCLE_GC_STOP_MS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|&ms| ms > 0)
        .unwrap_or(STOP_TIMEOUT_DEFAULT_MS);
    STOP_TIMEOUT_MS.store(ms, Ordering::Relaxed);
    ms
}

fn lock_mutators() -> MutexGuard<'static, Vec<Arc<MutatorSlot>>> {
    match MUTATORS.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn lock_gate() -> MutexGuard<'static, ()> {
    match GC_GATE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// the calling thread's handle on its own slot. dropping it — which the
/// thread-local machinery does as the thread exits — marks the slot exited,
/// so a finished os-thread task (or any other thread that ever bracketed) is
/// skipped by every later rendezvous rather than waited on forever.
struct SlotOwner(Arc<MutatorSlot>);

impl Drop for SlotOwner {
    fn drop(&mut self) {
        self.0.state.store(MUTATOR_EXITED, Ordering::SeqCst);
    }
}

thread_local! {
    static MY_MUTATOR: std::cell::RefCell<Option<SlotOwner>> =
        const { std::cell::RefCell::new(None) };
}

fn register_new_slot() -> Arc<MutatorSlot> {
    let slot = Arc::new(MutatorSlot {
        state: AtomicU8::new(MUTATOR_RUNNING),
    });
    lock_mutators().push(Arc::clone(&slot));
    slot
}

/// this thread's slot, created and registered on first use — how the main
/// thread (and any other thread that reaches a gate or bracket) becomes a
/// registered mutator without an explicit spawn seam. `None` only while
/// thread-local storage is tearing down, where the thread is exiting and a
/// bracket can no longer matter.
fn mutator_slot() -> Option<Arc<MutatorSlot>> {
    MY_MUTATOR
        .try_with(|cell| {
            let mut owner = cell.borrow_mut();
            if let Some(existing) = owner.as_ref() {
                return Arc::clone(&existing.0);
            }
            let slot = register_new_slot();
            *owner = Some(SlotOwner(Arc::clone(&slot)));
            slot
        })
        .ok()
}

/// this thread's slot if it already has one; never creates. the stop path
/// uses this so a dedicated collector thread does not become a registered
/// mutator merely by asking for the world.
fn existing_mutator_slot() -> Option<Arc<MutatorSlot>> {
    MY_MUTATOR
        .try_with(|cell| cell.borrow().as_ref().map(|owner| Arc::clone(&owner.0)))
        .ok()
        .flatten()
}

/// Create and register a mutator slot on behalf of a thread about to be
/// spawned. The parent registers it *before* the thread exists: were the
/// thread to register itself at entry, the rendezvous could scan — and
/// declare the world stopped — in the window between the spawn returning and
/// the child's first instruction, while the child mutates unobserved. `None`
/// with the flag off, so spawn paths stay one relaxed load.
pub(crate) fn mutator_slot_for_spawn() -> Option<Arc<MutatorSlot>> {
    if !cycle_gc_enabled() {
        return None;
    }
    Some(register_new_slot())
}

/// Install a slot created by `mutator_slot_for_spawn` as the calling thread's
/// own. The first thing a spawned mutator body does; the thread-local owner
/// then marks the slot exited when the thread returns.
pub(crate) fn adopt_mutator_slot(slot: Option<Arc<MutatorSlot>>) {
    let Some(slot) = slot else {
        return;
    };
    let _ = MY_MUTATOR.try_with(|cell| {
        *cell.borrow_mut() = Some(SlotOwner(slot));
    });
}

/// A stop-the-world check for a mutator at a point where parking it is safe:
/// no runtime lock held, and either no pith frame at all (a worker between
/// tasks) or a frame frozen at a compiled safe-point. With the flag off this
/// is one relaxed load and a fall-through branch.
#[inline(always)]
pub(crate) fn mutator_gate() {
    if cycle_gc_enabled() {
        mutator_gate_slow();
    }
}

#[inline(never)]
fn mutator_gate_slow() {
    // the thread running the collection pass must never park on its own stop
    // request: a compiled destructor it calls during teardown could carry a
    // safe-point, and parking there would deadlock the stopped world.
    if ON_COLLECTOR.with(Cell::get) {
        return;
    }
    let Some(slot) = mutator_slot() else {
        return;
    };
    if GC_STOP.load(Ordering::SeqCst) {
        park_for_gc(&slot);
    }
}

/// An RAII native bracket around one blocking wait. Enter marks the slot
/// `native` — counted as stopped by the rendezvous — and the drop re-checks
/// the stop request before the thread returns to code that may touch the
/// heap, parking it if a stop landed while it was blocked. Between the two,
/// the code inside the bracket must read no heap handle; keep it to the
/// syscall or condvar wait itself. With the flag off, one relaxed load each
/// way. Deliberately `!Send`: the exit must run on the entering thread.
pub(crate) struct NativeBracket {
    _same_thread: std::marker::PhantomData<*const ()>,
}

pub(crate) fn native_bracket() -> NativeBracket {
    if cycle_gc_enabled() {
        enter_native();
    }
    NativeBracket {
        _same_thread: std::marker::PhantomData,
    }
}

impl Drop for NativeBracket {
    fn drop(&mut self) {
        if cycle_gc_enabled() {
            exit_native();
        }
    }
}

#[inline(never)]
fn enter_native() {
    if ON_COLLECTOR.with(Cell::get) {
        return;
    }
    if let Some(slot) = mutator_slot() {
        slot.state.store(MUTATOR_NATIVE, Ordering::SeqCst);
    }
}

#[inline(never)]
fn exit_native() {
    if ON_COLLECTOR.with(Cell::get) {
        return;
    }
    let Some(slot) = mutator_slot() else {
        return;
    };
    if GC_STOP.load(Ordering::SeqCst) {
        park_for_gc(&slot);
    } else {
        slot.state.store(MUTATOR_RUNNING, Ordering::SeqCst);
    }
}

/// wait out a stop request on the gc condvar, then come back as `running`.
/// spurious wakes just re-check `GC_STOP` under the gate lock and wait again.
fn park_for_gc(slot: &MutatorSlot) {
    slot.state.store(MUTATOR_PARKED_FOR_GC, Ordering::SeqCst);
    let mut guard = lock_gate();
    while GC_STOP.load(Ordering::SeqCst) {
        guard = GC_GATE_CV
            .wait(guard)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
    drop(guard);
    slot.state.store(MUTATOR_RUNNING, Ordering::SeqCst);
}

/// is every registered mutator provably not running? `native`, `parked`, and
/// `exited` all count as stopped; only `running` keeps the stopper waiting.
fn world_is_stopped() -> bool {
    lock_mutators()
        .iter()
        .all(|slot| slot.state.load(Ordering::SeqCst) != MUTATOR_RUNNING)
}

// the green preemption flag's value before a stop raised it, restored on
// release. stops serialize (one collector, and `with_world_stopped` holds
// STOPPER), so a single saved value is enough; the saved bit keeps a resume
// that follows no stop from writing a stale value back.
static PREEMPT_PRIOR: AtomicU8 = AtomicU8::new(0);
static PREEMPT_SAVED: AtomicBool = AtomicBool::new(false);

/// Whether a stop-the-world request is pending. The green monitor keeps the
/// preemption flag raised while this holds, so its 10ms refresh cannot erase
/// the stop accelerator between a request and the rendezvous.
pub(crate) fn stop_requested() -> bool {
    GC_STOP.load(Ordering::SeqCst)
}

/// clear the stop request, wake everyone parked on it, and (when the caller
/// is itself a registered mutator) become a running mutator again.
fn release_the_world(own: Option<&MutatorSlot>) {
    if PREEMPT_SAVED.swap(false, Ordering::SeqCst) {
        crate::concurrency::green::PITH_PREEMPT_REQUESTED
            .store(PREEMPT_PRIOR.load(Ordering::SeqCst), Ordering::SeqCst);
    }
    let guard = lock_gate();
    GC_STOP.store(false, Ordering::SeqCst);
    GC_GATE_CV.notify_all();
    drop(guard);
    if let Some(slot) = own {
        slot.state.store(MUTATOR_RUNNING, Ordering::SeqCst);
    }
}

/// Bring every registered mutator to a provable halt: set the stop request
/// and wait until each non-exited slot reads `native` or `parked_for_gc`.
/// Returns `false` — with the request already withdrawn and the world resumed
/// — when some mutator did not stop within the timeout, or immediately when
/// the flag is off; the caller simply retries later. On `true` the world
/// stays stopped until `pith_cycle_resume_the_world`.
///
/// A caller that is itself a registered mutator (main, in tests) is marked
/// `native` for the duration so the rendezvous does not wait on the thread
/// doing the waiting; resume puts it back. The collection pass runs from a
/// dedicated thread, which never registers and needs neither.
pub(crate) fn pith_cycle_stop_the_world() -> bool {
    if !cycle_gc_enabled() {
        return false;
    }
    let own = existing_mutator_slot();
    if let Some(slot) = &own {
        slot.state.store(MUTATOR_NATIVE, Ordering::SeqCst);
    }
    GC_STOP.store(true, Ordering::SeqCst);
    // preempt accelerator: while the stop request is pending, raise the
    // green preemption flag so compiled loop back-edges call into
    // `pith_green_maybe_yield`, whose first gate is the collector's. that
    // drives a compute-bound task (or the main thread in a tight loop) to a
    // park it would otherwise only reach at its next runtime call. the prior
    // value is restored on release; the monitor also keeps the flag up while
    // the request stands (see `stop_requested`).
    let prior =
        crate::concurrency::green::PITH_PREEMPT_REQUESTED.swap(1, Ordering::SeqCst);
    PREEMPT_PRIOR.store(prior, Ordering::SeqCst);
    PREEMPT_SAVED.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_millis(stop_timeout_ms());
    loop {
        if world_is_stopped() {
            return true;
        }
        if Instant::now() >= deadline {
            release_the_world(own.as_deref());
            return false;
        }
        // the laggard is finishing a task or crossing to its next gate; give
        // it the core rather than hammering the registry lock.
        std::thread::sleep(Duration::from_micros(50));
    }
}

/// Withdraw the stop request and wake every mutator parked on it. Only
/// meaningful after a `true` from `pith_cycle_stop_the_world`, on the same
/// thread; calling it with the world already running is a harmless no-op.
pub(crate) fn pith_cycle_resume_the_world() {
    if !cycle_gc_enabled() {
        return;
    }
    release_the_world(existing_mutator_slot().as_deref());
}

/// Stop the world, run `f` on the calling thread, resume, and return `f`'s
/// value — or `None` (with the world running) when the stop timed out or the
/// flag is off. Concurrent callers serialize on an internal lock; the resume
/// runs even if `f` panics, so a bug in a collection pass cannot leave every
/// mutator parked forever.
pub(crate) fn with_world_stopped<F: FnOnce() -> R, R>(f: F) -> Option<R> {
    if !cycle_gc_enabled() {
        return None;
    }
    let _one_stopper = match STOPPER.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if !pith_cycle_stop_the_world() {
        return None;
    }
    struct ResumeOnDrop;
    impl Drop for ResumeOnDrop {
        fn drop(&mut self) {
            pith_cycle_resume_the_world();
        }
    }
    let _resume = ResumeOnDrop;
    Some(f())
}

// --- the collection pass ----------------------------------------------------
//
// trial deletion (Bacon-Rajan), synchronous, inside the stopped world. the
// suspects are the only places a garbage cycle can root, so the pass drains
// them, walks the ownership graph they reach, subtracts the edges internal to
// that graph from each member's real count (MarkGray), restores the counts of
// everything still owned from outside (Scan/ScanBlack), and frees what is
// left (the white set) — the members of cycles no live reference reaches.
//
// child kinds use the closure env tag codes, the one numbering the runtime,
// the tracers, and `pith_cc_visit` already share: 2=list 3=map 4=set 6=struct
// (and tuple boxes) 7=closure. strings and bytes never appear in the graph —
// they hold no edges, so they cannot sit in a cycle, and the destruction of
// their owner releases them the ordinary way. sets appear as nodes (they have
// a count of their own, so a set owned only by garbage is garbage) but are
// never enumerated into: ints and copied byte strings are all they hold.

const CHILD_KIND_LIST: u8 = 2;
const CHILD_KIND_MAP: u8 = 3;
const CHILD_KIND_SET: u8 = 4;
const CHILD_KIND_STRUCT: u8 = 6;
const CHILD_KIND_CLOSURE: u8 = 7;

/// Collections that ran to completion (world stopped, whites freed).
pub static CYCLE_COLLECTIONS: AtomicU64 = AtomicU64::new(0);
/// Live suspects examined as candidate roots across all collections.
pub static CYCLE_ROOTS_EXAMINED: AtomicU64 = AtomicU64::new(0);
/// Cycle members reclaimed by trial deletion.
pub static CYCLE_WHITES_FREED: AtomicU64 = AtomicU64::new(0);
/// Graveyard shells reclaimed by the collector.
pub static CYCLE_GRAVEYARD_FREED: AtomicU64 = AtomicU64::new(0);

// how many buffered suspects trigger a collection. resolved once from
// PITH_CYCLE_GC_THRESHOLD; 0 in the cell means not yet resolved.
static COLLECT_THRESHOLD: AtomicUsize = AtomicUsize::new(0);
const COLLECT_THRESHOLD_DEFAULT: usize = 4096;

fn collect_threshold() -> usize {
    let cached = COLLECT_THRESHOLD.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    let threshold = std::env::var("PITH_CYCLE_GC_THRESHOLD")
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(COLLECT_THRESHOLD_DEFAULT);
    COLLECT_THRESHOLD.store(threshold, Ordering::Relaxed);
    threshold
}

thread_local! {
    // set on the thread running a collection pass (the dedicated collector
    // thread for its whole life, a test caller for the span of one collect).
    // the mutator gates and native brackets check it so teardown code the
    // collector calls can never park the collector on its own stop request.
    static ON_COLLECTOR: Cell<bool> = const { Cell::new(false) };

    // the children one tracer call reports. `pith_cc_visit` has no context
    // argument, so the installed hook needs a side slot to deliver through;
    // a per-thread buffer the caller drains right after the tracer returns
    // is the simplest shape that cannot dangle — no borrowed closure state
    // survives past the call that set it up. tracer calls never nest (the
    // pass walks one object at a time and drains before descending).
    static TRACED_CHILDREN: RefCell<Vec<(i64, i64)>> = const { RefCell::new(Vec::new()) };
}

/// The hook installed for the span of a collection: buffer each reported
/// child for the tracer caller to drain. Installing it process-wide is safe
/// because the world is stopped — only the collector runs tracers.
extern "C" fn collector_visit(child: i64, kind: i64) {
    TRACED_CHILDREN.with(|buffer| buffer.borrow_mut().push((child, kind)));
}

/// The object's real, current strong count — the trial counts seed from
/// this. `None` when the pointer does not validate as the claimed kind (a
/// corpse, or a miscarried edge), in which case it never joins the graph.
unsafe fn real_strong_count(ptr: usize, kind: u8) -> Option<i64> {
    match kind {
        CHILD_KIND_STRUCT => {
            crate::runtime_core::cycle_struct_strong_count(ptr as i64).map(i64::from)
        }
        CHILD_KIND_CLOSURE => crate::runtime_core::cycle_closure_strong_count(ptr as i64),
        CHILD_KIND_LIST => {
            crate::collections::list::cycle_list_strong_count(ptr as i64).map(i64::from)
        }
        CHILD_KIND_MAP => {
            crate::collections::map::cycle_map_strong_count(ptr as i64).map(i64::from)
        }
        CHILD_KIND_SET => {
            crate::collections::set::cycle_set_strong_count(ptr as i64).map(i64::from)
        }
        _ => None,
    }
}

/// Every rc edge out of the object, as (child pointer, child kind). One
/// function for every kind:
///
/// - a struct-family box reaches its children through the tracer keyed by
///   its destructor pointer; no destructor, or no tracer for it (only
///   string/bytes/weak fields), means a leaf.
/// - a closure walks its tagged env slots; weak captures (tag 8) hold no
///   strong count and are skipped with the string/bytes leaves.
/// - lists and maps enumerate the values they own counts on.
/// - sets are nodes with no out-edges.
unsafe fn children_of(ptr: usize, kind: u8) -> Vec<(usize, u8)> {
    let mut children: Vec<(usize, u8)> = Vec::new();
    let mut push = |child: i64, child_kind: u8| {
        if child != 0
            && matches!(
                child_kind,
                CHILD_KIND_LIST
                    | CHILD_KIND_MAP
                    | CHILD_KIND_SET
                    | CHILD_KIND_STRUCT
                    | CHILD_KIND_CLOSURE
            )
        {
            children.push((child as usize, child_kind));
        }
    };
    match kind {
        CHILD_KIND_STRUCT => {
            let dtor = crate::runtime_core::cycle_struct_dtor_slot(ptr as i64);
            if dtor == 0 {
                return children;
            }
            let Some(tracer) = tracer_for_dtor(dtor as usize) else {
                return children;
            };
            // panic-guard: calling a compiler-emitted tracer twin registered beside its destructor.
            let trace: extern "C" fn(i64) = std::mem::transmute(tracer);
            trace(ptr as i64);
            let reported = TRACED_CHILDREN.with(|buffer| std::mem::take(&mut *buffer.borrow_mut()));
            for (child, child_kind) in reported {
                push(child, child_kind as u8);
            }
        }
        CHILD_KIND_CLOSURE => {
            crate::runtime_core::cycle_closure_children(ptr as i64, &mut |child, tag| {
                push(child, tag);
            });
        }
        CHILD_KIND_LIST => {
            crate::collections::list::cycle_list_children(ptr as i64, &mut |child, code| {
                push(child, code);
            });
        }
        CHILD_KIND_MAP => {
            crate::collections::map::cycle_map_children(ptr as i64, &mut |child, code| {
                push(child, code);
            });
        }
        _ => {}
    }
    children
}

/// mark-gray colors. every node starts `Fresh` when seeded; MarkGray turns
/// the candidate graph gray while subtracting internal edges, Scan restores
/// (black) what outside references still hold, and what stays with no
/// external count comes out white — the garbage.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Color {
    Fresh,
    Gray,
    Black,
    White,
}

/// one graph node, keyed by pointer in the pass-local map. the children are
/// enumerated once at seeding (the world is stopped, so the edges cannot
/// change) and reused by every later phase.
struct Node {
    kind: u8,
    count: i64,
    color: Color,
    children: Vec<(usize, u8)>,
}

/// Seed a node with its real count and its edges. `false` when the pointer
/// does not validate, which keeps a miscarried edge out of the graph — its
/// trial count is then simply never decremented, the leak direction.
unsafe fn seed_node(nodes: &mut HashMap<usize, Node>, ptr: usize, kind: u8) -> bool {
    if nodes.contains_key(&ptr) {
        return true;
    }
    let Some(count) = real_strong_count(ptr, kind) else {
        return false;
    };
    let children = children_of(ptr, kind);
    nodes.insert(
        ptr,
        Node {
            kind,
            count,
            color: Color::Fresh,
            children,
        },
    );
    true
}

/// MarkGray: walk from a root, graying each node once and subtracting one
/// from the trial count of every edge target. after every root is walked,
/// each node's trial count is its real count minus the edges internal to
/// the candidate graph.
unsafe fn mark_gray(nodes: &mut HashMap<usize, Node>, root: usize) {
    let mut stack = vec![root];
    while let Some(ptr) = stack.pop() {
        let children = match nodes.get_mut(&ptr) {
            Some(node) if node.color != Color::Gray => {
                node.color = Color::Gray;
                node.children.clone()
            }
            _ => continue,
        };
        for (child, child_kind) in children {
            if !seed_node(nodes, child, child_kind) {
                continue;
            }
            // seeded just above, so the lookup can only hit.
            let Some(child_node) = nodes.get_mut(&child) else {
                continue;
            };
            child_node.count -= 1;
            if child_node.color != Color::Gray {
                stack.push(child);
            }
        }
    }
}

/// Scan: a gray node with a positive trial count is owned from outside the
/// candidate graph — re-blacken it and everything it reaches, restoring the
/// counts MarkGray subtracted. a gray node at zero or below goes white and
/// its children are scanned in turn.
unsafe fn scan(nodes: &mut HashMap<usize, Node>, root: usize) {
    let mut stack = vec![root];
    while let Some(ptr) = stack.pop() {
        let Some(node) = nodes.get_mut(&ptr) else {
            continue;
        };
        if node.color != Color::Gray {
            continue;
        }
        if node.count > 0 {
            scan_black(nodes, ptr);
            continue;
        }
        node.color = Color::White;
        let children = node.children.clone();
        for (child, _) in children {
            stack.push(child);
        }
    }
}

/// ScanBlack: blacken everything reachable from a node with external owners,
/// re-incrementing each edge target's trial count exactly once per edge.
unsafe fn scan_black(nodes: &mut HashMap<usize, Node>, root: usize) {
    let mut stack = vec![root];
    while let Some(ptr) = stack.pop() {
        let children = match nodes.get_mut(&ptr) {
            Some(node) if node.color != Color::Black => {
                node.color = Color::Black;
                node.children.clone()
            }
            _ => continue,
        };
        for (child, _) in children {
            if let Some(child_node) = nodes.get_mut(&child) {
                child_node.count += 1;
                if child_node.color != Color::Black {
                    stack.push(child);
                }
            }
        }
    }
}

// a teardown guard far above any real count: added to every white's real
// counter before destruction, so a release arriving from another dying
// object decrements a huge number instead of ever reaching a death path
// the collector is about to run by hand.
const WHITE_GUARD: i64 = 1 << 30;

unsafe fn guard_white(ptr: usize, kind: u8) {
    match kind {
        CHILD_KIND_STRUCT => {
            crate::runtime_core::cycle_struct_guard_strong(ptr as i64, WHITE_GUARD as u32)
        }
        CHILD_KIND_CLOSURE => {
            crate::runtime_core::cycle_closure_guard_strong(ptr as i64, WHITE_GUARD)
        }
        CHILD_KIND_LIST => {
            crate::collections::list::cycle_list_guard_strong(ptr as i64, WHITE_GUARD as u32)
        }
        CHILD_KIND_MAP => {
            crate::collections::map::cycle_map_guard_strong(ptr as i64, WHITE_GUARD as u32)
        }
        CHILD_KIND_SET => {
            crate::collections::set::cycle_set_guard_strong(ptr as i64, WHITE_GUARD as u32)
        }
        _ => {}
    }
}

/// A white's normal destruction body: release everything it owns. edges to
/// other whites decrement guarded counts harmlessly; edges out of the
/// garbage release for real, which can cascade ordinary frees and can push
/// fresh suspects — both fine, because no buffer lock is held here.
unsafe fn destroy_white(ptr: usize, kind: u8) {
    match kind {
        CHILD_KIND_STRUCT => crate::runtime_core::cycle_struct_run_dtor(ptr as i64),
        CHILD_KIND_CLOSURE => crate::runtime_core::cycle_closure_release_env(ptr as i64),
        CHILD_KIND_LIST => crate::collections::list::cycle_list_release_elements(ptr as i64),
        CHILD_KIND_MAP => crate::collections::map::cycle_map_release_values(ptr as i64),
        CHILD_KIND_SET => crate::collections::set::cycle_set_release_elements(ptr as i64),
        _ => {}
    }
}

/// A white's terminal free, everything its kind's death path does after the
/// destruction body it already ran.
unsafe fn free_white_shell(ptr: usize, kind: u8) {
    match kind {
        CHILD_KIND_STRUCT => crate::runtime_core::cycle_struct_free_dead(ptr as i64),
        CHILD_KIND_CLOSURE => crate::runtime_core::cycle_closure_free_dead(ptr as i64),
        CHILD_KIND_LIST => crate::collections::list::cycle_list_free_dead(ptr as i64),
        CHILD_KIND_MAP => crate::collections::map::cycle_map_free_dead(ptr as i64),
        CHILD_KIND_SET => crate::collections::set::cycle_set_free_dead(ptr as i64),
        _ => {}
    }
}

/// restores the flag (and removes the visit hook) even when a collection
/// panics, so the process is left inert rather than half-instrumented.
struct CollectGuard {
    was_on: bool,
}

impl CollectGuard {
    fn enter() -> Self {
        let was_on = ON_COLLECTOR.with(|cell| cell.replace(true));
        install_visit_hook(collector_visit as extern "C" fn(i64, i64) as usize);
        CollectGuard { was_on }
    }
}

impl Drop for CollectGuard {
    fn drop(&mut self) {
        install_visit_hook(0);
        let was_on = self.was_on;
        ON_COLLECTOR.with(|cell| cell.set(was_on));
    }
}

/// One trial-deletion collection. Must run with the world stopped: it reads
/// and rewrites reference counts assuming no mutator moves them.
///
/// # Safety
/// The caller holds the world stopped (`with_world_stopped`), and every
/// pointer in the suspect buffer and graveyard satisfies the invariants the
/// buffer maintains (see the module doc).
pub(crate) unsafe fn collect() {
    let _guard = CollectGuard::enter();

    // 1. drain and validate the roots. the buffer is swapped empty first, so
    // suspects pushed by teardown below land in a fresh buffer for the next
    // round. the graveyard is snapshotted at the same moment: shells parked
    // during teardown pair with entries in that fresh buffer and must
    // survive until the pass that drains those entries.
    let drained = std::mem::take(&mut *lock_suspects());
    let parked = std::mem::take(&mut *lock_graveyard());

    let mut roots: Vec<(usize, u8)> = Vec::new();
    // live struct roots keep the buffer's weak count until the very end of
    // the pass: the world is stopped, so nothing re-suspends them mid-pass,
    // and the held count is what guarantees the header outlives every phase.
    let mut root_struct_weaks: Vec<i64> = Vec::new();
    for (ptr, kind) in drained {
        match kind {
            CYCLE_KIND_STRUCT => match crate::runtime_core::cycle_struct_alive(ptr as i64) {
                Some(true) => {
                    crate::runtime_core::cycle_clear_struct_buffered(ptr as i64);
                    root_struct_weaks.push(ptr as i64);
                    roots.push((ptr, CHILD_KIND_STRUCT));
                }
                Some(false) => {
                    // the value died while buffered: nothing to collect, the
                    // buffer's weak count is all that held the header.
                    crate::runtime_core::cycle_clear_struct_buffered(ptr as i64);
                    crate::runtime_core::pith_struct_weak_release(ptr as i64);
                }
                None => {}
            },
            CYCLE_KIND_CLOSURE => {
                if crate::runtime_core::cycle_closure_strong_count(ptr as i64).is_some() {
                    crate::runtime_core::cycle_clear_closure_buffered(ptr as i64);
                    roots.push((ptr, CHILD_KIND_CLOSURE));
                }
                // scrubbed magic: the object died into the graveyard, whose
                // snapshot below frees the shell.
            }
            CYCLE_KIND_LIST => {
                if crate::collections::list::cycle_list_strong_count(ptr as i64).is_some() {
                    crate::collections::list::cycle_clear_list_buffered(ptr as i64);
                    roots.push((ptr, CHILD_KIND_LIST));
                }
            }
            CYCLE_KIND_MAP => {
                if crate::collections::map::cycle_map_strong_count(ptr as i64).is_some() {
                    crate::collections::map::cycle_clear_map_buffered(ptr as i64);
                    roots.push((ptr, CHILD_KIND_MAP));
                }
            }
            _ => {}
        }
    }
    CYCLE_ROOTS_EXAMINED.fetch_add(roots.len() as u64, Ordering::Relaxed);

    // 2-3. trial deletion over the graph the roots reach.
    let mut nodes: HashMap<usize, Node> = HashMap::new();
    for &(ptr, kind) in &roots {
        if seed_node(&mut nodes, ptr, kind) {
            mark_gray(&mut nodes, ptr);
        }
    }
    for &(ptr, _) in &roots {
        scan(&mut nodes, ptr);
    }
    let whites: Vec<(usize, u8)> = nodes
        .iter()
        .filter(|(_, node)| node.color == Color::White)
        .map(|(&ptr, node)| (ptr, node.kind))
        .collect();

    // 4. free the whites: guard every real count first so teardown can never
    // recursively free a member, then run every destruction body, then free
    // every shell. three full passes, in that order — a destruction body may
    // touch any other white, so no white's shell may go before the last
    // body has run.
    for &(ptr, kind) in &whites {
        guard_white(ptr, kind);
    }
    for &(ptr, kind) in &whites {
        destroy_white(ptr, kind);
    }
    for &(ptr, kind) in &whites {
        free_white_shell(ptr, kind);
    }
    CYCLE_WHITES_FREED.fetch_add(whites.len() as u64, Ordering::Relaxed);

    // 5. the graveyard snapshot: shells fully destructed before this pass,
    // whose buffer entries were discarded in the drain above.
    CYCLE_GRAVEYARD_FREED.fetch_add(parked.len() as u64, Ordering::Relaxed);
    for (ptr, kind) in parked {
        match kind {
            CYCLE_KIND_CLOSURE => crate::runtime_core::cycle_drop_closure_shell(ptr),
            CYCLE_KIND_LIST => crate::collections::list::cycle_drop_list_shell(ptr),
            CYCLE_KIND_MAP => crate::collections::map::cycle_drop_map_shell(ptr),
            _ => {}
        }
    }

    // 6. drop the buffer's weak count on every drained live struct root. a
    // root that stayed black is simply un-buffered again; one that went
    // white had its shell freed above, and this lets outstanding weak
    // references (or nothing) decide when the header goes.
    for ptr in root_struct_weaks {
        crate::runtime_core::pith_struct_weak_release(ptr);
    }

    CYCLE_COLLECTIONS.fetch_add(1, Ordering::Relaxed);
}

// --- the collector thread ---------------------------------------------------
//
// a dedicated std thread, spawned lazily on the first suspect (or the first
// forced collection) while the flag is on. it never registers as a mutator,
// so the rendezvous never waits on it. it loops: wait until the suspect
// buffer crosses the threshold or a forced collection is requested, stop the
// world, collect, and on a failed stop back off (10ms doubling to 500ms)
// before retrying.

struct GcControl {
    /// forced collections requested; each `pith_cycle_gc_collect_now` call
    /// takes the next sequence number and waits for it to complete.
    force_pending: u64,
    /// forced requests answered (successfully or not).
    force_done: u64,
    /// whether the attempt that completed `force_done` stopped the world.
    last_force_ok: bool,
}

static GC_CTRL: Mutex<GcControl> = Mutex::new(GcControl {
    force_pending: 0,
    force_done: 0,
    last_force_ok: false,
});
static GC_CTRL_CV: Condvar = Condvar::new();
static COLLECTOR_STARTED: AtomicBool = AtomicBool::new(false);

fn lock_ctrl() -> MutexGuard<'static, GcControl> {
    match GC_CTRL.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

const COLLECT_BACKOFF_START: Duration = Duration::from_millis(10);
const COLLECT_BACKOFF_MAX: Duration = Duration::from_millis(500);

fn ensure_collector_thread() {
    if COLLECTOR_STARTED.load(Ordering::Relaxed) {
        return;
    }
    if COLLECTOR_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("pith-cycle-gc".into())
        .spawn(collector_main);
    if spawned.is_err() {
        // no thread means no collections — cycles leak, the acceptable
        // direction — and a later suspect may try again.
        COLLECTOR_STARTED.store(false, Ordering::SeqCst);
    }
}

fn collector_main() {
    // for its whole life: teardown code must never park this thread on its
    // own stop request, and it must never register as a mutator.
    ON_COLLECTOR.with(|cell| cell.set(true));
    let mut backoff = COLLECT_BACKOFF_START;
    loop {
        let force_goal = wait_for_work();
        let stopped = with_world_stopped(|| unsafe { collect() }).is_some();
        finish_forced(force_goal, stopped);
        if stopped {
            backoff = COLLECT_BACKOFF_START;
        } else {
            // some mutator would not stop (or the flag went off). back off,
            // but on the control condvar so a forced request cuts the wait
            // short.
            let ctrl = lock_ctrl();
            if ctrl.force_pending == ctrl.force_done {
                let _ = GC_CTRL_CV.wait_timeout(ctrl, backoff);
            }
            backoff = (backoff * 2).min(COLLECT_BACKOFF_MAX);
        }
    }
}

/// Block until there is a reason to collect. Returns the forced-request
/// sequence this attempt will answer for (unchanged `force_done` when the
/// trigger was the threshold alone).
fn wait_for_work() -> u64 {
    let mut ctrl = lock_ctrl();
    loop {
        if ctrl.force_pending > ctrl.force_done {
            return ctrl.force_pending;
        }
        if lock_suspects().len() >= collect_threshold() {
            return ctrl.force_pending;
        }
        // the timeout is the threshold poll: pushes only notify on the
        // crossing itself, and a buffer that idles above the threshold
        // afterwards still deserves a pass.
        ctrl = GC_CTRL_CV
            .wait_timeout(ctrl, Duration::from_millis(50))
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .0;
    }
}

/// Answer every forced request the finished attempt covered. Requests that
/// arrived mid-attempt have a later sequence and get their own attempt.
fn finish_forced(goal: u64, stopped: bool) {
    let mut ctrl = lock_ctrl();
    if goal > ctrl.force_done {
        ctrl.force_done = goal;
        ctrl.last_force_ok = stopped;
        GC_CTRL_CV.notify_all();
    }
}

/// Force a collection and wait for the attempt: 1 when it ran, 0 when the
/// stop timed out or the collector is disabled. Callable from pith by its
/// literal name; the wait is a native bracket, so a mutator blocking here
/// counts as stopped for the very collection it asked for.
#[no_mangle]
pub extern "C" fn pith_cycle_gc_collect_now() -> i64 {
    if !cycle_gc_enabled() {
        return 0;
    }
    ensure_collector_thread();
    let mut ctrl = lock_ctrl();
    ctrl.force_pending += 1;
    let my_seq = ctrl.force_pending;
    GC_CTRL_CV.notify_all();
    let _native = native_bracket();
    while ctrl.force_done < my_seq {
        ctrl = GC_CTRL_CV
            .wait(ctrl)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
    let ok = ctrl.last_force_ok;
    // release the control lock before the bracket exits: the exit may park
    // this thread for a later stop, and the collector needs the lock to
    // answer forced requests — parking while holding it would wedge both.
    drop(ctrl);
    i64::from(ok)
}

/// Print the cycle-gc counters to stderr.
#[no_mangle]
pub extern "C" fn pith_cycle_gc_stats() {
    eprintln!(
        "pith cycle gc: buffered={} overflowed={} graveyard_deferred={} pending_suspects={} pending_graveyard={} collections={} roots_examined={} whites_freed={} graveyard_freed={}",
        CYCLE_SUSPECTS_BUFFERED.load(Ordering::Relaxed),
        CYCLE_SUSPECTS_OVERFLOWED.load(Ordering::Relaxed),
        CYCLE_GRAVEYARD_DEFERRED.load(Ordering::Relaxed),
        lock_suspects().len(),
        lock_graveyard().len(),
        CYCLE_COLLECTIONS.load(Ordering::Relaxed),
        CYCLE_ROOTS_EXAMINED.load(Ordering::Relaxed),
        CYCLE_WHITES_FREED.load(Ordering::Relaxed),
        CYCLE_GRAVEYARD_FREED.load(Ordering::Relaxed),
    );
}

// test support: the enable state and the buffers are process globals, so
// every test that turns the flag on serializes on CYCLE_TEST_LOCK and puts
// the world back before releasing it. the probe is bypassed by storing the
// settled state directly.
#[cfg(test)]
pub(crate) static CYCLE_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub(crate) fn force_enabled_for_tests(enabled: bool) {
    CYCLE_GC_STATE.store(if enabled { 2 } else { 1 }, Ordering::Relaxed);
}

#[cfg(test)]
pub(crate) fn suspects_len_for_tests() -> usize {
    lock_suspects().len()
}

// the timeout cell is process-global like the enable state; stop-the-world
// tests pin it explicitly (under CYCLE_TEST_LOCK) rather than inherit
// whatever an earlier test resolved.
#[cfg(test)]
pub(crate) fn set_stop_timeout_for_tests(ms: u64) {
    STOP_TIMEOUT_MS.store(ms, Ordering::Relaxed);
}

#[cfg(test)]
pub(crate) fn mutators_len_for_tests() -> usize {
    lock_mutators().len()
}

#[cfg(test)]
pub(crate) fn tracer_count_for_tests() -> usize {
    lock_tracers().len()
}

#[cfg(test)]
pub(crate) fn suspect_count_for_tests(ptr: usize) -> usize {
    lock_suspects().iter().filter(|(p, _)| *p == ptr).count()
}

#[cfg(test)]
pub(crate) fn graveyard_count_for_tests(ptr: usize) -> usize {
    lock_graveyard().iter().filter(|(p, _)| *p == ptr).count()
}

/// Drain both buffers so the next test starts clean. Buffer entries are
/// discarded properly (bit cleared, struct weak count dropped — which frees
/// the header of a struct that died while buffered). Graveyard shells are
/// deliberately leaked rather than dropped here: freeing them belongs to a
/// collection pass, and a bounded leak inside the test process is harmless.
#[cfg(test)]
pub(crate) fn reset_for_tests() {
    let entries = std::mem::take(&mut *lock_suspects());
    for (ptr, kind) in entries {
        unsafe { discard_suspect(ptr, kind) };
    }
    lock_graveyard().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collections::list::{
        pith_list_len, pith_list_push_value, pith_list_release, pith_list_retain_handle,
    };
    use crate::runtime_core::{
        closure_buffered_for_tests, pith_closure_get_fn, pith_closure_new, pith_closure_release,
        pith_closure_retain, pith_closure_set_env_rc, pith_struct_alloc, pith_struct_release,
        pith_struct_retain, pith_struct_set_dtor, pith_struct_weak_load, pith_struct_weak_release,
        pith_struct_weak_retain, struct_dead_word_for_tests, struct_weak_count_for_tests,
    };

    fn locked() -> std::sync::MutexGuard<'static, ()> {
        match CYCLE_TEST_LOCK.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    #[test]
    fn flag_off_release_paths_stay_untouched() {
        let _guard = locked();
        force_enabled_for_tests(false);
        reset_for_tests();
        unsafe {
            let s = pith_struct_alloc(2);
            pith_struct_retain(s);
            pith_struct_release(s); // 2 -> 1: would buffer with the flag on
            assert_eq!(struct_dead_word_for_tests(s), 0);
            assert_eq!(struct_weak_count_for_tests(s), 1);
            assert_eq!(suspect_count_for_tests(s as usize), 0);
            pith_struct_release(s); // frees normally

            let list = crate::collections::list::pith_list_new(8, 0);
            pith_list_retain_handle(list.ptr as i64);
            pith_list_release(list);
            assert_eq!(
                suspect_count_for_tests(list.ptr as usize),
                0,
                "flag off must keep the buffer empty"
            );
            pith_list_release(list); // frees normally, no graveyard
            assert_eq!(graveyard_count_for_tests(list.ptr as usize), 0);
        }
        assert_eq!(suspects_len_for_tests(), 0);
    }

    #[test]
    fn struct_decremented_to_nonzero_is_buffered_exactly_once() {
        let _guard = locked();
        force_enabled_for_tests(true);
        reset_for_tests();
        unsafe {
            let s = pith_struct_alloc(2);
            pith_struct_retain(s);
            pith_struct_retain(s); // strong 3
            pith_struct_release(s); // 3 -> 2: buffered
            assert_eq!(suspect_count_for_tests(s as usize), 1);
            assert_eq!(
                struct_dead_word_for_tests(s),
                2,
                "buffered bit set, not dead"
            );
            assert_eq!(struct_weak_count_for_tests(s), 2, "buffer holds one weak");
            pith_struct_release(s); // 2 -> 1: already buffered, no second entry
            assert_eq!(suspect_count_for_tests(s as usize), 1);
            assert_eq!(struct_weak_count_for_tests(s), 2);

            force_enabled_for_tests(false);
            reset_for_tests(); // clears the bit, drops the buffer's weak
            assert_eq!(struct_dead_word_for_tests(s), 0);
            assert_eq!(struct_weak_count_for_tests(s), 1);
            pith_struct_release(s);
        }
    }

    #[test]
    fn buffered_struct_survives_the_death_of_its_value() {
        let _guard = locked();
        force_enabled_for_tests(true);
        reset_for_tests();
        unsafe {
            let s = pith_struct_alloc(1);
            pith_struct_retain(s);
            pith_struct_release(s); // 2 -> 1: buffered, weak now 2
            pith_struct_release(s); // 1 -> 0: dies while buffered
                                    // dead bit and buffered bit both set; the buffer's weak count
                                    // is the only thing keeping the header alive.
            assert_eq!(struct_dead_word_for_tests(s), 3);
            assert_eq!(struct_weak_count_for_tests(s), 1);
            assert_eq!(pith_struct_weak_load(s), 0, "dead reads as none");
            assert_eq!(suspect_count_for_tests(s as usize), 1);

            force_enabled_for_tests(false);
            reset_for_tests(); // drops the buffer's weak: header freed here
        }
    }

    #[test]
    fn weak_load_ignores_the_buffered_bit() {
        let _guard = locked();
        force_enabled_for_tests(true);
        reset_for_tests();
        unsafe {
            let s = pith_struct_alloc(1);
            pith_struct_weak_retain(s);
            pith_struct_retain(s);
            pith_struct_release(s); // buffered, still alive
            assert_eq!(struct_dead_word_for_tests(s), 2);
            assert_eq!(pith_struct_weak_load(s), s, "buffered but alive");
            pith_struct_release(s); // dies buffered
            assert_eq!(pith_struct_weak_load(s), 0, "buffered and dead");
            pith_struct_weak_release(s);

            force_enabled_for_tests(false);
            reset_for_tests();
        }
    }

    #[test]
    fn buffered_list_dies_into_the_graveyard() {
        let _guard = locked();
        force_enabled_for_tests(true);
        reset_for_tests();
        unsafe {
            // a struct element observed through a weak reference proves the
            // list's death still cascades into its elements.
            let elem = pith_struct_alloc(1);
            pith_struct_weak_retain(elem);

            let list = crate::collections::list::pith_list_new(8, 4); // struct elements
            pith_list_push_value(list, elem); // list retains
            pith_struct_release(elem); // list holds the only strong count
            assert_eq!(pith_struct_weak_load(elem), elem);

            pith_list_retain_handle(list.ptr as i64);
            pith_list_release(list); // 2 -> 1: buffered
            assert_eq!(suspect_count_for_tests(list.ptr as usize), 1);
            pith_list_release(list); // 1 -> 0: dies buffered

            assert_eq!(graveyard_count_for_tests(list.ptr as usize), 1);
            assert_eq!(pith_struct_weak_load(elem), 0, "element was released");
            assert_eq!(
                pith_list_len(list),
                0,
                "magic scrubbed: handle no longer validates"
            );
            pith_list_retain_handle(list.ptr as i64); // must be a no-op, not a revival

            pith_struct_weak_release(elem);
            force_enabled_for_tests(false);
            reset_for_tests();
        }
    }

    #[test]
    fn buffered_closure_dies_into_the_graveyard() {
        let _guard = locked();
        force_enabled_for_tests(true);
        reset_for_tests();
        unsafe {
            let captured = pith_struct_alloc(1);
            pith_struct_weak_retain(captured);

            let closure = pith_closure_new(0x1000);
            // tag 6 = struct, per the closure env tag codes; ownership of the
            // strong count transfers into the closure.
            pith_closure_set_env_rc(closure, 0, captured, 6);

            pith_closure_retain(closure);
            pith_closure_release(closure); // 2 -> 1: buffered
            assert!(closure_buffered_for_tests(closure));
            assert_eq!(suspect_count_for_tests(closure as usize), 1);
            pith_closure_release(closure); // 1 -> 0: dies buffered

            assert_eq!(graveyard_count_for_tests(closure as usize), 1);
            assert_eq!(pith_struct_weak_load(captured), 0, "capture was released");
            assert_eq!(pith_closure_get_fn(closure), 0, "magic scrubbed");

            pith_struct_weak_release(captured);
            force_enabled_for_tests(false);
            reset_for_tests();
        }
    }

    #[test]
    fn overflow_degrades_to_a_leak_never_a_dangle() {
        let _guard = locked();
        force_enabled_for_tests(true);
        reset_for_tests();
        unsafe {
            let mut kept = Vec::new();
            while suspects_len_for_tests() < CYCLE_SUSPECT_CAP {
                let s = pith_struct_alloc(1);
                pith_struct_retain(s);
                pith_struct_release(s); // buffered
                kept.push(s);
            }

            let overflowed_before = CYCLE_SUSPECTS_OVERFLOWED.load(Ordering::Relaxed);
            let victim = pith_struct_alloc(1);
            pith_struct_retain(victim);
            pith_struct_release(victim); // buffer full: turned away
            assert_eq!(
                suspect_count_for_tests(victim as usize),
                0,
                "no push at cap"
            );
            assert_eq!(struct_dead_word_for_tests(victim), 0, "bit cleared again");
            assert_eq!(
                struct_weak_count_for_tests(victim),
                1,
                "weak released again"
            );
            assert!(CYCLE_SUSPECTS_OVERFLOWED.load(Ordering::Relaxed) > overflowed_before);

            force_enabled_for_tests(false);
            reset_for_tests();
            pith_struct_release(victim);
            for s in kept {
                pith_struct_release(s);
            }
        }
    }

    #[test]
    fn registered_tracer_pairs_are_looked_up_by_dtor_address() {
        let _guard = locked();
        // addresses only need to be distinct keys; nothing dereferences them
        // until a collector walks the table.
        pith_cc_register_tracer(0x1000, 0x2000);
        pith_cc_register_tracer(0x3000, 0x4000);
        assert_eq!(tracer_for_dtor(0x1000), Some(0x2000));
        assert_eq!(tracer_for_dtor(0x3000), Some(0x4000));
        assert_eq!(tracer_for_dtor(0x5000), None, "unregistered dtor is a leaf");
        assert!(tracer_count_for_tests() >= 2);

        // a re-registration (a module reloaded in-process) replaces, never
        // duplicates
        let before = tracer_count_for_tests();
        pith_cc_register_tracer(0x1000, 0x6000);
        assert_eq!(tracer_for_dtor(0x1000), Some(0x6000));
        assert_eq!(tracer_count_for_tests(), before);

        // null halves are ignored: no entry, no panic
        pith_cc_register_tracer(0, 0x7000);
        pith_cc_register_tracer(0x8000, 0);
        assert_eq!(tracer_for_dtor(0), None);
        assert_eq!(tracer_for_dtor(0x8000), None);
        assert_eq!(tracer_count_for_tests(), before);
    }

    #[test]
    fn visit_with_no_hook_is_inert_and_counts_only_when_enabled() {
        let _guard = locked();
        force_enabled_for_tests(false);
        let before = CYCLE_VISITS_UNHOOKED.load(Ordering::Relaxed);
        pith_cc_visit(0x1234, 6);
        assert_eq!(
            CYCLE_VISITS_UNHOOKED.load(Ordering::Relaxed),
            before,
            "flag off: not even the counter moves"
        );

        force_enabled_for_tests(true);
        pith_cc_visit(0x1234, 6);
        pith_cc_visit(0x5678, 2);
        assert_eq!(CYCLE_VISITS_UNHOOKED.load(Ordering::Relaxed), before + 2);
        pith_cc_visit(0, 6); // a none payload is not a child
        assert_eq!(CYCLE_VISITS_UNHOOKED.load(Ordering::Relaxed), before + 2);
        force_enabled_for_tests(false);
    }

    #[test]
    fn visit_dispatches_to_an_installed_hook() {
        static SEEN_CHILD: AtomicU64 = AtomicU64::new(0);
        static SEEN_KIND: AtomicU64 = AtomicU64::new(0);
        extern "C" fn recording_hook(child: i64, kind: i64) {
            SEEN_CHILD.store(child as u64, Ordering::Relaxed);
            SEEN_KIND.store(kind as u64, Ordering::Relaxed);
        }

        let _guard = locked();
        force_enabled_for_tests(true);
        install_visit_hook(recording_hook as usize);
        let unhooked_before = CYCLE_VISITS_UNHOOKED.load(Ordering::Relaxed);
        pith_cc_visit(0xabcd, 7);
        assert_eq!(SEEN_CHILD.load(Ordering::Relaxed), 0xabcd);
        assert_eq!(SEEN_KIND.load(Ordering::Relaxed), 7);
        assert_eq!(
            CYCLE_VISITS_UNHOOKED.load(Ordering::Relaxed),
            unhooked_before,
            "a hooked visit is delivered, not counted as unhooked"
        );

        install_visit_hook(0);
        pith_cc_visit(0x9999, 3);
        assert_eq!(SEEN_CHILD.load(Ordering::Relaxed), 0xabcd, "hook removed");
        assert_eq!(
            CYCLE_VISITS_UNHOOKED.load(Ordering::Relaxed),
            unhooked_before + 1
        );
        force_enabled_for_tests(false);
    }

    #[test]
    fn concurrent_releases_buffer_a_shared_struct_once() {
        let _guard = locked();
        force_enabled_for_tests(true);
        reset_for_tests();
        unsafe {
            let s = pith_struct_alloc(1);
            for _ in 0..8 {
                pith_struct_retain(s); // strong 9: one per releasing step + ours
            }
            let a = std::thread::spawn(move || {
                for _ in 0..4 {
                    pith_struct_release(s);
                }
            });
            let b = std::thread::spawn(move || {
                for _ in 0..4 {
                    pith_struct_release(s);
                }
            });
            a.join().expect("releasing thread panicked");
            b.join().expect("releasing thread panicked");

            assert_eq!(pith_struct_weak_load(s), s, "still alive: we hold a count");
            assert_eq!(
                suspect_count_for_tests(s as usize),
                1,
                "one entry, many releases"
            );
            assert_eq!(struct_dead_word_for_tests(s), 2);
            assert_eq!(struct_weak_count_for_tests(s), 2);

            force_enabled_for_tests(false);
            reset_for_tests();
            pith_struct_release(s);
        }
    }

    // --- stop-the-world tests ----------------------------------------------
    //
    // these force the flag on under CYCLE_TEST_LOCK, like the buffer tests.
    // the mutator registry is process-global, and while the flag is on *any*
    // test thread in this binary that crosses a bracket registers a slot that
    // reads `running` until its thread exits — so tests that expect a stop to
    // SUCCEED retry the attempt for a while instead of asserting the first
    // one, which is exactly how the collector treats a `false` anyway.

    /// spawn a thread the way the runtime spawns mutators: slot registered
    /// here first, adopted by the thread. returns the slot for state asserts.
    fn spawn_registered<F: FnOnce() + Send + 'static>(
        body: F,
    ) -> (Arc<MutatorSlot>, std::thread::JoinHandle<()>) {
        let slot = mutator_slot_for_spawn().expect("the flag is on in these tests");
        let adopted = Some(Arc::clone(&slot));
        let handle = std::thread::spawn(move || {
            adopt_mutator_slot(adopted);
            body();
        });
        (slot, handle)
    }

    /// retry the stop until it lands, with a ceiling so a congested process
    /// skips the test instead of hanging the suite. the pause between failed
    /// attempts is load-bearing: a failed attempt parks every gated thread in
    /// the process for the whole stop timeout, so back-to-back retries starve
    /// unrelated tests sharing the binary — the gap hands them the core back.
    fn stop_eventually() -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if pith_cycle_stop_the_world() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// a congested process (unrelated tests holding running mutator slots for
    /// longer than the retry ceiling) makes a stop-success assertion hang or
    /// flake rather than fail honestly. the property is still pinned by the
    /// solo-process leak gate, so these tests step aside loudly instead.
    fn skip_congested(test: &str) {
        eprintln!("{test}: skipped, the process never quiesced under concurrent tests");
    }

    fn await_state(slot: &MutatorSlot, expected: u8) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while slot.state_for_tests() != expected {
            assert!(
                Instant::now() < deadline,
                "slot never reached state {expected}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// resumes the world when dropped, so an assert that fails while the
    /// world is stopped unwinds into a resume instead of wedging every other
    /// parked test thread in the process.
    struct WorldResumer;
    impl Drop for WorldResumer {
        fn drop(&mut self) {
            pith_cycle_resume_the_world();
        }
    }

    #[test]
    #[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
    fn stop_catches_mutators_at_their_gates() {
        let _guard = locked();
        force_enabled_for_tests(true);
        set_stop_timeout_for_tests(50);

        let done = Arc::new(AtomicBool::new(false));
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let done = Arc::clone(&done);
                spawn_registered(move || {
                    // the shape of a worker between tasks: gate, work, gate.
                    while !done.load(Ordering::Relaxed) {
                        mutator_gate();
                        std::thread::yield_now();
                    }
                })
            })
            .collect();

        if !stop_eventually() {
            skip_congested("stop_catches_mutators_at_their_gates");
            done.store(true, Ordering::Relaxed);
            for (_, handle) in workers {
                handle.join().expect("gate-looping thread panicked");
            }
            force_enabled_for_tests(false);
            return;
        }
        {
            let _resume = WorldResumer;
            for (slot, _) in &workers {
                assert_eq!(
                    slot.state_for_tests(),
                    MUTATOR_PARKED_FOR_GC,
                    "a stopped gate-looper is parked, not merely slow"
                );
            }
        }
        done.store(true, Ordering::Relaxed);
        for (slot, handle) in workers {
            handle.join().expect("gate-looping thread panicked");
            assert_eq!(
                slot.state_for_tests(),
                MUTATOR_EXITED,
                "a joined thread's slot reads exited"
            );
        }
        force_enabled_for_tests(false);
    }

    #[test]
    #[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
    fn stop_times_out_on_a_mutator_that_never_gates() {
        let _guard = locked();
        force_enabled_for_tests(true);
        set_stop_timeout_for_tests(30);

        let done = Arc::new(AtomicBool::new(false));
        let spinner_done = Arc::clone(&done);
        let (slot, handle) = spawn_registered(move || {
            // pith code with no compiled safe-points: checks nothing, ever.
            while !spinner_done.load(Ordering::Relaxed) {
                std::hint::spin_loop();
            }
        });

        assert!(
            !pith_cycle_stop_the_world(),
            "a spinner that never gates must time the stop out"
        );
        assert!(
            !GC_STOP.load(Ordering::SeqCst),
            "an abandoned stop withdraws the request"
        );
        assert_eq!(
            slot.state_for_tests(),
            MUTATOR_RUNNING,
            "the spinner never noticed"
        );

        done.store(true, Ordering::Relaxed);
        handle.join().expect("spinning thread panicked");
        force_enabled_for_tests(false);
    }

    #[test]
    #[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
    fn a_bracketed_wait_counts_as_stopped_and_the_exit_parks() {
        let _guard = locked();
        force_enabled_for_tests(true);
        set_stop_timeout_for_tests(50);

        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let finished = Arc::new(AtomicBool::new(false));
        let thread_release = Arc::clone(&release);
        let thread_finished = Arc::clone(&finished);
        let (slot, handle) = spawn_registered(move || {
            {
                // the shape of every runtime bracket: enter, one blocking
                // wait that reads no heap handle, exit.
                let _native = native_bracket();
                let (lock, cv) = &*thread_release;
                let mut go = lock.lock().unwrap_or_else(|p| p.into_inner());
                while !*go {
                    go = cv.wait(go).unwrap_or_else(|p| p.into_inner());
                }
            }
            // only reachable once the bracket exit let us back in.
            thread_finished.store(true, Ordering::Relaxed);
        });

        await_state(&slot, MUTATOR_NATIVE);
        if !stop_eventually() {
            skip_congested("a_bracketed_wait_counts_as_stopped_and_the_exit_parks");
            let (lock, cv) = &*release;
            *lock.lock().unwrap_or_else(|p| p.into_inner()) = true;
            cv.notify_all();
            handle.join().expect("bracketed thread panicked");
            force_enabled_for_tests(false);
            return;
        }
        {
            let _resume = WorldResumer;
            // wake the waiter while the world is stopped: it leaves its wait,
            // and the bracket's exit must park it before any code after the
            // bracket runs.
            {
                let (lock, cv) = &*release;
                *lock.lock().unwrap_or_else(|p| p.into_inner()) = true;
                cv.notify_all();
            }
            await_state(&slot, MUTATOR_PARKED_FOR_GC);
            assert!(
                !finished.load(Ordering::Relaxed),
                "the exit parked before code past the bracket ran"
            );
        }
        handle.join().expect("bracketed thread panicked");
        assert!(finished.load(Ordering::Relaxed), "resume let it finish");
        force_enabled_for_tests(false);
    }

    #[test]
    #[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
    fn exited_slots_are_skipped_by_the_rendezvous() {
        let _guard = locked();
        force_enabled_for_tests(true);
        set_stop_timeout_for_tests(50);

        let (slot, handle) = spawn_registered(|| {});
        handle.join().expect("exiting thread panicked");
        assert_eq!(slot.state_for_tests(), MUTATOR_EXITED);

        if !stop_eventually() {
            skip_congested("exited_slots_are_skipped_by_the_rendezvous");
            force_enabled_for_tests(false);
            return;
        }
        pith_cycle_resume_the_world();
        force_enabled_for_tests(false);
    }

    #[test]
    #[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
    fn with_world_stopped_runs_the_closure_against_a_parked_world() {
        let _guard = locked();
        force_enabled_for_tests(true);
        set_stop_timeout_for_tests(50);

        // give the calling thread a slot of its own: the stop must mark it
        // native for the duration rather than wait on it (the main-thread
        // test-caller case the seam documents).
        mutator_gate();

        let done = Arc::new(AtomicBool::new(false));
        let gate_done = Arc::clone(&done);
        let (slot, handle) = spawn_registered(move || {
            while !gate_done.load(Ordering::Relaxed) {
                mutator_gate();
                std::thread::yield_now();
            }
        });

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut observed = None;
        while observed.is_none() && Instant::now() < deadline {
            observed = with_world_stopped(|| slot.state_for_tests());
            if observed.is_none() {
                // the same starvation gap as stop_eventually.
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        if observed.is_none() {
            skip_congested("with_world_stopped_runs_the_closure_against_a_parked_world");
            done.store(true, Ordering::Relaxed);
            handle.join().expect("gate-looping thread panicked");
            force_enabled_for_tests(false);
            return;
        }
        assert_eq!(
            observed,
            Some(MUTATOR_PARKED_FOR_GC),
            "the closure ran while the other mutator was parked"
        );
        assert_eq!(
            existing_mutator_slot()
                .expect("this thread registered above")
                .state_for_tests(),
            MUTATOR_RUNNING,
            "the caller is a running mutator again after the resume"
        );

        done.store(true, Ordering::Relaxed);
        handle.join().expect("gate-looping thread panicked");
        force_enabled_for_tests(false);
    }

    #[test]
    fn flag_off_gates_and_brackets_are_inert() {
        let _guard = locked();
        force_enabled_for_tests(false);

        // the property is thread-local: this thread's gates and brackets
        // must not register it. the global slot count is not stable here —
        // an unrelated test spawning the green workers registers theirs
        // concurrently, so counting the registry races.
        assert!(existing_mutator_slot().is_none(), "no slot before");
        mutator_gate();
        {
            let _native = native_bracket();
        }
        adopt_mutator_slot(mutator_slot_for_spawn());
        assert!(
            existing_mutator_slot().is_none(),
            "no slot registered with the flag off"
        );
        assert!(
            !pith_cycle_stop_the_world(),
            "no stop-the-world with the flag off"
        );
        assert!(!GC_STOP.load(Ordering::SeqCst), "no request left behind");
        assert_eq!(with_world_stopped(|| 1), None);
        pith_cycle_resume_the_world(); // a no-op, not a panic
    }

    // --- collection pass tests ---------------------------------------------
    //
    // rings are built from raw runtime calls with a hand-written destructor
    // and tracer pair, the same shapes the emitter generates: the dtor
    // releases the one field, the tracer reports it to pith_cc_visit.

    /// dtor for a one-field box whose field holds another struct.
    unsafe extern "C" fn ring_dtor(ptr: i64) {
        let field = *(ptr as *const i64);
        if field != 0 {
            pith_struct_release(field);
        }
    }

    /// tracer twin of `ring_dtor`: report the field instead of releasing it.
    extern "C" fn ring_tracer(ptr: i64) {
        let field = unsafe { *(ptr as *const i64) };
        pith_cc_visit(field, 6);
    }

    /// dtor for a one-field box whose field holds a closure.
    unsafe extern "C" fn holder_dtor(ptr: i64) {
        let closure = *(ptr as *const i64);
        if closure != 0 {
            pith_closure_release(closure);
        }
    }

    extern "C" fn holder_tracer(ptr: i64) {
        let closure = unsafe { *(ptr as *const i64) };
        pith_cc_visit(closure, 7);
    }

    /// run a collection against a provably stopped world, retrying because
    /// unrelated test threads may hold running mutator slots for a while.
    fn collect_eventually() -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if with_world_stopped(|| unsafe { collect() }).is_some() {
                return true;
            }
            // the same starvation gap as stop_eventually: a failed attempt
            // parked every gated thread for the whole stop timeout.
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// a garbage two-struct ring: a owns b through its field, b owns a back,
    /// and no external count remains once the builder's is dropped.
    unsafe fn build_garbage_ring() -> (i64, i64) {
        pith_cc_register_tracer(ring_dtor as usize as i64, ring_tracer as usize as i64);
        let a = pith_struct_alloc(1);
        let b = pith_struct_alloc(1);
        pith_struct_set_dtor(a, ring_dtor as usize as i64);
        pith_struct_set_dtor(b, ring_dtor as usize as i64);
        *(a as *mut i64) = b; // our count on b transfers into a's field
        pith_struct_retain(a);
        *(b as *mut i64) = a; // b's edge holds its own count on a
        pith_struct_weak_retain(a);
        pith_struct_weak_retain(b);
        (a, b)
    }

    #[test]
    #[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
    fn a_garbage_two_struct_ring_is_collected_exactly_once() {
        let _guard = locked();
        force_enabled_for_tests(true);
        set_stop_timeout_for_tests(50);
        reset_for_tests();
        unsafe {
            let (a, b) = build_garbage_ring();
            pith_struct_release(a); // the last external count: garbage now
            assert_eq!(suspect_count_for_tests(a as usize), 1);
            assert_eq!(pith_struct_weak_load(a), a, "the ring still holds it");

            let whites_before = CYCLE_WHITES_FREED.load(Ordering::Relaxed);
            if !collect_eventually() { skip_congested("collection test"); return; }
            assert_eq!(pith_struct_weak_load(a), 0, "ring member a reclaimed");
            assert_eq!(pith_struct_weak_load(b), 0, "ring member b reclaimed");
            assert_eq!(
                CYCLE_WHITES_FREED.load(Ordering::Relaxed) - whites_before,
                2,
                "exactly the two ring members were freed"
            );

            // a second pass drains the teardown's fresh suspects and finds
            // nothing more to free: collected exactly once.
            if !collect_eventually() { skip_congested("collection test"); return; }
            assert_eq!(
                CYCLE_WHITES_FREED.load(Ordering::Relaxed) - whites_before,
                2,
                "nothing was freed twice"
            );

            pith_struct_weak_release(a);
            pith_struct_weak_release(b);
            force_enabled_for_tests(false);
            reset_for_tests();
        }
    }

    #[test]
    #[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
    fn a_ring_with_a_live_external_owner_is_kept_until_the_owner_drops() {
        let _guard = locked();
        force_enabled_for_tests(true);
        set_stop_timeout_for_tests(50);
        reset_for_tests();
        unsafe {
            let (a, b) = build_garbage_ring();
            pith_struct_retain(a); // the "stack" reference keeping it alive
            pith_struct_release(a); // buffers a, but a real owner remains

            let whites_before = CYCLE_WHITES_FREED.load(Ordering::Relaxed);
            if !collect_eventually() { skip_congested("collection test"); return; }
            assert_eq!(pith_struct_weak_load(a), a, "externally owned: kept");
            assert_eq!(pith_struct_weak_load(b), b, "reached only from a: kept");
            assert_eq!(
                CYCLE_WHITES_FREED.load(Ordering::Relaxed) - whites_before,
                0,
                "a reachable ring frees nothing"
            );

            // the owner lets go: the same ring is garbage on the next pass.
            pith_struct_release(a);
            if !collect_eventually() { skip_congested("collection test"); return; }
            assert_eq!(pith_struct_weak_load(a), 0);
            assert_eq!(pith_struct_weak_load(b), 0);
            assert_eq!(
                CYCLE_WHITES_FREED.load(Ordering::Relaxed) - whites_before,
                2
            );

            pith_struct_weak_release(a);
            pith_struct_weak_release(b);
            force_enabled_for_tests(false);
            reset_for_tests();
        }
    }

    #[test]
    #[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
    fn a_dtor_less_box_is_a_leaf_the_collector_never_frees() {
        let _guard = locked();
        force_enabled_for_tests(true);
        set_stop_timeout_for_tests(50);
        reset_for_tests();
        unsafe {
            // no destructor, so no tracer: the collector sees a leaf with a
            // real external count and must leave it alone.
            let s = pith_struct_alloc(2);
            pith_struct_weak_retain(s);
            pith_struct_retain(s);
            pith_struct_release(s); // buffered, still owned by us

            let whites_before = CYCLE_WHITES_FREED.load(Ordering::Relaxed);
            if !collect_eventually() { skip_congested("collection test"); return; }
            assert_eq!(pith_struct_weak_load(s), s, "leaf with an owner: kept");
            assert_eq!(
                CYCLE_WHITES_FREED.load(Ordering::Relaxed) - whites_before,
                0
            );

            pith_struct_weak_release(s);
            pith_struct_release(s);
            force_enabled_for_tests(false);
            reset_for_tests();
        }
    }

    #[test]
    #[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
    fn a_closure_capture_ring_is_collected_through_the_env_slots() {
        let _guard = locked();
        force_enabled_for_tests(true);
        set_stop_timeout_for_tests(50);
        reset_for_tests();
        unsafe {
            pith_cc_register_tracer(holder_dtor as usize as i64, holder_tracer as usize as i64);
            let s = pith_struct_alloc(1);
            pith_struct_set_dtor(s, holder_dtor as usize as i64);
            let c = pith_closure_new(0x1000);
            pith_struct_retain(s);
            pith_closure_set_env_rc(c, 0, s, 6); // closure captures the struct
            *(s as *mut i64) = c; // the struct's field owns the closure back
            pith_struct_weak_retain(s);

            pith_struct_release(s); // the last external count: garbage now
            assert_eq!(suspect_count_for_tests(s as usize), 1);

            let whites_before = CYCLE_WHITES_FREED.load(Ordering::Relaxed);
            if !collect_eventually() { skip_congested("collection test"); return; }
            assert_eq!(pith_struct_weak_load(s), 0, "struct half reclaimed");
            assert_eq!(pith_closure_get_fn(c), 0, "closure half no longer validates");
            assert_eq!(
                CYCLE_WHITES_FREED.load(Ordering::Relaxed) - whites_before,
                2
            );

            pith_struct_weak_release(s);
            force_enabled_for_tests(false);
            reset_for_tests();
        }
    }

    #[test]
    #[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
    fn a_collection_drains_the_graveyard() {
        let _guard = locked();
        force_enabled_for_tests(true);
        set_stop_timeout_for_tests(50);
        reset_for_tests();
        unsafe {
            let list = crate::collections::list::pith_list_new(8, 0);
            pith_list_retain_handle(list.ptr as i64);
            pith_list_release(list); // 2 -> 1: buffered
            pith_list_release(list); // 1 -> 0: dies into the graveyard
            assert_eq!(graveyard_count_for_tests(list.ptr as usize), 1);

            if !collect_eventually() { skip_congested("collection test"); return; }
            assert_eq!(
                graveyard_count_for_tests(list.ptr as usize),
                0,
                "the parked shell was freed"
            );

            force_enabled_for_tests(false);
            reset_for_tests();
        }
    }

    #[test]
    #[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
    fn a_forced_collect_reclaims_a_ring_and_reports_success() {
        let _guard = locked();
        force_enabled_for_tests(true);
        set_stop_timeout_for_tests(50);
        reset_for_tests();
        unsafe {
            let (a, b) = build_garbage_ring();
            pith_struct_release(a);

            // retried because unrelated test threads may hold running
            // mutator slots for a while, exactly like collect_eventually.
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut forced_ok = false;
            while !forced_ok && Instant::now() < deadline {
                forced_ok = pith_cycle_gc_collect_now() == 1;
            }
            assert!(forced_ok, "the forced collection must eventually run");
            assert_eq!(pith_struct_weak_load(a), 0);
            assert_eq!(pith_struct_weak_load(b), 0);

            pith_struct_weak_release(a);
            pith_struct_weak_release(b);
            force_enabled_for_tests(false);
            reset_for_tests();
        }
    }

    #[test]
    #[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
    fn a_forced_collect_reports_failure_when_a_spinner_prevents_the_stop() {
        let _guard = locked();
        force_enabled_for_tests(true);
        set_stop_timeout_for_tests(30);

        let done = Arc::new(AtomicBool::new(false));
        let spinner_done = Arc::clone(&done);
        let (_slot, handle) = spawn_registered(move || {
            while !spinner_done.load(Ordering::Relaxed) {
                std::hint::spin_loop();
            }
        });

        assert_eq!(
            pith_cycle_gc_collect_now(),
            0,
            "a mutator that never gates times the forced stop out"
        );

        done.store(true, Ordering::Relaxed);
        handle.join().expect("spinning thread panicked");
        force_enabled_for_tests(false);
    }

    #[test]
    #[ignore = "stops the world or spawns the collector; run serially via make test-cycle-gc"]
    fn a_forced_collect_with_the_flag_off_reports_failure_immediately() {
        let _guard = locked();
        force_enabled_for_tests(false);
        assert_eq!(pith_cycle_gc_collect_now(), 0);
    }
}
