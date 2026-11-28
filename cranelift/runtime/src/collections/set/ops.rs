//! Set constructors, CRUD operations, and list conversion views.

use super::{
    require_set_flavor as require_flavor, with_bytes_elem, with_cstr_elem, with_set_mut,
    with_set_mut_handle, with_set_ref, with_set_ref_handle, ElemRef, ElemType, PithSet,
    SetElement, SetImpl,
};

/// Create a new empty set
///
/// # Arguments
/// * `elem_type` - 0 for int elements, 1 for string elements, 2 for bytes
/// * `elem_size` - Size of each element in bytes
///
/// Elements are stored as owned copies (ints, or the set's own byte
/// buffers), so there is nothing element-wise to retain or release.
#[no_mangle]
pub unsafe extern "C" fn pith_set_new(elem_type: i32, elem_size: i64) -> PithSet {
    let set_impl = SetImpl::new(ElemType::from_code(elem_type), elem_size as usize);
    let boxed = Box::new(set_impl);
    let ptr = Box::into_raw(boxed) as *mut ();

    PithSet { ptr }
}

/// Get set length
#[no_mangle]
pub extern "C" fn pith_set_len(set: PithSet) -> i64 {
    // SAFETY: `with_set_ref` validates `set` before reading its length.
    unsafe { with_set_ref(set, |impl_ref| impl_ref.len() as i64) }.unwrap_or(0)
}

#[inline(always)]
fn set_contains_int_impl(impl_ref: &SetImpl, elem: i64) -> bool {
    matches!(impl_ref.elem_type, ElemType::Int) && impl_ref.contains(&ElemRef::Int(elem))
}

/// Check if set contains integer element
#[no_mangle]
pub extern "C" fn pith_set_contains_int(set: PithSet, elem: i64) -> bool {
    // SAFETY: `with_set_ref` validates `set` before probing it.
    unsafe { with_set_ref(set, |impl_ref| set_contains_int_impl(impl_ref, elem)) }.unwrap_or(false)
}

#[inline(always)]
fn set_remove_int_impl(impl_ref: &mut SetImpl, elem: i64) -> bool {
    matches!(impl_ref.elem_type, ElemType::Int) && impl_ref.remove(&ElemRef::Int(elem))
}

/// Remove integer element from set
///
/// Returns true if element was present and removed
#[no_mangle]
pub unsafe extern "C" fn pith_set_remove_int(set: *mut PithSet, elem: i64) -> bool {
    if set.is_null() {
        return false;
    }
    // SAFETY: `set` is non-null per caller contract.
    let set_val = unsafe { *set };
    // SAFETY: `with_set_mut` validates `set_val` before mutating it.
    unsafe { with_set_mut(set_val, |impl_ref| set_remove_int_impl(impl_ref, elem)) }
        .unwrap_or(false)
}

/// Clear all elements from set
#[no_mangle]
pub unsafe extern "C" fn pith_set_clear(set: *mut PithSet) {
    if set.is_null() {
        return;
    }
    // SAFETY: `set` is non-null per caller contract.
    let set_val = unsafe { *set };
    // SAFETY: `with_set_mut` validates `set_val` before clearing it.
    unsafe { with_set_mut(set_val, SetImpl::clear) };
}
fn set_to_list_int_impl(impl_ref: &SetImpl) -> crate::collections::list::PithList {
    use crate::collections::list::{pith_list_new, pith_list_push, PithList};

    if !matches!(impl_ref.elem_type, ElemType::Int) {
        return PithList {
            ptr: std::ptr::null_mut(),
        };
    }

    // SAFETY: Allocates a fresh primitive int list.
    let mut list = unsafe { pith_list_new(std::mem::size_of::<i64>() as i64, 0) };

    for elem in impl_ref.iter() {
        if let SetElement::Int(n) = elem {
            let n_ptr = n as *const i64 as *const u8;
            // SAFETY: `list` is valid and `n_ptr` points to a readable `i64`.
            unsafe { pith_list_push(&mut list, n_ptr, std::mem::size_of::<i64>() as i64) };
        }
    }

    list
}

/// Convert set to list (for int elements)
///
/// # Safety
/// Returns a new list that must be released
#[no_mangle]
pub unsafe extern "C" fn pith_set_to_list_int(set: PithSet) -> crate::collections::list::PithList {
    // SAFETY: `with_set_ref` validates `set` before iterating its elements.
    unsafe { with_set_ref(set, set_to_list_int_impl) }.unwrap_or(
        crate::collections::list::PithList {
            ptr: std::ptr::null_mut(),
        },
    )
}

