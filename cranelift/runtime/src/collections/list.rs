//! List[T] - ordered collection with ARC
//!
//! Hybrid approach: Uses idiomatic Rust Vec internally for all operations,
//! but presents FFI-compatible interface matching the C runtime.

use crate::handle_registry::{self, HandleKind};
use crate::runtime_core::optional_tuple;
/// FFI-compatible list handle
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PithList {
    /// Pointer to internal list implementation (pub for cross-module access)
    pub ptr: *mut (),
}

/// Internal list implementation using idiomatic Rust
///
/// Stores elements as `Vec<Arc<[u8]>>` where each element is a byte slice.
/// For primitive types (Int, Float), we store the raw bytes.
/// For heap types (String, List, Map), we store references.
#[repr(C)]
pub struct ListImpl {
    /// Magic number to identify ListImpl pointers (0x464F5247 = "FORG")
    pub magic: u32,
    /// Shared-handle refcount: retain on alias, release drops and frees at
    /// zero. Collections are shared handles by default in the language, so
    /// unlike strings they need real counting.
    pub rc: std::sync::atomic::AtomicU32,
    /// Size of each element in bytes
    pub elem_size: usize,
    /// Type tag for determining how to handle elements
    pub type_tag: ListTypeTag,
    /// Element data as byte vectors
    pub elements: Vec<Vec<u8>>,
    /// Packed storage for 8-byte values and handles.
    pub values8: Vec<i64>,
    /// Cached pointer to 8-byte storage for native inlined reads.
    pub values8_ptr: *const i64,
    /// Cached length for 8-byte storage.
    pub values8_len: usize,
}

/// Magic number to identify ListImpl pointers
pub const LIST_MAGIC: u32 = 0x464F5247;

/// Type tag for list elements
#[repr(C)]
#[derive(Clone, Copy)]
pub enum ListTypeTag {
    Primitive, // Int, Float, Bool - stored by value
    String,    // PithString - needs retain/release
    List,      // Nested list - needs retain/release
    Map,       // Map - needs retain/release
    Struct,    // struct handle - needs retain/release (rc-counted, magic-checked)
    Bytes,     // Bytes handle - needs retain/release
}

pub const LIST_IMPL_ELEM_SIZE_OFFSET: i32 = std::mem::offset_of!(ListImpl, elem_size) as i32;
pub const LIST_IMPL_VALUES8_PTR_OFFSET: i32 = std::mem::offset_of!(ListImpl, values8_ptr) as i32;
pub const LIST_IMPL_VALUES8_LEN_OFFSET: i32 = std::mem::offset_of!(ListImpl, values8_len) as i32;

impl ListImpl {
    fn new(elem_size: usize, type_tag: ListTypeTag) -> Self {
        crate::perf_count(&crate::PERF_LIST_NEWS, 1);
        let mut list = ListImpl {
            magic: LIST_MAGIC,
            rc: std::sync::atomic::AtomicU32::new(1),
            elements: Vec::new(),
            values8: Vec::new(),
            values8_ptr: std::ptr::null(),
            values8_len: 0,
            elem_size,
            type_tag,
        };
        list.sync_value_view();
        list
    }

    fn uses_value_storage(&self) -> bool {
        self.elem_size == 8
    }

    pub(crate) fn sync_value_view(&mut self) {
        if self.uses_value_storage() {
            self.values8_len = self.values8.len();
            self.values8_ptr = if self.values8_len == 0 {
                std::ptr::null()
            } else {
                self.values8.as_ptr()
            };
        } else {
            self.values8_len = 0;
            self.values8_ptr = std::ptr::null();
        }
    }

    pub(crate) fn len(&self) -> usize {
        if self.uses_value_storage() {
            self.values8.len()
        } else {
            self.elements.len()
        }
    }

    fn push(&mut self, elem: &[u8]) {
        if self.uses_value_storage() {
            self.values8
                .push(unsafe { std::ptr::read_unaligned(elem.as_ptr() as *const i64) });
            self.sync_value_view();
        } else {
            self.elements.push(elem.to_vec());
        }
    }

    fn insert_value_at(&mut self, index: usize, value: i64) {
        if self.uses_value_storage() {
            let idx = index.min(self.values8.len());
            self.values8.insert(idx, value);
            self.sync_value_view();
        } else {
            let bytes = value.to_ne_bytes();
            let elem_len = self.elem_size.min(bytes.len());
            let idx = index.min(self.elements.len());
            self.elements.insert(idx, bytes[..elem_len].to_vec());
        }
    }

    fn push_value(&mut self, value: i64) {
        if self.uses_value_storage() {
            self.values8.push(value);
            self.sync_value_view();
        } else {
            let bytes = value.to_ne_bytes();
            let elem_len = self.elem_size.min(bytes.len());
            self.elements.push(bytes[..elem_len].to_vec());
        }
    }

    fn pop(&mut self) -> Option<Vec<u8>> {
        if self.uses_value_storage() {
            let popped = self.values8.pop().map(|value| value.to_ne_bytes().to_vec());
            self.sync_value_view();
            popped
        } else {
            self.elements.pop()
        }
    }

