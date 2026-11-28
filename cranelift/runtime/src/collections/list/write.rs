//! List mutation entrypoints: push, insert, set, remove, clear, reverse.

use super::read::strict_index_bad_handle;
use super::{
    displaced_element, element_tag_from_code, release_element, release_taken_elements,
    retain_element, take_elements_for_release, with_list_mut, with_list_ref, ListImpl, ListTypeTag,
    PithList,
};

#[inline(always)]
unsafe fn list_push_bytes_impl(impl_ref: &mut ListImpl, elem: *const u8, elem_size: i64) {
    crate::perf_stats!(PERF_LIST_PUSHES += 1);

    // Verify element size matches
    if impl_ref.elem_size != elem_size as usize {
        eprintln!("pith: list element size mismatch");
        return;
    }

    // Copy element data
    // SAFETY: Caller guarantees `elem` points to `elem_size` readable bytes.
    let elem_slice = unsafe { std::slice::from_raw_parts(elem, elem_size as usize) };

    // The list holds one count per heap element, released again when the
    // element leaves the list.
    // SAFETY: Heap elements carry a valid handle for `impl_ref.type_tag`.
    unsafe { retain_element(impl_ref.type_tag, ListImpl::read_i64_padded(elem_slice)) };

    impl_ref.push(elem_slice);
}

/// Push element to end of list
///
/// # Safety
/// * `elem` must point to valid data of size `elem_size`
#[no_mangle]
pub unsafe extern "C" fn pith_list_push(list: *mut PithList, elem: *const u8, elem_size: i64) {
    if list.is_null() || elem.is_null() {
        return;
    }

    // SAFETY: `list` is non-null per caller contract.
    let list_val = unsafe { *list };
    // SAFETY: `with_list_mut` validates `list_val`; `elem` is readable for `elem_size` bytes.
    unsafe { with_list_mut(list_val, |impl_ref| list_push_bytes_impl(impl_ref, elem, elem_size)) };
}

/// Push an i64-sized value into a list using the list handle directly.
/// This is a simpler ABI used by generated method calls. The caller keeps
/// its own count and the list takes one of its own.
#[no_mangle]
pub unsafe extern "C" fn pith_list_push_value(list: PithList, value: i64) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { push_value_inner(list, value, false) };
}

/// Push a value the caller owns and is about to stop tracking: a freshly
/// built string, a collection literal, a transferred call result. The list
/// keeps the count the caller was holding instead of adding a second one,
/// which is what leaves a directly stored temporary with exactly one owner.
///
/// An untagged list holds no counts at all, so there is nothing for it to
/// take: the caller's count stays outstanding, exactly as it did before.
/// That is a leak rather than an element the list cannot keep alive, which
/// is why the ownership decision lives here and not in the emitter.
#[no_mangle]
pub unsafe extern "C" fn pith_list_push_value_owned(list: PithList, value: i64) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { push_value_inner(list, value, true) };
}

#[inline(always)]
unsafe fn push_value_kind_impl(impl_ref: &mut ListImpl, value: i64, elem_tag: i64) {
    crate::perf_stats!(PERF_LIST_PUSHES += 1);
    // SAFETY: Caller passes a value matching `elem_tag`.
    unsafe { borrowed_store_retains(impl_ref, value, elem_tag) };
    impl_ref.push_value(value);
}

/// Push a borrowed value of a named heap kind. The list adopts the kind when
/// it has none (`ListImpl::adopt_element_tag`) and takes the one count it
/// needs, which is what lets the caller stop adding a compensating one.
///
/// # Safety
/// * `list` must be a valid list handle.
#[no_mangle]
pub unsafe extern "C" fn pith_list_push_value_kind(list: PithList, value: i64, elem_tag: i64) {
    // SAFETY: `with_list_mut` validates `list` before mutating it.
    unsafe { with_list_mut(list, |impl_ref| push_value_kind_impl(impl_ref, value, elem_tag)) };
}