fn set_to_list_string_impl(impl_ref: &SetImpl) -> crate::collections::list::PithList {
    use crate::collections::list::{pith_list_new, pith_list_push_value, PithList};

    if !matches!(impl_ref.elem_type, ElemType::String) {
        return PithList {
            ptr: std::ptr::null_mut(),
        };
    }

    // string-tagged: the list owns the freshly copied element strings
    // SAFETY: Allocates a fresh string-tagged list.
    let list = unsafe { pith_list_new(8, 1) };

    for elem in impl_ref.iter() {
        if let SetElement::String(bytes) = elem {
            let ptr = crate::pith_copy_bytes_to_cstring(bytes);
            // SAFETY: `list` and `ptr` are freshly allocated handles.
            unsafe {
                pith_list_push_value(list, ptr as i64);
                crate::pith_cstring_release(ptr as *const i8);
            }
        }
    }

    list
}

/// Convert set to list (for string elements)
///
/// # Safety
/// Returns a new list that must be released
#[no_mangle]
pub unsafe extern "C" fn pith_set_to_list_string(
    set: PithSet,
) -> crate::collections::list::PithList {
    // SAFETY: `with_set_ref` validates `set` before iterating its elements.
    unsafe { with_set_ref(set, set_to_list_string_impl) }.unwrap_or(
        crate::collections::list::PithList {
            ptr: std::ptr::null_mut(),
        },
    )
}

fn set_to_list_cstr_impl(impl_ref: &SetImpl) -> i64 {
    require_flavor(impl_ref.elem_type, ElemType::String, "set_to_list");
    set_to_list_string_impl(impl_ref).ptr as i64
}

/// Convert a string set handle to a list handle of C-string pointers.
///
/// Returns the raw `ListImpl` pointer as i64 to match the Cranelift collection ABI.
#[no_mangle]
pub unsafe extern "C" fn pith_set_to_list_cstr(set_handle: i64) -> i64 {
    // SAFETY: `with_set_ref_handle` validates `set_handle` before converting it.
    unsafe { with_set_ref_handle(set_handle, set_to_list_cstr_impl) }
        .unwrap_or_else(|| unsafe { crate::collections::list::pith_list_new(8, 0) }.ptr as i64)
}

/// Convert an int set handle to a list handle of i64 elements.
///
/// Returns the raw `ListImpl` pointer as i64 to match the Cranelift collection ABI.
#[no_mangle]
pub unsafe extern "C" fn pith_set_to_list_int_handle(set_handle: i64) -> i64 {
    // SAFETY: `with_set_ref_handle` validates `set_handle` before converting it.
    unsafe { with_set_ref_handle(set_handle, |impl_ref| set_to_list_int_impl(impl_ref).ptr as i64) }
        .unwrap_or_else(|| unsafe { crate::collections::list::pith_list_new(8, 0) }.ptr as i64)
}

// ---------------------------------------------------------------------------
// Handle-based C-string variants for Cranelift codegen
// ---------------------------------------------------------------------------

#[inline(always)]
fn set_new_handle_impl(elem_type: i32) -> i64 {
    let set_impl = SetImpl::new(ElemType::from_code(elem_type), 8);
    let boxed = Box::new(set_impl);
    let ptr = Box::into_raw(boxed);
    ptr as i64
}

/// Create a new string set (handle-based). Returns SetImpl pointer as i64.
/// Create a new string set with default settings
#[no_mangle]
pub unsafe extern "C" fn pith_set_new_default() -> i64 {
    set_new_handle_impl(1)
}

#[no_mangle]
pub unsafe extern "C" fn pith_set_new_int() -> i64 {
    set_new_handle_impl(0)
}

/// Create a set of `Bytes` elements, compared and hashed by content.
#[no_mangle]
pub unsafe extern "C" fn pith_set_new_bytes() -> i64 {
    set_new_handle_impl(2)
}

#[no_mangle]
pub unsafe extern "C" fn pith_set_new_handle(elem_type: i32) -> i64 {
    set_new_handle_impl(elem_type)
}

/// Get set length (handle-based).
#[no_mangle]
pub unsafe extern "C" fn pith_set_len_handle(set_handle: i64) -> i64 {
    // SAFETY: `with_set_ref_handle` validates `set_handle` before reading its length.
    unsafe { with_set_ref_handle(set_handle, |impl_ref| impl_ref.len() as i64) }.unwrap_or(0)
}

