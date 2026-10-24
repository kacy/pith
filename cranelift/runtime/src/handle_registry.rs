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
    List,
    Map,
    Process,
    ProcessOutput,
    Semaphore,
    Set,
    Task,
    WaitGroup,
    X25519Key,
}

/// the live handle set, split into independently locked shards. every list and
/// map allocation registers here and every free unregisters, so a single lock
/// put all of a program's threads in one queue: an http server on 32 green
/// workers spent about 90% of its cpu in the kernel waiting on it, and served
/// a quarter of the requests it served on 4. a handle always hashes to the
/// same shard, so each operation still takes exactly one lock.
const SHARDS: usize = 64;

/// one shard, padded to a cache line so neighbouring locks do not share one.
#[repr(align(64))]
struct Shard(Mutex<HashSet<(usize, HandleKind)>>);

static HANDLES: LazyLock<[Shard; SHARDS]> =
    LazyLock::new(|| std::array::from_fn(|_| Shard(Mutex::new(HashSet::new()))));

/// the shard that holds `key`. a fibonacci hash spreads both aligned
/// allocation addresses and small sequential ids across the shards.
fn shard(key: usize) -> std::sync::MutexGuard<'static, HashSet<(usize, HandleKind)>> {
    let index = (key as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - SHARDS.trailing_zeros());
    HANDLES[index as usize]
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn register(ptr: *const (), kind: HandleKind) {
    if ptr.is_null() {
        return;
    }
    shard(ptr as usize).insert((ptr as usize, kind));
}

pub(crate) fn unregister(ptr: *const (), kind: HandleKind) {
    if ptr.is_null() {
        return;
    }
    shard(ptr as usize).remove(&(ptr as usize, kind));
}

pub(crate) fn is_valid(ptr: *const (), kind: HandleKind) -> bool {
    if ptr.is_null() {
        return false;
    }
    shard(ptr as usize).contains(&(ptr as usize, kind))
}

pub(crate) fn register_id(id: i64, kind: HandleKind) {
    if id > 0 {
        register(id as usize as *const (), kind);
    }
}

pub(crate) fn unregister_id(id: i64, kind: HandleKind) {
    if id > 0 {
        unregister(id as usize as *const (), kind);
    }
}

pub(crate) fn is_valid_id(id: i64, kind: HandleKind) -> bool {
    id > 0 && is_valid(id as usize as *const (), kind)
}
