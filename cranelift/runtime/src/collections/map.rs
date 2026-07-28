//! Map[K,V] - hash-indexed key-value collection
//!
//! Hybrid approach: Uses hashbrown::HashMap internally for O(1) lookups,
//! but presents FFI-compatible interface matching the C runtime.

use crate::collections::list::PithList;
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
    /// Whether values are heap types (need retain/release)
    val_is_heap: bool,
}

/// Key type enumeration
#[derive(Clone, Copy, Debug)]
pub enum KeyType {
    Int,
    String,
}

impl MapImpl {
    fn new(key_type: KeyType, val_size: usize, val_is_heap: bool) -> Self {
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
            val_is_heap,
        }
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
        if !self.val_is_heap || val.len() < 8 {
            return;
        }
        let raw = i64::from_le_bytes(val[..8].try_into().unwrap_or([0u8; 8]));
        crate::pith_cstring_release(raw as *const i8);
    }

    /// Drop the map's count on every stored value, for clear and free.
    ///
    /// # Safety
    /// The caller must not use the values afterwards.
    unsafe fn release_all_values(&self) {
        if !self.val_is_heap {
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
    let ktype = match key_type {
        1 => KeyType::String,
        _ => KeyType::Int,
    };

    let map_impl = MapImpl::new(ktype, val_size as usize, val_is_heap != 0);
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
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_MAP_INT_INSERTS, 1);

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

    // The map owns one count per stored string value.
    // (the retain must stay gated on val_is_heap: magic-checking an
    // arbitrary integer dereferences value-16, which faults on values
    // that resemble unmapped addresses.)
    if impl_ref.val_is_heap {
        crate::pith_cstring_retain(value as *const i8);
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

/// Get all values as a list
///
/// # Safety
/// Returns a new list that must be released
#[no_mangle]
pub unsafe extern "C" fn pith_map_values(map: PithMap) -> PithList {
    use crate::collections::list::pith_list_new;

    let Some(impl_ref) = map_ref(map) else {
        return PithList {
            ptr: std::ptr::null_mut(),
        };
    };
    let mut list = pith_list_new(
        impl_ref.val_size as i64,
        if impl_ref.val_is_heap { 1 } else { 0 },
    );

    for val in impl_ref.values() {
        crate::collections::list::pith_list_push(&mut list, val.as_ptr(), impl_ref.val_size as i64);

        // The destination list gets its own count for each copied value
        if impl_ref.val_is_heap {
            let v = std::ptr::read_unaligned(val.as_ptr() as *const i64);
            crate::pith_cstring_retain(v as *const i8);
        }
    }

    list
}

/// Release map and free memory
#[no_mangle]
pub unsafe extern "C" fn pith_map_release(map: PithMap) {
    let Some(impl_ref) = map_mut(map) else {
        return;
    };
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
    let _ = Box::from_raw(map.ptr as *mut MapImpl);
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
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_MAP_STRING_INSERTS, 1);
    let map_key = cstr_to_map_key(key);
    // the map retains the incoming value when it owns heap values, and
    // drops its count on whatever that displaces. an owned value arrives
    // with the caller's count, which the map keeps as its own.
    if impl_ref.val_is_heap {
        if !takes_caller_count {
            crate::pith_cstring_retain(value as *const i8);
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
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_MAP_STRING_GETS, 1);
    let map_key = cstr_to_map_key(key);

    match impl_ref.get(&map_key) {
        Some(val_data) if val_data.len() >= 8 => {
            let v = i64::from_le_bytes(val_data[..8].try_into().unwrap_or([0u8; 8]));
            if impl_ref.val_is_heap && map_trace_enabled() {
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
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_MAP_STRING_GETS, 1);
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
        std::process::exit(1);
    }
    let Some(impl_ref) = map_ref_from_handle(map_handle) else {
        eprintln!("pith runtime error: map indexing on invalid map handle");
        std::process::exit(1);
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_MAP_STRING_GETS, 1);
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
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_MAP_STRING_CONTAINS, 1);
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
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_MAP_STRING_GETS, 1);
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
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_MAP_INT_GETS, 1);
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
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_MAP_STRING_REMOVES, 1);
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

unsafe fn insert_ikey_inner(map_handle: i64, key: i64, value: i64, takes_caller_count: bool) {
    let Some(impl_ref) = map_mut_from_handle(map_handle) else {
        return;
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_MAP_INT_INSERTS, 1);
    if impl_ref.uses_int_values8() {
        crate::perf_count(&crate::PERF_MAP_INT_FAST_INSERTS, 1);
        impl_ref.insert_int_value(key, value);
    } else {
        crate::perf_count(&crate::PERF_MAP_INT_FALLBACK_INSERTS, 1);
        if impl_ref.val_is_heap && !takes_caller_count {
            crate::pith_cstring_retain(value as *const i8);
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
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_MAP_INT_GETS, 1);

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
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_MAP_INT_GETS, 1);
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
        std::process::exit(1);
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_MAP_INT_GETS, 1);
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
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_MAP_INT_CONTAINS, 1);

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
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_MAP_INT_REMOVES, 1);
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
    // a heap-valued map hands out its stored handles, so the list has to be
    // string-tagged: push takes a count of its own and the list's free
    // cascades it back. an untagged list would leave the caller holding raw
    // pointers the map is free to evict and release out from under.
    let list = pith_list_new(8, if impl_ref.val_is_heap { 1 } else { 0 });

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
}
