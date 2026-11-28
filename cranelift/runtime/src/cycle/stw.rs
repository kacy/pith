//! Stop-the-world rendezvous: mutator slots, native brackets, and safepoints.

use super::cycle_gc_enabled;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

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
pub(super) const MUTATOR_RUNNING: u8 = 0;
/// the mutator is inside a native bracket: blocked (or about to block) in a
/// wait that reads no heap handle until the bracket exits.
pub(super) const MUTATOR_NATIVE: u8 = 1;
/// the mutator saw the stop request at a gate or a bracket exit and is
/// waiting it out on the gc condvar.
pub(super) const MUTATOR_PARKED_FOR_GC: u8 = 2;
/// the mutator's thread has exited. slots are never removed from the
/// registry, only marked, so the rendezvous skips these.
pub(super) const MUTATOR_EXITED: u8 = 3;

/// One registered mutator thread's state word. Created once per thread and
/// kept in the registry for the life of the process; the owning thread holds
/// it through a thread-local and flips it with plain seq-cst stores.
///
/// seq-cst is what makes the rendezvous scan trustworthy: a mutator that
/// loaded `GC_STOP` as clear and re-entered `running` did so before the
/// stopper's store in the total order, so the stopper's later scan cannot
/// still read the slot's older `native`; it never observes "stopped" for a
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

// the mutator slot registry, in registration order. slots are pushed under
// the mutex and never popped (an exited thread's slot is marked and later
// RESURRECTED by the next registration), so the vec plateaus at the peak
// number of concurrently live mutators and the stopper's scan holds the
// lock only long enough to read the state words.
static MUTATORS: Mutex<Vec<Arc<MutatorSlot>>> = Mutex::new(Vec::new());

/// the stop request. set by the stopper, polled by mutators at gates and
/// bracket exits, cleared (under `GC_GATE`) by resume or an abandoned stop.
pub(super) static GC_STOP: AtomicBool = AtomicBool::new(false);

// the condvar a stopped mutator parks on until the request clears. the mutex
// guards only the wait handshake: resume clears GC_STOP and notifies while
// holding it, and a parker re-checks GC_STOP under it, so the clear can never
// slip between a parker's check and its wait.
pub(super) static GC_GATE: Mutex<()> = Mutex::new(());
pub(super) static GC_GATE_CV: Condvar = Condvar::new();

// stoppers must not interleave: two concurrent stop/resume pairs would clear
// each other's request mid-collection. `with_world_stopped` serializes on
// this; the raw pair's contract is a single caller (the collector thread).
static STOPPER: Mutex<()> = Mutex::new(());

/// how long a stop request waits before giving up, in milliseconds. resolved
/// once from `PITH_CYCLE_GC_STOP_MS`; 0 in the cell means not yet resolved
/// (0 is not a valid timeout: it would abandon every stop unconditionally).
pub(super) static STOP_TIMEOUT_MS: AtomicU64 = AtomicU64::new(0);
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

pub(super) fn lock_mutators() -> MutexGuard<'static, Vec<Arc<MutatorSlot>>> {
    crate::sync::lock(&MUTATORS)
}

pub(super) fn lock_gate() -> MutexGuard<'static, ()> {
    crate::sync::lock(&GC_GATE)
}

/// the calling thread's handle on its own slot. dropping it, which the
/// thread-local machinery does as the thread exits, marks the slot exited,
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
    let mut mutators = lock_mutators();
    // reuse an exited thread's slot instead of growing the registry. on the
    // os-thread backend every task is a fresh thread, so a push-only vec
    // grows for the life of the process; with reuse it plateaus at the peak
    // number of concurrently live mutators. the resurrection CAS runs under
    // the registry lock, and a resurrected slot has exactly a fresh slot's
    // contract: an EXITED state means the owning thread's teardown already
    // ran (it can never gate again), and the adopting thread's first gate
    // poll observes any stop already in progress before it mutates.
    for slot in mutators.iter() {
        if slot
            .state
            .compare_exchange(
                MUTATOR_EXITED,
                MUTATOR_RUNNING,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
        {
            return Arc::clone(slot);
        }
    }
    let slot = Arc::new(MutatorSlot {
        state: AtomicU8::new(MUTATOR_RUNNING),
    });
    mutators.push(Arc::clone(&slot));
    slot
}

