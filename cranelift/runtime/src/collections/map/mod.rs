//! Map[K,V]: a hash-indexed, reference-counted key-value collection.
//!
//! The table is a `hashbrown::HashMap`, used directly rather than through
//! `std` so lookups can probe with a borrowed key (`KeyRef`) and never build
//! an owned one.

pub use crate::collections::key::{KeyFlavor as KeyType, KeyRef, OwnedKey as MapKey};
use crate::collections::key::require_flavor;
use crate::collections::list::{element_tag_from_code, retain_element, ListTypeTag};
use crate::handle_registry;
use hashbrown::hash_map::EntryRef;
use hashbrown::HashMap;

mod fused;
mod lifecycle;
mod ops;
mod views;
#[cfg(test)]
mod tests;

pub use fused::*;
pub use lifecycle::*;
pub use ops::*;
pub use views::*;

#[inline(always)]
pub(super) fn require_key_flavor(actual: KeyType, wanted: KeyType, entry: &str) {
    require_flavor("map", "keys", actual, wanted, entry);
}

/// FFI-compatible map handle
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PithMap {
    /// Pointer to internal map implementation
    pub(super) ptr: *mut (),
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
pub(super) enum MapVal {
    Word(i64),
    Wide(Box<[u8]>),
}

impl MapVal {
    /// The stored form of a value handed over as raw bytes. The one place a
    /// value's representation is chosen.
    pub(super) fn from_bytes(bytes: &[u8]) -> MapVal {
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
    pub(super) fn word(&self) -> Option<i64> {
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
/// `#[repr(C)]` pins `magic` at offset 0 for raw-pointer validation.
#[repr(C)]
pub struct MapImpl {
    /// Magic word for the fast validity check (see map_magic_ok)
    pub(super) magic: u32,
    /// Shared-handle refcount (see ListImpl.rc)
    pub(super) rc: std::sync::atomic::AtomicU32,
    /// The actual hash map storing key -> value mappings
    pub(super) data: HashMap<MapKey, MapVal>,
    /// Specialized storage for int-key maps with 8-byte scalar values
    pub(super) int_values8: Option<HashMap<i64, i64>>,
    /// Type tag for keys (0=int, 1=string, 2=bytes)
    pub(super) key_type: KeyType,
    /// Size of values in bytes
    pub(super) val_size: usize,
    /// Which heap kind the map's values are, or `Primitive` when it owns no
    /// value counts. A map holds exactly one count per stored heap value, the
    /// same contract a tagged list has for its elements.
    pub(super) val_tag: ListTypeTag,
    /// Bit 0: this map sits in the cycle collector's suspect buffer.
    /// Bit 7: shared (`share::CONTAINER_SHARED_FLAG`).
    pub(super) cycle_flags: std::sync::atomic::AtomicU8,
}

/// Key type enumeration

impl MapImpl {
    pub(super) fn new(key_type: KeyType, val_size: usize, val_tag: ListTypeTag) -> Self {
        crate::alloc_sites::record("map");
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

    pub(super) fn val_is_heap(&self) -> bool {
        self.val_tag != ListTypeTag::Primitive
    }

    /// Learn the value kind from a store, when the emitter knows it and the
    /// map does not. Returns true when the map owns a count on values of that
    /// kind once this returns.
    ///
    /// The constructor cannot always pick the flavor (an empty `{}` in a
    /// position the checker could not type builds a plain map), so the store
    /// is the second chance, and the only place the value's kind is known for
    /// certain. Adopting is count-neutral because it is gated on the map being
    /// empty: there are no already-stored values whose counts the new tag
    /// would start releasing. A non-empty untagged map keeps its tag and the
    /// caller is told so, which is what keeps the fallback a leak rather than
    /// a freed value.
    pub(super) fn adopt_value_tag(&mut self, tag: ListTypeTag) -> bool {
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

    pub(super) fn len(&self) -> usize {
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
    pub(super) fn insert(&mut self, key: &KeyRef<'_>, value: MapVal) -> Option<MapVal> {
        // the write barrier of a store: a value stored into a map other tasks
        // can reach becomes reachable from them too (see `share.rs`)
        if self.cycle_flags.load(std::sync::atomic::Ordering::Relaxed) & crate::share::CONTAINER_SHARED_FLAG != 0
            && !matches!(self.val_tag, ListTypeTag::Primitive)
        {
            if let Some(word) = value.word() {
                unsafe { crate::share::share_value(word, self.val_tag) };
            }
        }
        match self.data.entry_ref(key) {
            EntryRef::Occupied(mut entry) => Some(entry.insert(value)),
            EntryRef::Vacant(entry) => {
                entry.insert(value);
                None
            }
        }
    }

    pub(super) fn get(&self, key: &KeyRef<'_>) -> Option<&MapVal> {
        self.data.get(key)
    }


    pub(super) fn remove(&mut self, key: &KeyRef<'_>) -> Option<MapVal> {
        self.data.remove(key)
    }

    pub(super) fn contains_key(&self, key: &KeyRef<'_>) -> bool {
        self.data.contains_key(key)
    }

    pub(super) fn clear(&mut self) {
        if let Some(data) = &mut self.int_values8 {
            data.clear();
        } else {
            self.data.clear();
        }
    }

    /// the stored keys, borrowed. a map with raw int values stores no `MapKey`,
    /// so this is for the string and bytes flavors.
    fn stored_keys(&self) -> impl Iterator<Item = &MapKey> {
        self.data.keys()
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

    pub(super) fn uses_int_values8(&self) -> bool {
        self.int_values8.is_some()
    }

    pub(super) fn insert_int_value(&mut self, key: i64, value: i64) -> Option<i64> {
        match &mut self.int_values8 {
            Some(data) => data.insert(key, value),
            None => None,
        }
    }

    pub(super) fn get_int_value(&self, key: i64) -> Option<i64> {
        match &self.int_values8 {
            Some(data) => data.get(&key).copied(),
            None => None,
        }
    }

    pub(super) fn contains_int_key(&self, key: i64) -> bool {
        match &self.int_values8 {
            Some(data) => data.contains_key(&key),
            None => false,
        }
    }

    pub(super) fn remove_int_value(&mut self, key: i64) -> Option<i64> {
        match &mut self.int_values8 {
            Some(data) => data.remove(&key),
            None => None,
        }
    }

    /// Drop the map's count on a value that has just left the map: removed,
    /// overwritten, or cleared. A heap-valued map holds exactly one count per
    /// stored value, so anyone still reading it after this point must be
    /// holding a count of their own (the emitter retains borrowed values into
    /// Extract `(val_tag, raw_handle)` for a displaced/removed value if this
    /// map owns heap values, so callers can drop `&mut MapImpl` before running
    /// any destructor.
    #[must_use]
    #[inline(always)]
    pub(super) fn displaced_value_handle(&self, val: MapVal) -> Option<(ListTypeTag, i64)> {
        if !self.val_is_heap() {
            return None;
        }
        val.word().map(|raw| (self.val_tag, raw))
    }

    /// Take the map's count on a value being stored.
    ///
    /// # Safety
    /// `raw` must be a handle of the map's value kind.
    unsafe fn retain_value(&self, raw: i64) {
        if !self.val_is_heap() {
            return;
        }
        // SAFETY: Caller passes a handle matching `self.val_tag`.
        unsafe { retain_element(self.val_tag, raw) };
    }
}

/// Magic word for MapImpl ("PMAP"). Distinct per collection kind so a list
/// handle passed where a map is expected still fails validation.
pub(super) const MAP_MAGIC: u32 = 0x504d4150;

/// Fast validity check: one memory read instead of a global registry lock
/// per access. Freed maps get their magic scrubbed in pith_map_free.
#[inline]
pub(super) unsafe fn map_magic_ok(ptr: *const ()) -> bool {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { map_magic_ok_for(ptr, crate::quarantine::Op::Use) }
}

/// `map_magic_ok`, naming what the caller does with the map in a quarantine
/// report.
#[inline(always)]
pub(super) unsafe fn map_magic_ok_for(ptr: *const (), op: crate::quarantine::Op) -> bool {
    handle_registry::plausibly_aligned::<MapImpl>(ptr)
        && unsafe {
            // SAFETY: Alignment verified above; `magic_is` reads the magic field to validate.
            crate::quarantine::magic_is(
                std::ptr::addr_of!((*(ptr as *const MapImpl)).magic),
                MAP_MAGIC,
                op,
            )
        }
}

/// Validate `map` and borrow its `MapImpl` immutably for the duration of `f`.
#[inline(always)]
pub(super) unsafe fn with_map_ref<R>(map: PithMap, f: impl FnOnce(&MapImpl) -> R) -> Option<R> {
    if !unsafe { map_magic_ok(map.ptr as *const ()) } {
        return None;
    }
    // SAFETY: Magic check validated pointer; borrow is scoped to `f`.
    let impl_ref = unsafe { &*(map.ptr as *const MapImpl) };
    Some(f(impl_ref))
}

/// Validate `map` and borrow its `MapImpl` mutably for the duration of `f`.
#[inline(always)]
pub(super) unsafe fn with_map_mut<R>(map: PithMap, f: impl FnOnce(&mut MapImpl) -> R) -> Option<R> {
    if !unsafe { map_magic_ok(map.ptr as *const ()) } {
        return None;
    }
    // SAFETY: Magic check validated pointer; caller ensures exclusive access for `f`.
    let impl_mut = unsafe { &mut *(map.ptr as *mut MapImpl) };
    Some(f(impl_mut))
}

/// Validate `handle` and borrow its `MapImpl` immutably for the duration of `f`.
#[inline(always)]
pub(crate) unsafe fn with_map_ref_handle<R>(
    handle: i64,
    f: impl FnOnce(&MapImpl) -> R,
) -> Option<R> {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { with_map_ref(PithMap { ptr: handle as *mut () }, f) }
}

/// Validate `handle` and borrow its `MapImpl` mutably for the duration of `f`.
#[inline(always)]
pub(crate) unsafe fn with_map_mut_handle<R>(
    handle: i64,
    f: impl FnOnce(&mut MapImpl) -> R,
) -> Option<R> {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { with_map_mut(PithMap { ptr: handle as *mut () }, f) }
}

/// Validate two maps and borrow both immutably for the duration of `f`.
#[allow(dead_code)]
#[inline(always)]
pub(super) unsafe fn with_map_ref2<R>(
    a: PithMap,
    b: PithMap,
    f: impl FnOnce(&MapImpl, &MapImpl) -> R,
) -> Option<R> {
    if !unsafe { map_magic_ok(a.ptr as *const ()) }
        || !unsafe { map_magic_ok(b.ptr as *const ()) }
    {
        return None;
    }
    // SAFETY: Both pointers passed magic validation and both borrows are shared (`&MapImpl`),
    // so aliasing `a.ptr == b.ptr` is sound.
    let a_ref = unsafe { &*(a.ptr as *const MapImpl) };
    let b_ref = unsafe { &*(b.ptr as *const MapImpl) };
    Some(f(a_ref, b_ref))
}

/// Validate `dst` (mutable) and `src` (shared) without aliasing UB.
#[allow(dead_code)]
#[inline(always)]
pub(super) unsafe fn with_map_mut_and_ref<R>(
    dst: PithMap,
    src: PithMap,
    on_distinct: impl FnOnce(&mut MapImpl, &MapImpl) -> R,
    on_same: impl FnOnce(&mut MapImpl) -> R,
) -> Option<R> {
    if !unsafe { map_magic_ok(dst.ptr as *const ()) } {
        return None;
    }
    if dst.ptr == src.ptr {
        // SAFETY: `dst` validated; only one `&mut` reference is minted.
        let same = unsafe { &mut *(dst.ptr as *mut MapImpl) };
        return Some(on_same(same));
    }
    if !unsafe { map_magic_ok(src.ptr as *const ()) } {
        return None;
    }
    // SAFETY: `dst` and `src` are both valid and distinct pointers.
    let dst_mut = unsafe { &mut *(dst.ptr as *mut MapImpl) };
    let src_ref = unsafe { &*(src.ptr as *const MapImpl) };
    Some(on_distinct(dst_mut, src_ref))
}

/// Borrow a non-null C-string key as `KeyRef::Str` for the duration of `f`.
/// Nothing is copied; the borrow cannot escape `f`.
#[inline(always)]
pub(super) unsafe fn with_cstr_key<R>(key: *const i8, f: impl FnOnce(&KeyRef<'_>) -> R) -> Option<R> {
    // SAFETY: Preconditions forwarded from caller; borrow is scoped to `f`.
    unsafe { crate::ffi_util::with_cstr_bytes(key, |bytes| f(&KeyRef::Str(bytes))) }
}

/// Borrow a bytes handle's content as `KeyRef::Bytes` for the duration of `f`.
/// A null handle is the empty value, matching `pith_bytes_eq`.
#[inline(always)]
pub(super) unsafe fn with_bytes_key<R>(key: i64, f: impl FnOnce(&KeyRef<'_>) -> R) -> Option<R> {
    if key == 0 {
        return Some(f(&KeyRef::Bytes(&[])));
    }
    // SAFETY: `with_bytes` validates `key` and scopes the borrowed slice to `f`.
    unsafe { crate::bytes::with_bytes(key, |bytes| f(&KeyRef::Bytes(bytes))) }
}

/// Create a new empty map
///
/// # Arguments
/// * `key_type` - 0 for int keys, 1 for string keys
/// * `val_size` - Size of each value in bytes
/// * `val_is_heap` - Whether values are heap types (need retain/release)
///   Create a new string-key map with default settings
#[no_mangle]
pub unsafe extern "C" fn pith_map_new_default() -> PithMap {
    pith_map_new_tagged(1, 8, ListTypeTag::Primitive as i32) // string keys, 8-byte values, not heap
}

/// Create a new int-key map with default settings
#[no_mangle]
pub unsafe extern "C" fn pith_map_new_int() -> PithMap {
    pith_map_new_tagged(0, 8, ListTypeTag::Primitive as i32) // int keys, 8-byte values, not heap
}

/// String-key map that owns cstring values: insert retains; overwrite,
/// remove, clear, and free release. The emitter uses this for
/// Map[String, String].
#[no_mangle]
pub unsafe extern "C" fn pith_map_new_cstr_val() -> PithMap {
    pith_map_new_tagged(1, 8, ListTypeTag::String as i32)
}

/// Int-key map that owns cstring values (Map[Int, String]).
#[no_mangle]
pub unsafe extern "C" fn pith_map_new_int_cstr_val() -> PithMap {
    pith_map_new_tagged(0, 8, ListTypeTag::String as i32)
}

/// Bytes-key map with 8-byte values it owns no counts on (Map[Bytes, Int]);
/// a heap value kind is learned at the first kind-carrying store.
#[no_mangle]
pub unsafe extern "C" fn pith_map_new_bytes() -> PithMap {
    pith_map_new_tagged(2, 8, ListTypeTag::Primitive as i32)
}

/// Bytes-key map that owns cstring values (Map[Bytes, String]).
#[no_mangle]
pub unsafe extern "C" fn pith_map_new_bytes_cstr_val() -> PithMap {
    pith_map_new_tagged(2, 8, ListTypeTag::String as i32)
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
fn pith_map_new_tagged(key_type: i32, val_size: i64, val_tag: i32) -> PithMap {
    let map_impl = MapImpl::new(
        KeyType::from_code(key_type),
        val_size as usize,
        element_tag_from_code(val_tag),
    );
    let boxed = Box::new(map_impl);
    let ptr = Box::into_raw(boxed) as *mut ();
    PithMap { ptr }
}

/// A string-keyed map that owns values of kind `val_tag`, as a handle: the
/// map a typed json decode builds for a `Map[String, T]` field, whose value
/// kind the spec names (`Primitive` for a map of ints, bools or floats).
pub(crate) unsafe fn new_string_keyed_handle(val_tag: ListTypeTag) -> i64 {
    pith_map_new_tagged(1, 8, val_tag as i32).ptr as i64
}

/// Get map length
#[no_mangle]
pub extern "C" fn pith_map_len(map: PithMap) -> i64 {
    // SAFETY: `with_map_ref` validates `map` before reading its length.
    unsafe { with_map_ref(map, |impl_ref| impl_ref.len() as i64) }.unwrap_or(0)
}
/// Clear all entries from map
#[no_mangle]
pub unsafe extern "C" fn pith_map_clear(map: *mut PithMap) {
    if map.is_null() {
        return;
    }
    // SAFETY: `map` is non-null per caller contract.
    unsafe { pith_map_clear_handle((*map).ptr as i64) };
}

/// Get map length by handle (accepts raw MapImpl pointer as i64).
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_len_handle(map_handle: i64) -> i64 {
    // SAFETY: `with_map_ref_handle` validates `map_handle` before reading its length.
    unsafe { with_map_ref_handle(map_handle, |impl_ref| impl_ref.len() as i64) }.unwrap_or(0)
}

/// Clear all entries from map (handle-based API).
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_clear_handle(map_handle: i64) {
    // SAFETY: `with_map_mut_handle` validates `map_handle` before detaching its storage.
    let taken = unsafe { with_map_mut_handle(map_handle, MapImpl::take_values_for_release) };
    if let Some((tag, vals)) = taken {
        // SAFETY: Detached handles were owned by `map_handle` under `tag`.
        unsafe { release_taken_values(tag, vals) };
    }
}

/// Check if map is empty (handle-based API). Returns 1 if empty, 0 otherwise.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_is_empty_handle(map_handle: i64) -> i64 {
    // SAFETY: `with_map_ref_handle` validates `map_handle` before checking its length.
    let empty = unsafe { with_map_ref_handle(map_handle, |impl_ref| impl_ref.len() == 0) }
        .unwrap_or(true);
    i64::from(empty)
}
