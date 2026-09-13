//! Map[K,V] - hash-indexed key-value collection
//!
//! Hybrid approach: Uses hashbrown::HashMap internally for O(1) lookups,
//! but presents FFI-compatible interface matching the C runtime.

use crate::collections::list::{
    element_tag_from_code, release_element, retain_element, ListTypeTag,
};
use crate::handle_registry::{self, HandleKind};
use crate::runtime_core::optional_tuple;
use hashbrown::hash_map::EntryRef;
use hashbrown::{Equivalent, HashMap};
use std::hash::{Hash, Hasher};
/// FFI-compatible map handle
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PithMap {
    /// Pointer to internal map implementation
    ptr: *mut (),
}

/// Key type for the internal HashMap
///
/// Integer, string and bytes keys. A string key and a bytes key are both
/// the map's own copy of the content, so a lookup is a content question:
/// two `Bytes` values built by different routes find one entry when their
/// bytes agree, exactly as two strings do. The variants stay distinct so a
/// map never mixes flavors; the constructor's key tag is what keeps each
/// entry point in its own flavor (see `require_key_flavor`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MapKey {
    Int(i64),
    String(Vec<u8>), // Byte representation of the string
    Bytes(Vec<u8>),  // The content of a Bytes value
}

impl Hash for MapKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            MapKey::Int(n) => {
                0u8.hash(state); // Type tag for int
                n.hash(state);
            }
            MapKey::String(bytes) => {
                1u8.hash(state); // Type tag for string
                bytes.hash(state);
            }
            MapKey::Bytes(bytes) => {
                2u8.hash(state); // Type tag for bytes
                bytes.hash(state);
            }
        }
    }
}

/// A key the caller already holds the bytes of: the probe form of `MapKey`.
///
/// Every lookup, membership test, removal and overwrite asks a question about
/// content the caller is already holding — the c-string it passed, or the
/// buffer inside its bytes object — so none of them needs the map's own copy.
/// Only an insert that misses does, and `From<&KeyRef> for MapKey` is the one
/// place that copy is made.
///
/// The `Hash` impl below has to write exactly the bytes `MapKey`'s writes for
/// the same content: the same tag byte, then the same slice hashing. A `Vec<u8>`
/// and a `&[u8]` hash identically (length prefix, then the bytes), which is
/// what makes the two agree. If they ever stopped agreeing the failure would
/// not be a crash: every string-keyed map in every program would start missing
/// keys it holds. `key_ref_hashes_like_map_key` in this file's tests pins it.
#[derive(Clone, Copy, Debug)]
pub enum KeyRef<'a> {
    Int(i64),
    Str(&'a [u8]),
    Bytes(&'a [u8]),
}

impl Hash for KeyRef<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match *self {
            KeyRef::Int(n) => {
                0u8.hash(state); // Type tag for int
                n.hash(state);
            }
            KeyRef::Str(bytes) => {
                1u8.hash(state); // Type tag for string
                bytes.hash(state);
            }
            KeyRef::Bytes(bytes) => {
                2u8.hash(state); // Type tag for bytes
                bytes.hash(state);
            }
        }
    }
}

impl Equivalent<MapKey> for KeyRef<'_> {
    fn equivalent(&self, key: &MapKey) -> bool {
        match (*self, key) {
            (KeyRef::Int(a), MapKey::Int(b)) => a == *b,
            (KeyRef::Str(a), MapKey::String(b)) => a == b.as_slice(),
            (KeyRef::Bytes(a), MapKey::Bytes(b)) => a == b.as_slice(),
            _ => false,
        }
    }
}

/// The map's own copy of a borrowed key. The single `to_vec` in the map's
/// key path, reached only when an insert misses.
impl From<&KeyRef<'_>> for MapKey {
    fn from(key: &KeyRef<'_>) -> MapKey {
        match *key {
            KeyRef::Int(n) => MapKey::Int(n),
            KeyRef::Str(bytes) => MapKey::String(bytes.to_vec()),
            KeyRef::Bytes(bytes) => MapKey::Bytes(bytes.to_vec()),
        }
    }
}

impl KeyRef<'_> {
    /// The key as text, for traces and the strict-miss diagnostic.
    fn display(&self) -> String {
        match *self {
            KeyRef::Int(n) => n.to_string(),
            KeyRef::Str(b) => format!("{:?}", String::from_utf8_lossy(b)),
            KeyRef::Bytes(b) => format!("bytes{:?}", b),
        }
    }
}

impl MapKey {
    /// The key as text, for traces and the strict-miss diagnostic.
    fn display(&self) -> String {
        match self {
            MapKey::Int(n) => n.to_string(),
            MapKey::String(b) => format!("{:?}", String::from_utf8_lossy(b)),
            MapKey::Bytes(b) => format!("bytes{:?}", b),
        }
    }
}

/// A stored value.
///
/// Everything a pith program puts in a map is one word: an int, a float's
/// bits, or a handle to a string, list, map, set or struct. A map's value
/// size is fixed at construction, so a map whose values are word-sized holds
/// every one of them inline and allocates no box for any of them.
///
/// The wide arm exists for `pith_map_insert_int`, the only entry point that
/// takes a value of some other size. No pith-level type reaches it: every
/// map constructor in this file passes a value size of 8, and the emitter
/// calls only those constructors. Keeping the arm costs nothing anyway,
/// since the enum occupies the same 24 bytes the `Vec<u8>` it replaces did.
enum MapVal {
    Word(i64),
    Wide(Box<[u8]>),
}

impl MapVal {
    /// The stored form of a value handed over as raw bytes. The one place a
    /// value's representation is chosen.
    fn from_bytes(bytes: &[u8]) -> MapVal {
        match <[u8; 8]>::try_from(bytes) {
            Ok(word) => MapVal::Word(i64::from_le_bytes(word)),
            Err(_) => MapVal::Wide(bytes.into()),
        }
    }

    /// The word a stored value carries, or `None` when it is too short to
    /// hold one. Lookups, takes, the values list, display and the collector
    /// all ask through here, so one place decides what a stored value means
    /// and the ownership calls below cannot disagree with the readers about
    /// which values carry a count.
    #[inline]
    fn word(&self) -> Option<i64> {
        match self {
            MapVal::Word(word) => Some(*word),
            MapVal::Wide(bytes) if bytes.len() >= 8 => {
                Some(i64::from_le_bytes(bytes[..8].try_into().unwrap_or([0u8; 8])))
            }
            MapVal::Wide(_) => None,
        }
    }
}

/// Internal map implementation using idiomatic Rust
///
/// Uses HashMap for O(1) lookups; values live in the table itself.
pub struct MapImpl {
    /// Magic word for the fast validity check (see map_magic_ok)
    magic: u32,
    /// Shared-handle refcount (see ListImpl.rc)
    rc: std::sync::atomic::AtomicU32,
    /// The actual hash map storing key -> value mappings
    data: HashMap<MapKey, MapVal>,
    /// Specialized storage for int-key maps with 8-byte scalar values
    int_values8: Option<HashMap<i64, i64>>,
    /// Type tag for keys (0=int, 1=string, 2=bytes)
    key_type: KeyType,
    /// Size of values in bytes
    val_size: usize,
    /// Which heap kind the map's values are, or `Primitive` when it owns no
    /// value counts. A map holds exactly one count per stored heap value, the
    /// same contract a tagged list has for its elements.
    val_tag: ListTypeTag,
    /// Bit 0: this map sits in the cycle collector's suspect buffer.
    cycle_flags: std::sync::atomic::AtomicU8,
}

/// Key type enumeration
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyType {
    Int,
    String,
    Bytes,
}

impl KeyType {
    fn from_code(code: i32) -> KeyType {
        match code {
            1 => KeyType::String,
            2 => KeyType::Bytes,
            _ => KeyType::Int,
        }
    }
}

/// A bytes-keyed map reached through a string entry point, or the reverse,
/// is an emitter defect: the string path would read the bytes handle's
/// header as a c-string and the bytes path would read a string as a bytes
/// object, and either way distinct keys collapse into one entry in silence.
/// That silent collapse is the bug E254 was introduced to stop (issues #920
/// and #955), so the mismatch fails loudly instead.
fn require_key_flavor(actual: KeyType, wanted: KeyType, entry: &str) {
    if actual == wanted || (actual != KeyType::Bytes && wanted != KeyType::Bytes) {
        return;
    }
    eprintln!(
        "pith runtime error: {} called on a map whose keys are {:?}, not {:?}",
        entry, actual, wanted
    );
    // the flavor is fixed at construction, so a caller of the wrong flavor is a
    // compiler bug with no answer that is not a wrong answer.
    // panic-guard: a container reached through the wrong flavor's entry point.
    std::process::exit(1);
}

impl MapImpl {
    fn new(key_type: KeyType, val_size: usize, val_tag: ListTypeTag) -> Self {
        let val_is_heap = val_tag != ListTypeTag::Primitive;
        let int_values8 = if matches!(key_type, KeyType::Int) && val_size == 8 && !val_is_heap {
            Some(HashMap::new())
        } else {
            None
        };
        MapImpl {
            magic: MAP_MAGIC,
            rc: std::sync::atomic::AtomicU32::new(1),
            data: HashMap::new(),
            int_values8,
            key_type,
            val_size,
            val_tag,
            cycle_flags: std::sync::atomic::AtomicU8::new(0),
        }
    }

    fn val_is_heap(&self) -> bool {
        self.val_tag != ListTypeTag::Primitive
    }

    /// Learn the value kind from a store, when the emitter knows it and the
    /// map does not. Returns true when the map owns a count on values of that
    /// kind once this returns.
    ///
    /// The constructor cannot always pick the flavor — an empty `{}` in a
    /// position the checker could not type builds a plain map — so the store
    /// is the second chance, and the only place the value's kind is known for
    /// certain. Adopting is count-neutral because it is gated on the map being
    /// empty: there are no already-stored values whose counts the new tag
    /// would start releasing. A non-empty untagged map keeps its tag and the
    /// caller is told so, which is what keeps the fallback a leak rather than
    /// a freed value.
    fn adopt_value_tag(&mut self, tag: ListTypeTag) -> bool {
        if tag == ListTypeTag::Primitive {
            return false;
        }
        if self.val_tag == tag {
            return true;
        }
        if self.val_tag == ListTypeTag::Primitive && self.len() == 0 {
            self.val_tag = tag;
            // heap values never live in the scalar fast path, which stores
            // raw i64s and skips every retain and release.
            self.int_values8 = None;
            return true;
        }
        false
    }

    fn len(&self) -> usize {
        match &self.int_values8 {
            Some(data) => data.len(),
            None => self.data.len(),
        }
    }

    /// Store `value` under a borrowed key, returning whatever it displaced.
    ///
    /// The probe runs against the caller's bytes; the map materialises its own
    /// copy of the key only on the vacant arm, so an overwrite of a key the map
    /// already holds allocates nothing for the key. An occupied entry keeps the
    /// key it already has, exactly as `HashMap::insert` does.
    fn insert(&mut self, key: &KeyRef<'_>, value: MapVal) -> Option<MapVal> {
        match self.data.entry_ref(key) {
            EntryRef::Occupied(mut entry) => Some(entry.insert(value)),
            EntryRef::Vacant(entry) => {
                entry.insert(value);
                None
            }
        }
    }

    fn get(&self, key: &KeyRef<'_>) -> Option<&MapVal> {
        self.data.get(key)
    }

    /// Add `delta` to the word stored under `key` and return the new word, or
    /// `None` when the map does not hold the key.
    ///
    /// One probe does the read and the write. The shape the emitter fuses into
    /// this, `m.insert(k, m[k] + d)`, hashed the key three times and probed
    /// three times for the one slot; the fused form hashes and probes once.
    /// Ownership does not enter into it: the fused path is only taken for a
    /// map of integer values, on which the map holds no counts.
    fn upsert_add(&mut self, key: &KeyRef<'_>, delta: i64) -> Option<i64> {
        if let KeyRef::Int(n) = key {
            if let Some(data) = &mut self.int_values8 {
                let slot = data.get_mut(n)?;
                *slot = slot.wrapping_add(delta);
                return Some(*slot);
            }
        }
        match self.data.get_mut(key)? {
            MapVal::Word(word) => {
                *word = word.wrapping_add(delta);
                Some(*word)
            }
            MapVal::Wide(_) => {
                eprintln!(
                    "pith runtime error: map update on a map whose values are not word-sized"
                );
                // panic-guard: the fused update is emitted only for an integer-valued map, so a wide value here is a compiler bug with no correct answer.
                std::process::exit(1);
            }
        }
    }

