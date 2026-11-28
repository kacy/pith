use super::lifecycle::*;
use super::ops::*;
use super::*;
use hashbrown::Equivalent;
use std::hash::{Hash, Hasher};

fn bogus_set() -> PithSet {
    PithSet {
        ptr: 12345usize as *mut (),
    }
}

#[test]
fn invalid_set_handles_return_safe_defaults() {
    unsafe {
        let mut set = bogus_set();
        assert_eq!(pith_set_len(bogus_set()), 0);
        assert!(!pith_set_contains_int(bogus_set(), 7));
        assert_eq!(pith_set_len_handle(12345), 0);
        assert_eq!(pith_set_contains_int_handle(12345, 7), 0);
        assert_eq!(pith_set_is_empty_handle(12345), 1);
        pith_set_clear(&mut set);
        pith_set_clear_handle(12345);
        pith_set_release(bogus_set());
    }
}

#[test]
fn released_set_handles_are_rejected() {
    unsafe {
        let set = pith_set_new(0, 8);
        let handle = set.ptr as i64;
        assert_eq!(pith_set_len(set), 0);
        pith_set_release(set);
        assert_eq!(pith_set_len(set), 0);
        assert_eq!(pith_set_len_handle(handle), 0);
        assert_eq!(pith_set_is_empty_handle(handle), 1);
        pith_set_release(set);
    }
}

/// A bytes set hashes and compares by content: two objects built by
/// different routes with the same bytes are one element, two values that
/// share a prefix and a length are two, and the empty value is a member
/// like any other. The set keeps copies, so the caller's count on each
/// handle is untouched by every operation.
#[test]
fn bytes_set_is_keyed_by_content() {
    unsafe {
        let set = pith_set_new_bytes();
        let from_vec = crate::bytes::pith_bytes_from_vec(b"abc".to_vec());
        let from_str = crate::bytes::pith_bytes_from_string_utf8(c"abc".as_ptr());
        let sibling = crate::bytes::pith_bytes_from_vec(b"abd".to_vec());
        let empty = crate::bytes::pith_bytes_from_vec(Vec::new());
        assert_ne!(from_vec, from_str, "two distinct objects");

        assert_eq!(pith_set_add_bytes(set, from_vec), 1);
        assert_eq!(
            pith_set_add_bytes(set, from_str),
            0,
            "same content, one entry"
        );
        assert_eq!(
            pith_set_add_bytes(set, sibling),
            1,
            "shared prefix, distinct"
        );
        assert_eq!(pith_set_add_bytes(set, empty), 1);
        assert_eq!(pith_set_len_handle(set), 3);

        assert_eq!(pith_set_contains_bytes(set, from_str), 1);
        assert_eq!(pith_set_contains_bytes(set, sibling), 1);
        assert_eq!(pith_set_contains_bytes(set, empty), 1);
        assert_eq!(
            pith_set_contains_bytes(set, 0),
            1,
            "null is the empty value"
        );
        let absent = crate::bytes::pith_bytes_from_vec(b"ab".to_vec());
        assert_eq!(
            pith_set_contains_bytes(set, absent),
            0,
            "a prefix is not a member"
        );

        pith_set_remove_bytes(set, from_vec);
        assert_eq!(
            pith_set_contains_bytes(set, from_str),
            0,
            "removed by content"
        );
        assert_eq!(pith_set_len_handle(set), 2);

        // the list of members owns fresh bytes objects, one per element
        let list = pith_set_to_list_bytes(set);
        let list_handle = crate::collections::list::PithList {
            ptr: list as *mut (),
        };
        assert_eq!(crate::collections::list::pith_list_len(list_handle), 2);
        let first = crate::collections::list::pith_list_get_value(list_handle, 0);
        assert_ne!(first, sibling);
        assert_ne!(first, empty);
        assert!(crate::bytes::with_bytes(first, |_| ()).is_some());
        crate::collections::list::pith_list_release_handle(list);

        for h in [from_vec, from_str, sibling, empty, absent] {
            let rc = crate::bytes::with_bytes_obj(h, |b| {
                b.rc.load(std::sync::atomic::Ordering::Relaxed)
            })
            .expect("the set never took a count");
            assert_eq!(rc, 1);
            crate::bytes::pith_bytes_release(h);
        }
        pith_set_release_handle(set);
    }
}

