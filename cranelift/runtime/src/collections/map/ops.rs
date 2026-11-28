//! Map CRUD operations across C-string, bytes, and integer keys.

use super::{
    require_key_flavor, with_bytes_key, with_cstr_key, with_map_mut, with_map_mut_handle,
    with_map_ref_handle, KeyRef, KeyType, MapImpl, MapVal, PithMap,
};
use crate::collections::list::{
    element_tag_from_code, release_element, retain_element, ListTypeTag,
};
use crate::ffi_util::ResultPair;
use crate::runtime_core::{optional_from, optional_pair};


/// Store `value` under a key given as bytes, with `pith_map_insert_cstr`'s
/// contract when `takes_caller_count` is false and `pith_map_insert_cstr_owned`'s
/// when it is true: a heap value is retained or its caller's count taken,
/// and whatever the key displaced is released. For a caller holding key
/// bytes that are not a C string, such as the json filler, which reads a
/// key straight out of the document.
///
/// # Safety
/// `map_handle` must be a live string-keyed map; `value` must be of the
/// map's value kind.
pub(crate) unsafe fn insert_bytes_key(
    map_handle: i64,
    key: &[u8],
    value: i64,
    takes_caller_count: bool,
) {
    let run = |impl_ref: &mut MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::String, "map_insert");
        // SAFETY: Caller guarantees `value` matches the map's value kind.
        unsafe { insert_keyed(impl_ref, &KeyRef::Str(key), value, takes_caller_count) }
    };
    // SAFETY: `with_map_mut_handle` validates `map_handle`; release displaced value after borrow ends.
    let displaced = unsafe { with_map_mut_handle(map_handle, run) }.flatten();
    if let Some((tag, raw)) = displaced {
        // SAFETY: `raw` was owned by `map_handle` under `tag`.
        unsafe { release_element(tag, raw) };
    }
}

#[must_use]
#[inline(always)]
unsafe fn map_insert_int_impl(
    impl_ref: &mut MapImpl,
    key: i64,
    value: *const u8,
    val_size: i64,
) -> Option<(ListTypeTag, i64)> {
    crate::perf_stats!(PERF_MAP_INT_INSERTS += 1);

    // Verify value size matches
    if impl_ref.val_size != val_size as usize {
        eprintln!("pith: map value size mismatch");
        return None;
    }

    // Verify key type
    if !matches!(impl_ref.key_type, KeyType::Int) {
        eprintln!("pith: map key type mismatch (expected int)");
        return None;
    }

    // Copy value data
    // SAFETY: Caller guarantees `value` points to `val_size` readable bytes.
    let val_slice = unsafe { std::slice::from_raw_parts(value, val_size as usize) };
    if impl_ref.uses_int_values8() {
        crate::perf_count(&crate::PERF_MAP_INT_FAST_INSERTS, 1);
        let int_value = i64::from_le_bytes(val_slice[..8].try_into().unwrap_or([0u8; 8]));
        impl_ref.insert_int_value(key, int_value);
        return None;
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
        // SAFETY: Caller passes a handle matching `impl_ref.val_tag` when heap-valued.
        unsafe { impl_ref.retain_value(raw) };
    }

    // Retain before insert, release the displaced value after `&mut MapImpl`
    // ends: `m[k] = m[k]` must not drop the last count before the new one is taken.
    impl_ref
        .insert(&KeyRef::Int(key), stored)
        .and_then(|old| impl_ref.displaced_value_handle(old))
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

    // SAFETY: `map` is non-null per caller contract.
    let map_val = unsafe { *map };
    // SAFETY: `with_map_mut` validates `map_val`; `value` is readable for `val_size` bytes.
    let displaced = unsafe {
        with_map_mut(map_val, |impl_ref| {
            map_insert_int_impl(impl_ref, key, value, val_size)
        })
    }
    .flatten();
    if let Some((tag, raw)) = displaced {
        // SAFETY: `raw` was owned by `map_val` under `tag`; `&mut MapImpl` borrow has ended.
        unsafe { release_element(tag, raw) };
    }
}

