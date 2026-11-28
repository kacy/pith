//! Per-task / per-thread storage for `threadlocal` module globals.

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
        self.isolation = 0;
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