    unsafe fn get_value_unchecked(&self, index: usize) -> i64 {
        if self.uses_value_storage() {
            *self.values8.get_unchecked(index)
        } else {
            let elem = self.elements.get_unchecked(index);
            std::ptr::read_unaligned(elem.as_ptr() as *const i64)
        }
    }

    pub(crate) fn get_value(&self, index: usize) -> Option<i64> {
        if self.uses_value_storage() {
            self.values8.get(index).copied()
        } else {
            self.elements
                .get(index)
                .map(|elem| unsafe { std::ptr::read_unaligned(elem.as_ptr() as *const i64) })
        }
    }

    fn set(&mut self, index: usize, elem: &[u8]) {
        if self.uses_value_storage() {
            if index < self.values8.len() {
                self.values8[index] =
                    unsafe { std::ptr::read_unaligned(elem.as_ptr() as *const i64) };
            }
        } else if index < self.elements.len() {
            self.elements[index] = elem.to_vec();
        }
    }

    fn set_value(&mut self, index: usize, value: i64) {
        if self.uses_value_storage() {
            if index < self.values8.len() {
                self.values8[index] = value;
            }
        } else if index < self.elements.len() {
            let bytes = value.to_ne_bytes();
            let elem_len = self.elem_size.min(bytes.len());
            self.elements[index] = bytes[..elem_len].to_vec();
        }
    }

    fn remove(&mut self, index: usize) -> Option<Vec<u8>> {
        if self.uses_value_storage() {
            if index < self.values8.len() {
                let removed = Some(self.values8.remove(index).to_ne_bytes().to_vec());
                self.sync_value_view();
                removed
            } else {
                None
            }
        } else if index < self.elements.len() {
            Some(self.elements.remove(index))
        } else {
            None
        }
    }

    fn clear(&mut self) {
        if self.uses_value_storage() {
            self.values8.clear();
            self.sync_value_view();
        } else {
            self.elements.clear();
        }
    }

    fn swap(&mut self, a: usize, b: usize) {
        if self.uses_value_storage() {
            if a < self.values8.len() && b < self.values8.len() {
                self.values8.swap(a, b);
            }
        } else if a < self.elements.len() && b < self.elements.len() {
            self.elements.swap(a, b);
        }
    }
}

/// The list owns one count per heap element. These pick the right retain
/// and release operation for the element tag.
unsafe fn retain_element(tag: ListTypeTag, raw: i64) {
    match tag {
        ListTypeTag::String => {
            crate::perf_count(&crate::PERF_CSTRING_RETAINS_PUSH, 1);
            crate::pith_cstring_retain(raw as *const i8)
        }
        ListTypeTag::List => pith_list_retain_handle(raw),
        ListTypeTag::Map => crate::collections::map::pith_map_retain_handle(raw),
        ListTypeTag::Struct => crate::runtime_core::pith_struct_retain(raw),
        ListTypeTag::Bytes => crate::bytes::pith_bytes_retain(raw),
        ListTypeTag::Primitive => {}
    }
}

/// The count the list is about to give up at `index`, or `None` when it owns
/// nothing there. Primitive lists own no counts and may store elements
/// narrower than 8 bytes, so this must not read their storage at all.
fn displaced_element(impl_ref: &ListImpl, index: usize) -> Option<i64> {
    if matches!(impl_ref.type_tag, ListTypeTag::Primitive) {
        return None;
    }
    impl_ref.get_value(index)
}

/// Drop the list's count on every element, for clear and free. The list
/// holds exactly one count per heap element, so anyone still reading one
/// after this point must hold a count of their own.
unsafe fn release_all_elements(impl_ref: &ListImpl) {
    if matches!(impl_ref.type_tag, ListTypeTag::Primitive) {
        return;
    }
    for i in 0..impl_ref.len() {
        if let Some(raw) = impl_ref.get_value(i) {
            crate::perf_count(&crate::PERF_LIST_CASCADE_RELEASES, 1);
            release_element(impl_ref.type_tag, raw);
        }
    }
}

unsafe fn release_element(tag: ListTypeTag, raw: i64) {
    match tag {
        ListTypeTag::String => crate::pith_cstring_release(raw as *const i8),
        ListTypeTag::List => pith_list_release_handle(raw),
        ListTypeTag::Map => crate::collections::map::pith_map_release_handle(raw),
        ListTypeTag::Struct => crate::runtime_core::pith_struct_release(raw),
        ListTypeTag::Bytes => crate::bytes::pith_bytes_release(raw),
        ListTypeTag::Primitive => {}
    }
}

/// Fast validity check for the hot accessors: one memory read against the
/// magic word instead of a global registry lock per access. Freed lists get
/// their magic scrubbed in pith_list_free, so stale handles fail the compare.
/// The registry still records every list so is_list_ptr can give an exact
/// answer where a magic collision would be unacceptable (pith_auto_len).
#[inline]
unsafe fn list_magic_ok(ptr: *const ()) -> bool {
    handle_registry::plausibly_aligned::<ListImpl>(ptr)
        && (*(ptr as *const ListImpl)).magic == LIST_MAGIC
}