/// this thread's slot, created and registered on first use. the main thread
/// registers through here at process start (`register_main_mutator`), and any
/// other thread that reaches a gate or bracket does on its first. `None` only while
/// thread-local storage is tearing down, where the thread is exiting and a
/// bracket can no longer matter.
///
/// this and the two readers below it are never inlined: the cell is a
/// `RefCell`, so touching it means holding its address as a value, and a
/// green task reaches the gate and the brackets from frames that can park. a
/// task resumed on another worker would then act on the first worker's slot.
/// behind a call the address is computed and consumed on the thread that is
/// running. the rule and the mechanism are in the accessor block of
/// `concurrency/green.rs`.
#[inline(never)]
pub(super) fn mutator_slot() -> Option<Arc<MutatorSlot>> {
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
#[inline(never)]
pub(super) fn existing_mutator_slot() -> Option<Arc<MutatorSlot>> {
    MY_MUTATOR
        .try_with(|cell| cell.borrow().as_ref().map(|owner| Arc::clone(&owner.0)))
        .ok()
        .flatten()
}

/// is this the thread running a collection pass? a plain cell, so the read
/// is one `%fs:`-relative load wherever it is inlined.
#[inline]
pub(super) fn on_collector() -> bool {
    ON_COLLECTOR.with(Cell::get)
}

/// Register the process main thread as a mutator, from the switches
/// constructor before main runs. Registering at its first gate or bracket
/// instead would let a collection that starts earlier (the collector thread
/// starts on the first suspect) declare the world stopped while main mutates.
pub(crate) fn register_main_mutator() {
    if cycle_gc_enabled() {
        let _ = mutator_slot();
    }
}

/// Create and register a mutator slot on behalf of a thread about to be
/// spawned. The parent registers it *before* the thread exists: were the
/// thread to register itself at entry, the rendezvous could scan (and
/// declare the world stopped) in the window between the spawn returning and
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
#[inline(never)]
pub(crate) fn adopt_mutator_slot(slot: Option<Arc<MutatorSlot>>) {
    let Some(slot) = slot else {
        return;
    };
    let _ = MY_MUTATOR.try_with(|cell| {
        *cell.borrow_mut() = Some(SlotOwner(slot));
    });
}

/// Mark a pre-registered spawn slot as exited when thread creation fails
/// before the child ever starts to adopt it. Without this, the slot would
/// stay `MUTATOR_RUNNING` forever and block every future stop-the-world pass.
pub(crate) fn abandon_mutator_slot(slot: Option<Arc<MutatorSlot>>) {
    if let Some(slot) = slot {
        slot.state.store(MUTATOR_EXITED, Ordering::SeqCst);
    }
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
    if on_collector() {
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
/// `native`, which the rendezvous counts as stopped, and the drop re-checks
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
pub(super) fn enter_native() {
    if on_collector() {
        return;
    }
    if let Some(slot) = mutator_slot() {
        slot.state.store(MUTATOR_NATIVE, Ordering::SeqCst);
    }
}

#[inline(never)]
pub(super) fn exit_native() {
    if on_collector() {
        return;
    }
    let Some(slot) = mutator_slot() else {
        return;
    };
    // running first, then the request: the stopper stores the request and
    // then scans, so with both sides seq-cst either this load sees the
    // request or the scan sees `running`. checking first would leave a
    // window where the scan still reads `native` while this thread has
    // already decided to run on.
    slot.state.store(MUTATOR_RUNNING, Ordering::SeqCst);
    #[cfg(test)]
    super::tests::between_exit_steps();
    if GC_STOP.load(Ordering::SeqCst) {
        park_for_gc(&slot);
    }
}

/// wait out a stop request on the gc condvar, then come back as `running`.
/// spurious wakes just re-check `GC_STOP` under the gate lock and wait again,
/// and after publishing `MUTATOR_RUNNING` we re-check `GC_STOP` (the same
/// Dekker handshake as `exit_native`) so a back-to-back stop that observed
/// `MUTATOR_PARKED_FOR_GC` right before our store cannot run concurrently.
pub(super) fn park_for_gc(slot: &MutatorSlot) {
    loop {
        slot.state.store(MUTATOR_PARKED_FOR_GC, Ordering::SeqCst);
        let mut guard = lock_gate();
        while GC_STOP.load(Ordering::SeqCst) {
            guard = GC_GATE_CV
                .wait(guard)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        drop(guard);
        slot.state.store(MUTATOR_RUNNING, Ordering::SeqCst);
        if !GC_STOP.load(Ordering::SeqCst) {
            break;
        }
    }
}

/// is every registered mutator provably not running? `native`, `parked`, and
/// `exited` all count as stopped; only `running` keeps the stopper waiting.
pub(super) fn world_is_stopped() -> bool {
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
/// Returns `false`, with the request already withdrawn and the world
/// resumed, when some mutator did not stop within the timeout, or immediately when
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
    // the preempt flag only reaches a RUNNING task, at its next back-edge. an
    // idle worker is asleep in `park` and reaches its mutator gate when that
    // wait ends, so wake the pool too: a parked worker answers the request
    // immediately instead of after its park timeout. that timeout backs off
    // while a worker stays idle, so without this the stop deadline (25 ms by
    // default) could expire on a pool that was ready to stop instantly.
    crate::concurrency::green::wake_pool();
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
/// value, or `None` (with the world running) when the stop timed out or the
/// flag is off. Concurrent callers serialize on an internal lock; the resume
/// runs even if `f` panics, so a bug in a collection pass cannot leave every
/// mutator parked forever.
pub(crate) fn with_world_stopped<F: FnOnce() -> R, R>(f: F) -> Option<R> {
    if !cycle_gc_enabled() {
        return None;
    }
    let _one_stopper = crate::sync::lock(&STOPPER);
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
thread_local! {
    // set on the thread running a collection pass (the dedicated collector
    // thread for its whole life, a test caller for the span of one collect).
    // the mutator gates and native brackets check it so teardown code the
    // collector calls can never park the collector on its own stop request.
    static ON_COLLECTOR: Cell<bool> = const { Cell::new(false) };
}

#[inline(never)]
pub(super) fn set_on_collector(v: bool) -> bool {
    ON_COLLECTOR.with(|cell| cell.replace(v))
}

