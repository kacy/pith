//! Destructor-to-tracer side table, user-drop registry, and tracer visit hooks.

use super::cycle_gc_enabled;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};

// --- destructor -> tracer side table -------------------------------------
//
// the emitter pairs every destructor that walks rc children with a tracer
// twin: the same field walk, but each child is reported to `pith_cc_visit`
// instead of released. keying the table by destructor address costs nothing
// per instance: the destructor pointer every rc box already carries doubles
// as the trace key, and a box with no destructor (or a destructor with no
// tracer: only string/bytes/weak fields) falls out as a leaf the collector
// never walks into.

static TRACERS: LazyLock<Mutex<HashMap<usize, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn lock_tracers() -> std::sync::MutexGuard<'static, HashMap<usize, usize>> {
    crate::sync::lock(&TRACERS)
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

// the destructors that run a user `drop` method. a garbage cycle through one
// of these is left alone: its drop could reach the other members through
// their strong fields and store one where it outlives the pass that frees
// them all, so the collector leaks that cycle rather than collect it.
pub(super) static USER_DROPS: LazyLock<Mutex<HashSet<usize>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// Record that a destructor runs a user `drop` method. Called from the same
/// generated registration body as `pith_cc_register_tracer`.
#[no_mangle]
pub extern "C" fn pith_cc_register_user_drop(dtor: i64) {
    if dtor != 0 {
        crate::sync::lock(&USER_DROPS).insert(dtor as usize);
    }
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
        // SAFETY: the release/acquire pair orders the hook store before its first use,
        // and `install_visit_hook` only publishes `extern "C" fn(i64, i64)` addresses.
        // panic-guard: the only writer is install_visit_hook, whose contract pins the fn shape, so the address is always a valid visitor.
        let visit: extern "C" fn(i64, i64) = unsafe { std::mem::transmute(hook) };
        visit(child, kind);
        return;
    }
    // no collector listening: the caller is walking for `share.rs`, which
    // drains the same per-thread buffer the collector's hook fills
    traced_children_push(child, kind);
    if cycle_gc_enabled() {
        CYCLE_VISITS_UNHOOKED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Invoke a compiler-emitted tracer twin registered beside a struct's destructor.
///
/// # Safety
/// `tracer` must be a registered `extern "C" fn(i64)` tracer matching `ptr`'s struct
/// layout, and `ptr` must be a live struct pointer.
#[inline(always)]
pub(super) unsafe fn invoke_tracer(tracer: usize, ptr: i64) {
    // SAFETY: `tracer` was registered by `pith_cc_register_tracer` as a tracer twin.
    // panic-guard: calling a compiler-emitted tracer twin registered beside its destructor.
    let trace: unsafe extern "C" fn(i64) = unsafe { std::mem::transmute(tracer) };
    // SAFETY: Caller guarantees `ptr` is a live struct matching `tracer`.
    unsafe { trace(ptr) };
}

/// The children a struct's tracer reports, weak fields included (kind 8), for
/// the shared-marking walk. A struct without a tracer reports none.
pub(crate) unsafe fn struct_children(ptr: i64, f: &mut dyn FnMut(i64, u8)) {
    // Never invoke a tracer on a dead or mid-destructor struct whose fields
    // have already been released or poisoned.
    // SAFETY: `cycle_struct_alive` validates the struct header magic.
    if unsafe { crate::runtime_core::cycle_struct_alive(ptr) } != Some(true) {
        return;
    }
    // SAFETY: Caller guarantees `ptr` is a live struct pointer.
    let dtor = unsafe { crate::runtime_core::cycle_struct_dtor_slot(ptr) };
    if dtor == 0 {
        return;
    }
    let Some(tracer) = tracer_for_dtor(dtor as usize) else {
        return;
    };
    traced_children_take();
    // SAFETY: `tracer` is registered for `ptr`'s destructor and `ptr` is live.
    unsafe { invoke_tracer(tracer, ptr) };
    for (child, kind) in traced_children_take() {
        f(child, kind as u8);
    }
}

// the buffer's touches behind calls of their own: the walk above runs a
// tracer between them, and no frame that runs pith code may hold a
// thread-local's address (tooling/check_tls_barriers.sh)
#[inline(never)]
pub(super) fn traced_children_push(child: i64, kind: i64) {
    TRACED_CHILDREN.with(|buffer| buffer.borrow_mut().push((child, kind)));
}

#[inline(never)]
pub(super) fn traced_children_take() -> Vec<(i64, i64)> {
    TRACED_CHILDREN.with(|buffer| std::mem::take(&mut *buffer.borrow_mut()))
}
thread_local! {
    // the children one tracer call reports. `pith_cc_visit` has no context
    // argument, so the installed hook needs a side slot to deliver through;
    // a per-thread buffer the caller drains right after the tracer returns
    // is the simplest shape that cannot dangle: no borrowed closure state
    // survives past the call that set it up. tracer calls never nest (the
    // pass walks one object at a time and drains before descending).
    static TRACED_CHILDREN: RefCell<Vec<(i64, i64)>> = const { RefCell::new(Vec::new()) };
}
