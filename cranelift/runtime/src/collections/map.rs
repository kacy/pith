//! Map[K,V] - hash-indexed key-value collection
//!
//! Hybrid approach: Uses hashbrown::HashMap internally for O(1) lookups,
//! but presents FFI-compatible interface matching the C runtime.

use crate::collections::list::ForgeList;
use crate::string::{forge_string_release, forge_string_retain, ForgeString};
use hashbrown::HashMap;
use std::hash::{Hash, Hasher};
/// FFI-compatible map handle
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ForgeMap {
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
}

/// Create a new empty map
///
/// # Arguments
/// * `key_type` - 0 for int keys, 1 for string keys
/// * `val_size` - Size of each value in bytes
/// * `val_is_heap` - Whether values are heap types (need retain/release)
/// Create a new string-key map with default settings
#[no_mangle]
pub unsafe extern "C" fn forge_map_new_default() -> ForgeMap {
    forge_map_new(1, 8, 0) // string keys, 8-byte values, not heap
}

/// Create a new int-key map with default settings
#[no_mangle]
pub unsafe extern "C" fn forge_map_new_int() -> ForgeMap {
    forge_map_new(0, 8, 0) // int keys, 8-byte values, not heap
}

#[no_mangle]
pub unsafe extern "C" fn forge_map_new(
    key_type: i32,
    val_size: i64,
    val_is_heap: i64,
) -> ForgeMap {
    let ktype = match key_type {
        1 => KeyType::String,
        _ => KeyType::Int,
    };

    let map_impl = MapImpl::new(ktype, val_size as usize, val_is_heap != 0);
    let boxed = Box::new(map_impl);
    ForgeMap {
        ptr: Box::into_raw(boxed) as *mut (),
    }
}

/// Get map length
#[no_mangle]
pub extern "C" fn forge_map_len(map: ForgeMap) -> i64 {
    if map.ptr.is_null() {
        return 0;
    }

    unsafe {
        let impl_ref = &*(map.ptr as *const MapImpl);
        impl_ref.len() as i64
    }
}

/// Insert key-value pair with integer key
///
/// # Safety
/// * `key` is the integer key value
/// * `value` must point to valid data of size `val_size`
#[no_mangle]
pub unsafe extern "C" fn forge_map_insert_int(
    map: *mut ForgeMap,
    key: i64,
    value: *const u8,
    val_size: i64,
) {
    if map.is_null() || (*map).ptr.is_null() || value.is_null() {
        return;
    }

    let impl_ref = &mut *((*map).ptr as *mut MapImpl);
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_MAP_INT_INSERTS, 1);

    // Verify value size matches
    if impl_ref.val_size != val_size as usize {
        eprintln!("forge: map value size mismatch");
        return;
    }

    // Verify key type
    if !matches!(impl_ref.key_type, KeyType::Int) {
        eprintln!("forge: map key type mismatch (expected int)");
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

    // Release old value if present
    if impl_ref.val_is_heap {
        if let Some(old_val) = impl_ref.get(&MapKey::Int(key)) {
            let s = old_val.as_ptr() as *const ForgeString;
            forge_string_release(*s);
        }
    }

    // Retain new value if heap type
    if impl_ref.val_is_heap {
        let s = value as *const ForgeString;
        forge_string_retain(*s);
    }

    // Insert into map
    impl_ref.insert(MapKey::Int(key), val_vec);
}

/// Clear all entries from map
#[no_mangle]
pub unsafe extern "C" fn forge_map_clear(map: *mut ForgeMap) {
    if map.is_null() || (*map).ptr.is_null() {
        return;
    }

    let impl_ref = &mut *((*map).ptr as *mut MapImpl);

    // Release all values if they're heap types
    if impl_ref.val_is_heap {
        for (_, val) in &impl_ref.data {
            let s = val.as_ptr() as *const ForgeString;
            forge_string_release(*s);
        }
    }

    impl_ref.clear();
}