    fn remove(&mut self, key: &KeyRef<'_>) -> Option<MapVal> {
        self.data.remove(key)
    }

    fn contains_key(&self, key: &KeyRef<'_>) -> bool {
        self.data.contains_key(key)
    }

    fn clear(&mut self) {
        if let Some(data) = &mut self.int_values8 {
            data.clear();
        } else {
            self.data.clear();
        }
    }

    fn keys(&self) -> Vec<MapKey> {
        match &self.int_values8 {
            Some(data) => data.keys().map(|key| MapKey::Int(*key)).collect(),
            None => self.data.keys().cloned().collect(),
        }
    }

    /// Every stored value as the word it carries, for the callers that hand
    /// the values out. A value too short to carry one is skipped rather than
    /// reported as zero, which is what the readers did with a short box.
    fn value_words(&self) -> Vec<i64> {
        match &self.int_values8 {
            Some(data) => data.values().copied().collect(),
            None => self.data.values().filter_map(MapVal::word).collect(),
        }
    }

    fn uses_int_values8(&self) -> bool {
        self.int_values8.is_some()
    }

    fn insert_int_value(&mut self, key: i64, value: i64) -> Option<i64> {
        match &mut self.int_values8 {
            Some(data) => data.insert(key, value),
            None => None,
        }
    }

    fn get_int_value(&self, key: i64) -> Option<i64> {
        match &self.int_values8 {
            Some(data) => data.get(&key).copied(),
            None => None,
        }
    }

    fn contains_int_key(&self, key: i64) -> bool {
        match &self.int_values8 {
            Some(data) => data.contains_key(&key),
            None => false,
        }
    }

    fn remove_int_value(&mut self, key: i64) -> Option<i64> {
        match &mut self.int_values8 {
            Some(data) => data.remove(&key),
            None => None,
        }
    }

    /// Drop the map's count on a value that has just left the map — removed,
    /// overwritten, or cleared. A heap-valued map holds exactly one count per
    /// stored value, so anyone still reading it after this point must be
    /// holding a count of their own (the emitter retains borrowed values into
    /// locals, fields, and other containers).
    ///
    /// # Safety
    /// `val` must be a value this map owned.
    unsafe fn release_value(&self, val: &MapVal) {
        if !self.val_is_heap() {
            return;
        }
        if let Some(raw) = val.word() {
            release_element(self.val_tag, raw);
        }
    }

    /// Take the map's count on a value being stored.
    ///
    /// # Safety
    /// `raw` must be a handle of the map's value kind.
    unsafe fn retain_value(&self, raw: i64) {
        if !self.val_is_heap() {
            return;
        }
        retain_element(self.val_tag, raw);
    }

    /// Drop the map's count on every stored value, for clear and free.
    ///
    /// # Safety
    /// The caller must not use the values afterwards.
    unsafe fn release_all_values(&self) {
        if !self.val_is_heap() {
            return;
        }
        for val in self.data.values() {
            self.release_value(val);
        }
    }
}

/// Magic word for MapImpl ("PMAP"). Distinct per collection kind so a list
/// handle passed where a map is expected still fails validation.
const MAP_MAGIC: u32 = 0x504d4150;

/// Fast validity check: one memory read instead of a global registry lock
/// per access. Freed maps get their magic scrubbed in pith_map_free.
#[inline]
unsafe fn map_magic_ok(ptr: *const ()) -> bool {
    handle_registry::plausibly_aligned::<MapImpl>(ptr)
        && (*(ptr as *const MapImpl)).magic == MAP_MAGIC
}

unsafe fn map_ref<'a>(map: PithMap) -> Option<&'a MapImpl> {
    if !map_magic_ok(map.ptr as *const ()) {
        return None;
    }
    Some(&*(map.ptr as *const MapImpl))
}

unsafe fn map_mut<'a>(map: PithMap) -> Option<&'a mut MapImpl> {
    if !map_magic_ok(map.ptr as *const ()) {
        return None;
    }
    Some(&mut *(map.ptr as *mut MapImpl))
}

unsafe fn map_ref_from_handle<'a>(handle: i64) -> Option<&'a MapImpl> {
    if !map_magic_ok(handle as *const ()) {
        return None;
    }
    Some(&*(handle as *const MapImpl))
}

unsafe fn map_mut_from_handle<'a>(handle: i64) -> Option<&'a mut MapImpl> {
    if !map_magic_ok(handle as *const ()) {
        return None;
    }
    Some(&mut *(handle as *mut MapImpl))
}

/// Create a new empty map
///
/// # Arguments
/// * `key_type` - 0 for int keys, 1 for string keys
/// * `val_size` - Size of each value in bytes
/// * `val_is_heap` - Whether values are heap types (need retain/release)
/// Create a new string-key map with default settings
#[no_mangle]
pub unsafe extern "C" fn pith_map_new_default() -> PithMap {
    pith_map_new(1, 8, 0) // string keys, 8-byte values, not heap
}

/// Create a new int-key map with default settings
#[no_mangle]
pub unsafe extern "C" fn pith_map_new_int() -> PithMap {
    pith_map_new(0, 8, 0) // int keys, 8-byte values, not heap
}

/// String-key map that owns cstring values: insert retains; overwrite,
/// remove, clear, and free release. The emitter uses this for
/// Map[String, String].
#[no_mangle]
pub unsafe extern "C" fn pith_map_new_cstr_val() -> PithMap {
    pith_map_new(1, 8, 1)
}

/// Int-key map that owns cstring values (Map[Int, String]).
#[no_mangle]
pub unsafe extern "C" fn pith_map_new_int_cstr_val() -> PithMap {
    pith_map_new(0, 8, 1)
}

/// Bytes-key map with 8-byte values it owns no counts on (Map[Bytes, Int]);
/// a heap value kind is learned at the first kind-carrying store.
#[no_mangle]
pub unsafe extern "C" fn pith_map_new_bytes() -> PithMap {
    pith_map_new(2, 8, 0)
}

/// Bytes-key map that owns cstring values (Map[Bytes, String]).
#[no_mangle]
pub unsafe extern "C" fn pith_map_new_bytes_cstr_val() -> PithMap {
    pith_map_new(2, 8, 1)
}

#[no_mangle]
pub unsafe extern "C" fn pith_map_new(key_type: i32, val_size: i64, val_is_heap: i64) -> PithMap {
    // the historical spelling: "heap" meant cstring, the only kind a map
    // could own. pith_map_new_tagged is the general form.
    pith_map_new_tagged(
        key_type,
        val_size,
        if val_is_heap != 0 {
            ListTypeTag::String as i32
        } else {
            ListTypeTag::Primitive as i32
        },
    )
}

/// Create a map whose values are of a named heap kind, or `Primitive` for a
/// map that owns no value counts. The tag codes are the element-tag codes
/// `pith_list_new` uses.
unsafe fn pith_map_new_tagged(key_type: i32, val_size: i64, val_tag: i32) -> PithMap {
    let map_impl = MapImpl::new(
        KeyType::from_code(key_type),
        val_size as usize,
        element_tag_from_code(val_tag),
    );
    let boxed = Box::new(map_impl);
    let ptr = Box::into_raw(boxed) as *mut ();
    handle_registry::register(ptr as *const (), HandleKind::Map);
    PithMap { ptr }
}

/// Get map length
#[no_mangle]
pub extern "C" fn pith_map_len(map: PithMap) -> i64 {
    unsafe {
        map_ref(map)
            .map(|impl_ref| impl_ref.len() as i64)
            .unwrap_or(0)
    }
}

/// Insert key-value pair with integer key
///
/// # Safety
/// * `key` is the integer key value
/// * `value` must point to valid data of size `val_size`
#[no_mangle]
pub unsafe extern "C" fn pith_map_insert_int(
    map: *mut PithMap,
    key: i64,
    value: *const u8,
    val_size: i64,
) {
    if map.is_null() || value.is_null() {
        return;
    }

    let Some(impl_ref) = map_mut(*map) else {
        return;
    };
    crate::perf_stats!(PERF_MAP_INT_INSERTS += 1);

    // Verify value size matches
    if impl_ref.val_size != val_size as usize {
        eprintln!("pith: map value size mismatch");
        return;
    }

    // Verify key type
    if !matches!(impl_ref.key_type, KeyType::Int) {
        eprintln!("pith: map key type mismatch (expected int)");
        return;
    }

    // Copy value data
    let val_slice = std::slice::from_raw_parts(value, val_size as usize);
    if impl_ref.uses_int_values8() {
        crate::perf_count(&crate::PERF_MAP_INT_FAST_INSERTS, 1);
        let int_value = i64::from_le_bytes(val_slice[..8].try_into().unwrap_or([0u8; 8]));
        impl_ref.insert_int_value(key, int_value);
        return;
    }
    crate::perf_count(&crate::PERF_MAP_INT_FALLBACK_INSERTS, 1);
    // the only caller that can supply a value that is not one word, so the
    // only place a boxed value is built. A value size of 8 stores inline
    // like every other entry point.
    let stored = MapVal::from_bytes(val_slice);

    // The map owns one count per stored heap value, read out of the same
    // word release_value drops it from.
    // (the retain must stay gated on the value tag: magic-checking an
    // arbitrary integer dereferences value-16, which faults on values
    // that resemble unmapped addresses.)
    if let Some(raw) = stored.word() {
        impl_ref.retain_value(raw);
    }

    // Retain before insert, release the displaced value after: `m[k] = m[k]`
    // must not drop the last count before the new one is taken.
    if let Some(old) = impl_ref.insert(&KeyRef::Int(key), stored) {
        impl_ref.release_value(&old);
    }
}

/// Clear all entries from map
#[no_mangle]
pub unsafe extern "C" fn pith_map_clear(map: *mut PithMap) {
    if map.is_null() {
        return;
    }

    let Some(impl_ref) = map_mut(*map) else {
        return;
    };

    impl_ref.release_all_values();
    impl_ref.clear();
}