/// Insert a C-string element into the set. Returns 1 if newly inserted, 0 if already present.
#[no_mangle]
pub unsafe extern "C" fn pith_set_add_cstr(set_handle: i64, elem: *const i8) -> i64 {
    if elem.is_null() {
        return 0;
    }
    let run = |impl_ref: &mut SetImpl| {
        require_flavor(impl_ref.elem_type, ElemType::String, "set_add");
        // SAFETY: `elem` is non-null and a valid C string per caller contract.
        let added = unsafe { with_cstr_elem(elem, |set_elem| impl_ref.insert(set_elem)) }
            .unwrap_or(false);
        i64::from(added)
    };
    // SAFETY: `with_set_mut_handle` validates `set_handle` before mutating it.
    unsafe { with_set_mut_handle(set_handle, run) }.unwrap_or(0)
}

#[inline(always)]
fn set_add_int_impl(impl_ref: &mut SetImpl, elem: i64) -> i64 {
    if !matches!(impl_ref.elem_type, ElemType::Int) {
        return 0;
    }
    i64::from(impl_ref.insert(&ElemRef::Int(elem)))
}

/// Insert an integer element into the set. Returns 1 if newly inserted, 0 if already present.
#[no_mangle]
pub unsafe extern "C" fn pith_set_add_int_handle(set_handle: i64, elem: i64) -> i64 {
    // SAFETY: `with_set_mut_handle` validates `set_handle` before mutating it.
    unsafe { with_set_mut_handle(set_handle, |impl_ref| set_add_int_impl(impl_ref, elem)) }
        .unwrap_or(0)
}

/// Check if a C-string element exists in the set. Returns 1 if present, 0 otherwise.
#[no_mangle]
pub unsafe extern "C" fn pith_set_contains_cstr(set_handle: i64, elem: *const i8) -> i64 {
    if elem.is_null() {
        return 0;
    }
    let run = |impl_ref: &SetImpl| {
        require_flavor(impl_ref.elem_type, ElemType::String, "set_contains");
        // SAFETY: `elem` is non-null and a valid C string per caller contract.
        let found = unsafe { with_cstr_elem(elem, |set_elem| impl_ref.contains(set_elem)) }
            .unwrap_or(false);
        i64::from(found)
    };
    // SAFETY: `with_set_ref_handle` validates `set_handle` before probing it.
    unsafe { with_set_ref_handle(set_handle, run) }.unwrap_or(0)
}

/// Check if an integer element exists in the set. Returns 1 if present, 0 otherwise.
#[no_mangle]
pub unsafe extern "C" fn pith_set_contains_int_handle(set_handle: i64, elem: i64) -> i64 {
    // SAFETY: `with_set_ref_handle` validates `set_handle` before probing it.
    let found = unsafe { with_set_ref_handle(set_handle, |impl_ref| set_contains_int_impl(impl_ref, elem)) }
        .unwrap_or(false);
    i64::from(found)
}

/// Remove a C-string element from the set.
#[no_mangle]
pub unsafe extern "C" fn pith_set_remove_cstr(set_handle: i64, elem: *const i8) {
    if elem.is_null() {
        return;
    }
    let run = |impl_ref: &mut SetImpl| {
        require_flavor(impl_ref.elem_type, ElemType::String, "set_remove");
        // SAFETY: `elem` is non-null and a valid C string per caller contract.
        unsafe { with_cstr_elem(elem, |set_elem| impl_ref.remove(set_elem)) };
    };
    // SAFETY: `with_set_mut_handle` validates `set_handle` before mutating it.
    unsafe { with_set_mut_handle(set_handle, run) };
}

// ---------------------------------------------------------------------------
// Bytes-element variants: the content-hashing flavor
// ---------------------------------------------------------------------------
//
// a `Bytes` element is stored the way a string element is: as the set's own
// copy of the content. the handle the caller passes is only read, never
// retained, so the caller keeps whatever count it holds on it, and a set
// releases nothing element-wise when it frees. iteration hands out fresh
// bytes objects the way string iteration hands out fresh strings, owned by
// the list `pith_set_to_list_bytes` builds.

