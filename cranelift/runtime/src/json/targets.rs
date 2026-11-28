//! Ownership-safe struct, list, and map fill targets plus field presence bitset.

use super::spec::{nested_elem_tag, nested_spec_field_count, NestedAbort};

/// The outcome of one object fill: the position after the object and, when
/// a field of it or of a struct under it was missing or of the wrong type,
/// the message for the first such field in declaration order. That error
/// waits with the caller until the whole document has parsed, because a
/// malformed byte anywhere outranks it and a field declared earlier in the
/// parent outranks it too.
pub(super) type NestedFill = Result<(usize, Option<Vec<u8>>), NestedAbort>;

/// A validated destructor address taken from the lowering's destructor table
/// (0 means no destructor is needed for the struct).
#[derive(Clone, Copy)]
pub(super) struct ValidDtor(i64);

impl ValidDtor {
    /// Allocate the struct a list element or a struct field under one needs:
    /// `field_count` zeroed slots, with this destructor attached when non-zero.
    /// Returns `None` when the allocation failed, which the caller treats as a
    /// malformed fill.
    #[inline(always)]
    pub(super) fn alloc_object(self, spec: &[u8]) -> Option<ObjectTarget<'_>> {
        let field_count = nested_spec_field_count(spec);
        // SAFETY: Allocates a fresh zeroed struct with `field_count` slots.
        let ptr = unsafe { crate::pith_struct_alloc(field_count as i64) };
        if ptr == 0 {
            return None;
        }
        if self.0 != 0 {
            // SAFETY: `ptr` is a freshly allocated struct and `self.0` came from `DtorTable`.
            unsafe { crate::pith_struct_set_dtor(ptr, self.0) };
        }
        // SAFETY: `ptr` has `nested_spec_field_count(spec)` zeroed slots matching `spec`.
        Some(unsafe { ObjectTarget::from_raw(spec, ptr) })
    }
}

/// The table the entry points thread through the fill: the destructor
/// addresses a list element struct may name.
pub(super) struct DtorTable<'a>(pub(super) &'a [i64]);

impl<'a> DtorTable<'a> {
    pub(super) const EMPTY: DtorTable<'static> = DtorTable(&[]);

    /// # Safety
    /// When `ptr != 0` and `count > 0`, `ptr` must point to at least `count` readable
    /// destructor function addresses (or 0 for no destructor) valid for `'a`.
    #[inline(always)]
    pub(super) unsafe fn from_raw(ptr: i64, count: i64) -> Self {
        if ptr == 0 || count <= 0 {
            return DtorTable(&[]);
        }
        // SAFETY: Preconditions forwarded from caller.
        DtorTable(unsafe { std::slice::from_raw_parts(ptr as *const i64, count as usize) })
    }

    /// The destructor address at index `k` of the table the lowering passed,
    /// or None when the table has no such entry (a spec naming an index the
    /// table lacks is a lowering defect, and the fill fails rather than
    /// attaching a wrong address). `k` is -1 for a pre-allocated struct, which
    /// never asks.
    #[inline(always)]
    pub(super) fn get(&self, k: i64) -> Option<ValidDtor> {
        let idx = usize::try_from(k).ok()?;
        self.0.get(idx).copied().map(ValidDtor)
    }
}

/// A live Pith struct allocation paired with the spec describing its slots.
///
/// Invariant: `obj` has one `i64` slot per field in `spec`, and each pre-allocated
/// `o` field (without `#<k>`) holds either 0 or a sub-struct laid out by its sub-spec.
#[derive(Clone, Copy)]
pub(super) struct ObjectTarget<'a> {
    pub(super) spec: &'a [u8],
    obj: *mut i64,
}

impl<'a> ObjectTarget<'a> {
    /// # Safety
    /// `struct_ptr` must be a `pith_struct_alloc` result laid out by `spec`, with
    /// each pre-allocated `o` slot holding a struct laid out by its sub-spec.
    #[inline(always)]
    pub(super) unsafe fn from_raw(spec: &'a [u8], struct_ptr: i64) -> Self {
        Self {
            spec,
            obj: struct_ptr as *mut i64,
        }
    }

