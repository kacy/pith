//! Materialized key/value list views and deterministic map display formatting.

use super::{require_key_flavor, with_map_ref_handle, KeyType, MapImpl, MapKey};

fn map_keys_bkey_impl(impl_ref: &MapImpl) -> i64 {
    use crate::collections::list::{pith_list_new, pith_list_push_value_owned};

    require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_keys_bkey");
    // SAFETY: Allocates a fresh bytes-tagged list.
    let list = unsafe { pith_list_new(8, 5) };
    for key in impl_ref.keys() {
        if let MapKey::Bytes(bytes) = key {
            let handle = crate::bytes::pith_bytes_from_vec(bytes);
            // SAFETY: `list` is freshly allocated and `handle` is an owned bytes handle.
            unsafe { pith_list_push_value_owned(list, handle) };
        }
    }
    list.ptr as i64
}

/// Return all bytes keys as a bytes-tagged list of fresh bytes objects the
/// list owns. The counterpart to `pith_map_keys_cstr`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_keys_bkey(map_handle: i64) -> i64 {
    // SAFETY: `with_map_ref_handle` validates `map_handle` before iterating its keys.
    unsafe { with_map_ref_handle(map_handle, map_keys_bkey_impl) }
        .unwrap_or_else(|| unsafe { crate::collections::list::pith_list_new(8, 0) }.ptr as i64)
}
unsafe fn display_map_impl(impl_ref: &MapImpl, key_kind: i64, val_kind: i64, out: &mut String) {
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
        // SAFETY: Caller guarantees `val_kind` matches the map's stored values.
        unsafe { crate::display_value_for_map(out, *raw, val_kind) };
    }
}

/// Render a map as "{k: v, ...}" with sorted keys, for interpolation.
/// Sorting keeps the output deterministic; hashbrown iteration is not.
/// Kind codes match pith_display_list: 0=int, 1=float, 2=bool, 3=string.
#[no_mangle]
pub unsafe extern "C" fn pith_display_map(handle: i64, key_kind: i64, val_kind: i64) -> i64 {
    let mut out = String::from("{");
    // SAFETY: `with_map_ref_handle` validates `handle`; `val_kind` matches stored values.
    unsafe {
        with_map_ref_handle(handle, |impl_ref| {
            display_map_impl(impl_ref, key_kind, val_kind, &mut out)
        })
    };
    out.push('}');
    crate::pith_copy_bytes_to_cstring(out.as_bytes()) as i64
}
fn map_keys_cstr_impl(impl_ref: &MapImpl) -> i64 {
    use crate::collections::list::{pith_list_new, pith_list_push_value};

    require_key_flavor(impl_ref.key_type, KeyType::String, "map_keys");
    // string-tagged: the list owns the freshly copied key strings
    // SAFETY: Allocates a fresh string-tagged list.
    let list = unsafe { pith_list_new(8, 1) };

    for key in impl_ref.stored_keys() {
        if let MapKey::String(bytes) = key {
            let ptr = crate::pith_copy_bytes_to_cstring(bytes);
            // SAFETY: `list` and `ptr` are freshly allocated handles.
            unsafe {
                pith_list_push_value(list, ptr as i64);
                crate::pith_cstring_release(ptr as *const i8);
            }
        }
    }

    list.ptr as i64
}

/// Return all keys as a PithList of C-string pointers (each element is an i64
/// pointer to a newly allocated null-terminated string). The PithList pointer
/// is returned as i64.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_keys_cstr(map_handle: i64) -> i64 {
    // SAFETY: `with_map_ref_handle` validates `map_handle` before iterating its keys.
    unsafe { with_map_ref_handle(map_handle, map_keys_cstr_impl) }
        .unwrap_or_else(|| unsafe { crate::collections::list::pith_list_new(8, 0) }.ptr as i64)
}

fn map_keys_ikey_impl(impl_ref: &MapImpl) -> i64 {
    use crate::collections::list::{pith_list_new, pith_list_push_value};

    // int-tagged: 8-byte, non-heap values, matching pith_map_values_handle
    // SAFETY: Allocates a fresh primitive list.
    let list = unsafe { pith_list_new(8, 0) };

    for key in impl_ref.keys() {
        if let MapKey::Int(k) = key {
            // SAFETY: `list` is freshly allocated and primitive-tagged.
            unsafe { pith_list_push_value(list, k) };
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
    // SAFETY: `with_map_ref_handle` validates `map_handle` before iterating its keys.
    unsafe { with_map_ref_handle(map_handle, map_keys_ikey_impl) }
        .unwrap_or_else(|| unsafe { crate::collections::list::pith_list_new(8, 0) }.ptr as i64)
}
fn map_values_impl(impl_ref: &MapImpl) -> i64 {
    use crate::collections::list::{pith_list_new, pith_list_push_value};

    // a heap-valued map hands out its stored handles, so the list has to
    // carry the map's own value tag: push takes a count of its own and the
    // list's free cascades it back. an untagged list would leave the caller
    // holding raw pointers the map is free to evict and release out from
    // under.
    // SAFETY: Allocates a fresh list tagged with `impl_ref.val_tag`.
    let list = unsafe { pith_list_new(8, impl_ref.val_tag as i32) };

    for v in impl_ref.value_words() {
        // SAFETY: `list` is freshly allocated and `v` matches `impl_ref.val_tag`.
        unsafe { pith_list_push_value(list, v) };
    }

    list.ptr as i64
}

/// Return all values as a PithList (handle-based API). The PithList pointer
/// is returned as i64.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_values_handle(map_handle: i64) -> i64 {
    // SAFETY: `with_map_ref_handle` validates `map_handle` before iterating its values.
    unsafe { with_map_ref_handle(map_handle, map_values_impl) }
        .unwrap_or_else(|| unsafe { crate::collections::list::pith_list_new(8, 0) }.ptr as i64)
}
