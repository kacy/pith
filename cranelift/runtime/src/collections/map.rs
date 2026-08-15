//! Map[K,V] - hash-indexed key-value collection
//!
//! Hybrid approach: Uses hashbrown::HashMap internally for O(1) lookups,
//! but presents FFI-compatible interface matching the C runtime.

use crate::collections::list::{
    element_tag_from_code, release_element, retain_element, ListTypeTag,
};
use crate::handle_registry::{self, HandleKind};
use crate::runtime_core::optional_tuple;
use hashbrown::HashMap;
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
/// We support both integer and string keys
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MapKey {
    Int(i64),
    String(Vec<u8>), // Byte representation of the string
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
        }
    }
}

/// Internal map implementation using idiomatic Rust
///
/// Uses HashMap for O(1) lookups with Vec<u8> storage for values
pub struct MapImpl {
    /// Magic word for the fast validity check (see map_magic_ok)
    magic: u32,
    /// Shared-handle refcount (see ListImpl.rc)
    rc: std::sync::atomic::AtomicU32,
    /// The actual hash map storing key -> value mappings
    data: HashMap<MapKey, Vec<u8>>,
    /// Specialized storage for int-key maps with 8-byte scalar values
    int_values8: Option<HashMap<i64, i64>>,
    /// Type tag for keys (0=int, 1=string)
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
#[derive(Clone, Copy, Debug)]
pub enum KeyType {
    Int,
    String,
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

    fn insert(&mut self, key: MapKey, value: Vec<u8>) -> Option<Vec<u8>> {
        self.data.insert(key, value)
    }

    fn get(&self, key: &MapKey) -> Option<&Vec<u8>> {
        self.data.get(key)
    }

    fn remove(&mut self, key: &MapKey) -> Option<Vec<u8>> {
        self.data.remove(key)
    }