#[inline(always)]
fn map_take_ikey_impl(impl_ref: &mut MapImpl, key: i64) -> i64 {
    if impl_ref.uses_int_values8() {
        return impl_ref.remove_int_value(key).unwrap_or(0);
    }
    take_keyed(impl_ref, &KeyRef::Int(key))
}

/// Remove an int-keyed entry and hand its value, count included, to the
/// caller. The map neither retains nor releases: ownership transfers, so
/// this is the reclaim-safe way to drop registry entries under the
/// free-only-cascade rule.
///
/// # Safety
/// map_handle must be a valid map handle or garbage (the magic check
/// rejects garbage).
#[no_mangle]
pub unsafe extern "C" fn pith_map_take_ikey(map_handle: i64, key: i64) -> i64 {
    // SAFETY: `with_map_mut_handle` validates `map_handle` before mutating it.
    unsafe { with_map_mut_handle(map_handle, |impl_ref| map_take_ikey_impl(impl_ref, key)) }
        .unwrap_or(0)
}

/// String-keyed take: remove and transfer the value's count to the caller.
///
/// # Safety
/// map_handle must be a valid map handle; key a valid cstring.
#[no_mangle]
pub unsafe extern "C" fn pith_map_take(map_handle: i64, key: *const i8) -> i64 {
    let run = |impl_ref: &mut MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::String, "map_take");
        // SAFETY: `with_cstr_key` rejects null and scopes the borrowed key bytes.
        unsafe { with_cstr_key(key, |map_key| take_keyed(impl_ref, map_key)) }.unwrap_or(0)
    };
    // SAFETY: `with_map_mut_handle` validates `map_handle` before mutating it.
    unsafe { with_map_mut_handle(map_handle, run) }.unwrap_or(0)
}

/// Bytes-keyed take: remove and transfer the value's count to the caller.
///
/// # Safety
/// map_handle must be a valid map handle; key a bytes handle or null.
#[no_mangle]
pub unsafe extern "C" fn pith_map_take_bkey(map_handle: i64, key: i64) -> i64 {
    let run = |impl_ref: &mut MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_take_bkey");
        // SAFETY: `with_bytes_key` validates `key` and scopes its borrowed slice.
        unsafe { with_bytes_key(key, |map_key| take_keyed(impl_ref, map_key)) }.unwrap_or(0)
    };
    // SAFETY: `with_map_mut_handle` validates `map_handle` before mutating it.
    unsafe { with_map_mut_handle(map_handle, run) }.unwrap_or(0)
}

/// Remove an entry and hand its value's count to the caller: the map's
/// count leaves with the value instead of being released here.
#[inline(always)]
fn take_keyed(impl_ref: &mut MapImpl, map_key: &KeyRef<'_>) -> i64 {
    impl_ref
        .remove(map_key)
        .and_then(|val| val.word())
        .unwrap_or(0)
}
// ---------------------------------------------------------------------------
// C-string-key variants for Cranelift codegen
//
// These functions accept a raw map_handle (the PithMap.ptr cast to i64) and
// null-terminated C string keys, providing a simpler ABI than the PithString
// variants above.
// ---------------------------------------------------------------------------

/// Whether `PITH_MAP_TRACE` is set, read once.
pub(super) fn map_trace_enabled() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var("PITH_MAP_TRACE").is_ok())
}

/// The stored word under `map_key`, or `None` on a miss.
#[inline(always)]
pub(super) fn get_keyed(impl_ref: &MapImpl, map_key: &KeyRef<'_>) -> Option<i64> {
    impl_ref.get(map_key).and_then(|val| val.word())
}

/// Drop the entry under `map_key`, returning any displaced heap value handle
/// for release after the `&mut MapImpl` borrow ends.
#[must_use]
#[inline(always)]
fn remove_keyed(impl_ref: &mut MapImpl, map_key: &KeyRef<'_>) -> Option<(ListTypeTag, i64)> {
    impl_ref
        .remove(map_key)
        .and_then(|old| impl_ref.displaced_value_handle(old))
}

/// Exit with the structured diagnostic for a strict miss: `m[k]` on a key
/// the map does not hold.
///
/// The caller passes the key's text by value: `fail` skips the caller's frame,
/// so a string the caller still owned would never be freed.
pub(super) fn strict_miss(key_display: String) -> ! {
    let message = format!(
        "map key not found: {} (use .contains_key first, .get(k) for Optional, or .get_default(k, d) for a fallback)",
        key_display
    );
    drop(key_display);
    crate::failure::fail(message)
}