unsafe fn list_ref<'a>(list: PithList) -> Option<&'a ListImpl> {
    if !list_magic_ok(list.ptr as *const ()) {
        return None;
    }
    Some(&*(list.ptr as *const ListImpl))
}

unsafe fn list_mut<'a>(list: PithList) -> Option<&'a mut ListImpl> {
    if !list_magic_ok(list.ptr as *const ()) {
        return None;
    }
    Some(&mut *(list.ptr as *mut ListImpl))
}

pub(crate) unsafe fn list_ref_from_handle<'a>(handle: i64) -> Option<&'a ListImpl> {
    if !list_magic_ok(handle as *const ()) {
        return None;
    }
    Some(&*(handle as *const ListImpl))
}

pub(crate) unsafe fn list_mut_from_handle<'a>(handle: i64) -> Option<&'a mut ListImpl> {
    if !list_magic_ok(handle as *const ()) {
        return None;
    }
    Some(&mut *(handle as *mut ListImpl))
}

/// Create a new empty list
///
/// # Arguments
/// * `elem_size` - Size of each element in bytes
/// * `type_tag` - Type tag for element handling (0=primitive, 1=string, 2=list, 3=map)
/// Create a new list with default element size (8 bytes, primitive)
#[no_mangle]
pub unsafe extern "C" fn pith_list_new_default() -> PithList {
    pith_list_new(8, 0)
}

/// Create a list that owns cstring elements: push/insert retain, remove and
/// free release. The emitter uses this for List[String].
#[no_mangle]
pub unsafe extern "C" fn pith_list_new_cstr() -> PithList {
    pith_list_new(8, 1)
}

/// List of list handles (List[List[T]]): elements retain/release recursively.
#[no_mangle]
pub unsafe extern "C" fn pith_list_new_nested_list() -> PithList {
    pith_list_new(8, 2)
}

/// List of map handles (List[Map[K, V]]).
#[no_mangle]
pub unsafe extern "C" fn pith_list_new_nested_map() -> PithList {
    pith_list_new(8, 3)
}

/// List that owns struct-handle elements (List[SomeStruct]): push/insert
/// retain, and the free-time cascade releases each element so the struct
/// headers don't leak. rc-counted and magic-checked, so a struct shared with
/// a variable or another list is not double-freed.
#[no_mangle]
pub unsafe extern "C" fn pith_list_new_struct() -> PithList {
    pith_list_new(8, 4)
}

/// List that owns Bytes elements (List[Bytes]): push/insert retain, and the
/// free-time cascade releases each element. without this a List[Bytes] was
/// created untagged, so a literal like `[buf]` stored a handle the list did
/// not own and the element was freed out from under it at scope exit.
#[no_mangle]
pub unsafe extern "C" fn pith_list_new_bytes() -> PithList {
    pith_list_new(8, 5)
}

#[no_mangle]
pub unsafe extern "C" fn pith_list_new(elem_size: i64, type_tag: i32) -> PithList {
    let tag = match type_tag {
        1 => ListTypeTag::String,
        2 => ListTypeTag::List,
        3 => ListTypeTag::Map,
        4 => ListTypeTag::Struct,
        5 => ListTypeTag::Bytes,
        _ => ListTypeTag::Primitive,
    };

    let list_impl = ListImpl::new(elem_size as usize, tag);
    let boxed = Box::new(list_impl);
    let ptr = Box::into_raw(boxed) as *mut ();
    handle_registry::register(ptr as *const (), HandleKind::List);

    PithList { ptr }
}

/// Get list length
#[no_mangle]
pub extern "C" fn pith_list_len(list: PithList) -> i64 {
    unsafe {
        list_ref(list)
            .map(|impl_ref| impl_ref.len() as i64)
            .unwrap_or(0)
    }
}

/// Check if a raw pointer really is a ListImpl. The magic word rejects
/// almost everything cheaply; the registry confirms the rare match, since a
/// C string could start with the same four bytes and this function is what
/// keeps pith_auto_len from misreading one as a list.
pub fn is_list_ptr(ptr: *const ()) -> bool {
    (unsafe { list_magic_ok(ptr) }) && handle_registry::is_valid(ptr, HandleKind::List)
}

/// Auto-detect len: works on both lists and C strings
/// If the pointer has the list magic number, returns list length.
/// Otherwise, returns C string length (strlen).
#[no_mangle]
pub extern "C" fn pith_auto_len(ptr: i64) -> i64 {
    if ptr == 0 {
        return 0;
    }
    let raw = ptr as *const ();
    if is_list_ptr(raw) {
        let list = PithList {
            ptr: raw as *mut (),
        };
        pith_list_len(list)
    } else {
        crate::string::pith_cstring_len(ptr as *const i8)
    }
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

    let Some(impl_ref) = list_mut(*list) else {
        return;
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_PUSHES, 1);

    // Verify element size matches
    if impl_ref.elem_size != elem_size as usize {
        eprintln!("pith: list element size mismatch");
        return;
    }

    // Copy element data
    let elem_slice = std::slice::from_raw_parts(elem, elem_size as usize);

    // The list holds one count per heap element, released again when the
    // element leaves the list.
    retain_element(
        impl_ref.type_tag,
        std::ptr::read_unaligned(elem as *const i64),
    );

    impl_ref.push(elem_slice);
}

