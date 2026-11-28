//! List[T]: an ordered, reference-counted collection.
//!
//! The storage is a Rust `Vec` behind a `#[repr(C)]` header whose field
//! offsets the codegen reads directly for its inlined fast paths.

use crate::handle_registry::{self, HandleKind};

mod ctor;
mod lifecycle;
mod read;
mod write;
#[cfg(test)]
mod tests;

pub use ctor::*;
pub use lifecycle::*;
pub use read::*;
pub use write::*;

/// FFI-compatible list handle
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PithList {
    /// Pointer to internal list implementation (pub for cross-module access)
    pub ptr: *mut (),
}

/// The list behind a handle.
///
/// Eight-byte elements (ints, floats, bools and every heap handle) live
/// packed in `values8`; any other element size keeps one byte vector per
/// element in `elements`. Heap elements are stored as handles, and
/// `type_tag` says which retain/release pair they take.
#[repr(C)]
pub struct ListImpl {
    /// Magic number to identify ListImpl pointers (0x464F5247 = "FORG")
    pub magic: u32,
    /// Shared-handle refcount: retain on alias, release drops and frees at
    /// zero. Collections are shared handles by default in the language, so
    /// unlike strings they need real counting.
    pub rc: std::sync::atomic::AtomicU32,
    /// `identity_tag(self)` while the list is live, 0 once it is freed. It
    /// sits right after the magic so is_list_ptr reads only the first 16
    /// bytes of a pointer that may be a string.
    pub(super) identity: u64,
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
    /// Bit 0: this list sits in the cycle collector's suspect buffer.
    /// Bit 7: shared (`share::CONTAINER_SHARED_FLAG`).
    /// Appended at the end so the `offset_of!` constants the codegen imports
    /// keep describing the same layout.
    pub(crate) cycle_flags: std::sync::atomic::AtomicU8,
}

/// Magic number to identify ListImpl pointers
pub const LIST_MAGIC: u32 = 0x464F5247;

/// Type tag for a heap-owning collection's elements. Lists carry one per
/// list; maps carry one for their values (`MapImpl.val_tag`), which is why
/// this lives here rather than being list-specific.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ListTypeTag {
    Primitive, // Int, Float, Bool - stored by value
    String,    // PithString - needs retain/release
    List,      // Nested list - needs retain/release
    Map,       // Map - needs retain/release
    Struct,    // struct handle - needs retain/release (rc-counted, magic-checked)
    Bytes,     // Bytes handle - needs retain/release
    Closure,   // closure handle - needs retain/release
    Set,       // Set handle - needs retain/release
    Channel,   // channel handle - needs retain/release
    Sync,      // Mutex, WaitGroup, Semaphore or AtomicInt handle - needs retain/release
}

pub const LIST_IMPL_ELEM_SIZE_OFFSET: i32 = std::mem::offset_of!(ListImpl, elem_size) as i32;
pub const LIST_IMPL_VALUES8_PTR_OFFSET: i32 = std::mem::offset_of!(ListImpl, values8_ptr) as i32;
pub const LIST_IMPL_VALUES8_LEN_OFFSET: i32 = std::mem::offset_of!(ListImpl, values8_len) as i32;
pub const LIST_IMPL_TYPE_TAG_OFFSET: i32 = std::mem::offset_of!(ListImpl, type_tag) as i32;
/// The Primitive tag's discriminant, for the codegen fast path: primitive
/// elements carry no counts, so an in-bounds store needs no retain/release.
pub const LIST_TYPE_TAG_PRIMITIVE: i32 = ListTypeTag::Primitive as i32;
/// The alignment `with_list_ref` requires of a handle before it reads the magic
/// word; the codegen's strict `xs[i]` fast path applies the same test.
pub const LIST_HANDLE_ALIGN: i64 = std::mem::align_of::<ListImpl>() as i64;