    #[inline(always)]
    pub(super) fn as_handle(self) -> i64 {
        self.obj as i64
    }

    #[inline(always)]
    pub(super) fn read_slot(self, idx: usize) -> i64 {
        // SAFETY: `idx` came from `nested_lookup(self.spec, ...)`, so it is in bounds of `self.obj`.
        unsafe { *self.obj.add(idx) }
    }

    #[inline(always)]
    pub(super) fn write_slot(self, idx: usize, value: i64) {
        // SAFETY: `idx` came from `nested_lookup(self.spec, ...)`, so it is in bounds of `self.obj`.
        unsafe { *self.obj.add(idx) = value };
    }

    #[inline(always)]
    pub(super) fn replace_string(self, idx: usize, bytes: &[u8]) {
        let prev = self.read_slot(idx);
        if prev != 0 {
            // SAFETY: Slot holds an owned pith C string written by an earlier occurrence of this key.
            unsafe { crate::pith_cstring_release(prev as *const i8) };
        }
        self.write_slot(idx, crate::pith_copy_bytes_to_cstring(bytes) as i64);
    }

    #[inline(always)]
    pub(super) fn has_object_slot(self, idx: usize, dtor: i64) -> bool {
        dtor >= 0 || self.read_slot(idx) != 0
    }

    #[inline(always)]
    pub(super) fn child_object(self, idx: usize, sub_spec: &'a [u8]) -> ObjectTarget<'a> {
        let child = self.read_slot(idx);
        // SAFETY: By `ObjectTarget`'s invariant, slot `idx` holds a struct laid out by `sub_spec`.
        unsafe { ObjectTarget::from_raw(sub_spec, child) }
    }

    #[inline(always)]
    pub(super) fn clear_struct(self, idx: usize) {
        let prev = self.read_slot(idx);
        if prev != 0 {
            // SAFETY: Slot holds an owned struct allocated by an earlier occurrence of this key.
            unsafe { crate::pith_struct_release(prev) };
            self.write_slot(idx, 0);
        }
    }

    #[inline(always)]
    pub(super) fn clear_list(self, idx: usize) {
        let prev = self.read_slot(idx);
        if prev != 0 {
            // SAFETY: Slot holds an owned list allocated by an earlier occurrence of this key.
            unsafe { crate::collections::list::pith_list_release_handle(prev) };
            self.write_slot(idx, 0);
        }
    }

    #[inline(always)]
    pub(super) fn clear_map(self, idx: usize) {
        let prev = self.read_slot(idx);
        if prev != 0 {
            // SAFETY: Slot holds an owned map allocated by an earlier occurrence of this key.
            unsafe { crate::collections::map::pith_map_release_handle(prev) };
            self.write_slot(idx, 0);
        }
    }
}
/// A live list handle allocated for a decoded list field or element.
#[derive(Clone, Copy)]
pub(super) struct NestedList(crate::collections::list::PithList);

impl NestedList {
    /// The list a field of element spec `elem` holds: the constructor is the
    /// element kind's, the same one the emitter picks for a list literal of
    /// that type (`ir_list_ctor_for_elem_kind`), so a list of strings, structs
    /// or lists owns its elements and its release cascades, and a list of
    /// ints, bools or floats holds them by value.
    #[inline(always)]
    pub(super) fn new(elem: &[u8]) -> Option<Self> {
        use crate::collections::list;
        // SAFETY: All `pith_list_new_*` constructors allocate a fresh list with no pointer inputs.
        let handle = match elem.first() {
            Some(b's') => unsafe { list::pith_list_new_cstr() },
            Some(b'o') => unsafe { list::pith_list_new_struct() },
            Some(b'l') => unsafe { list::pith_list_new_nested_list() },
            Some(b'm') => unsafe { list::pith_list_new_nested_map() },
            _ => unsafe { list::pith_list_new_default() },
        };
        (!handle.ptr.is_null()).then_some(NestedList(handle))
    }