/// Get all values as a list
///
/// # Safety
/// Returns a new list that must be released
#[no_mangle]
pub unsafe extern "C" fn forge_map_values(map: ForgeMap) -> ForgeList {
    use crate::collections::list::forge_list_new;

    if map.ptr.is_null() {
        return ForgeList {
            ptr: std::ptr::null_mut(),
        };
    }

    let impl_ref = &*(map.ptr as *const MapImpl);
    let mut list = forge_list_new(
        impl_ref.val_size as i64,
        if impl_ref.val_is_heap { 1 } else { 0 },
    );

    for val in impl_ref.values() {
        crate::collections::list::forge_list_push(
            &mut list,
            val.as_ptr(),
            impl_ref.val_size as i64,
        );

        // Retain values as they're being copied to the list
        if impl_ref.val_is_heap {
            let s = val.as_ptr() as *const ForgeString;
            forge_string_retain(*s);
        }
    }

    list
}

/// Release map and free memory
#[no_mangle]
pub unsafe extern "C" fn forge_map_release(map: ForgeMap) {
    if map.ptr.is_null() {
        return;
    }

    let impl_ref = &mut *(map.ptr as *mut MapImpl);

    // Release all values if they're heap types
    if impl_ref.val_is_heap {
        for (_, val) in &impl_ref.data {
            let s = val.as_ptr() as *const ForgeString;
            forge_string_release(*s);
        }
    }

    // Free the map implementation
    let _ = Box::from_raw(map.ptr as *mut MapImpl);
}

/// Destructor for map elements in collections
///
/// Called by cycle collector when freeing cyclic map objects
#[no_mangle]
pub extern "C" fn forge_map_destructor(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }

    unsafe {
        let map = ptr as *const ForgeMap;
        forge_map_release(*map);
    }
}

// ---------------------------------------------------------------------------
// C-string-key variants for Cranelift codegen
//
// These functions accept a raw map_handle (the ForgeMap.ptr cast to i64) and
// null-terminated C string keys, providing a simpler ABI than the ForgeString
// variants above.
// ---------------------------------------------------------------------------

/// Compute the byte length of a null-terminated C string (helper).
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
pub unsafe extern "C" fn forge_map_insert_cstr(map_handle: i64, key: *const i8, value: i64) {
    if map_handle == 0 || key.is_null() {
        return;
    }

    let impl_ref = &mut *(map_handle as *mut MapImpl);
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_MAP_STRING_INSERTS, 1);
    let map_key = cstr_to_map_key(key);
    let val_bytes = value.to_le_bytes().to_vec();
    impl_ref.insert(map_key, val_bytes);
}

/// Get an i64 value by C-string key. Returns 0 if the key is not found.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn forge_map_get_cstr(map_handle: i64, key: *const i8) -> i64 {
    if map_handle == 0 || key.is_null() {
        return 0;
    }

    let impl_ref = &*(map_handle as *const MapImpl);
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_MAP_STRING_GETS, 1);
    let map_key = cstr_to_map_key(key);

    match impl_ref.get(&map_key) {
        Some(val_data) if val_data.len() >= 8 => {
            i64::from_le_bytes(val_data[..8].try_into().unwrap_or([0u8; 8]))
        }
        _ => 0,
    }
}

/// Check if a C-string key exists in the map. Returns 1 if present, 0 otherwise.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn forge_map_contains_cstr(map_handle: i64, key: *const i8) -> i64 {
    if map_handle == 0 || key.is_null() {
        return 0;
    }

    let impl_ref = &*(map_handle as *const MapImpl);
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
pub unsafe extern "C" fn forge_map_get_default_cstr(map_handle: i64, key: *const i8, default: i64) -> i64 {
    if map_handle == 0 || key.is_null() {
        return default;
    }
    let impl_ref = &*(map_handle as *const MapImpl);
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
pub unsafe extern "C" fn forge_map_get_default_ikey(map_handle: i64, key: i64, default: i64) -> i64 {
    if map_handle == 0 {
        return default;
    }
    let impl_ref = &*(map_handle as *const MapImpl);
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
pub unsafe extern "C" fn forge_map_remove_cstr(map_handle: i64, key: *const i8) {
    if map_handle == 0 || key.is_null() {
        return;
    }

    let impl_ref = &mut *(map_handle as *mut MapImpl);
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_MAP_STRING_REMOVES, 1);
    let map_key = cstr_to_map_key(key);
    impl_ref.remove(&map_key);
}

// ---------------------------------------------------------------------------
// Integer-key variants for Cranelift codegen (handle-based, like cstr variants)
// ---------------------------------------------------------------------------

/// Insert an i64 value with an integer key (handle-based API).
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn forge_map_insert_ikey(map_handle: i64, key: i64, value: i64) {
    if map_handle == 0 {
        return;
    }

    let impl_ref = &mut *(map_handle as *mut MapImpl);
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_MAP_INT_INSERTS, 1);
    if impl_ref.uses_int_values8() {
        crate::perf_count(&crate::PERF_MAP_INT_FAST_INSERTS, 1);
        impl_ref.insert_int_value(key, value);
    } else {
        crate::perf_count(&crate::PERF_MAP_INT_FALLBACK_INSERTS, 1);
        let val_bytes = value.to_le_bytes().to_vec();
        impl_ref.insert(MapKey::Int(key), val_bytes);
    }
}