impl ListImpl {
    #[inline(always)]
    pub(super) fn new(elem_size: usize, type_tag: ListTypeTag) -> Self {
        crate::perf_count(&crate::PERF_LIST_NEWS, 1);
        crate::alloc_sites::record("list");
        let mut list = ListImpl {
            magic: LIST_MAGIC,
            rc: std::sync::atomic::AtomicU32::new(1),
            elements: Vec::new(),
            values8: Vec::new(),
            values8_ptr: std::ptr::null(),
            values8_len: 0,
            elem_size,
            type_tag,
            cycle_flags: std::sync::atomic::AtomicU8::new(0),
            identity: 0,
        };
        list.sync_value_view();
        list
    }

    pub(super) fn uses_value_storage(&self) -> bool {
        self.elem_size == 8
    }

    /// Learn the element kind a store is carrying, and answer whether the
    /// list owns counts on that kind afterwards.
    ///
    /// A list normally picks its flavor at construction from the checked
    /// element type, and that is right nearly everywhere. It is not right
    /// when the checker had no type to give (an empty literal in a position
    /// that supplies none), and the store is the one place the kind is known
    /// for certain. Adoption is gated on the list being empty, which is what
    /// makes it count-neutral: there are no already-stored elements whose
    /// counts a new tag would start releasing. It is gated on 8-byte storage
    /// too, because a narrower element is not a handle at all.
    ///
    /// A list that already owns some *other* heap kind does not adopt, even
    /// when empty. That would be the constructor and the store disagreeing
    /// about the element type, and there is no way to tell which of them is
    /// right: retagging would make every later store of the original kind
    /// retain and release as the wrong one. Refusing leaves the caller on the
    /// fallback, which is a leak.
    fn adopt_element_tag(&mut self, tag: ListTypeTag) -> bool {
        if tag == ListTypeTag::Primitive {
            return false;
        }
        if self.type_tag == tag {
            return true;
        }
        if self.type_tag == ListTypeTag::Primitive && self.len() == 0 && self.uses_value_storage() {
            self.type_tag = tag;
            return true;
        }
        false
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

    #[inline]
    fn read_i64_padded(elem: &[u8]) -> i64 {
        if elem.len() >= 8 {
            unsafe { std::ptr::read_unaligned(elem.as_ptr() as *const i64) }
        } else {
            let mut buf = [0u8; 8];
            buf[..elem.len()].copy_from_slice(elem);
            i64::from_ne_bytes(buf)
        }
    }

    pub(super) fn push(&mut self, elem: &[u8]) {
        if self.uses_value_storage() {
            let value = Self::read_i64_padded(elem);
            self.share_stored(value);
            self.values8.push(value);
            self.sync_value_view();
        } else {
            self.elements.push(elem.to_vec());
        }
    }

    /// The write barrier of a store: a value stored into a list other tasks
    /// can reach becomes reachable from them too (see `share.rs`).
    #[inline(always)]
    pub(super) fn share_stored(&self, value: i64) {
        if self.cycle_flags.load(std::sync::atomic::Ordering::Relaxed) & crate::share::CONTAINER_SHARED_FLAG != 0 {
            self.share_stored_slow(value);
        }
    }

    #[cold]
    #[inline(never)]
    fn share_stored_slow(&self, value: i64) {
        if !matches!(self.type_tag, ListTypeTag::Primitive) {
            unsafe { crate::share::share_value(value, self.type_tag) };
        }
    }

    pub(super) fn insert_value_at(&mut self, index: usize, value: i64) {
        self.share_stored(value);
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

    #[inline(always)]
    pub(super) fn push_value(&mut self, value: i64) {
        self.share_stored(value);
        if self.uses_value_storage() {
            self.values8.push(value);
            // the view after a push is never empty, so this is
            // sync_value_view without its branches
            self.values8_len = self.values8.len();
            self.values8_ptr = self.values8.as_ptr();
        } else {
            let bytes = value.to_ne_bytes();
            let elem_len = self.elem_size.min(bytes.len());
            self.elements.push(bytes[..elem_len].to_vec());
        }
    }

    fn pop_value(&mut self) -> Option<i64> {
        if self.uses_value_storage() {
            let popped = self.values8.pop();
            self.sync_value_view();
            popped
        } else {
            self.elements.pop().map(|elem| Self::read_i64_padded(&elem))
        }
    }

    unsafe fn get_value_unchecked(&self, index: usize) -> i64 {
        if self.uses_value_storage() {
            // SAFETY: Caller guarantees `index < self.len()`.
            unsafe { *self.values8.get_unchecked(index) }
        } else {
            // SAFETY: Caller guarantees `index < self.len()`.
            let elem = unsafe { self.elements.get_unchecked(index) };
            Self::read_i64_padded(elem)
        }
    }

    pub(crate) fn get_value(&self, index: usize) -> Option<i64> {
        if self.uses_value_storage() {
            self.values8.get(index).copied()
        } else {
            self.elements.get(index).map(|elem| Self::read_i64_padded(elem))
        }
    }

    pub(super) fn set(&mut self, index: usize, elem: &[u8]) {
        if self.uses_value_storage() {
            if index < self.values8.len() {
                let value = Self::read_i64_padded(elem);
                self.share_stored(value);
                self.values8[index] = value;
            }
        } else if index < self.elements.len() {
            self.elements[index] = elem.to_vec();
        }
    }

    pub(super) fn set_value(&mut self, index: usize, value: i64) {
        self.share_stored(value);
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

    pub(super) fn remove(&mut self, index: usize) -> Option<Vec<u8>> {
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

    pub(super) fn clear(&mut self) {
        if self.uses_value_storage() {
            self.values8.clear();
            self.sync_value_view();
        } else {
            self.elements.clear();
        }
    }
}

/// Decode the wire form of an element tag. The codes are part of the ir
/// contract: the emitter writes them into `pith_list_new` and into the
/// kind-carrying map stores, so they must not be renumbered.
pub(crate) fn element_tag_from_code(code: i32) -> ListTypeTag {
    match code {
        1 => ListTypeTag::String,
        2 => ListTypeTag::List,
        3 => ListTypeTag::Map,
        4 => ListTypeTag::Struct,
        5 => ListTypeTag::Bytes,
        6 => ListTypeTag::Closure,
        7 => ListTypeTag::Set,
        8 => ListTypeTag::Channel,
        9 => ListTypeTag::Sync,
        _ => ListTypeTag::Primitive,
    }
}

/// The list owns one count per heap element. These pick the right retain
/// and release operation for the element tag.
pub(crate) unsafe fn retain_element(tag: ListTypeTag, raw: i64) {
    match tag {
        ListTypeTag::String => {
            crate::perf_count(&crate::PERF_CSTRING_RETAINS_PUSH, 1);
            // SAFETY: Caller guarantees `raw` is a valid cstring handle for this tag.
            unsafe { crate::pith_cstring_retain(raw as *const i8) };
        }
        ListTypeTag::List => unsafe { pith_list_retain_handle(raw) },
        ListTypeTag::Map => unsafe { crate::collections::map::pith_map_retain_handle(raw) },
        ListTypeTag::Struct => unsafe { crate::runtime_core::pith_struct_retain(raw) },
        ListTypeTag::Bytes => unsafe { crate::bytes::pith_bytes_retain(raw) },
        ListTypeTag::Closure => unsafe { crate::runtime_core::pith_closure_retain(raw) },
        ListTypeTag::Set => unsafe { crate::collections::set::pith_set_retain_handle(raw) },
        ListTypeTag::Channel => unsafe { crate::concurrency::channel::pith_channel_retain(raw) },
        ListTypeTag::Sync => unsafe { crate::concurrency::sync_handle::pith_sync_retain(raw) },
        ListTypeTag::Primitive => {}
    }
}

/// The count the list is about to give up at `index`, or `None` when it owns
/// nothing there. Primitive lists own no counts and may store elements
/// narrower than 8 bytes, so this must not read their storage at all.
pub(super) fn displaced_element(impl_ref: &ListImpl, index: usize) -> Option<i64> {
    if matches!(impl_ref.type_tag, ListTypeTag::Primitive) {
        return None;
    }
    impl_ref.get_value(index)
}

/// Detach and clear every element the list owns, returning `(tag, raw_handles)`
/// so the caller can drop its `&mut ListImpl` borrow (and, on final release,
/// scrub magic and free the shell) BEFORE releasing any child handles.
/// Primitive lists clear in place with zero allocation.
#[must_use]
pub(super) fn take_elements_for_release(impl_ref: &mut ListImpl) -> (ListTypeTag, Vec<i64>) {
    let tag = impl_ref.type_tag;
    if matches!(tag, ListTypeTag::Primitive) {
        impl_ref.clear();
        return (tag, Vec::new());
    }
    if impl_ref.uses_value_storage() {
        let vals = std::mem::take(&mut impl_ref.values8);
        impl_ref.sync_value_view();
        (tag, vals)
    } else {
        let mut vals = Vec::with_capacity(impl_ref.len());
        for i in 0..impl_ref.len() {
            if let Some(raw) = impl_ref.get_value(i) {
                vals.push(raw);
            }
        }
        impl_ref.clear();
        (tag, vals)
    }
}

/// Release elements previously detached by `take_elements_for_release`.
///
/// # Safety
/// Every handle in `elems` must hold one count of kind `tag`.
pub(super) unsafe fn release_taken_elements(tag: ListTypeTag, elems: Vec<i64>) {
    if matches!(tag, ListTypeTag::Primitive) {
        return;
    }
    for raw in elems {
        crate::perf_count(&crate::PERF_LIST_CASCADE_RELEASES, 1);
        // SAFETY: List owned one count per detached heap element.
        unsafe { release_element(tag, raw) };
    }
}

pub(crate) unsafe fn release_element(tag: ListTypeTag, raw: i64) {
    match tag {
        ListTypeTag::String => unsafe { crate::pith_cstring_release(raw as *const i8) },
        ListTypeTag::List => unsafe { pith_list_release_handle(raw) },
        ListTypeTag::Map => unsafe { crate::collections::map::pith_map_release_handle(raw) },
        ListTypeTag::Struct => unsafe { crate::runtime_core::pith_struct_release(raw) },
        ListTypeTag::Bytes => unsafe { crate::bytes::pith_bytes_release(raw) },
        ListTypeTag::Closure => unsafe { crate::runtime_core::pith_closure_release(raw) },
        ListTypeTag::Set => unsafe { crate::collections::set::pith_set_release_handle(raw) },
        ListTypeTag::Channel => unsafe { crate::concurrency::channel::pith_channel_release(raw) },
        ListTypeTag::Sync => unsafe { crate::concurrency::sync_handle::pith_sync_release(raw) },
        ListTypeTag::Primitive => {}
    }
}

/// Fast validity check for the hot accessors: one memory read against the
/// magic word instead of a global registry lock per access. Freed lists get
/// their magic scrubbed in pith_list_free, so stale handles fail the compare.
/// is_list_ptr adds a second check where a magic collision would be
/// unacceptable (pith_auto_len).
#[inline]
pub(super) unsafe fn list_magic_ok(ptr: *const ()) -> bool {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { list_magic_ok_for(ptr, crate::quarantine::Op::Use) }
}

/// `list_magic_ok`, naming what the caller does with the list in a
/// quarantine report.
#[inline(always)]
pub(super) unsafe fn list_magic_ok_for(ptr: *const (), op: crate::quarantine::Op) -> bool {
    handle_registry::plausibly_aligned::<ListImpl>(ptr)
        && unsafe {
            // SAFETY: Alignment verified above; `magic_is` reads the magic field to validate.
            crate::quarantine::magic_is(
                std::ptr::addr_of!((*(ptr as *const ListImpl)).magic),
                LIST_MAGIC,
                op,
            )
        }
}

/// Validate `list` and borrow its `ListImpl` immutably for the duration of `f`.
#[inline(always)]
pub(super) unsafe fn with_list_ref<R>(list: PithList, f: impl FnOnce(&ListImpl) -> R) -> Option<R> {
    // SAFETY: Magic check validates pointer before dereference; borrow is scoped to `f`.
    if !unsafe { list_magic_ok(list.ptr as *const ()) } {
        return None;
    }
    let impl_ref = unsafe { &*(list.ptr as *const ListImpl) };
    Some(f(impl_ref))
}

/// Validate `list` and borrow its `ListImpl` mutably for the duration of `f`.
#[inline(always)]
pub(super) unsafe fn with_list_mut<R>(list: PithList, f: impl FnOnce(&mut ListImpl) -> R) -> Option<R> {
    // SAFETY: Magic check validates pointer before dereference; caller ensures exclusive access for `f`.
    if !unsafe { list_magic_ok(list.ptr as *const ()) } {
        return None;
    }
    let impl_mut = unsafe { &mut *(list.ptr as *mut ListImpl) };
    Some(f(impl_mut))
}

/// Validate `handle` and borrow its `ListImpl` immutably for the duration of `f`.
#[inline(always)]
pub(crate) unsafe fn with_list_ref_handle<R>(
    handle: i64,
    f: impl FnOnce(&ListImpl) -> R,
) -> Option<R> {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { with_list_ref(PithList { ptr: handle as *mut () }, f) }
}

/// Validate `handle` and borrow its `ListImpl` mutably for the duration of `f`.
#[inline(always)]
pub(crate) unsafe fn with_list_mut_handle<R>(
    handle: i64,
    f: impl FnOnce(&mut ListImpl) -> R,
) -> Option<R> {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { with_list_mut(PithList { ptr: handle as *mut () }, f) }
}

/// Validate two lists and borrow both immutably for the duration of `f`.
#[inline(always)]
unsafe fn with_list_ref2<R>(
    a: PithList,
    b: PithList,
    f: impl FnOnce(&ListImpl, &ListImpl) -> R,
) -> Option<R> {
    if !unsafe { list_magic_ok(a.ptr as *const ()) }
        || !unsafe { list_magic_ok(b.ptr as *const ()) }
    {
        return None;
    }
    // SAFETY: Both pointers passed magic validation and both borrows are shared (`&ListImpl`),
    // so aliasing `a.ptr == b.ptr` is sound.
    let a_ref = unsafe { &*(a.ptr as *const ListImpl) };
    let b_ref = unsafe { &*(b.ptr as *const ListImpl) };
    Some(f(a_ref, b_ref))
}

/// Validate two list handles and borrow both immutably for the duration of `f`.
#[inline(always)]
pub(crate) unsafe fn with_list_ref2_handle<R>(
    a: i64,
    b: i64,
    f: impl FnOnce(&ListImpl, &ListImpl) -> R,
) -> Option<R> {
    // SAFETY: Preconditions forwarded from caller.
    unsafe {
        with_list_ref2(
            PithList { ptr: a as *mut () },
            PithList { ptr: b as *mut () },
            f,
        )
    }
}

/// Validate `dst` (mutable) and `src` (shared) without aliasing UB.
///
/// When `dst` and `src` point to the same list, invokes `on_same` with a single `&mut ListImpl`
/// instead of minting overlapping `&mut` and `&` references.
#[allow(dead_code)]
#[inline(always)]
pub(super) unsafe fn with_list_mut_and_ref<R>(
    dst: PithList,
    src: PithList,
    on_distinct: impl FnOnce(&mut ListImpl, &ListImpl) -> R,
    on_same: impl FnOnce(&mut ListImpl) -> R,
) -> Option<R> {
    if !unsafe { list_magic_ok(dst.ptr as *const ()) } {
        return None;
    }
    if dst.ptr == src.ptr {
        // SAFETY: `dst` validated; only one `&mut` reference is minted.
        let same = unsafe { &mut *(dst.ptr as *mut ListImpl) };
        return Some(on_same(same));
    }
    if !unsafe { list_magic_ok(src.ptr as *const ()) } {
        return None;
    }
    // SAFETY: `dst` and `src` are both valid and distinct pointers.
    let dst_mut = unsafe { &mut *(dst.ptr as *mut ListImpl) };
    let src_ref = unsafe { &*(src.ptr as *const ListImpl) };
    Some(on_distinct(dst_mut, src_ref))
}

/// Borrow the elements of a primitive 8-byte list mutably for the duration of `f`.
#[inline(always)]
pub(crate) unsafe fn with_primitive_words_mut<R>(
    handle: i64,
    f: impl FnOnce(&mut [i64]) -> R,
) -> Option<R> {
    let body = |list: &mut ListImpl| {
        if list.type_tag != ListTypeTag::Primitive || !list.uses_value_storage() {
            return None;
        }
        Some(f(&mut list.values8[..]))
    };
    // SAFETY: Preconditions forwarded from caller.
    unsafe { with_list_mut_handle(handle, body) }.flatten()
}
/// A per-process random key, so that no string can be written to pass for a
/// list: the identity word is the list's own address mixed with it.
pub(super) fn identity_key() -> u64 {
    static KEY: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *KEY.get_or_init(|| {
        use std::hash::BuildHasher;
        std::collections::hash_map::RandomState::new().hash_one(0x4c49_5354u64) | 1
    })
}

/// The identity word a live list at `ptr` carries. Never 0, because the key
/// is odd and the address is aligned, so a freed list's 0 never matches.
pub(super) fn identity_tag(ptr: *const ()) -> u64 {
    (ptr as u64) ^ identity_key()
}

/// Whether the magic and identity words at `ptr` would reach into the next
/// page. A pointer that is really a C string is only known to be readable up
/// to its NUL, but the page it starts on is mapped, so is_list_ptr reads the
/// identity word only when it lies on that page. A list that would cross is
/// recorded in the handle registry instead, which answers for it without
/// reading anything. The allocator hands out 16-aligned blocks, so in
/// practice no list does.
pub(super) fn crosses_page(ptr: *const ()) -> bool {
    const PAGE: usize = 4096;
    const CHECKED: usize = std::mem::offset_of!(ListImpl, identity) + std::mem::size_of::<u64>();
    (ptr as usize) % PAGE > PAGE - CHECKED
}

/// Forget a list that is being freed: clear the identity word and drop the
/// registry entry a page-crossing list has.
pub(super) unsafe fn retire_identity(ptr: *mut ()) {
    // SAFETY: Caller has validated `ptr` as a live `ListImpl` being torn down.
    unsafe { (*(ptr as *mut ListImpl)).identity = 0 };
    if crosses_page(ptr) {
        handle_registry::unregister(ptr as *const (), HandleKind::List);
    }
}

/// Get list length
#[no_mangle]
pub extern "C" fn pith_list_len(list: PithList) -> i64 {
    // SAFETY: `with_list_ref` validates `list` before reading its length.
    unsafe { with_list_ref(list, |impl_ref| impl_ref.len() as i64) }.unwrap_or(0)
}

/// Check if a raw pointer really is a ListImpl. The magic word rejects
/// almost everything cheaply. A C string could start with the same four
/// bytes, and this function is what keeps pith_auto_len from misreading one
/// as a list, so a match is confirmed by the identity word: the address mixed
/// with a random per-process key, which a string cannot be written to hold.
/// A pointer whose identity word would cross into the next page is answered
/// by the registry, so that nothing past the page a string starts on is read.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn is_list_ptr(ptr: *const ()) -> bool {
    if !(unsafe { list_magic_ok(ptr) }) {
        return false;
    }
    if crosses_page(ptr) {
        return handle_registry::is_valid(ptr, HandleKind::List);
    }
    // SAFETY: the magic read showed `ptr` is aligned and readable, and the
    // identity word lies on the page it starts on.
    unsafe { (*(ptr as *const ListImpl)).identity == identity_tag(ptr) }
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