/// Release map and free memory
#[no_mangle]
pub unsafe extern "C" fn pith_map_release(map: PithMap) {
    let Some(impl_ref) = map_mut(map) else {
        return;
    };
    // cycle-gc suspect hook, before our count is given up: while we still
    // hold it the map cannot die under the hook (see `maybe_suspect_struct`
    // in runtime_core for why the ordering matters).
    if crate::cycle::cycle_gc_enabled()
        && impl_ref.rc.load(std::sync::atomic::Ordering::Relaxed) > 1
    {
        maybe_suspect_map(impl_ref, map.ptr as usize);
    }
    let prev = impl_ref
        .rc
        .fetch_sub(1, std::sync::atomic::Ordering::Release);
    if prev > 1 {
        return;
    }
    if prev == 0 {
        impl_ref.rc.store(0, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);

    impl_ref.release_all_values();

    // Free the map implementation. Scrub the magic first so any handle
    // that outlives the map fails the fast validity check.
    (*(map.ptr as *mut MapImpl)).magic = 0;
    handle_registry::unregister(map.ptr as *const (), HandleKind::Map);
    // a map that dies while the suspect buffer points at it keeps its shell:
    // the values are already released and the handle no longer validates
    // anywhere, so deferring the box drop to the graveyard leaks only the
    // impl struct until the collector frees it.
    if crate::cycle::cycle_gc_enabled()
        && impl_ref
            .cycle_flags
            .load(std::sync::atomic::Ordering::Relaxed)
            & 1
            != 0
    {
        crate::cycle::graveyard_defer(map.ptr as usize, crate::cycle::CYCLE_KIND_MAP);
        return;
    }
    let _ = Box::from_raw(map.ptr as *mut MapImpl);
}

/// Mark a map as a cycle suspect and hand it to the buffer. Cold and
/// outlined so the release fast path stays frameless with the flag off.
#[cold]
#[inline(never)]
unsafe fn maybe_suspect_map(impl_ref: &MapImpl, ptr: usize) {
    if impl_ref
        .cycle_flags
        .fetch_or(1, std::sync::atomic::Ordering::Relaxed)
        & 1
        != 0
    {
        return; // already buffered
    }
    crate::cycle::cycle_suspect(ptr, crate::cycle::CYCLE_KIND_MAP);
}

/// Drop a map's buffered mark (overflow, or a collector drain). Magic-
/// checked, so a map that already died and was freed is a no-op.
pub(crate) unsafe fn cycle_clear_map_buffered(handle: i64) {
    if let Some(impl_ref) = map_ref_from_handle(handle) {
        impl_ref
            .cycle_flags
            .fetch_and(!1, std::sync::atomic::Ordering::Relaxed);
    }
}

// --- collector-facing accessors ---------------------------------------------
//
// mirrors the list accessors: the collection pass holds the world stopped,
// and every entry point magic-checks so a bad edge degrades to None or a
// no-op rather than a wild read.

/// The map's current reference count, or `None` when the handle no longer
/// validates (the map died into the graveyard).
pub(crate) unsafe fn cycle_map_strong_count(handle: i64) -> Option<u32> {
    map_ref_from_handle(handle)
        .map(|impl_ref| impl_ref.rc.load(std::sync::atomic::Ordering::Relaxed))
}

/// Add `delta` to the reference count — the collector's teardown guard.
pub(crate) unsafe fn cycle_map_guard_strong(handle: i64, delta: u32) {
    if let Some(impl_ref) = map_ref_from_handle(handle) {
        impl_ref
            .rc
            .fetch_add(delta, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Report every value the map owns a count on, as (value, child code).
/// Keys are copies (ints, byte strings), never counted handles, so only the
/// value side has edges; the scalar fast path stores raw ints and owns none.
pub(crate) unsafe fn cycle_map_children(handle: i64, f: &mut dyn FnMut(i64, u8)) {
    let Some(impl_ref) = map_ref_from_handle(handle) else {
        return;
    };
    let Some(code) = crate::collections::list::cycle_child_code(impl_ref.val_tag) else {
        return;
    };
    for val in impl_ref.data.values() {
        match val.word() {
            Some(raw) if raw != 0 => f(raw, code),
            _ => {}
        }
    }
}

/// The destruction body of a garbage map: drop the count it holds on every
/// value, then empty the storage so the shell free below can never release
/// them a second time.
pub(crate) unsafe fn cycle_map_release_values(handle: i64) {
    let Some(impl_ref) = map_mut_from_handle(handle) else {
        return;
    };
    impl_ref.release_all_values();
    impl_ref.clear();
}

/// Free a garbage map's shell: the tail of `pith_map_release` minus the value
/// cascade, which `cycle_map_release_values` already ran. A shell re-buffered
/// during teardown parks in the graveyard so the fresh suspect entry never
/// dangles.
pub(crate) unsafe fn cycle_map_free_dead(handle: i64) {
    let Some(impl_ref) = map_ref_from_handle(handle) else {
        return;
    };
    let buffered = impl_ref
        .cycle_flags
        .load(std::sync::atomic::Ordering::Relaxed)
        & 1
        != 0;
    (*(handle as *mut MapImpl)).magic = 0;
    handle_registry::unregister(handle as *const (), HandleKind::Map);
    if buffered && crate::cycle::cycle_gc_enabled() {
        crate::cycle::graveyard_defer(handle as usize, crate::cycle::CYCLE_KIND_MAP);
        return;
    }
    drop(Box::from_raw(handle as *mut MapImpl));
}

/// Drop a map shell the graveyard parked: values already released, magic
/// already scrubbed, registry entry already gone — only the box remains.
pub(crate) unsafe fn cycle_drop_map_shell(ptr: usize) {
    drop(Box::from_raw(ptr as *mut MapImpl));
}

/// Remove an int-keyed entry and hand its value — count included — to the
/// caller. The map neither retains nor releases: ownership transfers, so
/// this is the reclaim-safe way to drop registry entries under the
/// free-only-cascade rule.
///
/// # Safety
/// map_handle must be a valid map handle or garbage (the magic check
/// rejects garbage).
#[no_mangle]
pub unsafe extern "C" fn pith_map_take_ikey(map_handle: i64, key: i64) -> i64 {
    let Some(impl_ref) = map_mut_from_handle(map_handle) else {
        return 0;
    };
    if impl_ref.uses_int_values8() {
        return impl_ref.remove_int_value(key).unwrap_or(0);
    }
    take_keyed(impl_ref, &KeyRef::Int(key))
}

/// String-keyed take: remove and transfer the value's count to the caller.
///
/// # Safety
/// map_handle must be a valid map handle; key a valid cstring.
#[no_mangle]
pub unsafe extern "C" fn pith_map_take(map_handle: i64, key: *const i8) -> i64 {
    let Some(impl_ref) = map_mut_from_handle(map_handle) else {
        return 0;
    };
    require_key_flavor(impl_ref.key_type, KeyType::String, "map_take");
    take_keyed(impl_ref, &cstr_key_ref(key))
}

/// Bytes-keyed take: remove and transfer the value's count to the caller.
///
/// # Safety
/// map_handle must be a valid map handle; key a bytes handle or null.
#[no_mangle]
pub unsafe extern "C" fn pith_map_take_bkey(map_handle: i64, key: i64) -> i64 {
    let Some(impl_ref) = map_mut_from_handle(map_handle) else {
        return 0;
    };
    require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_take_bkey");
    let Some(map_key) = bytes_key_ref(key) else {
        return 0;
    };
    take_keyed(impl_ref, &map_key)
}

/// Remove an entry and hand its value's count to the caller: the map's
/// count leaves with the value instead of being released here.
unsafe fn take_keyed(impl_ref: &mut MapImpl, map_key: &KeyRef<'_>) -> i64 {
    impl_ref
        .remove(map_key)
        .and_then(|val| val.word())
        .unwrap_or(0)
}

/// Retain a map handle: one more owner of this shared handle.
#[no_mangle]
pub unsafe extern "C" fn pith_map_retain_handle(handle: i64) {
    if let Some(impl_ref) = map_mut_from_handle(handle) {
        impl_ref
            .rc
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Release a map handle; frees the map and its owned values at zero.
#[no_mangle]
pub unsafe extern "C" fn pith_map_release_handle(handle: i64) {
    pith_map_release(PithMap {
        ptr: handle as *mut (),
    });
}

/// Destructor for map elements in collections
///
/// Called by cycle collector when freeing cyclic map objects
#[no_mangle]
pub extern "C" fn pith_map_destructor(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }

    unsafe {
        let map = ptr as *const PithMap;
        pith_map_release(*map);
    }
}

// ---------------------------------------------------------------------------
// C-string-key variants for Cranelift codegen
//
// These functions accept a raw map_handle (the PithMap.ptr cast to i64) and
// null-terminated C string keys, providing a simpler ABI than the PithString
// variants above.
// ---------------------------------------------------------------------------

/// Compute the byte length of a null-terminated C string (helper).
fn map_trace_enabled() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var("PITH_MAP_TRACE").is_ok())
}

/// Borrow a c-string key: walk it for its length and hand back the caller's
/// own bytes. Nothing is copied, so the borrow is only good for the length of
/// the call that took it.
unsafe fn cstr_key_ref<'a>(key: *const i8) -> KeyRef<'a> {
    let mut len = 0usize;
    let mut p = key;
    while *p != 0 {
        len += 1;
        p = p.add(1);
    }
    KeyRef::Str(std::slice::from_raw_parts(key as *const u8, len))
}

/// Borrow a bytes handle's buffer as a key, or `None` for a handle that is not
/// a live bytes object. A null handle is the empty value, which `pith_bytes_eq`
/// already treats as equal to an empty bytes object. The handle is only read:
/// the map keeps its own copy of the content and never a count on the caller's
/// object, and the caller's object is alive for the whole of the entry point
/// that borrowed it, so the borrow must not outlive that call.
unsafe fn bytes_key_ref<'a>(key: i64) -> Option<KeyRef<'a>> {
    if key == 0 {
        return Some(KeyRef::Bytes(&[]));
    }
    crate::bytes::pith_bytes_ref(key).map(|b| KeyRef::Bytes(b.data.as_slice()))
}

/// The stored word under `map_key`, or `None` on a miss.
unsafe fn get_keyed(impl_ref: &MapImpl, map_key: &KeyRef<'_>) -> Option<i64> {
    impl_ref.get(map_key).and_then(|val| val.word())
}

/// Drop the entry under `map_key`, releasing the map's count on its value.
unsafe fn remove_keyed(impl_ref: &mut MapImpl, map_key: &KeyRef<'_>) {
    if let Some(old) = impl_ref.remove(map_key) {
        impl_ref.release_value(&old);
    }
}

/// Exit with the structured diagnostic for a strict miss: `m[k]` on a key
/// the map does not hold.
fn strict_miss(key_display: &str) -> ! {
    eprintln!(
        "pith runtime error: map key not found: {} (use .contains_key first, .get(k) for Optional, or .get_default(k, d) for a fallback)",
        key_display
    );
    // panic-guard: a missing key under strict indexing is a program bug with no value to return.
    std::process::exit(1);
}

/// Insert an i64 value with a C-string key.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_map_insert_cstr(map_handle: i64, key: *const i8, value: i64) {
    insert_cstr_inner(map_handle, key, value, false);
}

/// Insert a value the caller owns under a C-string key. The map takes the
/// caller's count instead of adding one of its own, so a value built
/// straight into the store ends up with exactly one owner. A map that owns
/// no value counts has nothing to take, and the caller's count stays
/// outstanding there — see `pith_list_push_value_owned`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_map_insert_cstr_owned(map_handle: i64, key: *const i8, value: i64) {
    insert_cstr_inner(map_handle, key, value, true);
}

/// Insert a borrowed value of a named heap kind. The map learns the kind
/// here when its constructor could not supply one — see
/// `MapImpl::adopt_value_tag` — which is what lets a container stored into a
/// map be owned by it. When the map cannot own the kind, the count the
/// emitter would once have added at the call site is added here instead, so
/// the value still outlives the caller's local: a leak, never a freed value.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_map_insert_cstr_kind(
    map_handle: i64,
    key: *const i8,
    value: i64,
    val_tag: i64,
) {
    let owns = map_adopt_value_tag(map_handle, val_tag);
    if !owns {
        retain_element(element_tag_from_code(val_tag as i32), value);
    }
    insert_cstr_inner(map_handle, key, value, false);
}

/// Insert an owned value of a named heap kind: the map takes the caller's
/// count, adopting the kind first when it has none. A map that cannot own
/// the kind takes nothing and the caller's count stays outstanding.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_map_insert_cstr_owned_kind(
    map_handle: i64,
    key: *const i8,
    value: i64,
    val_tag: i64,
) {
    map_adopt_value_tag(map_handle, val_tag);
    insert_cstr_inner(map_handle, key, value, true);
}

/// Let a map learn the heap kind of the values being stored into it.
/// Returns true when the map owns a count on that kind afterwards.
unsafe fn map_adopt_value_tag(map_handle: i64, val_tag: i64) -> bool {
    match map_mut_from_handle(map_handle) {
        Some(impl_ref) => impl_ref.adopt_value_tag(element_tag_from_code(val_tag as i32)),
        None => false,
    }
}

unsafe fn insert_cstr_inner(map_handle: i64, key: *const i8, value: i64, takes_caller_count: bool) {
    if key.is_null() {
        return;
    }

    let Some(impl_ref) = map_mut_from_handle(map_handle) else {
        return;
    };
    require_key_flavor(impl_ref.key_type, KeyType::String, "map_insert");
    crate::perf_stats!(PERF_MAP_STRING_INSERTS += 1);
    insert_keyed(impl_ref, &cstr_key_ref(key), value, takes_caller_count);
}

unsafe fn insert_bkey_inner(map_handle: i64, key: i64, value: i64, takes_caller_count: bool) {
    let Some(impl_ref) = map_mut_from_handle(map_handle) else {
        return;
    };
    require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_insert_bkey");
    let Some(map_key) = bytes_key_ref(key) else {
        return;
    };
    insert_keyed(impl_ref, &map_key, value, takes_caller_count);
}

/// Store `value` under `map_key`. the map retains the incoming value when it
/// owns heap values, and drops its count on whatever that displaces. an owned
/// value arrives with the caller's count, which the map keeps as its own.
unsafe fn insert_keyed(
    impl_ref: &mut MapImpl,
    map_key: &KeyRef<'_>,
    value: i64,
    takes_caller_count: bool,
) {
    if impl_ref.val_is_heap() {
        if !takes_caller_count {
            impl_ref.retain_value(value);
        }
        if map_trace_enabled() {
            eprintln!("map_ins {} -> {:p}", map_key.display(), value as *const i8);
        }
    }
    if let Some(old) = impl_ref.insert(map_key, MapVal::Word(value)) {
        impl_ref.release_value(&old);
    }
}