// --- the borrowed-element contract --------------------------------------

fn hash_probe_inputs() -> Vec<Vec<u8>> {
    vec![
        Vec::new(),
        b"a".to_vec(),
        b"ab".to_vec(),
        b"abc".to_vec(),
        b"user-1000-region".to_vec(),
        vec![0u8],
        vec![b'a', 0, b'b'],
        vec![0, 0, 0, 0],
        "\u{e9}".as_bytes().to_vec(),
        "\u{65e5}\u{672c}\u{8a9e}".as_bytes().to_vec(),
        "\u{1f600}".as_bytes().to_vec(),
        vec![b'x'; 255],
        vec![b'y'; 4096],
        vec![0u8; 8],
    ]
}

fn hash_with(state: &std::collections::hash_map::RandomState, value: &impl Hash) -> u64 {
    use std::hash::BuildHasher;
    let mut hasher = state.build_hasher();
    value.hash(&mut hasher);
    hasher.finish()
}

/// The set's half of the silent hazard: `ElemRef` is what every probe
/// hashes and `SetElement` is what the table stores. If the two ever wrote
/// different bytes for the same content, every string set in every program
/// would stop finding elements it holds, with no diagnostic anywhere.
#[test]
fn elem_ref_hashes_like_set_element() {
    let state = std::collections::hash_map::RandomState::new();
    for bytes in hash_probe_inputs() {
        assert_eq!(
            hash_with(&state, &ElemRef::Str(&bytes)),
            hash_with(&state, &SetElement::String(bytes.clone())),
            "string flavor, {} bytes",
            bytes.len()
        );
        assert_eq!(
            hash_with(&state, &ElemRef::Bytes(&bytes)),
            hash_with(&state, &SetElement::Bytes(bytes.clone())),
            "bytes flavor, {} bytes",
            bytes.len()
        );
        assert_ne!(
            hash_with(&state, &ElemRef::Str(&bytes)),
            hash_with(&state, &ElemRef::Bytes(&bytes)),
            "flavors must not collide, {} bytes",
            bytes.len()
        );
        assert!(ElemRef::Str(&bytes).equivalent(&SetElement::String(bytes.clone())));
        assert!(ElemRef::Bytes(&bytes).equivalent(&SetElement::Bytes(bytes.clone())));
        assert!(!ElemRef::Str(&bytes).equivalent(&SetElement::Bytes(bytes.clone())));
        assert_eq!(
            SetElement::from(&ElemRef::Str(&bytes)),
            SetElement::String(bytes.clone())
        );
        assert_eq!(
            SetElement::from(&ElemRef::Bytes(&bytes)),
            SetElement::Bytes(bytes.clone())
        );

        // and through the table rather than the hasher
        let mut table: HashMap<SetElement, ()> = HashMap::new();
        table.insert(SetElement::String(bytes.clone()), ());
        table.insert(SetElement::Bytes(bytes.clone()), ());
        assert!(table.contains_key(&ElemRef::Str(&bytes)));
        assert!(table.contains_key(&ElemRef::Bytes(&bytes)));
    }
    assert_ne!(
        hash_with(&state, &ElemRef::Str(b"ab")),
        hash_with(&state, &ElemRef::Str(b"abc"))
    );
    for n in [i64::MIN, -1, 0, 1, 255, i64::MAX] {
        assert_eq!(
            hash_with(&state, &ElemRef::Int(n)),
            hash_with(&state, &SetElement::Int(n)),
            "int flavor, {n}"
        );
        assert!(ElemRef::Int(n).equivalent(&SetElement::Int(n)));
    }
}

#[test]
fn set_hits_allocate_nothing_for_the_element() {
    use crate::collections::alloc_probe::allocations_during;
    unsafe {
        let set = pith_set_new_default();
        let elem = b"user-1000-region\0".as_ptr() as *const i8;
        let absent = b"user-1001-region\0".as_ptr() as *const i8;
        pith_set_add_cstr(set, elem);

        assert_eq!(
            allocations_during(|| {
                pith_set_contains_cstr(set, elem);
            }),
            0
        );
        assert_eq!(
            allocations_during(|| {
                pith_set_contains_cstr(set, absent);
            }),
            0
        );
        // adding what the set already holds reports "not new" and
        // allocates nothing
        assert_eq!(
            allocations_during(|| {
                assert_eq!(pith_set_add_cstr(set, elem), 0);
            }),
            0
        );
        assert_eq!(
            allocations_during(|| {
                pith_set_remove_cstr(set, elem);
            }),
            0
        );
        assert_eq!(pith_set_len_handle(set), 0);

        pith_set_release_handle(set);
    }
}