    fn contains_key(&self, key: &MapKey) -> bool {
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

    fn values(&self) -> Vec<Vec<u8>> {
        match &self.int_values8 {
            Some(data) => data
                .values()
                .map(|value| value.to_le_bytes().to_vec())
                .collect(),
            None => self.data.values().cloned().collect(),
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
    /// `val` must be the raw storage of a value this map owned.
    unsafe fn release_value(&self, val: &[u8]) {
        if !self.val_is_heap() || val.len() < 8 {
            return;
        }
        let raw = i64::from_le_bytes(val[..8].try_into().unwrap_or([0u8; 8]));
        release_element(self.val_tag, raw);
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
    let ktype = match key_type {
        1 => KeyType::String,
        _ => KeyType::Int,
    };

    let map_impl = MapImpl::new(ktype, val_size as usize, element_tag_from_code(val_tag));
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
    let val_vec = val_slice.to_vec();

    // The map owns one count per stored heap value, read out of the same
    // eight bytes release_value drops it from.
    // (the retain must stay gated on the value tag: magic-checking an
    // arbitrary integer dereferences value-16, which faults on values
    // that resemble unmapped addresses.)
    if val_slice.len() >= 8 {
        let raw = i64::from_le_bytes(val_slice[..8].try_into().unwrap_or([0u8; 8]));
        impl_ref.retain_value(raw);
    }

    // Retain before insert, release the displaced value after: `m[k] = m[k]`
    // must not drop the last count before the new one is taken.
    if let Some(old) = impl_ref.insert(MapKey::Int(key), val_vec) {
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
        if val.len() >= 8 {
            let raw = i64::from_le_bytes(val[..8].try_into().unwrap_or([0u8; 8]));
            if raw != 0 {
                f(raw, code);
            }
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
    match impl_ref.remove(&MapKey::Int(key)) {
        Some(val) if val.len() >= 8 => {
            i64::from_le_bytes(val[..8].try_into().unwrap_or([0u8; 8]))
        }
        _ => 0,
    }
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
    let map_key = cstr_to_map_key(key);
    match impl_ref.remove(&map_key) {
        Some(val) if val.len() >= 8 => {
            i64::from_le_bytes(val[..8].try_into().unwrap_or([0u8; 8]))
        }
        _ => 0,
    }
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

unsafe fn cstr_to_map_key(key: *const i8) -> MapKey {
    let mut len = 0usize;
    let mut p = key;
    while *p != 0 {
        len += 1;
        p = p.add(1);
    }
    let bytes = std::slice::from_raw_parts(key as *const u8, len);
    MapKey::String(bytes.to_vec())
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

unsafe fn insert_cstr_inner(
    map_handle: i64,
    key: *const i8,
    value: i64,
    takes_caller_count: bool,
) {
    if key.is_null() {
        return;
    }

    let Some(impl_ref) = map_mut_from_handle(map_handle) else {
        return;
    };
    crate::perf_stats!(PERF_MAP_STRING_INSERTS += 1);
    let map_key = cstr_to_map_key(key);
    // the map retains the incoming value when it owns heap values, and
    // drops its count on whatever that displaces. an owned value arrives
    // with the caller's count, which the map keeps as its own.
    if impl_ref.val_is_heap() {
        if !takes_caller_count {
            impl_ref.retain_value(value);
        }
        if map_trace_enabled() {
            let kb = match &map_key {
                MapKey::String(b) => String::from_utf8_lossy(b).into_owned(),
                MapKey::Int(n) => n.to_string(),
            };
            eprintln!("map_ins {:?} -> {:p}", kb, value as *const i8);
        }
    }
    let val_bytes = value.to_le_bytes().to_vec();
    if let Some(old) = impl_ref.insert(map_key, val_bytes) {
        impl_ref.release_value(&old);
    }
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
    crate::perf_stats!(PERF_MAP_STRING_GETS += 1);
    let map_key = cstr_to_map_key(key);

    match impl_ref.get(&map_key) {
        Some(val_data) if val_data.len() >= 8 => {
            let v = i64::from_le_bytes(val_data[..8].try_into().unwrap_or([0u8; 8]));
            if impl_ref.val_is_heap() && map_trace_enabled() {
                let kb = match &map_key {
                    MapKey::String(b) => String::from_utf8_lossy(b).into_owned(),
                    MapKey::Int(n) => n.to_string(),
                };
                eprintln!("map_get {:?} -> {:p}", kb, v as *const i8);
            }
            v
        }
        _ => 0,
    }
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
    crate::perf_stats!(PERF_MAP_STRING_GETS += 1);
    let map_key = cstr_to_map_key(key);
    match impl_ref.get(&map_key) {
        Some(val_data) if val_data.len() >= 8 => optional_tuple(
            true,
            i64::from_le_bytes(val_data[..8].try_into().unwrap_or([0u8; 8])),
        ),
        _ => optional_tuple(false, 0),
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
    crate::perf_stats!(PERF_MAP_STRING_GETS += 1);
    let map_key = cstr_to_map_key(key);
    match impl_ref.get(&map_key) {
        Some(val_data) if val_data.len() >= 8 => {
            i64::from_le_bytes(val_data[..8].try_into().unwrap_or([0u8; 8]))
        }
        _ => {
            let key_display = match &map_key {
                MapKey::String(b) => format!("{:?}", String::from_utf8_lossy(b)),
                MapKey::Int(n) => n.to_string(),
            };
            eprintln!(
                "pith runtime error: map key not found: {} (use .contains_key first, .get(k) for Optional, or .get_default(k, d) for a fallback)",
                key_display
            );
            // panic-guard: a missing key under strict indexing is a program bug with no value to return.
            std::process::exit(1);
        }
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
    crate::perf_stats!(PERF_MAP_STRING_CONTAINS += 1);
    let map_key = cstr_to_map_key(key);

    if impl_ref.contains_key(&map_key) {
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
    crate::perf_stats!(PERF_MAP_STRING_GETS += 1);
    let map_key = cstr_to_map_key(key);
    match impl_ref.get(&map_key) {
        Some(val_data) if val_data.len() >= 8 => {
            i64::from_le_bytes(val_data[..8].try_into().unwrap_or([0u8; 8]))
        }
        _ => default,
    }
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
        let map_key = MapKey::Int(key);
        match impl_ref.get(&map_key) {
            Some(val_data) if val_data.len() >= 8 => {
                i64::from_le_bytes(val_data[..8].try_into().unwrap_or([0u8; 8]))
            }
            _ => default,
        }
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
    crate::perf_stats!(PERF_MAP_STRING_REMOVES += 1);
    let map_key = cstr_to_map_key(key);
    if let Some(old) = impl_ref.remove(&map_key) {
        impl_ref.release_value(&old);
    }
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
        let val_bytes = value.to_le_bytes().to_vec();
        if let Some(old) = impl_ref.insert(MapKey::Int(key), val_bytes) {
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
        match impl_ref.get(&MapKey::Int(key)) {
            Some(val_data) if val_data.len() >= 8 => {
                i64::from_le_bytes(val_data[..8].try_into().unwrap_or([0u8; 8]))
            }
            _ => 0,
        }
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
        match impl_ref.get(&MapKey::Int(key)) {
            Some(val_data) if val_data.len() >= 8 => optional_tuple(
                true,
                i64::from_le_bytes(val_data[..8].try_into().unwrap_or([0u8; 8])),
            ),
            _ => optional_tuple(false, 0),
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
        match impl_ref.get(&MapKey::Int(key)) {
            Some(val_data) if val_data.len() >= 8 => Some(i64::from_le_bytes(
                val_data[..8].try_into().unwrap_or([0u8; 8]),
            )),
            _ => None,
        }
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
        impl_ref.contains_key(&MapKey::Int(key))
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
        if let Some(old) = impl_ref.remove(&MapKey::Int(key)) {
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
                };
                let raw = if val.len() >= 8 {
                    i64::from_le_bytes(val[..8].try_into().unwrap_or([0u8; 8]))
                } else {
                    0
                };
                entries.push((key_text, raw));
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

    for val in impl_ref.values() {
        if val.len() >= 8 {
            let v = i64::from_le_bytes(val[..8].try_into().unwrap_or([0u8; 8]));
            pith_list_push_value(list, v);
        }
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
}