/// Push an i64-sized value into a list using the list handle directly.
/// This is a simpler ABI used by generated method calls. The caller keeps
/// its own count and the list takes one of its own.
#[no_mangle]
pub unsafe extern "C" fn pith_list_push_value(list: PithList, value: i64) {
    push_value_inner(list, value, false);
}

/// Push a value the caller owns and is about to stop tracking — a freshly
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
    push_value_inner(list, value, true);
}

unsafe fn push_value_inner(list: PithList, value: i64, takes_caller_count: bool) {
    let Some(impl_ref) = list_mut(list) else {
        return;
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_PUSHES, 1);
    if !takes_caller_count {
        retain_element(impl_ref.type_tag, value);
    }
    impl_ref.push_value(value);
}

/// Insert an element at an index, shifting the tail right. A negative
/// index clamps to the front, past-the-end clamps to a push. The list
/// takes an owner's count for tagged element kinds, like push.
#[no_mangle]
pub unsafe extern "C" fn pith_list_insert_value(list: PithList, index: i64, value: i64) {
    let Some(impl_ref) = list_mut(list) else {
        return;
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_INSERTS, 1);
    retain_element(impl_ref.type_tag, value);
    impl_ref.insert_value_at(index.max(0) as usize, value);
}

/// Set element at index (value-based API, stores i64).
#[no_mangle]
pub unsafe extern "C" fn pith_list_set_value(list: PithList, index: i64, value: i64) {
    set_value_inner(list, index, value, false);
}

/// Set element at index with a value the caller owns. The list takes the
/// caller's count rather than adding one, the same transfer
/// `pith_list_push_value_owned` performs.
#[no_mangle]
pub unsafe extern "C" fn pith_list_set_value_owned(list: PithList, index: i64, value: i64) {
    set_value_inner(list, index, value, true);
}

unsafe fn set_value_inner(list: PithList, index: i64, value: i64, takes_caller_count: bool) {
    let Some(impl_ref) = list_mut(list) else {
        return;
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_SETS, 1);
    let idx = index as usize;
    if idx >= impl_ref.len() {
        return;
    }
    // retain the incoming element before dropping the count on what it
    // displaces, so `l[i] = l[i]` cannot free the value mid-assignment.
    // an owned value is a buffer nobody else holds, so it can never be
    // what it displaces and needs no count of its own.
    if !takes_caller_count {
        retain_element(impl_ref.type_tag, value);
    }
    let displaced = displaced_element(impl_ref, idx);
    impl_ref.set_value(idx, value);
    if let Some(raw) = displaced {
        release_element(impl_ref.type_tag, raw);
    }
}

/// Join a list of C string pointers with a separator.
/// Returns a newly allocated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_list_join(list: PithList, sep: *const i8) -> *mut i8 {
    let Some(impl_ref) = list_ref(list) else {
        return std::ptr::null_mut();
    };
    if impl_ref.len() == 0 {
        return crate::pith_cstring_empty();
    }

    let sep_len = if sep.is_null() {
        0usize
    } else {
        crate::string::pith_cstring_len(sep) as usize
    };

    let mut total_len = 0usize;
    let mut i = 0usize;
    while i < impl_ref.len() {
        if let Some(raw) = impl_ref.get_value(i) {
            let ptr_val = raw as *const i8;
            if !ptr_val.is_null() {
                total_len += crate::string::pith_cstring_len(ptr_val) as usize;
            }
        }
        if i + 1 < impl_ref.len() {
            total_len += sep_len;
        }
        i += 1;
    }

    let out = crate::pith_alloc_cstring(total_len);

    let mut write = 0usize;
    let mut i = 0usize;
    while i < impl_ref.len() {
        if let Some(raw) = impl_ref.get_value(i) {
            let ptr_val = raw as *const i8;
            if !ptr_val.is_null() {
                let len = crate::string::pith_cstring_len(ptr_val) as usize;
                std::ptr::copy_nonoverlapping(ptr_val as *const u8, out.add(write) as *mut u8, len);
                write += len;
            }
        }
        if i + 1 < impl_ref.len() && sep_len > 0 {
            std::ptr::copy_nonoverlapping(sep as *const u8, out.add(write) as *mut u8, sep_len);
            write += sep_len;
        }
        i += 1;
    }

    *out.add(write) = 0;
    out
}

/// Join a list of Int elements with a separator, formatting each as decimal.
/// Unlike `pith_list_join`, the elements are values rather than string pointers.
/// Returns a newly allocated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_list_join_int(list: PithList, sep: *const i8) -> *mut i8 {
    let Some(impl_ref) = list_ref(list) else {
        return std::ptr::null_mut();
    };

    let sep_str = if sep.is_null() {
        ""
    } else {
        let len = crate::string::pith_cstring_len(sep) as usize;
        std::str::from_utf8(std::slice::from_raw_parts(sep as *const u8, len)).unwrap_or("")
    };

    let mut out = String::new();
    let mut i = 0usize;
    while i < impl_ref.len() {
        if let Some(raw) = impl_ref.get_value(i) {
            if i > 0 {
                out.push_str(sep_str);
            }
            out.push_str(&raw.to_string());
        }
        i += 1;
    }

    crate::pith_copy_bytes_to_cstring(out.as_bytes())
}