#[cold]
#[inline(never)]
pub(super) fn null_map_key_exit() -> ! {
    runtime_fatal!("map key not found: <null>")
}

#[cold]
#[inline(never)]
pub(super) fn invalid_map_handle_exit() -> ! {
    runtime_fatal!("map indexing on invalid map handle")
}

/// Insert an i64 value with a C-string key.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_map_insert_cstr(map_handle: i64, key: *const i8, value: i64) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { insert_cstr_inner(map_handle, key, value, false) };
}

/// Insert a value the caller owns under a C-string key. The map takes the
/// caller's count instead of adding one of its own, so a value built
/// straight into the store ends up with exactly one owner. A map that owns
/// no value counts has nothing to take, and the caller's count stays
/// outstanding there; see `pith_list_push_value_owned`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_map_insert_cstr_owned(map_handle: i64, key: *const i8, value: i64) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { insert_cstr_inner(map_handle, key, value, true) };
}

/// Insert a borrowed value of a named heap kind. The map learns the kind
/// here when its constructor could not supply one (see
/// `MapImpl::adopt_value_tag`), which is what lets a container stored into a
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
    // SAFETY: `map_adopt_value_tag` validates `map_handle`.
    let Some(owns) = (unsafe { map_adopt_value_tag(map_handle, val_tag) }) else {
        return;
    };
    if !owns {
        // SAFETY: Caller passes a value matching `val_tag`.
        unsafe { retain_element(element_tag_from_code(val_tag as i32), value) };
    }
    // SAFETY: Preconditions forwarded from caller.
    unsafe { insert_cstr_inner(map_handle, key, value, false) };
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
    // SAFETY: Preconditions forwarded from caller.
    unsafe {
        map_adopt_value_tag(map_handle, val_tag);
        insert_cstr_inner(map_handle, key, value, true);
    }
}

/// Let a map learn the heap kind of the values being stored into it.
/// Returns true when the map owns a count on that kind afterwards.
unsafe fn map_adopt_value_tag(map_handle: i64, val_tag: i64) -> Option<bool> {
    let tag = element_tag_from_code(val_tag as i32);
    // SAFETY: `with_map_mut_handle` validates `map_handle` before updating its tag.
    unsafe { with_map_mut_handle(map_handle, |impl_ref| impl_ref.adopt_value_tag(tag)) }
        
}

#[inline(always)]
unsafe fn insert_cstr_inner(
    map_handle: i64,
    key: *const i8,
    value: i64,
    takes_caller_count: bool,
) {
    if key.is_null() {
        return;
    }

    let run = |impl_ref: &mut MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::String, "map_insert");
        crate::perf_stats!(PERF_MAP_STRING_INSERTS += 1);
        let do_insert = |map_key: &KeyRef<'_>| {
            // SAFETY: Caller passes a value matching the map's value kind.
            unsafe { insert_keyed(impl_ref, map_key, value, takes_caller_count) }
        };
        // SAFETY: `key` is non-null and a valid C string per caller contract.
        unsafe { with_cstr_key(key, do_insert) }.flatten()
    };
    // SAFETY: `with_map_mut_handle` validates `map_handle`; release displaced value after borrow ends.
    let displaced = unsafe { with_map_mut_handle(map_handle, run) }.flatten();
    if let Some((tag, raw)) = displaced {
        // SAFETY: `raw` was owned by `map_handle` under `tag`.
        unsafe { release_element(tag, raw) };
    }
}

#[inline(always)]
unsafe fn insert_bkey_inner(map_handle: i64, key: i64, value: i64, takes_caller_count: bool) {
    let run = |impl_ref: &mut MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_insert_bkey");
        let do_insert = |map_key: &KeyRef<'_>| {
            // SAFETY: Caller passes a value matching the map's value kind.
            unsafe { insert_keyed(impl_ref, map_key, value, takes_caller_count) }
        };
        // SAFETY: `with_bytes_key` validates `key` and scopes its borrowed slice.
        unsafe { with_bytes_key(key, do_insert) }.flatten()
    };
    // SAFETY: `with_map_mut_handle` validates `map_handle`; release displaced value after borrow ends.
    let displaced = unsafe { with_map_mut_handle(map_handle, run) }.flatten();
    if let Some((tag, raw)) = displaced {
        // SAFETY: `raw` was owned by `map_handle` under `tag`.
        unsafe { release_element(tag, raw) };
    }
}