#[test]
fn bytes_set_hits_allocate_nothing_for_the_element() {
    use crate::collections::alloc_probe::allocations_during;
    unsafe {
        let set = pith_set_new_bytes();
        let elem = crate::bytes::pith_bytes_from_vec(b"user-1000-region".to_vec());
        let absent = crate::bytes::pith_bytes_from_vec(b"user-1001-region".to_vec());
        pith_set_add_bytes(set, elem);

        assert_eq!(
            allocations_during(|| {
                pith_set_contains_bytes(set, elem);
            }),
            0
        );
        assert_eq!(
            allocations_during(|| {
                pith_set_contains_bytes(set, absent);
            }),
            0
        );
        assert_eq!(
            allocations_during(|| {
                assert_eq!(pith_set_add_bytes(set, elem), 0);
            }),
            0
        );
        assert_eq!(
            allocations_during(|| {
                pith_set_remove_bytes(set, elem);
            }),
            0
        );
        assert_eq!(pith_set_len_handle(set), 0);

        crate::bytes::pith_bytes_release(elem);
        crate::bytes::pith_bytes_release(absent);
        pith_set_release_handle(set);
    }
}

/// A fresh element costs exactly the one copy the set keeps.
#[test]
fn a_new_set_element_allocates_once() {
    use crate::collections::alloc_probe::allocations_during;
    unsafe {
        let set = pith_set_new_default();
        pith_set_add_cstr(set, b"seed\0".as_ptr() as *const i8);
        let fresh = b"an-element-the-set-has-never-seen\0".as_ptr() as *const i8;
        assert_eq!(
            allocations_during(|| {
                assert_eq!(pith_set_add_cstr(set, fresh), 1);
            }),
            1
        );
        assert_eq!(pith_set_contains_cstr(set, fresh), 1);
        pith_set_release_handle(set);
    }
}

#[test]
fn with_set_mut_and_ref_handles_self_aliasing_and_distinct_sets() {
    unsafe {
        let a = pith_set_new(0, 8);
        let b = pith_set_new(0, 8);
        pith_set_add_int_handle(a.ptr as i64, 1);
        pith_set_add_int_handle(b.ptr as i64, 2);

        // distinct sets take `on_distinct`
        let distinct = with_set_mut_and_ref(
            a,
            b,
            |dst, src| {
                for elem in src.iter() {
                    if let SetElement::Int(n) = elem {
                        dst.insert(&ElemRef::Int(*n));
                    }
                }
                "distinct"
            },
            |_| "same",
        );
        assert_eq!(distinct, Some("distinct"));
        assert_eq!(pith_set_len(a), 2);
        assert!(pith_set_contains_int(a, 1));
        assert!(pith_set_contains_int(a, 2));

        // self-aliasing takes `on_same` with a single `&mut SetImpl`
        let same = with_set_mut_and_ref(
            a,
            a,
            |_, _| "distinct",
            |set| {
                set.insert(&ElemRef::Int(3));
                "same"
            },
        );
        assert_eq!(same, Some("same"));
        assert!(pith_set_contains_int(a, 3));

        // invalid handles return None
        assert!(with_set_mut_and_ref(bogus_set(), a, |_, _| (), |_| ()).is_none());
        assert!(with_set_mut_and_ref(a, bogus_set(), |_, _| (), |_| ()).is_none());

        // shared ref2 supports both distinct and aliased handles
        assert_eq!(
            with_set_ref2_handle(a.ptr as i64, b.ptr as i64, |x, y| (x.len(), y.len())),
            Some((3, 1))
        );
        assert_eq!(
            with_set_ref2_handle(a.ptr as i64, a.ptr as i64, |x, y| x.len() + y.len()),
            Some(6)
        );
        assert!(with_set_ref2_handle(a.ptr as i64, 12345, |_, _| ()).is_none());

        pith_set_release(a);
        pith_set_release(b);
    }
}