/// Pop element from end of list
///
/// Returns true if successful, false if list is empty
#[no_mangle]
pub unsafe extern "C" fn pith_list_pop(list: *mut PithList, elem_size: i64, out: *mut u8) -> bool {
    if list.is_null() || out.is_null() {
        return false;
    }

    let Some(impl_ref) = list_mut(*list) else {
        return false;
    };

    if impl_ref.elem_size != elem_size as usize {
        eprintln!("pith: list element size mismatch");
        return false;
    }

    match impl_ref.pop() {
        Some(elem_data) => {
            // Copy to output buffer
            std::ptr::copy_nonoverlapping(elem_data.as_ptr(), out, elem_data.len());

            // The list's count transfers to the caller with the element,
            // so there is no release here.
            true
        }
        None => false,
    }
}

/// Get element at index wrapped in an Optional tuple. Returns `Some(value)`
/// when `0 <= index < len`, and `None` otherwise — so callers can
/// distinguish an out-of-bounds miss from a legitimately stored `0`.
///
/// # Safety
/// * `list` must be a valid `PithList` handle or all-zero.
#[no_mangle]
pub unsafe extern "C" fn pith_list_get_opt(list: PithList, index: i64) -> i64 {
    let Some(impl_ref) = list_ref(list) else {
        return optional_tuple(false, 0);
    };
    if index < 0 || index >= impl_ref.len() as i64 {
        return optional_tuple(false, 0);
    }
    if impl_ref.elem_size != 8 {
        return optional_tuple(false, 0);
    }
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_GETS, 1);
    optional_tuple(true, impl_ref.get_value_unchecked(index as usize))
}

/// Return the first element of the list wrapped in an Optional tuple, or
/// `None` when the list is empty.
///
/// # Safety
/// * `list` must be a valid `PithList` handle or all-zero.
#[no_mangle]
pub unsafe extern "C" fn pith_list_first_opt(list: PithList) -> i64 {
    let Some(impl_ref) = list_ref(list) else {
        return optional_tuple(false, 0);
    };
    if impl_ref.len() == 0 || impl_ref.elem_size != 8 {
        return optional_tuple(false, 0);
    }
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_GETS, 1);
    optional_tuple(true, impl_ref.get_value_unchecked(0))
}

/// Return the last element of the list wrapped in an Optional tuple, or
/// `None` when the list is empty.
///
/// # Safety
/// * `list` must be a valid `PithList` handle or all-zero.
#[no_mangle]
pub unsafe extern "C" fn pith_list_last_opt(list: PithList) -> i64 {
    let Some(impl_ref) = list_ref(list) else {
        return optional_tuple(false, 0);
    };
    if impl_ref.len() == 0 || impl_ref.elem_size != 8 {
        return optional_tuple(false, 0);
    }
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_GETS, 1);
    optional_tuple(true, impl_ref.get_value_unchecked(impl_ref.len() - 1))
}

/// Strict pointer-sized indexed access for `list[i]`. Aborts with a
/// structured diagnostic on out-of-bounds instead of returning a silent
/// zero. Callers that want a checked shape have `.get(i)` (returns T?).
///
/// # Safety
/// * `list` must be a valid `PithList` handle or all-zero.
#[no_mangle]
pub unsafe extern "C" fn pith_list_get_value_strict(list: PithList, index: i64) -> i64 {
    let Some(impl_ref) = list_ref(list) else {
        eprintln!("pith runtime error: list indexing on invalid list handle");
        std::process::exit(1);
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_GETS, 1);
    let len = impl_ref.len() as i64;
    if index < 0 || index >= len {
        eprintln!(
            "pith runtime error: list index out of bounds: {} for list of length {} (use .get(i) for Optional access)",
            index, len
        );
        std::process::exit(1);
    }
    if impl_ref.elem_size != 8 {
        eprintln!(
            "pith runtime error: list index requires 8-byte element (elem_size={})",
            impl_ref.elem_size
        );
        std::process::exit(1);
    }
    impl_ref.get_value_unchecked(index as usize)
}

/// Get element at index for pointer-sized elements (returns i64 directly)
/// Returns 0 if out of bounds or on error
#[no_mangle]
pub unsafe extern "C" fn pith_list_get_value(list: PithList, index: i64) -> i64 {
    let Some(impl_ref) = list_ref(list) else {
        return 0;
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_GETS, 1);
    crate::perf_count(&crate::PERF_LIST_GET_VALUE_CALLS, 1);
    crate::perf_count(&crate::PERF_LIST_GET_VALUE_CHECKED_CALLS, 1);

    if index < 0 || index >= impl_ref.len() as i64 {
        return 0;
    }

    if impl_ref.elem_size != 8 {
        crate::perf_count(&crate::PERF_LIST_GET_ELEM_OTHER, 1);
        return 0;
    }

    crate::perf_count(&crate::PERF_LIST_GET_ELEM8, 1);
    impl_ref.get_value_unchecked(index as usize)
}