/// Store `value` under `map_key`. the map retains the incoming value when it
/// owns heap values, and returns whatever that displaces for release after the
/// `&mut MapImpl` borrow ends. an owned value arrives with the caller's count,
/// which the map keeps as its own.
#[must_use]
#[inline(always)]
unsafe fn insert_keyed(
    impl_ref: &mut MapImpl,
    map_key: &KeyRef<'_>,
    value: i64,
    takes_caller_count: bool,
) -> Option<(ListTypeTag, i64)> {
    if impl_ref.val_is_heap() {
        if !takes_caller_count {
            // SAFETY: Caller passes a handle matching `impl_ref.val_tag`.
            unsafe { impl_ref.retain_value(value) };
        }
        if map_trace_enabled() {
            eprintln!("map_ins {} -> {:p}", map_key.display(), value as *const i8);
        }
    }
    impl_ref
        .insert(map_key, MapVal::Word(value))
        .and_then(|old| impl_ref.displaced_value_handle(old))
}
#[inline(always)]
fn map_get_cstr_traced(impl_ref: &MapImpl, map_key: &KeyRef<'_>) -> i64 {
    let Some(v) = get_keyed(impl_ref, map_key) else {
        return 0;
    };
    if impl_ref.val_is_heap() && map_trace_enabled() {
        eprintln!("map_get {} -> {:p}", map_key.display(), v as *const i8);
    }
    v
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

    let run = |impl_ref: &MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::String, "map_get");
        crate::perf_stats!(PERF_MAP_STRING_GETS += 1);
        // SAFETY: `key` is non-null and a valid C string per caller contract.
        unsafe { with_cstr_key(key, |map_key| map_get_cstr_traced(impl_ref, map_key)) }.unwrap_or(0)
    };
    // SAFETY: `with_map_ref_handle` validates `map_handle` before reading from it.
    unsafe { with_map_ref_handle(map_handle, run) }.unwrap_or(0)
}

/// Get an i64 value by C-string key, wrapped in an Optional tuple. Returns
/// `Some(value)` when the key is present and `None` otherwise, so callers
/// can distinguish "not present" from a legitimately-stored `0`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a valid null-terminated C string.
unsafe fn map_get_cstr(map_handle: i64, key: *const i8) -> Option<i64> {
    if key.is_null() {
        return None;
    }
    let run = |impl_ref: &MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::String, "map_get_opt");
        crate::perf_stats!(PERF_MAP_STRING_GETS += 1);
        // SAFETY: `key` is non-null and a valid C string per caller contract.
        unsafe { with_cstr_key(key, |map_key| get_keyed(impl_ref, map_key)) }.flatten()
    };
    // SAFETY: `with_map_ref_handle` validates `map_handle` before reading from it.
    unsafe { with_map_ref_handle(map_handle, run) }.flatten()
}

#[no_mangle]
pub unsafe extern "C" fn pith_map_get_cstr_opt(map_handle: i64, key: *const i8) -> i64 {
    // SAFETY: Preconditions forwarded from caller.
    optional_from(unsafe { map_get_cstr(map_handle, key) })
}

/// `pith_map_get_cstr_opt` as a register pair; see `optional_pair`.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_cstr_opt_pair(map_handle: i64, key: *const i8) -> ResultPair {
    // SAFETY: Preconditions forwarded from caller.
    optional_pair(unsafe { map_get_cstr(map_handle, key) })
}

#[inline(always)]
fn get_keyed_strict(impl_ref: &MapImpl, map_key: &KeyRef<'_>) -> i64 {
    match get_keyed(impl_ref, map_key) {
        Some(v) => v,
        None => strict_miss(map_key.display()),
    }
}