/// Body of the fused update: add `delta` to the value under a borrowed key and
/// hand back the new value. A key the map does not hold takes the same exit
/// `m[k]` takes, because the strict get this replaces is what would have run.
unsafe fn upsert_add_keyed(impl_ref: &mut MapImpl, map_key: &KeyRef<'_>, delta: i64) -> i64 {
    if impl_ref.val_is_heap() {
        eprintln!("pith runtime error: map update on a map whose values are reference-counted");
        // panic-guard: the fused update is emitted only for an integer-valued map, so a counted value here is a compiler bug with no correct answer.
        std::process::exit(1);
    }
    match impl_ref.upsert_add(map_key, delta) {
        Some(v) => v,
        None => strict_miss(&map_key.display()),
    }
}

/// Add `delta` to the value under a C-string key, returning the new value.
///
/// The emitter calls this for `m.insert(k, m[k] + d)` and `m[k] = m[k] + d`
/// when both key expressions are the same side-effect-free expression and the
/// map's values are integers. The unfused shape hashes the key twice here,
/// once for the strict get and once for the store; this hashes it once. The
/// store counter is the one that moves, because one call is one store.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_map_upsert_add_cstr(
    map_handle: i64,
    key: *const i8,
    delta: i64,
) -> i64 {
    if key.is_null() {
        eprintln!("pith runtime error: map key not found: <null>");
        // panic-guard: a null map key is a program bug with no value to return.
        std::process::exit(1);
    }
    let Some(impl_ref) = map_mut_from_handle(map_handle) else {
        eprintln!("pith runtime error: map indexing on invalid map handle");
        // panic-guard: a fused update on an invalid handle is a program bug with no value to return.
        std::process::exit(1);
    };
    require_key_flavor(impl_ref.key_type, KeyType::String, "map_upsert_add");
    crate::perf_stats!(PERF_MAP_STRING_INSERTS += 1);
    upsert_add_keyed(impl_ref, &cstr_key_ref(key), delta)
}

/// Add `delta` to the value under an integer key, returning the new value.
/// The int-keyed twin of `pith_map_upsert_add_cstr`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_upsert_add_ikey(map_handle: i64, key: i64, delta: i64) -> i64 {
    let Some(impl_ref) = map_mut_from_handle(map_handle) else {
        eprintln!("pith runtime error: map indexing on invalid map handle");
        // panic-guard: a fused update on an invalid handle is a program bug with no value to return.
        std::process::exit(1);
    };
    crate::perf_stats!(PERF_MAP_INT_INSERTS += 1);
    upsert_add_keyed(impl_ref, &KeyRef::Int(key), delta)
}

/// Add `delta` to the value under a bytes key, returning the new value. The
/// bytes-keyed twin of `pith_map_upsert_add_cstr`; the key is borrowed from
/// the caller's bytes object for the length of the call, the way every other
/// bytes entry point borrows it.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_upsert_add_bkey(map_handle: i64, key: i64, delta: i64) -> i64 {
    let Some(impl_ref) = map_mut_from_handle(map_handle) else {
        eprintln!("pith runtime error: map indexing on invalid map handle");
        // panic-guard: a fused update on an invalid handle is a program bug with no value to return.
        std::process::exit(1);
    };
    require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_upsert_add_bkey");
    let Some(map_key) = bytes_key_ref(key) else {
        strict_miss("<not a bytes value>");
    };
    upsert_add_keyed(impl_ref, &map_key, delta)
}

/// Get an i64 value by C-string key. Returns 0 if the key is not found.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_cstr(map_handle: i64, key: *const i8) -> i64 {
    if key.is_null() {
        return 0;
    }

    let Some(impl_ref) = map_ref_from_handle(map_handle) else {
        return 0;
    };
    require_key_flavor(impl_ref.key_type, KeyType::String, "map_get");
    crate::perf_stats!(PERF_MAP_STRING_GETS += 1);
    let map_key = cstr_key_ref(key);
    let Some(v) = get_keyed(impl_ref, &map_key) else {
        return 0;
    };
    if impl_ref.val_is_heap() && map_trace_enabled() {
        eprintln!("map_get {} -> {:p}", map_key.display(), v as *const i8);
    }
    v
}

/// Get an i64 value by C-string key, wrapped in an Optional tuple. Returns
/// `Some(value)` when the key is present and `None` otherwise — so callers
/// can distinguish "not present" from a legitimately-stored `0`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_cstr_opt(map_handle: i64, key: *const i8) -> i64 {
    if key.is_null() {
        return optional_tuple(false, 0);
    }
    let Some(impl_ref) = map_ref_from_handle(map_handle) else {
        return optional_tuple(false, 0);
    };
    require_key_flavor(impl_ref.key_type, KeyType::String, "map_get_opt");
    crate::perf_stats!(PERF_MAP_STRING_GETS += 1);
    match get_keyed(impl_ref, &cstr_key_ref(key)) {
        Some(v) => optional_tuple(true, v),
        None => optional_tuple(false, 0),
    }
}

/// Get an i64 value by C-string key. If the key is not present, prints a
/// structured diagnostic to stderr and exits non-zero — the strict path
/// for `map[k]`, replacing the old silent-zero behavior. Callers that
/// want fallback behavior have `.get_default(k, d)` and `.get(k)`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_cstr_strict(map_handle: i64, key: *const i8) -> i64 {
    if key.is_null() {
        eprintln!("pith runtime error: map key not found: <null>");
        // panic-guard: a null map key is a program bug with no value to return.
        std::process::exit(1);
    }
    let Some(impl_ref) = map_ref_from_handle(map_handle) else {
        eprintln!("pith runtime error: map indexing on invalid map handle");
        // panic-guard: strict map indexing on an invalid handle is a program bug with no value to return.
        std::process::exit(1);
    };
    require_key_flavor(impl_ref.key_type, KeyType::String, "map_get_strict");
    crate::perf_stats!(PERF_MAP_STRING_GETS += 1);
    let map_key = cstr_key_ref(key);
    match get_keyed(impl_ref, &map_key) {
        Some(v) => v,
        None => strict_miss(&map_key.display()),
    }
}

/// Check if a C-string key exists in the map. Returns 1 if present, 0 otherwise.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_map_contains_cstr(map_handle: i64, key: *const i8) -> i64 {
    if key.is_null() {
        return 0;
    }

    let Some(impl_ref) = map_ref_from_handle(map_handle) else {
        return 0;
    };
    require_key_flavor(impl_ref.key_type, KeyType::String, "map_contains_key");
    crate::perf_stats!(PERF_MAP_STRING_CONTAINS += 1);
    if impl_ref.contains_key(&cstr_key_ref(key)) {
        1
    } else {
        0
    }
}

/// Get value by C-string key with a default if not found.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_default_cstr(
    map_handle: i64,
    key: *const i8,
    default: i64,
) -> i64 {
    if key.is_null() {
        return default;
    }
    let Some(impl_ref) = map_ref_from_handle(map_handle) else {
        return default;
    };
    require_key_flavor(impl_ref.key_type, KeyType::String, "map_get_default");
    crate::perf_stats!(PERF_MAP_STRING_GETS += 1);
    get_keyed(impl_ref, &cstr_key_ref(key)).unwrap_or(default)
}

/// Get value by integer key with a default if not found.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_default_ikey(map_handle: i64, key: i64, default: i64) -> i64 {
    let Some(impl_ref) = map_ref_from_handle(map_handle) else {
        return default;
    };
    crate::perf_stats!(PERF_MAP_INT_GETS += 1);
    if impl_ref.uses_int_values8() {
        crate::perf_count(&crate::PERF_MAP_INT_FAST_GETS, 1);
        impl_ref.get_int_value(key).unwrap_or(default)
    } else {
        crate::perf_count(&crate::PERF_MAP_INT_FALLBACK_GETS, 1);
        get_keyed(impl_ref, &KeyRef::Int(key)).unwrap_or(default)
    }
}

/// Remove an entry by C-string key.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_map_remove_cstr(map_handle: i64, key: *const i8) {
    if key.is_null() {
        return;
    }

    let Some(impl_ref) = map_mut_from_handle(map_handle) else {
        return;
    };
    require_key_flavor(impl_ref.key_type, KeyType::String, "map_remove");
    crate::perf_stats!(PERF_MAP_STRING_REMOVES += 1);
    remove_keyed(impl_ref, &cstr_key_ref(key));
}

// ---------------------------------------------------------------------------
// Bytes-key variants: the content-hashing flavor
// ---------------------------------------------------------------------------
//
// a `Bytes` key is stored the way a string key is: as the map's own copy of
// the content. the handle the caller passes is only read, never retained,
// so the caller keeps whatever count it holds on it, and a map releases
// nothing key-wise when it frees. the value side is the same as every other
// flavor: the map owns one count per stored heap value. `keys()` hands out
// fresh bytes objects the way the string flavor hands out fresh strings,
// owned by the list it builds.

/// Insert a value under a bytes key.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a bytes handle or null.
#[no_mangle]
pub unsafe extern "C" fn pith_map_insert_bkey(map_handle: i64, key: i64, value: i64) {
    insert_bkey_inner(map_handle, key, value, false);
}

/// Insert a value the caller owns under a bytes key — see
/// `pith_map_insert_cstr_owned`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a bytes handle or null.
#[no_mangle]
pub unsafe extern "C" fn pith_map_insert_bkey_owned(map_handle: i64, key: i64, value: i64) {
    insert_bkey_inner(map_handle, key, value, true);
}

/// Insert a borrowed value of a named heap kind under a bytes key — see
/// `pith_map_insert_cstr_kind`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a bytes handle or null.
#[no_mangle]
pub unsafe extern "C" fn pith_map_insert_bkey_kind(
    map_handle: i64,
    key: i64,
    value: i64,
    val_tag: i64,
) {
    let owns = map_adopt_value_tag(map_handle, val_tag);
    if !owns {
        retain_element(element_tag_from_code(val_tag as i32), value);
    }
    insert_bkey_inner(map_handle, key, value, false);
}

/// Insert an owned value of a named heap kind under a bytes key — see
/// `pith_map_insert_cstr_owned_kind`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a bytes handle or null.
#[no_mangle]
pub unsafe extern "C" fn pith_map_insert_bkey_owned_kind(
    map_handle: i64,
    key: i64,
    value: i64,
    val_tag: i64,
) {
    map_adopt_value_tag(map_handle, val_tag);
    insert_bkey_inner(map_handle, key, value, true);
}

/// Get a value by bytes key. Returns 0 if the key is not found.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_bkey(map_handle: i64, key: i64) -> i64 {
    let Some(impl_ref) = map_ref_from_handle(map_handle) else {
        return 0;
    };
    require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_get_bkey");
    bytes_key_ref(key)
        .and_then(|map_key| get_keyed(impl_ref, &map_key))
        .unwrap_or(0)
}

/// Get a value by bytes key, wrapped in an Optional tuple — see
/// `pith_map_get_cstr_opt`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_bkey_opt(map_handle: i64, key: i64) -> i64 {
    let Some(impl_ref) = map_ref_from_handle(map_handle) else {
        return optional_tuple(false, 0);
    };
    require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_get_bkey_opt");
    let found = bytes_key_ref(key).and_then(|map_key| get_keyed(impl_ref, &map_key));
    match found {
        Some(v) => optional_tuple(true, v),
        None => optional_tuple(false, 0),
    }
}

/// Get a value by bytes key, exiting with a diagnostic on a miss — the
/// strict path for `m[k]`, see `pith_map_get_cstr_strict`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_bkey_strict(map_handle: i64, key: i64) -> i64 {
    let Some(impl_ref) = map_ref_from_handle(map_handle) else {
        eprintln!("pith runtime error: map indexing on invalid map handle");
        // panic-guard: strict map indexing on an invalid handle is a program bug with no value to return.
        std::process::exit(1);
    };
    require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_get_bkey_strict");
    let Some(map_key) = bytes_key_ref(key) else {
        strict_miss("<not a bytes value>");
    };
    match get_keyed(impl_ref, &map_key) {
        Some(v) => v,
        None => strict_miss(&map_key.display()),
    }
}

