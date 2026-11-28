//! List constructors and literal builders.

use super::{crosses_page, element_tag_from_code, identity_tag, ListImpl, PithList};
use crate::handle_registry::{self, HandleKind};

#[inline(always)]
fn list_new_impl(elem_size: i64, type_tag: i32) -> PithList {
    let tag = element_tag_from_code(type_tag);

    // written straight into the allocation. building the struct on the
    // stack and moving it into a box copies it in 16-byte pieces, each read
    // back while the field stores it covers are still in flight.
    let slot = Box::into_raw(Box::<ListImpl>::new_uninit()) as *mut ListImpl;
    let ptr = slot as *mut ();
    let id = identity_tag(ptr);
    // SAFETY: `slot` is freshly allocated and exclusively owned; writing initializes it.
    unsafe {
        slot.write(ListImpl::new(elem_size as usize, tag));
        (*slot).identity = id;
    }
    if crosses_page(ptr) {
        handle_registry::register(ptr as *const (), HandleKind::List);
    }

    PithList { ptr }
}

/// Create a new empty list
///
/// # Arguments
/// * `elem_size` - Size of each element in bytes
/// * `type_tag` - Type tag for element handling (0=primitive, 1=string, 2=list, 3=map)
///   Create a new list with default element size (8 bytes, primitive)
#[no_mangle]
pub unsafe extern "C" fn pith_list_new_default() -> PithList {
    list_new_impl(8, 0)
}

/// Create a list that owns cstring elements: push/insert retain, remove and
/// free release. The emitter uses this for List[String].
#[no_mangle]
pub unsafe extern "C" fn pith_list_new_cstr() -> PithList {
    list_new_impl(8, 1)
}

/// List of list handles (List[List[T]]): elements retain/release recursively.
#[no_mangle]
pub unsafe extern "C" fn pith_list_new_nested_list() -> PithList {
    list_new_impl(8, 2)
}

/// List of map handles (List[Map[K, V]]).
#[no_mangle]
pub unsafe extern "C" fn pith_list_new_nested_map() -> PithList {
    list_new_impl(8, 3)
}

/// List that owns struct-handle elements (List[SomeStruct]): push/insert
/// retain, and the free-time cascade releases each element so the struct
/// headers don't leak. rc-counted and magic-checked, so a struct shared with
/// a variable or another list is not double-freed.
#[no_mangle]
pub unsafe extern "C" fn pith_list_new_struct() -> PithList {
    list_new_impl(8, 4)
}

/// List that owns Bytes elements (List[Bytes]): push/insert retain, and the
/// free-time cascade releases each element. without this a List[Bytes] was
/// created untagged, so a literal like `[buf]` stored a handle the list did
/// not own and the element was freed out from under it at scope exit.
#[no_mangle]
pub unsafe extern "C" fn pith_list_new_bytes() -> PithList {
    list_new_impl(8, 5)
}

/// List that owns closure elements (List[fn(...) -> T]): push/insert retain,
/// and the free-time cascade releases each element. a route table or a
/// middleware chain is exactly this shape, and untagged it held handles it
/// did not own -- so an owned closure handed to a function that stored one
/// was freed at the call site while the list still pointed at it.
#[no_mangle]
pub unsafe extern "C" fn pith_list_new_closure() -> PithList {
    list_new_impl(8, 6)
}

/// List that owns set elements (List[Set[T]]): push/insert retain, and the
/// free-time cascade releases each element. a set is a shared handle like any
/// other collection, so an untagged list of them held handles it did not own --
/// which left `xs.push({1, 2})` stranding its element and made the value
/// position of `xs.insert(i, {1, 2})` the one store the caller could not
/// safely release into.
#[no_mangle]
pub unsafe extern "C" fn pith_list_new_set() -> PithList {
    list_new_impl(8, 7)
}

/// a list of channel handles owns one count per element, like a list of any
/// other counted handle; without the tag it held handles it did not own.
#[no_mangle]
pub unsafe extern "C" fn pith_list_new_channel() -> PithList {
    list_new_impl(8, 8)
}

/// a list of sync primitive handles (Mutex, WaitGroup, Semaphore, AtomicInt)
/// owns one count per element, like a list of channels.
#[no_mangle]
pub unsafe extern "C" fn pith_list_new_sync() -> PithList {
    list_new_impl(8, 9)
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_new(elem_size: i64, type_tag: i32) -> PithList {
    list_new_impl(elem_size, type_tag)
}

/// The most elements one `pith_list_of` call takes.
pub const LIST_OF_MAX: usize = 8;

/// A new list holding the first `count` of up to eight values, for a list
/// literal: `["a", "b", label]` is one call instead of a constructor and a
/// push per element. `type_tag` is the element tag code `pith_list_new`
/// takes. The storage is allocated once at its final size. For a counted
/// element kind the list takes over the count the caller holds on each
/// value, the way `pith_list_push_value_owned` does, so the caller must not
/// release them afterwards.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn pith_list_of(
    type_tag: i64,
    count: i64,
    v0: i64,
    v1: i64,
    v2: i64,
    v3: i64,
    v4: i64,
    v5: i64,
    v6: i64,
    v7: i64,
) -> PithList {
    let count = (count.max(0) as usize).min(LIST_OF_MAX);
    let list = list_new_impl(8, type_tag as i32);
    crate::perf_stats!(PERF_LIST_PUSHES += count);
    let mut values: Vec<i64> = Vec::with_capacity(count);
    // one store per value, from the register it arrived in. gathering them
    // into an array and copying that reads the array back in wider pieces
    // than it was written in, which stalls on every literal.
    let slots = values.as_mut_ptr();
    for (i, value) in [v0, v1, v2, v3, v4, v5, v6, v7]
        .into_iter()
        .enumerate()
        .take(count)
    {
        // SAFETY: `values` has capacity `count` and `i < count`.
        unsafe { std::ptr::write_volatile(slots.add(i), value) };
    }
    // SAFETY: The first `count` elements were initialized above.
    unsafe { values.set_len(count) };
    // SAFETY: `list` was freshly allocated above and is exclusively owned.
    let impl_ref = unsafe { &mut *(list.ptr as *mut ListImpl) };
    impl_ref.values8 = values;
    impl_ref.sync_value_view();
    list
}