/// Get an i64 value by C-string key. If the key is not present, prints a
/// structured diagnostic to stderr and exits non-zero: the strict path
/// for `map[k]`, replacing the old silent-zero behavior. Callers that
/// want fallback behavior have `.get_default(k, d)` and `.get(k)`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_cstr_strict(map_handle: i64, key: *const i8) -> i64 {
    if key.is_null() {
        null_map_key_exit();
    }
    let run = |impl_ref: &MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::String, "map_get_strict");
        crate::perf_stats!(PERF_MAP_STRING_GETS += 1);
        // SAFETY: `key` is non-null and a valid C string per caller contract.
        unsafe { with_cstr_key(key, |map_key| get_keyed_strict(impl_ref, map_key)) }.unwrap_or(0)
    };
    // SAFETY: `with_map_ref_handle` validates `map_handle` before reading from it.
    match unsafe { with_map_ref_handle(map_handle, run) } {
        Some(v) => v,
        None => invalid_map_handle_exit(),
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

    let run = |impl_ref: &MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::String, "map_contains_key");
        crate::perf_stats!(PERF_MAP_STRING_CONTAINS += 1);
        // SAFETY: `key` is non-null and a valid C string per caller contract.
        let found = unsafe { with_cstr_key(key, |map_key| impl_ref.contains_key(map_key)) }
            .unwrap_or(false);
        i64::from(found)
    };
    // SAFETY: `with_map_ref_handle` validates `map_handle` before probing it.
    unsafe { with_map_ref_handle(map_handle, run) }.unwrap_or(0)
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
    let run = |impl_ref: &MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::String, "map_get_default");
        crate::perf_stats!(PERF_MAP_STRING_GETS += 1);
        // SAFETY: `key` is non-null and a valid C string per caller contract.
        unsafe { with_cstr_key(key, |map_key| get_keyed(impl_ref, map_key)) }.flatten()
    };
    // SAFETY: `with_map_ref_handle` validates `map_handle` before reading from it.
    unsafe { with_map_ref_handle(map_handle, run) }
        .flatten()
        .unwrap_or(default)
}

#[inline(always)]
fn map_get_ikey_impl(impl_ref: &MapImpl, key: i64) -> Option<i64> {
    crate::perf_stats!(PERF_MAP_INT_GETS += 1);
    if impl_ref.uses_int_values8() {
        crate::perf_count(&crate::PERF_MAP_INT_FAST_GETS, 1);
        impl_ref.get_int_value(key)
    } else {
        crate::perf_count(&crate::PERF_MAP_INT_FALLBACK_GETS, 1);
        get_keyed(impl_ref, &KeyRef::Int(key))
    }
}

/// Get value by integer key with a default if not found.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_default_ikey(map_handle: i64, key: i64, default: i64) -> i64 {
    // SAFETY: `with_map_ref_handle` validates `map_handle` before reading from it.
    unsafe { with_map_ref_handle(map_handle, |impl_ref| map_get_ikey_impl(impl_ref, key)) }
        .flatten()
        .unwrap_or(default)
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

    let run = |impl_ref: &mut MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::String, "map_remove");
        crate::perf_stats!(PERF_MAP_STRING_REMOVES += 1);
        let do_remove = |map_key: &KeyRef<'_>| remove_keyed(impl_ref, map_key);
        // SAFETY: `key` is non-null and a valid C string per caller contract.
        unsafe { with_cstr_key(key, do_remove) }.flatten()
    };
    // SAFETY: `with_map_mut_handle` validates `map_handle`; release displaced value after borrow ends.
    let displaced = unsafe { with_map_mut_handle(map_handle, run) }.flatten();
    if let Some((tag, raw)) = displaced {
        // SAFETY: `raw` was owned by `map_handle` under `tag`.
        unsafe { release_element(tag, raw) };
    }
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
    // SAFETY: Preconditions forwarded from caller.
    unsafe { insert_bkey_inner(map_handle, key, value, false) };
}

/// Insert a value the caller owns under a bytes key; see
/// `pith_map_insert_cstr_owned`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a bytes handle or null.
#[no_mangle]
pub unsafe extern "C" fn pith_map_insert_bkey_owned(map_handle: i64, key: i64, value: i64) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { insert_bkey_inner(map_handle, key, value, true) };
}

