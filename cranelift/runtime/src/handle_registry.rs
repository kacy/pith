use std::collections::HashSet;
use std::hash::{BuildHasherDefault, Hasher};
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
    Process,
    ProcessOutput,
    Semaphore,
    Task,
    WaitGroup,
    X25519Key,
}

/// the live handle set, split into independently locked shards. every list
/// allocation registers here and every free unregisters, so a single lock put
/// all of a program's threads in one queue: an http server on 32 green workers
/// spent about 90% of its cpu in the kernel waiting on it, and served a quarter
/// of the requests it served on 4. a handle always hashes to the same shard,
/// so each operation still takes exactly one lock.
///
/// 256 shards rather than 64: at 64, a `std.web` server on 32 workers still
/// had a fifth of its thread samples waiting on a shard.
const SHARDS: usize = 256;

/// one shard, padded to a cache line so neighbouring locks do not share one.
#[repr(align(64))]
struct Shard(Mutex<HandleSet>);

type HandleSet = HashSet<(usize, HandleKind), BuildHasherDefault<KeyHasher>>;

/// a multiplicative hash for handle keys. the keys are addresses and small
/// ids, so the default siphash spends time on protection the set does not
/// need, inside the lock.
#[derive(Default)]
struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(b as u64);
        }
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0 ^ n).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }

    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }

    fn write_isize(&mut self, n: isize) {
        self.write_u64(n as u64);
    }

    fn finish(&self) -> u64 {
        // fold the high bits down: the product of an aligned address has
        // zero low bits, and the table indexes buckets by the low bits.
        self.0 ^ (self.0 >> 32)
    }
}

static HANDLES: LazyLock<[Shard; SHARDS]> =
    LazyLock::new(|| std::array::from_fn(|_| Shard(Mutex::new(HandleSet::default()))));

/// the shard that holds `key`. a fibonacci hash spreads both aligned
/// allocation addresses and small sequential ids across the shards.
fn shard(key: usize) -> std::sync::MutexGuard<'static, HandleSet> {
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