#[inline(always)]
unsafe fn insert_value_kind_impl(impl_ref: &mut ListImpl, index: i64, value: i64, elem_tag: i64) {
    crate::perf_stats!(PERF_LIST_INSERTS += 1);
    // SAFETY: Caller passes a value matching `elem_tag`.
    unsafe { borrowed_store_retains(impl_ref, value, elem_tag) };
    impl_ref.insert_value_at(index.max(0) as usize, value);
}

/// Insert a borrowed value of a named heap kind at an index.
///
/// # Safety
/// * `list` must be a valid list handle.
#[no_mangle]
pub unsafe extern "C" fn pith_list_insert_value_kind(
    list: PithList,
    index: i64,
    value: i64,
    elem_tag: i64,
) {
    // SAFETY: `with_list_mut` validates `list` before mutating it.
    unsafe {
        with_list_mut(list, |impl_ref| {
            insert_value_kind_impl(impl_ref, index, value, elem_tag)
        })
    };
}

#[must_use]
#[inline(always)]
unsafe fn set_value_kind_impl(
    impl_ref: &mut ListImpl,
    index: i64,
    value: i64,
    elem_tag: i64,
) -> Option<(ListTypeTag, i64)> {
    crate::perf_stats!(PERF_LIST_SETS += 1);
    let idx = index as usize;
    if idx >= impl_ref.len() {
        return None;
    }
    // retain before dropping the count on what this displaces, so `l[i] = l[i]`
    // cannot free the value mid-assignment.
    // SAFETY: Caller passes a value matching `elem_tag`.
    unsafe { borrowed_store_retains(impl_ref, value, elem_tag) };
    let displaced = displaced_element(impl_ref, idx);
    impl_ref.set_value(idx, value);
    displaced.map(|raw| (impl_ref.type_tag, raw))
}

/// Overwrite an element with a borrowed value of a named heap kind. An index
/// store always lands in a non-empty list, so this never adopts; it either
/// finds the kind it was handed or falls back like the others.
///
/// # Safety
/// * `list` must be a valid list handle.
#[no_mangle]
pub unsafe extern "C" fn pith_list_set_value_kind(
    list: PithList,
    index: i64,
    value: i64,
    elem_tag: i64,
) {
    // SAFETY: `with_list_mut` validates `list`; release displaced element after borrow ends.
    let displaced = unsafe {
        with_list_mut(list, |impl_ref| {
            set_value_kind_impl(impl_ref, index, value, elem_tag)
        })
    }
    .flatten();
    if let Some((tag, raw)) = displaced {
        // SAFETY: `raw` was owned by `list` under `tag`.
        unsafe { release_element(tag, raw) };
    }
}

/// The counts a borrowed value needs before it is stored under `elem_tag`.
///
/// The list takes its own count exactly as the plain store does. When it
/// cannot own the kind (a non-empty list built for something else), the
/// count the emitter used to add at the call site is added here instead, so
/// the element still outlives the caller's local. That fallback is the leak
/// this change removes everywhere else, and never a freed value.
unsafe fn borrowed_store_retains(impl_ref: &mut ListImpl, value: i64, elem_tag: i64) {
    let tag = element_tag_from_code(elem_tag as i32);
    let owns = impl_ref.adopt_element_tag(tag);
    // SAFETY: Caller passes a value matching `tag` (and `impl_ref.type_tag` if adopted).
    unsafe { retain_element(impl_ref.type_tag, value) };
    if !owns {
        // SAFETY: Caller passes a value matching `tag`.
        unsafe { retain_element(tag, value) };
    }
}

#[inline(always)]
unsafe fn push_value_impl(impl_ref: &mut ListImpl, value: i64, takes_caller_count: bool) {
    crate::perf_stats!(PERF_LIST_PUSHES += 1);
    // a list of plain values owns no counts, which is the common push: the
    // retain is a call, so it is only made for a kind that has one
    if !takes_caller_count && impl_ref.type_tag != ListTypeTag::Primitive {
        // SAFETY: Caller passes a value matching `impl_ref.type_tag`.
        unsafe { retain_element(impl_ref.type_tag, value) };
    }
    impl_ref.push_value(value);
}