/// Insert a borrowed value of a named heap kind under a bytes key; see
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
    // SAFETY: `map_adopt_value_tag` validates `map_handle`.
    let Some(owns) = (unsafe { map_adopt_value_tag(map_handle, val_tag) }) else {
        return;
    };
    if !owns {
        // SAFETY: Caller passes a value matching `val_tag`.
        unsafe { retain_element(element_tag_from_code(val_tag as i32), value) };
    }
    // SAFETY: Preconditions forwarded from caller.
    unsafe { insert_bkey_inner(map_handle, key, value, false) };
}

/// Insert an owned value of a named heap kind under a bytes key; see
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
    // SAFETY: Preconditions forwarded from caller.
    unsafe {
        map_adopt_value_tag(map_handle, val_tag);
        insert_bkey_inner(map_handle, key, value, true);
    }
}

/// Get a value by bytes key. Returns 0 if the key is not found.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_bkey(map_handle: i64, key: i64) -> i64 {
    let run = |impl_ref: &MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_get_bkey");
        // SAFETY: `with_bytes_key` validates `key` and scopes its borrowed slice.
        unsafe { with_bytes_key(key, |map_key| get_keyed(impl_ref, map_key)) }.flatten()
    };
    // SAFETY: `with_map_ref_handle` validates `map_handle` before reading from it.
    unsafe { with_map_ref_handle(map_handle, run) }
        .flatten()
        .unwrap_or(0)
}

/// Get a value by bytes key, wrapped in an Optional tuple; see
/// `pith_map_get_cstr_opt`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
unsafe fn map_get_bkey(map_handle: i64, key: i64) -> Option<i64> {
    let run = |impl_ref: &MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_get_bkey_opt");
        // SAFETY: `with_bytes_key` validates `key` and scopes its borrowed slice.
        unsafe { with_bytes_key(key, |map_key| get_keyed(impl_ref, map_key)) }.flatten()
    };
    // SAFETY: `with_map_ref_handle` validates `map_handle` before reading from it.
    unsafe { with_map_ref_handle(map_handle, run) }.flatten()
}

#[no_mangle]
pub unsafe extern "C" fn pith_map_get_bkey_opt(map_handle: i64, key: i64) -> i64 {
    // SAFETY: Preconditions forwarded from caller.
    optional_from(unsafe { map_get_bkey(map_handle, key) })
}

/// `pith_map_get_bkey_opt` as a register pair; see `optional_pair`.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_bkey_opt_pair(map_handle: i64, key: i64) -> ResultPair {
    // SAFETY: Preconditions forwarded from caller.
    optional_pair(unsafe { map_get_bkey(map_handle, key) })
}

/// Get a value by bytes key, exiting with a diagnostic on a miss: the
/// strict path for `m[k]`, see `pith_map_get_cstr_strict`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_bkey_strict(map_handle: i64, key: i64) -> i64 {
    let run = |impl_ref: &MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_get_bkey_strict");
        // SAFETY: `with_bytes_key` validates `key` and scopes its borrowed slice.
        match unsafe { with_bytes_key(key, |map_key| get_keyed_strict(impl_ref, map_key)) } {
            Some(v) => v,
            None => strict_miss(String::from("<not a bytes value>")),
        }
    };
    // SAFETY: `with_map_ref_handle` validates `map_handle` before reading from it.
    match unsafe { with_map_ref_handle(map_handle, run) } {
        Some(v) => v,
        None => invalid_map_handle_exit(),
    }
}

/// Check if a bytes key exists in the map. Returns 1 if present, 0 otherwise.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_contains_bkey(map_handle: i64, key: i64) -> i64 {
    let run = |impl_ref: &MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_contains_bkey");
        // SAFETY: `with_bytes_key` validates `key` and scopes its borrowed slice.
        let found = unsafe { with_bytes_key(key, |map_key| impl_ref.contains_key(map_key)) }
            .unwrap_or(false);
        i64::from(found)
    };
    // SAFETY: `with_map_ref_handle` validates `map_handle` before probing it.
    unsafe { with_map_ref_handle(map_handle, run) }.unwrap_or(0)
}