/// Get element at index for pointer-sized elements without an upper-bound check.
/// This is used only in compiler-generated loops that already guard the index.
#[no_mangle]
pub unsafe extern "C" fn pith_list_get_value_unchecked(list: PithList, index: i64) -> i64 {
    if index < 0 {
        return 0;
    }

    let Some(impl_ref) = list_ref(list) else {
        return 0;
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_GETS, 1);
    crate::perf_count(&crate::PERF_LIST_GET_VALUE_CALLS, 1);
    crate::perf_count(&crate::PERF_LIST_GET_VALUE_UNCHECKED_CALLS, 1);

    if impl_ref.elem_size != 8 {
        crate::perf_count(&crate::PERF_LIST_GET_ELEM_OTHER, 1);
        return 0;
    }
    if index >= impl_ref.len() as i64 {
        return 0;
    }

    crate::perf_count(&crate::PERF_LIST_GET_ELEM8, 1);
    impl_ref.get_value_unchecked(index as usize)
}

/// Get element at index (copies to out buffer)
///
/// Returns true if successful, false if index out of bounds
#[no_mangle]
pub unsafe extern "C" fn pith_list_get(
    list: PithList,
    index: i64,
    elem_size: i64,
    out: *mut u8,
) -> bool {
    if out.is_null() {
        return false;
    }

    let Some(impl_ref) = list_ref(list) else {
        return false;
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_GETS, 1);
    crate::perf_count(&crate::PERF_LIST_GET_BYTES_CALLS, 1);

    if index < 0 || index >= impl_ref.len() as i64 {
        return false;
    }

    if impl_ref.elem_size != elem_size as usize {
        eprintln!("pith: list element size mismatch");
        return false;
    }
    if elem_size == 8 {
        crate::perf_count(&crate::PERF_LIST_GET_ELEM8, 1);
    } else {
        crate::perf_count(&crate::PERF_LIST_GET_ELEM_OTHER, 1);
    }

    match impl_ref.get_value(index as usize) {
        Some(value) => {
            if elem_size == 8 {
                std::ptr::copy_nonoverlapping(value.to_ne_bytes().as_ptr(), out, 8);
            } else if let Some(elem_data) = impl_ref.elements.get(index as usize) {
                std::ptr::copy_nonoverlapping(elem_data.as_ptr(), out, elem_data.len());
            } else {
                return false;
            }

            // Reads borrow: the element stays in the list with its count,
            // and the caller retains only if it keeps the value.
            true
        }
        None => false,
    }
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

    let Some(impl_ref) = list_mut(list) else {
        return false;
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_SETS, 1);

    if index < 0 || index >= impl_ref.len() as i64 {
        return false;
    }

    if impl_ref.elem_size != elem_size as usize {
        eprintln!("pith: list element size mismatch");
        return false;
    }

    // retain the incoming element before dropping the count on what it
    // displaces, so `l[i] = l[i]` cannot free the value mid-assignment
    retain_element(
        impl_ref.type_tag,
        std::ptr::read_unaligned(elem as *const i64),
    );

    // Copy new element
    let elem_slice = std::slice::from_raw_parts(elem, elem_size as usize);

    let displaced = displaced_element(impl_ref, index as usize);
    impl_ref.set(index as usize, elem_slice);
    if let Some(raw) = displaced {
        release_element(impl_ref.type_tag, raw);
    }
    true
}

/// Remove element at index
///
/// Returns true if successful
#[no_mangle]
pub unsafe extern "C" fn pith_list_remove(list: *mut PithList, index: i64, elem_size: i64) -> bool {
    if list.is_null() {
        return false;
    }

    let Some(impl_ref) = list_mut(*list) else {
        return false;
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_REMOVES, 1);

    if index < 0 || index >= impl_ref.len() as i64 {
        return false;
    }

    if impl_ref.elem_size != elem_size as usize {
        eprintln!("pith: list element size mismatch");
        return false;
    }

    let displaced = displaced_element(impl_ref, index as usize);
    impl_ref.remove(index as usize);
    if let Some(raw) = displaced {
        release_element(impl_ref.type_tag, raw);
    }
    true
}

/// Remove element at index (by-value variant — works with internal pointer)
#[no_mangle]
pub unsafe extern "C" fn pith_list_remove_value(list: PithList, index: i64) -> i64 {
    let Some(impl_ref) = list_mut(list) else {
        return 0;
    };
    crate::ensure_perf_stats_registered();
    crate::perf_count(&crate::PERF_LIST_REMOVES, 1);

    if index < 0 || index >= impl_ref.len() as i64 {
        return 0;
    }

    let displaced = displaced_element(impl_ref, index as usize);
    impl_ref.remove(index as usize);
    if let Some(raw) = displaced {
        release_element(impl_ref.type_tag, raw);
    }
    1
}