#[inline(always)]
unsafe fn push_value_inner(list: PithList, value: i64, takes_caller_count: bool) {
    // SAFETY: `with_list_mut` validates `list` before mutating it.
    unsafe { with_list_mut(list, |impl_ref| push_value_impl(impl_ref, value, takes_caller_count)) };
}

/// Insert an element at an index, shifting the tail right. A negative
/// index clamps to the front, past-the-end clamps to a push. The list
/// takes an owner's count for tagged element kinds, like push.
#[no_mangle]
pub unsafe extern "C" fn pith_list_insert_value(list: PithList, index: i64, value: i64) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { insert_value_inner(list, index, value, false) };
}

/// Insert a value the caller owns and is about to stop tracking. The list
/// keeps the count the caller was holding instead of adding a second one,
/// the same transfer `pith_list_push_value_owned` performs, and, like it,
/// takes nothing when the list is untagged, so the caller's count stays
/// outstanding rather than leaving the list pointing at a freed element.
#[no_mangle]
pub unsafe extern "C" fn pith_list_insert_value_owned(list: PithList, index: i64, value: i64) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { insert_value_inner(list, index, value, true) };
}

#[inline(always)]
unsafe fn insert_value_impl(
    impl_ref: &mut ListImpl,
    index: i64,
    value: i64,
    takes_caller_count: bool,
) {
    crate::perf_stats!(PERF_LIST_INSERTS += 1);
    if !takes_caller_count {
        // SAFETY: Caller passes a value matching `impl_ref.type_tag`.
        unsafe { retain_element(impl_ref.type_tag, value) };
    }
    impl_ref.insert_value_at(index.max(0) as usize, value);
}

unsafe fn insert_value_inner(list: PithList, index: i64, value: i64, takes_caller_count: bool) {
    // SAFETY: `with_list_mut` validates `list` before mutating it.
    unsafe {
        with_list_mut(list, |impl_ref| {
            insert_value_impl(impl_ref, index, value, takes_caller_count)
        })
    };
}

/// Set element at index (value-based API, stores i64).
#[no_mangle]
pub unsafe extern "C" fn pith_list_set_value(list: PithList, index: i64, value: i64) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { set_value_inner(list, index, value, false) };
}

/// Set element at index with a value the caller owns. The list takes the
/// caller's count rather than adding one, the same transfer
/// `pith_list_push_value_owned` performs.
#[no_mangle]
pub unsafe extern "C" fn pith_list_set_value_owned(list: PithList, index: i64, value: i64) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { set_value_inner(list, index, value, true) };
}

#[must_use]
#[inline(always)]
unsafe fn set_scalar_impl(
    impl_ref: &mut ListImpl,
    index: i64,
    value: i64,
) -> Option<(ListTypeTag, i64)> {
    if impl_ref.type_tag != ListTypeTag::Primitive {
        // SAFETY: Preconditions forwarded from caller; reuse the active `&mut ListImpl`.
        return unsafe { set_value_impl(impl_ref, index, value, false) };
    }
    crate::perf_stats!(PERF_LIST_SETS += 1);
    let idx = index as usize;
    if idx < impl_ref.len() {
        impl_ref.set_value(idx, value);
    }
    None
}

/// Set an element the compiler knows is a scalar (an Int, Float, Bool or a
/// plain enum tag). A list of scalars is primitive, so the word goes in as
/// is, with nothing to retain or release: the call never frees anything or
/// runs a `drop`. The codegen's loop pass relies on that to treat the call
/// as leaving every list header alone. A list tagged for counted elements,
/// which a scalar never goes into, would still take the counting path.
#[no_mangle]
pub unsafe extern "C" fn pith_list_set_scalar(list: PithList, index: i64, value: i64) {
    // SAFETY: `with_list_mut` validates `list`; release displaced element after borrow ends.
    let displaced = unsafe { with_list_mut(list, |impl_ref| set_scalar_impl(impl_ref, index, value)) }
        .flatten();
    if let Some((tag, raw)) = displaced {
        // SAFETY: `raw` was owned by `list` under `tag`.
        unsafe { release_element(tag, raw) };
    }
}