/// Get value by bytes key with a default if not found.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_default_bkey(map_handle: i64, key: i64, default: i64) -> i64 {
    let run = |impl_ref: &MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_get_default_bkey");
        // SAFETY: `with_bytes_key` validates `key` and scopes its borrowed slice.
        unsafe { with_bytes_key(key, |map_key| get_keyed(impl_ref, map_key)) }.flatten()
    };
    // SAFETY: `with_map_ref_handle` validates `map_handle` before reading from it.
    unsafe { with_map_ref_handle(map_handle, run) }
        .flatten()
        .unwrap_or(default)
}

/// Remove an entry by bytes key.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_remove_bkey(map_handle: i64, key: i64) {
    let run = |impl_ref: &mut MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_remove_bkey");
        let do_remove = |map_key: &KeyRef<'_>| remove_keyed(impl_ref, map_key);
        // SAFETY: `with_bytes_key` validates `key` and scopes its borrowed slice.
        unsafe { with_bytes_key(key, do_remove) }.flatten()
    };
    // SAFETY: `with_map_mut_handle` validates `map_handle`; release displaced value after borrow ends.
    let displaced = unsafe { with_map_mut_handle(map_handle, run) }.flatten();
    if let Some((tag, raw)) = displaced {
        // SAFETY: `raw` was owned by `map_handle` under `tag`.
        unsafe { release_element(tag, raw) };
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
    // SAFETY: Preconditions forwarded from caller.
    unsafe { insert_ikey_inner(map_handle, key, value, false) };
}

/// Insert a value the caller owns under an integer key. The map takes the
/// caller's count instead of adding one of its own; see
/// `pith_map_insert_cstr_owned`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_insert_ikey_owned(map_handle: i64, key: i64, value: i64) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { insert_ikey_inner(map_handle, key, value, true) };
}

/// Int-keyed borrowed store of a named heap kind; see
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
    // SAFETY: `map_adopt_value_tag` validates `map_handle`.
    let Some(owns) = (unsafe { map_adopt_value_tag(map_handle, val_tag) }) else {
        return;
    };
    if !owns {
        // SAFETY: Caller passes a value matching `val_tag`.
        unsafe { retain_element(element_tag_from_code(val_tag as i32), value) };
    }
    // SAFETY: Preconditions forwarded from caller.
    unsafe { insert_ikey_inner(map_handle, key, value, false) };
}

/// Int-keyed owned store of a named heap kind; see
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
    // SAFETY: Preconditions forwarded from caller.
    unsafe {
        map_adopt_value_tag(map_handle, val_tag);
        insert_ikey_inner(map_handle, key, value, true);
    }
}

#[must_use]
#[inline(always)]
unsafe fn insert_ikey_impl(
    impl_ref: &mut MapImpl,
    key: i64,
    value: i64,
    takes_caller_count: bool,
) -> Option<(ListTypeTag, i64)> {
    crate::perf_stats!(PERF_MAP_INT_INSERTS += 1);
    if impl_ref.uses_int_values8() {
        crate::perf_count(&crate::PERF_MAP_INT_FAST_INSERTS, 1);
        impl_ref.insert_int_value(key, value);
        None
    } else {
        crate::perf_count(&crate::PERF_MAP_INT_FALLBACK_INSERTS, 1);
        if !takes_caller_count {
            // SAFETY: Caller passes a handle matching `impl_ref.val_tag`.
            unsafe { impl_ref.retain_value(value) };
        }
        impl_ref
            .insert(&KeyRef::Int(key), MapVal::Word(value))
            .and_then(|old| impl_ref.displaced_value_handle(old))
    }
}

#[inline(always)]
unsafe fn insert_ikey_inner(map_handle: i64, key: i64, value: i64, takes_caller_count: bool) {
    // SAFETY: `with_map_mut_handle` validates `map_handle`; release displaced value after borrow ends.
    let displaced = unsafe {
        with_map_mut_handle(map_handle, |impl_ref| {
            insert_ikey_impl(impl_ref, key, value, takes_caller_count)
        })
    }
    .flatten();
    if let Some((tag, raw)) = displaced {
        // SAFETY: `raw` was owned by `map_handle` under `tag`.
        unsafe { release_element(tag, raw) };
    }
}