/// Check if a bytes key exists in the map. Returns 1 if present, 0 otherwise.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_contains_bkey(map_handle: i64, key: i64) -> i64 {
    let Some(impl_ref) = map_ref_from_handle(map_handle) else {
        return 0;
    };
    require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_contains_bkey");
    match bytes_key_ref(key) {
        Some(map_key) if impl_ref.contains_key(&map_key) => 1,
        _ => 0,
    }
}

/// Get value by bytes key with a default if not found.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_default_bkey(map_handle: i64, key: i64, default: i64) -> i64 {
    let Some(impl_ref) = map_ref_from_handle(map_handle) else {
        return default;
    };
    require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_get_default_bkey");
    bytes_key_ref(key)
        .and_then(|map_key| get_keyed(impl_ref, &map_key))
        .unwrap_or(default)
}

/// Remove an entry by bytes key.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_remove_bkey(map_handle: i64, key: i64) {
    let Some(impl_ref) = map_mut_from_handle(map_handle) else {
        return;
    };
    require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_remove_bkey");
    if let Some(map_key) = bytes_key_ref(key) {
        remove_keyed(impl_ref, &map_key);
    }
}

/// Return all bytes keys as a bytes-tagged list of fresh bytes objects the
/// list owns — the counterpart to `pith_map_keys_cstr`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_keys_bkey(map_handle: i64) -> i64 {
    use crate::collections::list::{pith_list_new, pith_list_push_value_owned};

    let Some(impl_ref) = map_ref_from_handle(map_handle) else {
        let empty = pith_list_new(8, 0);
        return empty.ptr as i64;
    };
    require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_keys_bkey");
    let list = pith_list_new(8, 5);
    for key in impl_ref.keys() {
        if let MapKey::Bytes(bytes) = key {
            pith_list_push_value_owned(list, crate::bytes::pith_bytes_from_vec(bytes));
        }
    }
    list.ptr as i64
}

// ---------------------------------------------------------------------------
// Integer-key variants for Cranelift codegen (handle-based, like cstr variants)
// ---------------------------------------------------------------------------

/// Insert an i64 value with an integer key (handle-based API).
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_insert_ikey(map_handle: i64, key: i64, value: i64) {
    insert_ikey_inner(map_handle, key, value, false);
}

/// Insert a value the caller owns under an integer key. The map takes the
/// caller's count instead of adding one of its own — see
/// `pith_map_insert_cstr_owned`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_insert_ikey_owned(map_handle: i64, key: i64, value: i64) {
    insert_ikey_inner(map_handle, key, value, true);
}

/// Int-keyed borrowed store of a named heap kind — see
/// `pith_map_insert_cstr_kind`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_insert_ikey_kind(
    map_handle: i64,
    key: i64,
    value: i64,
    val_tag: i64,
) {
    let owns = map_adopt_value_tag(map_handle, val_tag);
    if !owns {
        retain_element(element_tag_from_code(val_tag as i32), value);
    }
    insert_ikey_inner(map_handle, key, value, false);
}

/// Int-keyed owned store of a named heap kind — see
/// `pith_map_insert_cstr_owned_kind`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_insert_ikey_owned_kind(
    map_handle: i64,
    key: i64,
    value: i64,
    val_tag: i64,
) {
    map_adopt_value_tag(map_handle, val_tag);
    insert_ikey_inner(map_handle, key, value, true);
}

unsafe fn insert_ikey_inner(map_handle: i64, key: i64, value: i64, takes_caller_count: bool) {
    let Some(impl_ref) = map_mut_from_handle(map_handle) else {
        return;
    };
    crate::perf_stats!(PERF_MAP_INT_INSERTS += 1);
    if impl_ref.uses_int_values8() {
        crate::perf_count(&crate::PERF_MAP_INT_FAST_INSERTS, 1);
        impl_ref.insert_int_value(key, value);
    } else {
        crate::perf_count(&crate::PERF_MAP_INT_FALLBACK_INSERTS, 1);
        if !takes_caller_count {
            impl_ref.retain_value(value);
        }
        if let Some(old) = impl_ref.insert(&KeyRef::Int(key), MapVal::Word(value)) {
            impl_ref.release_value(&old);
        }
    }
}

/// Get an i64 value by integer key. Returns 0 if the key is not found.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_ikey(map_handle: i64, key: i64) -> i64 {
    let Some(impl_ref) = map_ref_from_handle(map_handle) else {
        return 0;
    };
    crate::perf_stats!(PERF_MAP_INT_GETS += 1);

    if impl_ref.uses_int_values8() {
        crate::perf_count(&crate::PERF_MAP_INT_FAST_GETS, 1);
        impl_ref.get_int_value(key).unwrap_or(0)
    } else {
        crate::perf_count(&crate::PERF_MAP_INT_FALLBACK_GETS, 1);
        get_keyed(impl_ref, &KeyRef::Int(key)).unwrap_or(0)
    }
}

/// Get an i64 value by integer key, wrapped in an Optional tuple. Returns
/// `Some(value)` when the key is present and `None` otherwise.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_ikey_opt(map_handle: i64, key: i64) -> i64 {
    let Some(impl_ref) = map_ref_from_handle(map_handle) else {
        return optional_tuple(false, 0);
    };
    crate::perf_stats!(PERF_MAP_INT_GETS += 1);
    if impl_ref.uses_int_values8() {
        crate::perf_count(&crate::PERF_MAP_INT_FAST_GETS, 1);
        match impl_ref.get_int_value(key) {
            Some(v) => optional_tuple(true, v),
            None => optional_tuple(false, 0),
        }
    } else {
        crate::perf_count(&crate::PERF_MAP_INT_FALLBACK_GETS, 1);
        match get_keyed(impl_ref, &KeyRef::Int(key)) {
            Some(v) => optional_tuple(true, v),
            None => optional_tuple(false, 0),
        }
    }
}

/// Strict integer-key lookup for `map[k]`. Aborts with a structured
/// diagnostic on miss instead of returning a silent zero.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_ikey_strict(map_handle: i64, key: i64) -> i64 {
    let Some(impl_ref) = map_ref_from_handle(map_handle) else {
        eprintln!("pith runtime error: map indexing on invalid map handle");
        // panic-guard: strict map indexing on an invalid handle is a program bug with no value to return.
        std::process::exit(1);
    };
    crate::perf_stats!(PERF_MAP_INT_GETS += 1);
    let found = if impl_ref.uses_int_values8() {
        crate::perf_count(&crate::PERF_MAP_INT_FAST_GETS, 1);
        impl_ref.get_int_value(key)
    } else {
        crate::perf_count(&crate::PERF_MAP_INT_FALLBACK_GETS, 1);
        get_keyed(impl_ref, &KeyRef::Int(key))
    };
    match found {
        Some(v) => v,
        None => {
            eprintln!(
                "pith runtime error: map key not found: {} (use .contains_key first, .get(k) for Optional, or .get_default(k, d) for a fallback)",
                key
            );
            // panic-guard: a missing key under strict indexing is a program bug with no value to return.
            std::process::exit(1);
        }
    }
}

/// Check if an integer key exists in the map. Returns 1 if present, 0 otherwise.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_contains_ikey(map_handle: i64, key: i64) -> i64 {
    let Some(impl_ref) = map_ref_from_handle(map_handle) else {
        return 0;
    };
    crate::perf_stats!(PERF_MAP_INT_CONTAINS += 1);

    let contains = if impl_ref.uses_int_values8() {
        crate::perf_count(&crate::PERF_MAP_INT_FAST_CONTAINS, 1);
        impl_ref.contains_int_key(key)
    } else {
        crate::perf_count(&crate::PERF_MAP_INT_FALLBACK_CONTAINS, 1);
        impl_ref.contains_key(&KeyRef::Int(key))
    };

    if contains {
        1
    } else {
        0
    }
}

/// Remove an entry by integer key (handle-based API).
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_remove_ikey(map_handle: i64, key: i64) {
    let Some(impl_ref) = map_mut_from_handle(map_handle) else {
        return;
    };
    crate::perf_stats!(PERF_MAP_INT_REMOVES += 1);
    if impl_ref.uses_int_values8() {
        crate::perf_count(&crate::PERF_MAP_INT_FAST_REMOVES, 1);
        impl_ref.remove_int_value(key);
    } else {
        crate::perf_count(&crate::PERF_MAP_INT_FALLBACK_REMOVES, 1);
        if let Some(old) = impl_ref.remove(&KeyRef::Int(key)) {
            impl_ref.release_value(&old);
        }
    }
}

/// Render a map as "{k: v, ...}" with sorted keys, for interpolation.
/// Sorting keeps the output deterministic; hashbrown iteration is not.
/// Kind codes match pith_display_list: 0=int, 1=float, 2=bool, 3=string.
#[no_mangle]
pub unsafe extern "C" fn pith_display_map(handle: i64, key_kind: i64, val_kind: i64) -> i64 {
    let mut out = String::from("{");
    if let Some(impl_ref) = map_ref_from_handle(handle) {
        let mut entries: Vec<(String, i64)> = Vec::new();
        if let Some(fast) = &impl_ref.int_values8 {
            for (k, v) in fast {
                entries.push((k.to_string(), *v));
            }
            entries.sort_by(|a, b| {
                a.0.parse::<i64>()
                    .unwrap_or(0)
                    .cmp(&b.0.parse::<i64>().unwrap_or(0))
            });
        } else {
            for (k, val) in &impl_ref.data {
                let key_text = match k {
                    MapKey::Int(n) => n.to_string(),
                    MapKey::String(bytes) => String::from_utf8_lossy(bytes).into_owned(),
                    MapKey::Bytes(_) => k.display(),
                };
                entries.push((key_text, val.word().unwrap_or(0)));
            }
            if key_kind == 0 {
                entries.sort_by_key(|e| e.0.parse::<i64>().unwrap_or(0));
            } else {
                entries.sort();
            }
        }
        for (i, (k, raw)) in entries.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(k);
            out.push_str(": ");
            crate::display_value_for_map(&mut out, *raw, val_kind);
        }
    }
    out.push('}');
    crate::pith_copy_bytes_to_cstring(out.as_bytes()) as i64
}

/// Get map length by handle (accepts raw MapImpl pointer as i64).
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_len_handle(map_handle: i64) -> i64 {
    map_ref_from_handle(map_handle)
        .map(|impl_ref| impl_ref.len() as i64)
        .unwrap_or(0)
}

/// Return all keys as a PithList of C-string pointers (each element is an i64
/// pointer to a newly allocated null-terminated string). The PithList pointer
/// is returned as i64.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_keys_cstr(map_handle: i64) -> i64 {
    use crate::collections::list::{pith_list_new, pith_list_push_value};

    let Some(impl_ref) = map_ref_from_handle(map_handle) else {
        let empty = pith_list_new(8, 0);
        return empty.ptr as i64;
    };
    require_key_flavor(impl_ref.key_type, KeyType::String, "map_keys");
    // string-tagged: the list owns the freshly copied key strings
    let list = pith_list_new(8, 1);

    for key in impl_ref.keys() {
        if let MapKey::String(ref bytes) = key {
            let ptr = crate::pith_copy_bytes_to_cstring(bytes);
            pith_list_push_value(list, ptr as i64);
            crate::pith_cstring_release(ptr as *const i8);
        }
    }

    list.ptr as i64
}

/// Return all int keys as a PithList of i64 values (each element is a raw
/// key). The counterpart to `pith_map_keys_cstr` for integer-keyed maps,
/// whose keys the string variant drops. The PithList pointer is returned as
/// i64.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_keys_ikey(map_handle: i64) -> i64 {
    use crate::collections::list::{pith_list_new, pith_list_push_value};

    let Some(impl_ref) = map_ref_from_handle(map_handle) else {
        let empty = pith_list_new(8, 0);
        return empty.ptr as i64;
    };
    // int-tagged: 8-byte, non-heap values, matching pith_map_values_handle
    let list = pith_list_new(8, 0);

    for key in impl_ref.keys() {
        if let MapKey::Int(k) = key {
            pith_list_push_value(list, k);
        }
    }

    list.ptr as i64
}

/// Clear all entries from map (handle-based API).
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_clear_handle(map_handle: i64) {
    let Some(impl_ref) = map_mut_from_handle(map_handle) else {
        return;
    };
    impl_ref.release_all_values();
    impl_ref.clear();
}