#[must_use]
#[inline(always)]
unsafe fn set_value_impl(
    impl_ref: &mut ListImpl,
    index: i64,
    value: i64,
    takes_caller_count: bool,
) -> Option<(ListTypeTag, i64)> {
    crate::perf_stats!(PERF_LIST_SETS += 1);
    let idx = index as usize;
    if idx >= impl_ref.len() {
        // an index outside the list stores nothing, so an owned value the
        // caller handed over has nobody else to release it.
        return if takes_caller_count {
            Some((impl_ref.type_tag, value))
        } else {
            None
        };
    }
    // retain the incoming element before dropping the count on what it
    // displaces, so `l[i] = l[i]` cannot free the value mid-assignment.
    // an owned value is a buffer nobody else holds, so it can never be
    // what it displaces and needs no count of its own.
    if !takes_caller_count {
        // SAFETY: Caller passes a value matching `impl_ref.type_tag`.
        unsafe { retain_element(impl_ref.type_tag, value) };
    }
    let displaced = displaced_element(impl_ref, idx);
    impl_ref.set_value(idx, value);
    displaced.map(|raw| (impl_ref.type_tag, raw))
}

unsafe fn set_value_inner(list: PithList, index: i64, value: i64, takes_caller_count: bool) {
    // SAFETY: `with_list_mut` validates `list`; release displaced element after borrow ends.
    let displaced = unsafe {
        with_list_mut(list, |impl_ref| {
            set_value_impl(impl_ref, index, value, takes_caller_count)
        })
    }
    .flatten();
    if let Some((tag, raw)) = displaced {
        // SAFETY: `raw` was owned by `list` (or transferred by caller on OOB) under `tag`.
        unsafe { release_element(tag, raw) };
    }
}

// The strict stores behind `xs[i] = v`. The stores above ignore an index
// outside the list, which `.set(i, v)` and std internals rely on; an index
// assignment is a program bug there and stops with the same error a read
// gives. The codegen inlines the in-bounds store of a primitive list and only
// calls these on its slow path, so the extra check costs nothing on the hot
// path.
#[cold]
#[inline(never)]
fn strict_store_out_of_bounds(index: i64, len: i64) -> ! {
    crate::failure::fail(format!(
        "list index out of bounds: {} for list of length {}",
        index, len
    ))
}

