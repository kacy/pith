use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

/// Pre-check before reading a magic word: dereferencing an unverified handle
/// requires at least struct alignment. Misaligned garbage (a common
/// corruption shape, and what the safe-defaults tests throw at us) fails
/// here instead of faulting; stale handles fail the magic compare because
/// free scrubs the word.
#[inline]
pub(crate) fn plausibly_aligned<T>(ptr: *const ()) -> bool {
    !ptr.is_null() && (ptr as usize) % std::mem::align_of::<T>() == 0
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum HandleKind {
    AtomicInt,
    Channel,
    Closure,
    List,
    Map,
    Mutex,
    Process,
    ProcessOutput,
    Semaphore,
    Set,
    Task,
    WaitGroup,
    X25519Key,
}

static HANDLES: LazyLock<Mutex<HashSet<(usize, HandleKind)>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

pub(crate) fn register(ptr: *const (), kind: HandleKind) {
    if ptr.is_null() {
        return;
    }
    let mut handles = HANDLES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    handles.insert((ptr as usize, kind));
}

pub(crate) fn unregister(ptr: *const (), kind: HandleKind) {
    if ptr.is_null() {
        return;
    }
    let mut handles = HANDLES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    handles.remove(&(ptr as usize, kind));
}

pub(crate) fn is_valid(ptr: *const (), kind: HandleKind) -> bool {
    if ptr.is_null() {
        return false;
    }
    let handles = HANDLES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    handles.contains(&(ptr as usize, kind))
}

pub(crate) fn register_id(id: i64, kind: HandleKind) {
    if id <= 0 {
        return;
    }
    let mut handles = HANDLES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    handles.insert((id as usize, kind));
}

pub(crate) fn unregister_id(id: i64, kind: HandleKind) {
    if id <= 0 {
        return;
    }
    let mut handles = HANDLES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    handles.remove(&(id as usize, kind));
}

pub(crate) fn is_valid_id(id: i64, kind: HandleKind) -> bool {
    if id <= 0 {
        return false;
    }
    let handles = HANDLES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    handles.contains(&(id as usize, kind))
}