/// Clear all elements from list (by-value variant)
#[no_mangle]
pub unsafe extern "C" fn pith_list_clear_value(list: PithList) {
    let Some(impl_ref) = list_mut(list) else {
        return;
    };

    release_all_elements(impl_ref);
    impl_ref.clear();
}

/// Reverse list in-place (by-value variant — works with internal pointer)
#[no_mangle]
pub unsafe extern "C" fn pith_list_reverse_value(list: PithList) {
    let Some(impl_ref) = list_mut(list) else {
        return;
    };
    let len = impl_ref.len();
    for i in 0..len / 2 {
        impl_ref.swap(i, len - 1 - i);
    }
}

/// Clear all elements from list
#[no_mangle]
pub unsafe extern "C" fn pith_list_clear(list: *mut PithList) {
    if list.is_null() {
        return;
    }

    let Some(impl_ref) = list_mut(*list) else {
        return;
    };

    release_all_elements(impl_ref);
    impl_ref.clear();
}

/// Reverse list elements in-place
#[no_mangle]
pub unsafe extern "C" fn pith_list_reverse(list: PithList) {
    let Some(impl_ref) = list_mut(list) else {
        return;
    };
    if impl_ref.uses_value_storage() {
        impl_ref.values8.reverse();
        impl_ref.sync_value_view();
    } else {
        impl_ref.elements.reverse();
    }
}