/// Insert a bytes element by content. Returns 1 if newly inserted, 0 if it
/// was already present or the handle is not a bytes object.
#[no_mangle]
pub unsafe extern "C" fn pith_set_add_bytes(set_handle: i64, elem: i64) -> i64 {
    let run = |impl_ref: &mut SetImpl| {
        require_flavor(impl_ref.elem_type, ElemType::Bytes, "set_add_bytes");
        // SAFETY: `with_bytes_elem` validates `elem` before borrowing its bytes.
        let added = unsafe { with_bytes_elem(elem, |set_elem| impl_ref.insert(set_elem)) }
            .unwrap_or(false);
        i64::from(added)
    };
    // SAFETY: `with_set_mut_handle` validates `set_handle` before mutating it.
    unsafe { with_set_mut_handle(set_handle, run) }.unwrap_or(0)
}

/// Check membership of a bytes element by content. Returns 1 if present.
#[no_mangle]
pub unsafe extern "C" fn pith_set_contains_bytes(set_handle: i64, elem: i64) -> i64 {
    let run = |impl_ref: &SetImpl| {
        require_flavor(impl_ref.elem_type, ElemType::Bytes, "set_contains_bytes");
        // SAFETY: `with_bytes_elem` validates `elem` before borrowing its bytes.
        let found = unsafe { with_bytes_elem(elem, |set_elem| impl_ref.contains(set_elem)) }
            .unwrap_or(false);
        i64::from(found)
    };
    // SAFETY: `with_set_ref_handle` validates `set_handle` before probing it.
    unsafe { with_set_ref_handle(set_handle, run) }.unwrap_or(0)
}

/// Remove a bytes element by content.
#[no_mangle]
pub unsafe extern "C" fn pith_set_remove_bytes(set_handle: i64, elem: i64) {
    let run = |impl_ref: &mut SetImpl| {
        require_flavor(impl_ref.elem_type, ElemType::Bytes, "set_remove_bytes");
        // SAFETY: `with_bytes_elem` validates `elem` before borrowing its bytes.
        unsafe { with_bytes_elem(elem, |set_elem| impl_ref.remove(set_elem)) };
    };
    // SAFETY: `with_set_mut_handle` validates `set_handle` before mutating it.
    unsafe { with_set_mut_handle(set_handle, run) };
}

fn set_to_list_bytes_impl(impl_ref: &SetImpl) -> i64 {
    use crate::collections::list::{pith_list_new, pith_list_push_value_owned};

    require_flavor(impl_ref.elem_type, ElemType::Bytes, "set_to_list_bytes");
    // SAFETY: Allocates a fresh bytes-tagged list.
    let list = unsafe { pith_list_new(8, 5) };
    for elem in impl_ref.iter() {
        if let SetElement::Bytes(bytes) = elem {
            let handle = crate::bytes::pith_bytes_from_vec(bytes.clone());
            // SAFETY: `list` is freshly allocated and `handle` is an owned bytes handle.
            unsafe { pith_list_push_value_owned(list, handle) };
        }
    }
    list.ptr as i64
}

/// Convert a bytes set handle to a list handle of fresh bytes objects. The
/// list is bytes-tagged and owns each element, so releasing the list
/// releases them; the set's own storage is untouched.
#[no_mangle]
pub unsafe extern "C" fn pith_set_to_list_bytes(set_handle: i64) -> i64 {
    // SAFETY: `with_set_ref_handle` validates `set_handle` before iterating its elements.
    unsafe { with_set_ref_handle(set_handle, set_to_list_bytes_impl) }
        .unwrap_or_else(|| unsafe { crate::collections::list::pith_list_new(8, 0) }.ptr as i64)
}

/// Remove an integer element from the set.
#[no_mangle]
pub unsafe extern "C" fn pith_set_remove_int_handle(set_handle: i64, elem: i64) {
    // SAFETY: `with_set_mut_handle` validates `set_handle` before mutating it.
    unsafe { with_set_mut_handle(set_handle, |impl_ref| set_remove_int_impl(impl_ref, elem)) };
}

/// Clear all elements from set (handle-based).
#[no_mangle]
pub unsafe extern "C" fn pith_set_clear_handle(set_handle: i64) {
    // SAFETY: `with_set_mut_handle` validates `set_handle` before clearing it.
    unsafe { with_set_mut_handle(set_handle, SetImpl::clear) };
}

/// Check if set is empty (handle-based). Returns 1 if empty, 0 otherwise.
#[no_mangle]
pub unsafe extern "C" fn pith_set_is_empty_handle(set_handle: i64) -> i64 {
    // SAFETY: `with_set_ref_handle` validates `set_handle` before checking its length.
    let empty = unsafe { with_set_ref_handle(set_handle, |impl_ref| impl_ref.len() == 0) }
        .unwrap_or(true);
    i64::from(empty)
}
