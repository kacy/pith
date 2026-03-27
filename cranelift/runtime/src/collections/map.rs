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
        MapImpl {
            data: HashMap::new(),
            key_type,
            val_size,
            val_is_heap,
        }
    }

    fn len(&self) -> usize {
        self.data.len()
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
        self.data.clear();
    }

    fn keys(&self) -> Vec<MapKey> {
        self.data.keys().cloned().collect()
    }

    fn values(&self) -> Vec<Vec<u8>> {
        self.data.values().cloned().collect()
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

/// Insert key-value pair with string key
///
/// # Safety
/// * `key` is a ForgeString
/// * `value` must point to valid data of size `val_size`
#[no_mangle]
pub unsafe extern "C" fn forge_map_insert_string(
    map: *mut ForgeMap,
    key: ForgeString,
    value: *const u8,
    val_size: i64,
) {
    if map.is_null() || (*map).ptr.is_null() || value.is_null() {
        return;
    }

    let impl_ref = &mut *((*map).ptr as *mut MapImpl);

    // Verify value size matches
    if impl_ref.val_size != val_size as usize {
        eprintln!("forge: map value size mismatch");
        return;
    }

    // Verify key type
    if !matches!(impl_ref.key_type, KeyType::String) {
        eprintln!("forge: map key type mismatch (expected string)");
        return;
    }

    // Copy key data
    let key_slice = std::slice::from_raw_parts(key.ptr, key.len as usize);
    let key_vec = key_slice.to_vec();
    let map_key = MapKey::String(key_vec);

    // Copy value data
    let val_slice = std::slice::from_raw_parts(value, val_size as usize);
    let val_vec = val_slice.to_vec();

    // Release old value if present
    if impl_ref.val_is_heap {
        if let Some(old_val) = impl_ref.get(&map_key) {
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
    impl_ref.insert(map_key, val_vec);
}

/// Get value by integer key (copies to out buffer)
///
/// Returns true if key found, false otherwise
#[no_mangle]
pub unsafe extern "C" fn forge_map_get_int(
    map: ForgeMap,
    key: i64,
    val_size: i64,
    out: *mut u8,
) -> bool {
    if map.ptr.is_null() || out.is_null() {
        return false;
    }

    let impl_ref = &*(map.ptr as *const MapImpl);

    if impl_ref.val_size != val_size as usize {
        eprintln!("forge: map value size mismatch");
        return false;
    }

    if !matches!(impl_ref.key_type, KeyType::Int) {
        return false;
    }

    match impl_ref.get(&MapKey::Int(key)) {
        Some(val_data) => {
            std::ptr::copy_nonoverlapping(val_data.as_ptr(), out, val_data.len());

            // Retain value if heap type (caller gets a reference)
            if impl_ref.val_is_heap {
                let s = out as *const ForgeString;
                forge_string_retain(*s);
            }

            true
        }
        None => false,
    }
}

/// Get value by string key (copies to out buffer)
///
/// Returns true if key found, false otherwise
#[no_mangle]
pub unsafe extern "C" fn forge_map_get_string(
    map: ForgeMap,
    key: ForgeString,
    val_size: i64,
    out: *mut u8,
) -> bool {
    if map.ptr.is_null() || out.is_null() {
        return false;
    }

    let impl_ref = &*(map.ptr as *const MapImpl);

    if impl_ref.val_size != val_size as usize {
        eprintln!("forge: map value size mismatch");
        return false;
    }

    if !matches!(impl_ref.key_type, KeyType::String) {
        return false;
    }

    let key_slice = std::slice::from_raw_parts(key.ptr, key.len as usize);
    let map_key = MapKey::String(key_slice.to_vec());

    match impl_ref.get(&map_key) {
        Some(val_data) => {
            std::ptr::copy_nonoverlapping(val_data.as_ptr(), out, val_data.len());

            // Retain value if heap type (caller gets a reference)
            if impl_ref.val_is_heap {
                let s = out as *const ForgeString;
                forge_string_retain(*s);
            }

            true
        }
        None => false,
    }
}

/// Check if map contains integer key
#[no_mangle]
pub extern "C" fn forge_map_contains_int(map: ForgeMap, key: i64) -> bool {
    if map.ptr.is_null() {
        return false;
    }

    unsafe {
        let impl_ref = &*(map.ptr as *const MapImpl);

        if !matches!(impl_ref.key_type, KeyType::Int) {
            return false;
        }

        impl_ref.contains_key(&MapKey::Int(key))
    }
}

/// Check if map contains string key
#[no_mangle]
pub unsafe extern "C" fn forge_map_contains_string(map: ForgeMap, key: ForgeString) -> bool {
    if map.ptr.is_null() {
        return false;
    }

    let impl_ref = &*(map.ptr as *const MapImpl);

    if !matches!(impl_ref.key_type, KeyType::String) {
        return false;
    }

    let key_slice = std::slice::from_raw_parts(key.ptr, key.len as usize);
    let map_key = MapKey::String(key_slice.to_vec());

    impl_ref.contains_key(&map_key)
}

/// Remove key-value pair by integer key
///
/// Returns true if key was present and removed
#[no_mangle]
pub unsafe extern "C" fn forge_map_remove_int(map: *mut ForgeMap, key: i64, val_size: i64) -> bool {
    if map.is_null() || (*map).ptr.is_null() {
        return false;
    }

    let impl_ref = &mut *((*map).ptr as *mut MapImpl);

    if impl_ref.val_size != val_size as usize {
        eprintln!("forge: map value size mismatch");
        return false;
    }

    if !matches!(impl_ref.key_type, KeyType::Int) {
        return false;
    }

    let map_key = MapKey::Int(key);

    // Release value before removal
    if impl_ref.val_is_heap {
        if let Some(val) = impl_ref.get(&map_key) {
            let s = val.as_ptr() as *const ForgeString;
            forge_string_release(*s);
        }
    }

    impl_ref.remove(&map_key).is_some()
}

/// Remove key-value pair by string key
///
/// Returns true if key was present and removed
#[no_mangle]
pub unsafe extern "C" fn forge_map_remove_string(
    map: *mut ForgeMap,
    key: ForgeString,
    val_size: i64,
) -> bool {
    if map.is_null() || (*map).ptr.is_null() {
        return false;
    }

    let impl_ref = &mut *((*map).ptr as *mut MapImpl);

    if impl_ref.val_size != val_size as usize {
        eprintln!("forge: map value size mismatch");
        return false;
    }

    if !matches!(impl_ref.key_type, KeyType::String) {
        return false;
    }

    let key_slice = std::slice::from_raw_parts(key.ptr, key.len as usize);
    let map_key = MapKey::String(key_slice.to_vec());

    // Release value before removal
    if impl_ref.val_is_heap {
        if let Some(val) = impl_ref.get(&map_key) {
            let s = val.as_ptr() as *const ForgeString;
            forge_string_release(*s);
        }
    }

    impl_ref.remove(&map_key).is_some()
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

/// Get all keys as a list (for int keys)
///
/// # Safety
/// Returns a new list that must be released
#[no_mangle]
pub unsafe extern "C" fn forge_map_keys_int(map: ForgeMap) -> ForgeList {
    use crate::collections::list::forge_list_new;

    if map.ptr.is_null() {
        return ForgeList {
            ptr: std::ptr::null_mut(),
        };
    }

    let impl_ref = &*(map.ptr as *const MapImpl);

    if !matches!(impl_ref.key_type, KeyType::Int) {
        return ForgeList {
            ptr: std::ptr::null_mut(),
        };
    }

    let mut list = forge_list_new(std::mem::size_of::<i64>() as i64, 0);

    for key in impl_ref.keys() {
        if let MapKey::Int(k) = key {
            let k_ptr = &k as *const i64 as *const u8;
            crate::collections::list::forge_list_push(
                &mut list,
                k_ptr,
                std::mem::size_of::<i64>() as i64,
            );
        }
    }

    list
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
    let map_key = cstr_to_map_key(key);

    if impl_ref.contains_key(&map_key) {
        1
    } else {
        0
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
    let val_bytes = value.to_le_bytes().to_vec();
    impl_ref.insert(MapKey::Int(key), val_bytes);
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

    match impl_ref.get(&MapKey::Int(key)) {
        Some(val_data) if val_data.len() >= 8 => {
            i64::from_le_bytes(val_data[..8].try_into().unwrap_or([0u8; 8]))
        }
        _ => 0,
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

    if impl_ref.contains_key(&MapKey::Int(key)) {
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
    impl_ref.remove(&MapKey::Int(key));
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