/// Check if map is empty (handle-based API). Returns 1 if empty, 0 otherwise.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_is_empty_handle(map_handle: i64) -> i64 {
    let Some(impl_ref) = map_ref_from_handle(map_handle) else {
        return 1;
    };
    if impl_ref.len() == 0 {
        1
    } else {
        0
    }
}

/// Return all values as a PithList (handle-based API). The PithList pointer
/// is returned as i64.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_values_handle(map_handle: i64) -> i64 {
    use crate::collections::list::{pith_list_new, pith_list_push_value};

    let Some(impl_ref) = map_ref_from_handle(map_handle) else {
        let empty = pith_list_new(8, 0);
        return empty.ptr as i64;
    };
    // a heap-valued map hands out its stored handles, so the list has to
    // carry the map's own value tag: push takes a count of its own and the
    // list's free cascades it back. an untagged list would leave the caller
    // holding raw pointers the map is free to evict and release out from
    // under.
    let list = pith_list_new(8, impl_ref.val_tag as i32);

    for v in impl_ref.value_words() {
        pith_list_push_value(list, v);
    }

    list.ptr as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bogus_map() -> PithMap {
        PithMap {
            ptr: 12345usize as *mut (),
        }
    }

    #[test]
    fn invalid_map_handles_return_safe_defaults() {
        unsafe {
            let mut map = bogus_map();
            assert_eq!(pith_map_len(bogus_map()), 0);
            assert_eq!(pith_map_len_handle(12345), 0);
            assert_eq!(pith_map_is_empty_handle(12345), 1);
            pith_map_clear(&mut map);
            pith_map_clear_handle(12345);
            pith_map_release(bogus_map());
        }
    }

    /// A string-valued int-key map holding one value the caller has already
    /// let go of, so the map's count is the only one left.
    unsafe fn map_owning_one_value(text: &[u8]) -> (i64, *mut i8) {
        let handle = pith_map_new_int_cstr_val().ptr as i64;
        let s = crate::pith_copy_bytes_to_cstring(text);
        pith_map_insert_ikey(handle, 1, s as i64);
        crate::pith_cstring_release(s);
        assert_eq!(crate::cstring_refcount_for_tests(s), Some(1));
        (handle, s)
    }

    #[test]
    fn removing_a_key_drops_the_maps_count_on_its_value() {
        unsafe {
            let (handle, s) = map_owning_one_value(b"evicted");
            pith_map_remove_ikey(handle, 1);
            assert_eq!(crate::cstring_refcount_for_tests(s), None);
            pith_map_release_handle(handle);
        }
    }

    #[test]
    fn clearing_drops_the_maps_count_on_every_value() {
        unsafe {
            let (handle, s) = map_owning_one_value(b"cleared");
            pith_map_clear_handle(handle);
            assert_eq!(crate::cstring_refcount_for_tests(s), None);
            assert_eq!(pith_map_len_handle(handle), 0);
            pith_map_release_handle(handle);
        }
    }

    #[test]
    fn overwriting_retains_the_new_value_before_releasing_the_old() {
        unsafe {
            let (handle, s) = map_owning_one_value(b"old");
            let t = crate::pith_copy_bytes_to_cstring(b"new");
            pith_map_insert_ikey(handle, 1, t as i64);
            assert_eq!(crate::cstring_refcount_for_tests(s), None);
            assert_eq!(crate::cstring_refcount_for_tests(t), Some(2));

            // storing a key over itself must not free the value in between
            pith_map_insert_ikey(handle, 1, t as i64);
            assert_eq!(crate::cstring_refcount_for_tests(t), Some(2));
            assert_eq!(pith_map_get_ikey(handle, 1), t as i64);

            pith_map_release_handle(handle);
            assert_eq!(crate::cstring_refcount_for_tests(t), Some(1));
            crate::pith_cstring_release(t);
        }
    }

    #[test]
    fn a_second_owner_survives_the_maps_eviction() {
        unsafe {
            let handle = pith_map_new_cstr_val().ptr as i64;
            let s = crate::pith_copy_bytes_to_cstring(b"held elsewhere");
            let key = b"k\0".as_ptr() as *const i8;
            pith_map_insert_cstr(handle, key, s as i64);
            // the caller keeps its own count, as a binding of `m[k]` would
            assert_eq!(crate::cstring_refcount_for_tests(s), Some(2));

            pith_map_remove_cstr(handle, key);
            assert_eq!(crate::cstring_refcount_for_tests(s), Some(1));

            pith_map_release_handle(handle);
            crate::pith_cstring_release(s);
            assert_eq!(crate::cstring_refcount_for_tests(s), None);
        }
    }

    #[test]
    fn values_returns_a_list_that_owns_what_it_hands_back() {
        unsafe {
            use crate::collections::list::{pith_list_get_value, pith_list_release_handle};

            let (handle, s) = map_owning_one_value(b"handed out");
            let vals = pith_map_values_handle(handle);
            // the list took its own count, so the map is free to evict
            assert_eq!(crate::cstring_refcount_for_tests(s), Some(2));

            pith_map_clear_handle(handle);
            assert_eq!(crate::cstring_refcount_for_tests(s), Some(1));
            assert_eq!(pith_list_get_value(
                crate::collections::list::PithList { ptr: vals as *mut () },
                0
            ), s as i64);

            pith_list_release_handle(vals);
            assert_eq!(crate::cstring_refcount_for_tests(s), None);
            pith_map_release_handle(handle);
        }
    }

    #[test]
    fn plain_int_maps_never_touch_their_values() {
        unsafe {
            // 7 is not a pointer; a value-releasing map would dereference it
            let handle = pith_map_new_int().ptr as i64;
            pith_map_insert_ikey(handle, 1, 7);
            pith_map_insert_ikey(handle, 1, 8);
            assert_eq!(pith_map_get_ikey(handle, 1), 8);
            pith_map_remove_ikey(handle, 1);
            pith_map_clear_handle(handle);
            pith_map_release_handle(handle);
        }
    }

    /// A one-element list, and a liveness probe for it: a freed list has its
    /// magic scrubbed, so its length reads back as 0 rather than 1.
    unsafe fn one_element_list() -> i64 {
        use crate::collections::list::{pith_list_new_cstr, pith_list_push_value};
        let list = pith_list_new_cstr();
        let s = crate::pith_copy_bytes_to_cstring(b"elem");
        pith_list_push_value(list, s as i64);
        crate::pith_cstring_release(s);
        list.ptr as i64
    }

    unsafe fn list_is_alive(handle: i64) -> bool {
        use crate::collections::list::{pith_list_len, PithList};
        pith_list_len(PithList {
            ptr: handle as *mut (),
        }) == 1
    }

    #[test]
    fn a_map_learns_its_value_kind_from_a_borrowed_store() {
        unsafe {
            use crate::collections::list::pith_list_release_handle;

            // built with no value flavor at all, the shape an empty `{}` in a
            // position the checker could not type produces
            let handle = pith_map_new_default().ptr as i64;
            let list = one_element_list();
            let key = b"k\0".as_ptr() as *const i8;
            pith_map_insert_cstr_kind(handle, key, list, ListTypeTag::List as i64);

            // the map took a count of its own, so the caller letting go is not
            // the last release
            pith_list_release_handle(list);
            assert!(list_is_alive(list));
            assert_eq!(pith_map_get_cstr(handle, key), list);

            pith_map_release_handle(handle);
            assert!(!list_is_alive(list));
        }
    }

    #[test]
    fn an_owned_value_hands_its_count_to_the_map() {
        unsafe {
            let handle = pith_map_new_default().ptr as i64;
            let list = one_element_list();
            let key = b"k\0".as_ptr() as *const i8;
            // the caller stops tracking the value here and never releases it
            pith_map_insert_cstr_owned_kind(handle, key, list, ListTypeTag::List as i64);
            assert!(list_is_alive(list));

            pith_map_release_handle(handle);
            assert!(!list_is_alive(list));
        }
    }

    #[test]
    fn an_int_keyed_map_leaves_the_scalar_fast_path_to_hold_handles() {
        unsafe {
            use crate::collections::list::pith_list_release_handle;

            let handle = pith_map_new_int().ptr as i64;
            assert!(map_ref_from_handle(handle).unwrap().uses_int_values8());
            let list = one_element_list();
            pith_map_insert_ikey_kind(handle, 7, list, ListTypeTag::List as i64);
            assert!(!map_ref_from_handle(handle).unwrap().uses_int_values8());
            assert_eq!(pith_map_get_ikey(handle, 7), list);
            assert_eq!(pith_map_len_handle(handle), 1);

            pith_list_release_handle(list);
            assert!(list_is_alive(list));
            pith_map_release_handle(handle);
            assert!(!list_is_alive(list));
        }
    }

    #[test]
    fn a_map_that_cannot_adopt_leaks_rather_than_frees() {
        unsafe {
            use crate::collections::list::pith_list_release_handle;

            // a value already stored without a kind leaves the map holding no
            // count on it, so adopting a tag now would start releasing counts
            // the map never took. it must refuse, and take the compensating
            // count itself instead.
            let handle = pith_map_new_default().ptr as i64;
            let first = one_element_list();
            pith_map_insert_cstr(handle, b"a\0".as_ptr() as *const i8, first);

            let second = one_element_list();
            pith_map_insert_cstr_kind(
                handle,
                b"b\0".as_ptr() as *const i8,
                second,
                ListTypeTag::List as i64,
            );
            pith_list_release_handle(second);
            assert!(list_is_alive(second));

            pith_map_release_handle(handle);
            // neither value was freed by the map: `first` is still the
            // caller's, `second` is the leak this trades for the dangle
            assert!(list_is_alive(first));
            assert!(list_is_alive(second));
            pith_list_release_handle(first);
            pith_list_release_handle(second);
        }
    }

    #[test]
    fn values_of_a_list_valued_map_come_back_list_tagged() {
        unsafe {
            use crate::collections::list::{pith_list_len, pith_list_release_handle, PithList};

            let handle = pith_map_new_default().ptr as i64;
            let list = one_element_list();
            pith_map_insert_cstr_owned_kind(
                handle,
                b"k\0".as_ptr() as *const i8,
                list,
                ListTypeTag::List as i64,
            );
            let vals = pith_map_values_handle(handle);
            assert_eq!(
                pith_list_len(PithList {
                    ptr: vals as *mut ()
                }),
                1
            );
            // the values list took its own count, so evicting the entry does
            // not free what the caller is now holding
            pith_map_clear_handle(handle);
            assert!(list_is_alive(list));

            pith_list_release_handle(vals);
            assert!(!list_is_alive(list));
            pith_map_release_handle(handle);
        }
    }

    #[test]
    fn released_map_handles_are_rejected() {
        unsafe {
            let map = pith_map_new(0, 8, 0);
            let handle = map.ptr as i64;
            assert_eq!(pith_map_len(map), 0);
            pith_map_release(map);
            assert_eq!(pith_map_len(map), 0);
            assert_eq!(pith_map_len_handle(handle), 0);
            assert_eq!(pith_map_is_empty_handle(handle), 1);
            pith_map_release(map);
        }
    }

    // this test lives here rather than in cycle.rs because building a map and
    // reading its handle needs the private PithMap internals. it serializes
    // on the cycle test lock like every test that turns the flag on.
    #[test]
    #[ignore = "enables the collector flag; run serially via make test-cycle-gc"]
    fn buffered_map_dies_into_the_graveyard() {
        let _guard = match crate::cycle::CYCLE_TEST_LOCK.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        crate::cycle::force_enabled_for_tests(true);
        crate::cycle::reset_for_tests();
        unsafe {
            // a struct value observed through a weak reference proves the
            // map's death still cascades into its values.
            let value = crate::runtime_core::pith_struct_alloc(1);
            crate::runtime_core::pith_struct_weak_retain(value);

            let map = pith_map_new_tagged(0, 8, 4); // int keys, struct values
            let handle = map.ptr as i64;
            pith_map_insert_ikey(handle, 1, value); // map retains
            crate::runtime_core::pith_struct_release(value); // map holds the only count
            assert_eq!(crate::runtime_core::pith_struct_weak_load(value), value);

            pith_map_retain_handle(handle);
            pith_map_release(map); // 2 -> 1: buffered
            assert_eq!(crate::cycle::suspect_count_for_tests(handle as usize), 1);
            pith_map_release(map); // 1 -> 0: dies buffered

            assert_eq!(crate::cycle::graveyard_count_for_tests(handle as usize), 1);
            assert_eq!(
                crate::runtime_core::pith_struct_weak_load(value),
                0,
                "value was released"
            );
            assert_eq!(
                pith_map_len(map),
                0,
                "magic scrubbed: handle no longer validates"
            );
            pith_map_retain_handle(handle); // must be a no-op, not a revival

            crate::runtime_core::pith_struct_weak_release(value);
            crate::cycle::force_enabled_for_tests(false);
            crate::cycle::reset_for_tests();
        }
    }

    /// A bytes-keyed map finds an entry by the key's content: a key built
    /// from a string and one built from a byte vector are one entry, two keys
    /// that share a prefix and a length are two, the empty key is an entry
    /// like any other, and the map's own copy of each key means the caller's
    /// count on the handle is untouched. The value side keeps the string
    /// contract: one count per stored value, dropped on overwrite, remove and
    /// free, transferred by take.
    #[test]
    fn bytes_keyed_map_is_keyed_by_content() {
        unsafe {
            let map = pith_map_new_bytes_cstr_val().ptr as i64;
            let from_vec = crate::bytes::pith_bytes_from_vec(b"key".to_vec());
            let from_str = crate::bytes::pith_bytes_from_string_utf8(c"key".as_ptr());
            let sibling = crate::bytes::pith_bytes_from_vec(b"kex".to_vec());
            let empty = crate::bytes::pith_bytes_from_vec(Vec::new());

            let v1 = crate::pith_copy_bytes_to_cstring(b"one");
            let v2 = crate::pith_copy_bytes_to_cstring(b"two");
            let v3 = crate::pith_copy_bytes_to_cstring(b"three");
            pith_map_insert_bkey(map, from_vec, v1 as i64);
            pith_map_insert_bkey(map, sibling, v2 as i64);
            pith_map_insert_bkey(map, empty, v3 as i64);
            assert_eq!(pith_map_len_handle(map), 3);
            assert_eq!(
                pith_map_get_bkey(map, from_str) as *mut i8,
                v1,
                "found by content"
            );
            assert_eq!(
                pith_map_get_bkey(map, sibling) as *mut i8,
                v2,
                "shared prefix, distinct"
            );
            assert_eq!(
                pith_map_get_bkey(map, 0) as *mut i8,
                v3,
                "null is the empty key"
            );
            assert_eq!(pith_map_contains_bkey(map, from_str), 1);
            assert_eq!(pith_map_get_default_bkey(map, from_str, 7) as *mut i8, v1);
            let absent = crate::bytes::pith_bytes_from_vec(b"ke".to_vec());
            assert_eq!(
                pith_map_contains_bkey(map, absent),
                0,
                "a prefix is not a key"
            );
            assert_eq!(pith_map_get_default_bkey(map, absent, 7), 7);

            // the map holds one count per value; overwriting through the other
            // spelling of the same key releases the displaced value
            assert_eq!(crate::cstring_refcount_for_tests(v1), Some(2));
            let v4 = crate::pith_copy_bytes_to_cstring(b"four");
            pith_map_insert_bkey_owned(map, from_str, v4 as i64);
            assert_eq!(pith_map_len_handle(map), 3, "same key, one entry");
            assert_eq!(crate::cstring_refcount_for_tests(v1), Some(1), "displaced");
            assert_eq!(
                crate::cstring_refcount_for_tests(v4),
                Some(1),
                "owned: transferred"
            );

            // keys() hands out fresh bytes objects the list owns
            let list = pith_map_keys_bkey(map);
            let list_handle = crate::collections::list::PithList {
                ptr: list as *mut (),
            };
            assert_eq!(crate::collections::list::pith_list_len(list_handle), 3);
            let first = crate::collections::list::pith_list_get_value(list_handle, 0);
            assert!(crate::bytes::pith_bytes_ref(first).is_some());
            assert!(first != from_vec && first != sibling && first != empty);
            crate::collections::list::pith_list_release_handle(list);

            // take transfers the value's count; remove releases it
            let taken = pith_map_take_bkey(map, sibling) as *mut i8;
            assert_eq!(taken, v2);
            assert_eq!(
                crate::cstring_refcount_for_tests(v2),
                Some(2),
                "count came with it"
            );
            crate::pith_cstring_release(v2);
            pith_map_remove_bkey(map, from_vec);
            assert_eq!(pith_map_contains_bkey(map, from_str), 0);
            assert_eq!(pith_map_len_handle(map), 1);

            for h in [from_vec, from_str, sibling, empty, absent] {
                let b = crate::bytes::pith_bytes_ref(h).expect("the map never took a count");
                assert_eq!(b.rc.load(std::sync::atomic::Ordering::Relaxed), 1);
                crate::bytes::pith_bytes_release(h);
            }
            for v in [v1, v2, v3] {
                crate::pith_cstring_release(v);
            }
            pith_map_release_handle(map);
        }
    }

    // --- the borrowed-key contract -----------------------------------------

    /// The inputs the hash-equality proof runs over. A key that is empty, one
    /// byte, or a single ascii word is the ordinary case; the rest are the ones
    /// a naive "hash the bytes" would get wrong. A zero byte cannot reach a
    /// c-string key, but it can reach a bytes key, and the two `KeyRef` arms
    /// share the impl, so it is covered here where it can be.
    fn hash_probe_inputs() -> Vec<Vec<u8>> {
        vec![
            Vec::new(),
            b"a".to_vec(),
            b"ab".to_vec(),
            b"region".to_vec(),
            b"user-1000-region".to_vec(),
            // a prefix and the same bytes with more after them: the length has
            // to enter the hash, or these two collide.
            b"abc".to_vec(),
            // embedded zero bytes (bytes keys can hold them)
            vec![0u8],
            vec![b'a', 0, b'b'],
            vec![0, 0, 0, 0],
            // multi-byte utf-8
            "\u{e9}".as_bytes().to_vec(),
            "\u{65e5}\u{672c}\u{8a9e}".as_bytes().to_vec(),
            "\u{1f600}".as_bytes().to_vec(),
            // long: past every small-buffer boundary in sight
            vec![b'x'; 255],
            vec![b'y'; 4096],
            // the same width as an int key's payload, in case a tag were ever
            // dropped
            vec![0u8; 8],
        ]
    }

    fn hash_with(state: &std::collections::hash_map::RandomState, value: &impl Hash) -> u64 {
        use std::hash::BuildHasher;
        let mut hasher = state.build_hasher();
        value.hash(&mut hasher);
        hasher.finish()
    }

    /// The one that would fail silently.
    ///
    /// `KeyRef` is what every probe hashes and `MapKey` is what the table
    /// stores, so if the two ever wrote different bytes for the same content
    /// the table would stop finding keys it holds: no crash, no diagnostic,
    /// just every string-keyed map in every program missing entries. This
    /// asserts the two agree under the map's own hasher, for every shape in
    /// `hash_probe_inputs`, and that the tag keeps the three flavors apart.
    #[test]
    fn key_ref_hashes_like_map_key() {
        let state = std::collections::hash_map::RandomState::new();
        for bytes in hash_probe_inputs() {
            assert_eq!(
                hash_with(&state, &KeyRef::Str(&bytes)),
                hash_with(&state, &MapKey::String(bytes.clone())),
                "string flavor, {} bytes",
                bytes.len()
            );
            assert_eq!(
                hash_with(&state, &KeyRef::Bytes(&bytes)),
                hash_with(&state, &MapKey::Bytes(bytes.clone())),
                "bytes flavor, {} bytes",
                bytes.len()
            );
            // and the tag has to keep the flavors apart, or a Map[Bytes, V]
            // and a Map[String, V] would share entries
            assert_ne!(
                hash_with(&state, &KeyRef::Str(&bytes)),
                hash_with(&state, &KeyRef::Bytes(&bytes)),
                "flavors must not collide, {} bytes",
                bytes.len()
            );
            assert!(KeyRef::Str(&bytes).equivalent(&MapKey::String(bytes.clone())));
            assert!(KeyRef::Bytes(&bytes).equivalent(&MapKey::Bytes(bytes.clone())));
            assert!(!KeyRef::Str(&bytes).equivalent(&MapKey::Bytes(bytes.clone())));
            assert!(!KeyRef::Bytes(&bytes).equivalent(&MapKey::String(bytes.clone())));
            assert_eq!(
                MapKey::from(&KeyRef::Str(&bytes)),
                MapKey::String(bytes.clone())
            );
            assert_eq!(
                MapKey::from(&KeyRef::Bytes(&bytes)),
                MapKey::Bytes(bytes.clone())
            );
        }
        // a prefix must not hash like the longer key it is a prefix of
        assert_ne!(
            hash_with(&state, &KeyRef::Str(b"ab")),
            hash_with(&state, &KeyRef::Str(b"abc"))
        );
        for n in [i64::MIN, -1, 0, 1, 255, i64::MAX] {
            assert_eq!(
                hash_with(&state, &KeyRef::Int(n)),
                hash_with(&state, &MapKey::Int(n)),
                "int flavor, {n}"
            );
            assert!(KeyRef::Int(n).equivalent(&MapKey::Int(n)));
        }
    }

    /// The same claim through the table rather than the hasher: a key stored
    /// as `MapKey` is found by the `KeyRef` for the same content.
    #[test]
    fn a_stored_key_is_found_by_its_borrowed_form() {
        for bytes in hash_probe_inputs() {
            let mut table: HashMap<MapKey, Vec<u8>> = HashMap::new();
            table.insert(MapKey::String(bytes.clone()), vec![1]);
            table.insert(MapKey::Bytes(bytes.clone()), vec![2]);
            assert_eq!(table.get(&KeyRef::Str(&bytes)), Some(&vec![1]));
            assert_eq!(table.get(&KeyRef::Bytes(&bytes)), Some(&vec![2]));
        }
    }

    /// A c-string key one byte longer than the one already stored must not
    /// find it. The borrowed probe carries its own length, and this is the
    /// shape a length mistake in `cstr_key_ref` would show up as.
    #[test]
    fn a_borrowed_cstr_key_carries_its_length() {
        unsafe {
            let map = pith_map_new_default().ptr as i64;
            let ab = b"ab\0".as_ptr() as *const i8;
            let abc = b"abc\0".as_ptr() as *const i8;
            let empty = b"\0".as_ptr() as *const i8;
            pith_map_insert_cstr(map, ab, 1);
            assert_eq!(pith_map_contains_cstr(map, ab), 1);
            assert_eq!(pith_map_contains_cstr(map, abc), 0);
            assert_eq!(pith_map_contains_cstr(map, empty), 0);
            pith_map_insert_cstr(map, empty, 7);
            assert_eq!(pith_map_get_cstr(map, empty), 7);
            assert_eq!(pith_map_len_handle(map), 2);
            pith_map_release_handle(map);
        }
    }

    // --- allocation counts -------------------------------------------------
    //
    // what the borrowed probes and the inline values buy, stated as the only
    // number that can show it: an operation on a key the map already holds
    // allocates nothing at all, and a new entry allocates once, for the key.

    use crate::collections::alloc_probe::allocations_during;

    #[test]
    fn string_key_hits_allocate_nothing_for_the_key() {
        unsafe {
            let map = pith_map_new_default().ptr as i64;
            let key = b"user-1000-region\0".as_ptr() as *const i8;
            let absent = b"user-1001-region\0".as_ptr() as *const i8;
            pith_map_insert_cstr(map, key, 1);

            assert_eq!(allocations_during(|| { pith_map_get_cstr(map, key); }), 0);
            assert_eq!(
                allocations_during(|| {
                    pith_map_get_cstr_strict(map, key);
                }),
                0
            );
            // `.get(k)` returns an Optional tuple, and that tuple is a heap
            // allocation by construction. It is the caller's value, not a copy
            // of the key, so the claim here is that it is the ONLY allocation:
            // one warm-up call to get the struct pool past its own setup, then
            // exactly one per call, hit or miss.
            pith_map_get_cstr_opt(map, key);
            assert_eq!(
                allocations_during(|| {
                    pith_map_get_cstr_opt(map, key);
                }),
                1
            );
            assert_eq!(
                allocations_during(|| {
                    pith_map_get_cstr_opt(map, absent);
                }),
                1
            );
            assert_eq!(
                allocations_during(|| {
                    pith_map_get_default_cstr(map, key, 0);
                }),
                0
            );
            assert_eq!(
                allocations_during(|| {
                    pith_map_contains_cstr(map, key);
                }),
                0
            );
            // a miss probes and allocates nothing either
            assert_eq!(
                allocations_during(|| {
                    pith_map_contains_cstr(map, absent);
                }),
                0
            );
            assert_eq!(
                allocations_during(|| {
                    pith_map_get_cstr(map, absent);
                }),
                0
            );
            // an overwrite of a key the map holds: nothing for the key, which
            // the map already has, and nothing for the value, which goes into
            // the table itself
            assert_eq!(
                allocations_during(|| {
                    pith_map_insert_cstr(map, key, 2);
                }),
                0
            );
            assert_eq!(pith_map_get_cstr(map, key), 2);
            // a remove of a key the map holds frees, and allocates nothing
            assert_eq!(
                allocations_during(|| {
                    pith_map_remove_cstr(map, key);
                }),
                0
            );
            assert_eq!(pith_map_len_handle(map), 0);

            pith_map_release_handle(map);
        }
    }

    #[test]
    fn a_new_string_key_allocates_the_key_once() {
        unsafe {
            let map = pith_map_new_default().ptr as i64;
            // insert one first, so the table is past its initial allocation
            // and the count below is the entry's own cost
            pith_map_insert_cstr(map, b"seed\0".as_ptr() as *const i8, 0);
            let fresh = b"a-key-the-map-has-never-seen\0".as_ptr() as *const i8;
            // the map's own copy of the key, and nothing else
            assert_eq!(
                allocations_during(|| {
                    pith_map_insert_cstr(map, fresh, 5);
                }),
                1
            );
            assert_eq!(pith_map_get_cstr(map, fresh), 5);
            pith_map_release_handle(map);
        }
    }

    #[test]
    fn bytes_key_hits_allocate_nothing_for_the_key() {
        unsafe {
            let map = pith_map_new_bytes().ptr as i64;
            let key = crate::bytes::pith_bytes_from_vec(b"user-1000-region".to_vec());
            let absent = crate::bytes::pith_bytes_from_vec(b"user-1001-region".to_vec());
            pith_map_insert_bkey(map, key, 1);

            assert_eq!(allocations_during(|| { pith_map_get_bkey(map, key); }), 0);
            assert_eq!(
                allocations_during(|| {
                    pith_map_get_bkey_strict(map, key);
                }),
                0
            );
            // as in the string test: the Optional tuple is the one allocation,
            // and it is the returned value rather than a copy of the key
            pith_map_get_bkey_opt(map, key);
            assert_eq!(
                allocations_during(|| {
                    pith_map_get_bkey_opt(map, key);
                }),
                1
            );
            assert_eq!(
                allocations_during(|| {
                    pith_map_get_bkey_opt(map, absent);
                }),
                1
            );
            assert_eq!(
                allocations_during(|| {
                    pith_map_get_default_bkey(map, key, 0);
                }),
                0
            );
            assert_eq!(
                allocations_during(|| {
                    pith_map_contains_bkey(map, key);
                }),
                0
            );
            assert_eq!(
                allocations_during(|| {
                    pith_map_contains_bkey(map, absent);
                }),
                0
            );
            assert_eq!(
                allocations_during(|| {
                    pith_map_insert_bkey(map, key, 2);
                }),
                0
            );
            assert_eq!(pith_map_get_bkey(map, key), 2);
            // a key the map has never seen: one allocation, the map's own
            // copy of the content, and nothing for the value
            assert_eq!(
                allocations_during(|| {
                    pith_map_insert_bkey(map, absent, 9);
                }),
                1
            );
            assert_eq!(pith_map_get_bkey(map, absent), 9);
            assert_eq!(
                allocations_during(|| {
                    pith_map_remove_bkey(map, key);
                    pith_map_remove_bkey(map, absent);
                }),
                0
            );
            assert_eq!(pith_map_len_handle(map), 0);

            crate::bytes::pith_bytes_release(key);
            crate::bytes::pith_bytes_release(absent);
            pith_map_release_handle(map);
        }
    }

    /// An int-keyed map that cannot use the scalar fast path (this one owns
    /// heap values) goes through the shared table, so it gets the same
    /// treatment.
    #[test]
    fn int_key_hits_allocate_nothing_for_the_key() {
        unsafe {
            let map = pith_map_new_int_cstr_val().ptr as i64;
            let s = crate::pith_copy_bytes_to_cstring(b"value");
            pith_map_insert_ikey(map, 7, s as i64);

            assert_eq!(allocations_during(|| { pith_map_get_ikey(map, 7); }), 0);
            assert_eq!(
                allocations_during(|| {
                    pith_map_contains_ikey(map, 7);
                }),
                0
            );
            assert_eq!(
                allocations_during(|| {
                    pith_map_contains_ikey(map, 8);
                }),
                0
            );
            // nothing for the key, nothing for the value
            assert_eq!(
                allocations_during(|| {
                    pith_map_insert_ikey(map, 7, s as i64);
                }),
                0
            );
            // and an int key the map has never seen allocates nothing at all:
            // there is no key content to copy and the value goes inline
            assert_eq!(
                allocations_during(|| {
                    pith_map_insert_ikey(map, 8, s as i64);
                }),
                0
            );
            assert_eq!(
                allocations_during(|| {
                    pith_map_remove_ikey(map, 7);
                    pith_map_remove_ikey(map, 8);
                }),
                0
            );

            crate::pith_cstring_release(s);
            pith_map_release_handle(map);
        }
    }

    // --- the fused update ---------------------------------------------------

    /// The fused update has to answer exactly what the read-then-store pair it
    /// replaces answers, for every key flavor and whatever the values are
    /// doing: negative, zero, and wrapping past the end of the range the way
    /// the `add` instruction does.
    #[test]
    fn a_fused_update_matches_the_pair_it_replaces() {
        unsafe {
            let deltas = [1i64, 0, -1, -40, i64::MAX, i64::MIN];
            let starts = [0i64, 7, -9, i64::MAX, i64::MIN];
            let key = b"user-1000-region\0".as_ptr() as *const i8;
            let bkey = crate::bytes::pith_bytes_from_vec(b"user-1000-region".to_vec());
            for start in starts {
                for delta in deltas {
                    let expected = start.wrapping_add(delta);

                    let smap = pith_map_new_default().ptr as i64;
                    pith_map_insert_cstr(smap, key, start);
                    assert_eq!(pith_map_upsert_add_cstr(smap, key, delta), expected);
                    assert_eq!(pith_map_get_cstr_strict(smap, key), expected);
                    assert_eq!(pith_map_len_handle(smap), 1);
                    pith_map_release_handle(smap);

                    let imap = pith_map_new_int().ptr as i64;
                    pith_map_insert_ikey(imap, 7, start);
                    assert_eq!(pith_map_upsert_add_ikey(imap, 7, delta), expected);
                    assert_eq!(pith_map_get_ikey_strict(imap, 7), expected);
                    assert_eq!(pith_map_len_handle(imap), 1);
                    pith_map_release_handle(imap);

                    let bmap = pith_map_new_bytes().ptr as i64;
                    pith_map_insert_bkey(bmap, bkey, start);
                    assert_eq!(pith_map_upsert_add_bkey(bmap, bkey, delta), expected);
                    assert_eq!(pith_map_get_bkey_strict(bmap, bkey), expected);
                    assert_eq!(pith_map_len_handle(bmap), 1);
                    pith_map_release_handle(bmap);
                }
            }
            crate::bytes::pith_bytes_release(bkey);
        }
    }

    /// The point of the entry point: an update of a key the map already holds
    /// allocates nothing, the same claim the separate read and store each make
    /// on their own.
    #[test]
    fn a_fused_update_allocates_nothing() {
        unsafe {
            let key = b"user-1000-region\0".as_ptr() as *const i8;
            let map = pith_map_new_default().ptr as i64;
            pith_map_insert_cstr(map, key, 1);
            assert_eq!(
                allocations_during(|| {
                    pith_map_upsert_add_cstr(map, key, 1);
                }),
                0
            );
            pith_map_release_handle(map);

            let imap = pith_map_new_int().ptr as i64;
            pith_map_insert_ikey(imap, 7, 1);
            assert_eq!(
                allocations_during(|| {
                    pith_map_upsert_add_ikey(imap, 7, 1);
                }),
                0
            );
            pith_map_release_handle(imap);

            let bkey = crate::bytes::pith_bytes_from_vec(b"user-1000-region".to_vec());
            let bmap = pith_map_new_bytes().ptr as i64;
            pith_map_insert_bkey(bmap, bkey, 1);
            assert_eq!(
                allocations_during(|| {
                    pith_map_upsert_add_bkey(bmap, bkey, 1);
                }),
                0
            );
            pith_map_release_handle(bmap);
            crate::bytes::pith_bytes_release(bkey);
        }
    }

    // --- what the collector and the wide writer see -------------------------

    /// The collector reaches a map's values only through `cycle_map_children`,
    /// so a ring through a map's value side is collectable exactly when every
    /// value the map owns a count on is reported there once. A value missing
    /// from the report leaks its ring and nothing says so; a value reported
    /// twice has its count dropped further than the map ever raised it.
    #[test]
    fn the_collector_sees_every_value_the_map_owns_exactly_once() {
        unsafe {
            // list values, because strings and primitives are not graph nodes
            // and the collector is told nothing about them (cycle_child_code)
            let map = pith_map_new_default().ptr as i64;
            let mut stored: Vec<i64> = Vec::new();
            for i in 0..8 {
                let key = format!("key-{i}\0");
                let value = one_element_list();
                pith_map_insert_cstr_owned_kind(
                    map,
                    key.as_ptr() as *const i8,
                    value,
                    ListTypeTag::List as i64,
                );
                stored.push(value);
            }

            let mut seen: Vec<(i64, u8)> = Vec::new();
            cycle_map_children(map, &mut |child, code| seen.push((child, code)));

            assert_eq!(seen.len(), stored.len());
            for value in &stored {
                assert_eq!(seen.iter().filter(|(child, _)| child == value).count(), 1);
            }
            let code = crate::collections::list::cycle_child_code(ListTypeTag::List).unwrap();
            assert!(seen.iter().all(|(_, reported)| *reported == code));

            pith_map_release_handle(map);
            for value in &stored {
                assert!(!list_is_alive(*value));
            }
        }
    }

    /// `pith_map_insert_int` is the one writer that can hand a map a value
    /// that is not a word, and a map constructed for one is the only way to
    /// reach the boxed arm. Nothing the emitter produces gets here: every map
    /// constructor above passes a value size of 8, and the emitter calls only
    /// those. The arm is kept for the raw FFI, so it is tested through it.
    #[test]
    fn a_map_of_wide_values_reads_its_first_word_back() {
        unsafe {
            let mut map = pith_map_new(0, 16, 0);
            let handle = map.ptr as i64;
            let wide: [u8; 16] = [
                0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x08, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
                0x01, 0x02,
            ];
            pith_map_insert_int(&mut map, 42, wide.as_ptr(), 16);
            assert_eq!(pith_map_len(map), 1);
            assert_eq!(
                pith_map_get_ikey(handle, 42),
                i64::from_le_bytes(wide[..8].try_into().unwrap())
            );
            // a value of the wrong size is still refused rather than stored
            pith_map_insert_int(&mut map, 43, wide.as_ptr(), 8);
            assert_eq!(pith_map_len(map), 1);
            pith_map_release(map);
        }
    }

    /// A value too short to carry a word reads back as absent, the way a
    /// short box did. Only the raw FFI can build such a map.
    #[test]
    fn a_value_shorter_than_a_word_carries_none() {
        unsafe {
            let mut map = pith_map_new(0, 4, 0);
            let handle = map.ptr as i64;
            let narrow: [u8; 4] = [0xde, 0xad, 0xbe, 0xef];
            pith_map_insert_int(&mut map, 7, narrow.as_ptr(), 4);
            assert_eq!(pith_map_len(map), 1);
            assert_eq!(pith_map_get_ikey(handle, 7), 0);
            // the Optional says absent rather than "present, zero"
            let opt = pith_map_get_ikey_opt(handle, 7) as *const i64;
            assert_eq!(*opt, 0);
            pith_map_release(map);
        }
    }
}