/// Release list and free memory
#[no_mangle]
pub unsafe extern "C" fn pith_list_release(list: PithList) {
    let Some(impl_ref) = list_mut(list) else {
        return;
    };
    let prev = impl_ref
        .rc
        .fetch_sub(1, std::sync::atomic::Ordering::Release);
    if prev > 1 {
        return;
    }
    if prev == 0 {
        // over-release: put the count back and refuse to double-free
        impl_ref.rc.store(0, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);

    // Last count: release the elements this list owns, then free.
    release_all_elements(impl_ref);

    // Scrub the magic first so any handle that outlives the list fails the
    // fast validity check.
    (*(list.ptr as *mut ListImpl)).magic = 0;
    handle_registry::unregister(list.ptr as *const (), HandleKind::List);
    crate::perf_count(&crate::PERF_LIST_FREES, 1);
    let _ = Box::from_raw(list.ptr as *mut ListImpl);
}

/// Retain a list handle: one more owner of this shared handle.
#[no_mangle]
pub unsafe extern "C" fn pith_list_retain_handle(handle: i64) {
    if let Some(impl_ref) = list_mut_from_handle(handle) {
        impl_ref
            .rc
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Release a list handle; frees the list and its owned elements at zero.
#[no_mangle]
pub unsafe extern "C" fn pith_list_release_handle(handle: i64) {
    pith_list_release(PithList {
        ptr: handle as *mut (),
    });
}

/// Destructor for list elements in collections
///
/// Called by cycle collector when freeing cyclic list objects
#[no_mangle]
pub extern "C" fn pith_list_destructor(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }

    unsafe {
        let list = ptr as *const PithList;
        pith_list_release(*list);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bogus_list() -> PithList {
        PithList {
            ptr: 12345usize as *mut (),
        }
    }

    #[test]
    fn list_handles_refcount_and_cascade() {
        unsafe {
            let list = pith_list_new(8, 1); // string elements
            let s = crate::pith_copy_bytes_to_cstring(b"kept");
            pith_list_push_value(list, s as i64);
            crate::pith_cstring_release(s); // list holds the only count

            // second owner keeps the list alive through one release
            pith_list_retain_handle(list.ptr as i64);
            pith_list_release(list);
            assert_eq!(crate::cstring_refcount_for_tests(s), Some(1));
            assert_eq!(pith_list_len(list), 1);

            // last release frees the list and cascades into the element
            pith_list_release_handle(list.ptr as i64);
            assert_eq!(crate::cstring_refcount_for_tests(s), None);

            // stale handle: magic scrubbed, further release is a no-op
            pith_list_release_handle(list.ptr as i64);
            assert_eq!(pith_list_len(list), 0);
        }
    }

    #[test]
    fn nested_lists_release_recursively() {
        unsafe {
            let inner = pith_list_new(8, 1);
            let s = crate::pith_copy_bytes_to_cstring(b"deep");
            pith_list_push_value(inner, s as i64);
            crate::pith_cstring_release(s);

            let outer = pith_list_new(8, 2); // list elements
            pith_list_push_value(outer, inner.ptr as i64);

            // outer holds inner's only remaining count after we drop ours
            pith_list_release(inner);
            assert_eq!(pith_list_len(inner), 1);

            pith_list_release(outer);
            // inner freed with outer; its string cascaded too
            assert_eq!(crate::cstring_refcount_for_tests(s), None);
        }
    }

    #[test]
    fn string_lists_own_one_count_per_element() {
        unsafe {
            let list = pith_list_new(8, 1); // ListTypeTag::String
            let s = crate::pith_copy_bytes_to_cstring(b"owned");
            pith_list_push_value(list, s as i64);
            assert_eq!(crate::cstring_refcount_for_tests(s), Some(2));

            // the caller drops its count; the list's count keeps it alive
            crate::pith_cstring_release(s);
            assert_eq!(crate::cstring_refcount_for_tests(s), Some(1));
            assert_eq!(pith_list_get_value(list, 0), s as i64);

            // overwriting retains the new element and drops the list's count
            // on the old one, which was its last owner
            let t = crate::pith_copy_bytes_to_cstring(b"next");
            pith_list_set_value(list, 0, t as i64);
            assert_eq!(crate::cstring_refcount_for_tests(s), None);
            assert_eq!(crate::cstring_refcount_for_tests(t), Some(2));

            // freeing the list drops its count on every element
            pith_list_release(list);
            assert_eq!(crate::cstring_refcount_for_tests(t), Some(1));
            crate::pith_cstring_release(t);
        }
    }

    #[test]
    fn string_list_pop_transfers_ownership() {
        unsafe {
            let list = pith_list_new(8, 1);
            let s = crate::pith_copy_bytes_to_cstring(b"moved");
            pith_list_push_value(list, s as i64);
            crate::pith_cstring_release(s); // caller keeps nothing

            let mut list_val = list;
            let mut out = [0u8; 8];
            assert!(pith_list_pop(&mut list_val, 8, out.as_mut_ptr()));
            let popped = i64::from_ne_bytes(out) as *const i8;
            // the list's count moved out with the element
            assert_eq!(crate::cstring_refcount_for_tests(popped), Some(1));
            crate::pith_cstring_release(popped);
            pith_list_release(list);
        }
    }

    /// A string list holding one element the caller has already let go of,
    /// so the list's count is the only one left.
    unsafe fn list_owning_one_element(text: &[u8]) -> (PithList, *mut i8) {
        let list = pith_list_new(8, 1);
        let s = crate::pith_copy_bytes_to_cstring(text);
        pith_list_push_value(list, s as i64);
        crate::pith_cstring_release(s);
        assert_eq!(crate::cstring_refcount_for_tests(s), Some(1));
        (list, s)
    }

    #[test]
    fn removing_an_element_drops_the_lists_count_on_it() {
        unsafe {
            let (list, s) = list_owning_one_element(b"evicted");
            pith_list_remove_value(list, 0);
            assert_eq!(crate::cstring_refcount_for_tests(s), None);
            assert_eq!(pith_list_len(list), 0);
            pith_list_release(list);
        }
    }

    #[test]
    fn clearing_drops_the_lists_count_on_every_element() {
        unsafe {
            let (list, s) = list_owning_one_element(b"cleared");
            pith_list_clear_value(list);
            assert_eq!(crate::cstring_refcount_for_tests(s), None);
            assert_eq!(pith_list_len(list), 0);
            pith_list_release(list);
        }
    }

    #[test]
    fn setting_an_index_over_itself_keeps_the_element_alive() {
        unsafe {
            let (list, s) = list_owning_one_element(b"self");
            pith_list_set_value(list, 0, s as i64);
            assert_eq!(crate::cstring_refcount_for_tests(s), Some(1));
            assert_eq!(pith_list_get_value(list, 0), s as i64);
            pith_list_release(list);
            assert_eq!(crate::cstring_refcount_for_tests(s), None);
        }
    }

    #[test]
    fn a_second_owner_survives_the_lists_eviction() {
        unsafe {
            let list = pith_list_new(8, 1);
            let s = crate::pith_copy_bytes_to_cstring(b"held elsewhere");
            pith_list_push_value(list, s as i64);
            // the caller keeps its own count, as a binding of `l[0]` would
            assert_eq!(crate::cstring_refcount_for_tests(s), Some(2));

            pith_list_remove_value(list, 0);
            assert_eq!(crate::cstring_refcount_for_tests(s), Some(1));

            pith_list_release(list);
            crate::pith_cstring_release(s);
            assert_eq!(crate::cstring_refcount_for_tests(s), None);
        }
    }

    #[test]
    fn primitive_lists_never_touch_their_elements() {
        unsafe {
            // 7 is not a pointer; an element-releasing list would follow it
            let list = pith_list_new(8, 0);
            pith_list_push_value(list, 7);
            pith_list_set_value(list, 0, 8);
            assert_eq!(pith_list_get_value(list, 0), 8);
            pith_list_remove_value(list, 0);
            pith_list_clear_value(list);
            pith_list_release(list);
        }
    }

    #[test]
    fn invalid_list_handles_return_safe_defaults() {
        unsafe {
            assert_eq!(pith_list_len(bogus_list()), 0);
            assert_eq!(pith_list_get_value(bogus_list(), 0), 0);
            assert_eq!(pith_list_get_value_unchecked(bogus_list(), 0), 0);
            pith_list_set_value(bogus_list(), 0, 7);
            pith_list_reverse(bogus_list());
            pith_list_release(bogus_list());
        }
    }

    #[test]
    fn released_list_handles_are_rejected() {
        unsafe {
            let list = pith_list_new(8, 0);
            pith_list_push_value(list, 7);
            assert_eq!(pith_list_len(list), 1);
            pith_list_release(list);
            assert_eq!(pith_list_len(list), 0);
            assert_eq!(pith_list_get_value(list, 0), 0);
            pith_list_release(list);
        }
    }

}
