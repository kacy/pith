use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum HandleKind {
    Bytes,
    ByteBuffer,
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