/// Get an i64 value by integer key. Returns 0 if the key is not found.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_ikey(map_handle: i64, key: i64) -> i64 {
    // SAFETY: `with_map_ref_handle` validates `map_handle` before reading from it.
    unsafe { with_map_ref_handle(map_handle, |impl_ref| map_get_ikey_impl(impl_ref, key)) }
        .flatten()
        .unwrap_or(0)
}

/// Get an i64 value by integer key, wrapped in an Optional tuple. Returns
/// `Some(value)` when the key is present and `None` otherwise.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
unsafe fn map_get_ikey(map_handle: i64, key: i64) -> Option<i64> {
    // SAFETY: `with_map_ref_handle` validates `map_handle` before reading from it.
    unsafe { with_map_ref_handle(map_handle, |impl_ref| map_get_ikey_impl(impl_ref, key)) }
        .flatten()
}

#[no_mangle]
pub unsafe extern "C" fn pith_map_get_ikey_opt(map_handle: i64, key: i64) -> i64 {
    // SAFETY: Preconditions forwarded from caller.
    optional_from(unsafe { map_get_ikey(map_handle, key) })
}

/// `pith_map_get_ikey_opt` as a register pair; see `optional_pair`.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_ikey_opt_pair(map_handle: i64, key: i64) -> ResultPair {
    // SAFETY: Preconditions forwarded from caller.
    optional_pair(unsafe { map_get_ikey(map_handle, key) })
}

/// Strict integer-key lookup for `map[k]`. Aborts with a structured
/// diagnostic on miss instead of returning a silent zero.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_get_ikey_strict(map_handle: i64, key: i64) -> i64 {
    // SAFETY: `with_map_ref_handle` validates `map_handle` before reading from it.
    let Some(found) = (unsafe {
        with_map_ref_handle(map_handle, |impl_ref| map_get_ikey_impl(impl_ref, key))
    }) else {
        invalid_map_handle_exit();
    };
    match found {
        Some(v) => v,
        None => strict_miss(key.to_string()),
    }
}

#[inline(always)]
fn map_contains_ikey_impl(impl_ref: &MapImpl, key: i64) -> i64 {
    crate::perf_stats!(PERF_MAP_INT_CONTAINS += 1);

    let contains = if impl_ref.uses_int_values8() {
        crate::perf_count(&crate::PERF_MAP_INT_FAST_CONTAINS, 1);
        impl_ref.contains_int_key(key)
    } else {
        crate::perf_count(&crate::PERF_MAP_INT_FALLBACK_CONTAINS, 1);
        impl_ref.contains_key(&KeyRef::Int(key))
    };

    i64::from(contains)
}

/// Check if an integer key exists in the map. Returns 1 if present, 0 otherwise.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_contains_ikey(map_handle: i64, key: i64) -> i64 {
    // SAFETY: `with_map_ref_handle` validates `map_handle` before probing it.
    unsafe { with_map_ref_handle(map_handle, |impl_ref| map_contains_ikey_impl(impl_ref, key)) }
        .unwrap_or(0)
}

#[must_use]
#[inline(always)]
fn map_remove_ikey_impl(impl_ref: &mut MapImpl, key: i64) -> Option<(ListTypeTag, i64)> {
    crate::perf_stats!(PERF_MAP_INT_REMOVES += 1);
    if impl_ref.uses_int_values8() {
        crate::perf_count(&crate::PERF_MAP_INT_FAST_REMOVES, 1);
        impl_ref.remove_int_value(key);
        None
    } else {
        crate::perf_count(&crate::PERF_MAP_INT_FALLBACK_REMOVES, 1);
        impl_ref
            .remove(&KeyRef::Int(key))
            .and_then(|old| impl_ref.displaced_value_handle(old))
    }
}

/// Remove an entry by integer key (handle-based API).
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_remove_ikey(map_handle: i64, key: i64) {
    // SAFETY: `with_map_mut_handle` validates `map_handle`; release displaced value after borrow ends.
    let displaced = unsafe { with_map_mut_handle(map_handle, |impl_ref| map_remove_ikey_impl(impl_ref, key)) }
        .flatten();
    if let Some((tag, raw)) = displaced {
        // SAFETY: `raw` was owned by `map_handle` under `tag`.
        unsafe { release_element(tag, raw) };
    }
}