/// Get an i64 value by integer key. Returns 0 if the key is not found.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn forge_map_get_ikey(map_handle: i64, key: i64) -> i64 {
    if map_handle == 0 {
        return 0;
    }

    let impl_ref = &*(map_handle as *const MapImpl);
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

/// Check if an integer key exists in the map. Returns 1 if present, 0 otherwise.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn forge_map_contains_ikey(map_handle: i64, key: i64) -> i64 {
    if map_handle == 0 {
        return 0;
    }

    let impl_ref = &*(map_handle as *const MapImpl);
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
pub unsafe extern "C" fn forge_map_remove_ikey(map_handle: i64, key: i64) {
    if map_handle == 0 {
        return;
    }

    let impl_ref = &mut *(map_handle as *mut MapImpl);
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_MAP_INT_REMOVES, 1);
    if impl_ref.uses_int_values8() {
        crate::perf_count(&crate::PERF_MAP_INT_FAST_REMOVES, 1);
        impl_ref.remove_int_value(key);
    } else {
        crate::perf_count(&crate::PERF_MAP_INT_FALLBACK_REMOVES, 1);
        impl_ref.remove(&MapKey::Int(key));
    }
}

/// Get map length by handle (accepts raw MapImpl pointer as i64).
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn forge_map_len_handle(map_handle: i64) -> i64 {
    if map_handle == 0 {
        return 0;
    }

    let impl_ref = &*(map_handle as *const MapImpl);
    impl_ref.len() as i64
}

/// Return all keys as a ForgeList of C-string pointers (each element is an i64
/// pointer to a newly allocated null-terminated string). The ForgeList pointer
/// is returned as i64.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn forge_map_keys_cstr(map_handle: i64) -> i64 {
    use crate::collections::list::{forge_list_new, forge_list_push_value};
    use std::alloc::{alloc, Layout};

    if map_handle == 0 {
        let empty = forge_list_new(8, 0);
        return empty.ptr as i64;
    }

    let impl_ref = &*(map_handle as *const MapImpl);
    let list = forge_list_new(8, 0); // list of i64 (pointer-sized primitives)

    for key in impl_ref.keys() {
        if let MapKey::String(ref bytes) = key {
            let len = bytes.len();
            let layout = Layout::from_size_align(len + 1, 1).unwrap();
            let ptr = alloc(layout) as *mut i8;
            if !ptr.is_null() {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, len);
                *ptr.add(len) = 0;
                forge_list_push_value(list, ptr as i64);
            }
        }
    }

    list.ptr as i64
}

/// Clear all entries from map (handle-based API).
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn forge_map_clear_handle(map_handle: i64) {
    if map_handle == 0 {
        return;
    }

    let impl_ref = &mut *(map_handle as *mut MapImpl);
    impl_ref.clear();
}

/// Check if map is empty (handle-based API). Returns 1 if empty, 0 otherwise.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn forge_map_is_empty_handle(map_handle: i64) -> i64 {
    if map_handle == 0 {
        return 1;
    }

    let impl_ref = &*(map_handle as *const MapImpl);
    if impl_ref.len() == 0 { 1 } else { 0 }
}

/// Return all values as a ForgeList (handle-based API). The ForgeList pointer
/// is returned as i64.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn forge_map_values_handle(map_handle: i64) -> i64 {
    use crate::collections::list::{forge_list_new, forge_list_push_value};

    if map_handle == 0 {
        let empty = forge_list_new(8, 0);
        return empty.ptr as i64;
    }

    let impl_ref = &*(map_handle as *const MapImpl);
    let list = forge_list_new(8, 0);

    for val in impl_ref.values() {
        if val.len() >= 8 {
            let v = i64::from_le_bytes(val[..8].try_into().unwrap_or([0u8; 8]));
            forge_list_push_value(list, v);
        }
    }

    list.ptr as i64
}