#[inline]
unsafe fn check_store_index(list: PithList, index: i64) {
    // SAFETY: `with_list_ref` validates `list` before reading its length.
    let Some(len) = (unsafe { with_list_ref(list, |impl_ref| impl_ref.len() as i64) }) else {
        strict_index_bad_handle();
    };
    if index < 0 || index >= len {
        strict_store_out_of_bounds(index, len);
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_set_value_strict(list: PithList, index: i64, value: i64) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe {
        check_store_index(list, index);
        set_value_inner(list, index, value, false);
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_set_value_owned_strict(list: PithList, index: i64, value: i64) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe {
        check_store_index(list, index);
        set_value_inner(list, index, value, true);
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_set_scalar_strict(list: PithList, index: i64, value: i64) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe {
        check_store_index(list, index);
        pith_list_set_scalar(list, index, value);
    }
}

// The checked stores behind `xs[i] = v` inside a fallible function, where a
// bad index fails the function instead of stopping the process. Each returns
// 1 when it stored and 0 for an index outside the list (or a dead handle),
// and on 0 it leaves the value with the caller, whose miss arm releases an
// owned one before it fails. The codegen inlines the in-bounds store of a
// primitive list and hands back its own bounds test as the flag, so the store
// and the check are one compare.

/// Whether `index` is inside the list behind `list`; false for a dead handle.
unsafe fn store_index_in_bounds(list: PithList, index: i64) -> bool {
    // SAFETY: `with_list_ref` validates `list` before checking bounds.
    unsafe { with_list_ref(list, |impl_ref| index >= 0 && (index as usize) < impl_ref.len()) }
        .unwrap_or(false)
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_set_value_checked(list: PithList, index: i64, value: i64) -> i64 {
    // SAFETY: Preconditions forwarded from caller.
    if !unsafe { store_index_in_bounds(list, index) } {
        return 0;
    }
    // SAFETY: Preconditions forwarded from caller.
    unsafe { set_value_inner(list, index, value, false) };
    1
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_set_value_owned_checked(
    list: PithList,
    index: i64,
    value: i64,
) -> i64 {
    // SAFETY: Preconditions forwarded from caller.
    if !unsafe { store_index_in_bounds(list, index) } {
        return 0;
    }
    // SAFETY: Preconditions forwarded from caller.
    unsafe { set_value_inner(list, index, value, true) };
    1
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_set_scalar_checked(list: PithList, index: i64, value: i64) -> i64 {
    // SAFETY: Preconditions forwarded from caller.
    if !unsafe { store_index_in_bounds(list, index) } {
        return 0;
    }
    // SAFETY: Preconditions forwarded from caller.
    unsafe { pith_list_set_scalar(list, index, value) };
    1
}

/// # Safety
/// * `list` must be a valid list handle.
#[no_mangle]
pub unsafe extern "C" fn pith_list_set_value_kind_strict(
    list: PithList,
    index: i64,
    value: i64,
    elem_tag: i64,
) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe {
        check_store_index(list, index);
        pith_list_set_value_kind(list, index, value, elem_tag);
    }
}
#[inline(always)]
unsafe fn list_set_bytes_impl(
    impl_ref: &mut ListImpl,
    index: i64,
    elem: *const u8,
    elem_size: i64,
) -> (bool, Option<(ListTypeTag, i64)>) {
    crate::perf_stats!(PERF_LIST_SETS += 1);

    if index < 0 || index >= impl_ref.len() as i64 {
        return (false, None);
    }

    if impl_ref.elem_size != elem_size as usize {
        eprintln!("pith: list element size mismatch");
        return (false, None);
    }

    // Copy new element
    // SAFETY: Caller guarantees `elem` points to `elem_size` readable bytes.
    let elem_slice = unsafe { std::slice::from_raw_parts(elem, elem_size as usize) };

    // retain the incoming element before dropping the count on what it
    // displaces, so `l[i] = l[i]` cannot free the value mid-assignment
    // SAFETY: Heap elements carry a valid handle for `impl_ref.type_tag`.
    unsafe { retain_element(impl_ref.type_tag, ListImpl::read_i64_padded(elem_slice)) };

    let displaced = displaced_element(impl_ref, index as usize);
    impl_ref.set(index as usize, elem_slice);
    (true, displaced.map(|raw| (impl_ref.type_tag, raw)))
}

/// Set element at index (copies from elem buffer)
///
/// Returns true if successful, false if index out of bounds
#[no_mangle]
pub unsafe extern "C" fn pith_list_set(
    list: PithList,
    index: i64,
    elem: *const u8,
    elem_size: i64,
) -> bool {
    if elem.is_null() {
        return false;
    }

    // SAFETY: `with_list_mut` validates `list`; `elem` is readable for `elem_size` bytes.
    let res = unsafe {
        with_list_mut(list, |impl_ref| {
            list_set_bytes_impl(impl_ref, index, elem, elem_size)
        })
    };
    let Some((ok, displaced)) = res else {
        return false;
    };
    if let Some((tag, raw)) = displaced {
        // SAFETY: `raw` was owned by `list` under `tag`; `&mut ListImpl` borrow has ended.
        unsafe { release_element(tag, raw) };
    }
    ok
}

#[inline(always)]
unsafe fn list_remove_bytes_impl(
    impl_ref: &mut ListImpl,
    index: i64,
    elem_size: i64,
) -> (bool, Option<(ListTypeTag, i64)>) {
    crate::perf_stats!(PERF_LIST_REMOVES += 1);

    if index < 0 || index >= impl_ref.len() as i64 {
        return (false, None);
    }

    if impl_ref.elem_size != elem_size as usize {
        eprintln!("pith: list element size mismatch");
        return (false, None);
    }

    let displaced = displaced_element(impl_ref, index as usize);
    impl_ref.remove(index as usize);
    (true, displaced.map(|raw| (impl_ref.type_tag, raw)))
}

/// Remove element at index
///
/// Returns true if successful
#[no_mangle]
pub unsafe extern "C" fn pith_list_remove(list: *mut PithList, index: i64, elem_size: i64) -> bool {
    if list.is_null() {
        return false;
    }

    // SAFETY: `list` is non-null per caller contract.
    let list_val = unsafe { *list };
    // SAFETY: `with_list_mut` validates `list_val`; release displaced element after borrow ends.
    let res = unsafe {
        with_list_mut(list_val, |impl_ref| {
            list_remove_bytes_impl(impl_ref, index, elem_size)
        })
    };
    let Some((ok, displaced)) = res else {
        return false;
    };
    if let Some((tag, raw)) = displaced {
        // SAFETY: `raw` was owned by `list_val` under `tag`.
        unsafe { release_element(tag, raw) };
    }
    ok
}

#[inline(always)]
unsafe fn list_remove_value_impl(
    impl_ref: &mut ListImpl,
    index: i64,
) -> (i64, Option<(ListTypeTag, i64)>) {
    crate::perf_stats!(PERF_LIST_REMOVES += 1);

    if index < 0 || index >= impl_ref.len() as i64 {
        return (0, None);
    }

    let displaced = displaced_element(impl_ref, index as usize);
    impl_ref.remove(index as usize);
    (1, displaced.map(|raw| (impl_ref.type_tag, raw)))
}

/// Remove the element at `index`, with the list handle passed by value
/// rather than through a pointer to it. Returns 1 when an element was
/// removed and 0 otherwise.
#[no_mangle]
pub unsafe extern "C" fn pith_list_remove_value(list: PithList, index: i64) -> i64 {
    // SAFETY: `with_list_mut` validates `list`; release displaced element after borrow ends.
    let res = unsafe { with_list_mut(list, |impl_ref| list_remove_value_impl(impl_ref, index)) };
    let Some((ok, displaced)) = res else {
        return 0;
    };
    if let Some((tag, raw)) = displaced {
        // SAFETY: `raw` was owned by `list` under `tag`.
        unsafe { release_element(tag, raw) };
    }
    ok
}

/// Clear all elements from list (by-value variant)
#[no_mangle]
pub unsafe extern "C" fn pith_list_clear_value(list: PithList) {
    // Detach storage while holding `&mut`, then release elements after the
    // borrow ends so re-entrant `Drop` methods or `isolate_call` aborts can
    // never observe freed handles or alias `&mut ListImpl`.
    // SAFETY: `with_list_mut` validates `list` before detaching its storage.
    let taken = unsafe { with_list_mut(list, take_elements_for_release) };
    if let Some((tag, elems)) = taken {
        // SAFETY: Detached handles were owned by `list` under `tag`.
        unsafe { release_taken_elements(tag, elems) };
    }
}

/// Reverse the list in place, with the handle passed by value.
#[no_mangle]
pub unsafe extern "C" fn pith_list_reverse_value(list: PithList) {
    // SAFETY: Delegates directly to `pith_list_reverse`.
    unsafe { pith_list_reverse(list) };
}

/// Clear all elements from list
#[no_mangle]
pub unsafe extern "C" fn pith_list_clear(list: *mut PithList) {
    if list.is_null() {
        return;
    }
    // SAFETY: `list` is non-null per caller contract.
    unsafe { pith_list_clear_value(*list) };
}

fn list_reverse_impl(impl_ref: &mut ListImpl) {
    if impl_ref.uses_value_storage() {
        impl_ref.values8.reverse();
        impl_ref.sync_value_view();
    } else {
        impl_ref.elements.reverse();
    }
}

/// Reverse list elements in-place
#[no_mangle]
pub unsafe extern "C" fn pith_list_reverse(list: PithList) {
    // SAFETY: `with_list_mut` validates `list` before reversing it.
    unsafe { with_list_mut(list, list_reverse_impl) };
}