    #[inline(always)]
    pub(super) fn as_handle(self) -> i64 {
        self.0.ptr as i64
    }

    #[inline(always)]
    pub(super) fn push_value(self, value: i64) {
        // SAFETY: `self.0` is a live list built by `NestedList::new` for primitive elements.
        unsafe { crate::collections::list::pith_list_push_value(self.0, value) };
    }

    #[inline(always)]
    pub(super) fn push_owned(self, value: i64) {
        // SAFETY: `self.0` is a live list built by `NestedList::new` and `value` transfers one count.
        unsafe { crate::collections::list::pith_list_push_value_owned(self.0, value) };
    }
}

/// A live string-keyed map handle allocated for a decoded map field or element.
#[derive(Clone, Copy)]
pub(super) struct NestedMap(i64);

impl NestedMap {
    /// The map a `Map[String, T]` field holds: string-keyed, owning its values
    /// by the element kind's tag, as the map a literal of that type builds
    /// does, so a map of strings, structs, lists or maps releases its values
    /// with itself and a map of ints, bools or floats holds them by value.
    #[inline(always)]
    pub(super) fn new(elem: &[u8]) -> Option<Self> {
        // SAFETY: Allocates a fresh string-keyed map with value tag `nested_elem_tag(elem)`.
        let handle = unsafe { crate::collections::map::new_string_keyed_handle(nested_elem_tag(elem)) };
        (handle != 0).then_some(NestedMap(handle))
    }

    #[inline(always)]
    pub(super) fn as_handle(self) -> i64 {
        self.0
    }

    /// Store a member into a decode-built map: a heap value hands the map its
    /// one count (the allocation's, or the fresh string's), a scalar is copied.
    /// A key already present has its earlier value released by the map, so a
    /// repeated key leaves one value, the last, and nothing stranded.
    #[inline(always)]
    pub(super) fn insert(self, elem: &[u8], key: &[u8], value: i64) {
        use crate::collections::list::ListTypeTag;
        let owned = nested_elem_tag(elem) != ListTypeTag::Primitive;
        // SAFETY: `self.0` is a live string-keyed map built for `elem`; `value` matches `elem`.
        unsafe { crate::collections::map::insert_bytes_key(self.0, key, value, owned) };
    }
}
/// Compact bitset for field presence/mismatch tracking: inline `u64` for the
/// common <=64-field struct (zero heap allocation), spilling into `Vec<u64>`
/// for structs with 65+ fields so `1u64 << idx` never overflows.
pub(super) struct FieldBits {
    lo: u64,
    hi: Vec<u64>,
}

impl FieldBits {
    #[inline]
    pub(super) fn new() -> Self {
        FieldBits {
            lo: 0,
            hi: Vec::new(),
        }
    }

    #[inline]
    pub(super) fn set(&mut self, idx: usize) {
        if idx < 64 {
            self.lo |= 1u64 << idx;
        } else {
            let word = (idx - 64) / 64;
            let bit = (idx - 64) % 64;
            if self.hi.len() <= word {
                self.hi.resize(word + 1, 0);
            }
            self.hi[word] |= 1u64 << bit;
        }
    }

    #[inline]
    pub(super) fn clear(&mut self, idx: usize) {
        if idx < 64 {
            self.lo &= !(1u64 << idx);
        } else {
            let word = (idx - 64) / 64;
            let bit = (idx - 64) % 64;
            if let Some(slot) = self.hi.get_mut(word) {
                *slot &= !(1u64 << bit);
            }
        }
    }

    #[inline]
    pub(super) fn contains(&self, idx: usize) -> bool {
        if idx < 64 {
            (self.lo & (1u64 << idx)) != 0
        } else {
            let word = (idx - 64) / 64;
            let bit = (idx - 64) % 64;
            self.hi
                .get(word)
                .is_some_and(|w| (*w & (1u64 << bit)) != 0)
        }
    }
}
