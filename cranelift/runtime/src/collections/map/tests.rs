use super::lifecycle::*;
use super::ops::*;
use super::*;
use crate::collections::list::ListTypeTag;
use hashbrown::Equivalent;
use std::hash::{Hash, Hasher};

fn bogus_map() -> PithMap {
    PithMap {
        ptr: 12345usize as *mut (),
    }
}

#[test]
fn invalid_map_handles_return_safe_defaults() {
    unsafe {
        let mut map = bogus_map();
        assert_eq!(pith_map_len(bogus_map()), 0);
        assert_eq!(pith_map_len_handle(12345), 0);
        assert_eq!(pith_map_is_empty_handle(12345), 1);
        pith_map_clear(&mut map);
        pith_map_clear_handle(12345);
        pith_map_release(bogus_map());
    }
}

/// A string-valued int-key map holding one value the caller has already
/// let go of, so the map's count is the only one left.
unsafe fn map_owning_one_value(text: &[u8]) -> (i64, *mut i8) {
    let s = crate::pith_copy_bytes_to_cstring(text);
    let (handle, rc) = unsafe {
        let handle = pith_map_new_int_cstr_val().ptr as i64;
        pith_map_insert_ikey(handle, 1, s as i64);
        crate::pith_cstring_release(s);
        let rc = crate::cstring_refcount_for_tests(s);
        (handle, rc)
    };
    assert_eq!(rc, Some(1));
    (handle, s)
}

#[test]
fn removing_a_key_drops_the_maps_count_on_its_value() {
    unsafe {
        let (handle, s) = map_owning_one_value(b"evicted");
        pith_map_remove_ikey(handle, 1);
        assert_eq!(crate::cstring_refcount_for_tests(s), None);
        pith_map_release_handle(handle);
    }
}

#[test]
fn clearing_drops_the_maps_count_on_every_value() {
    unsafe {
        let (handle, s) = map_owning_one_value(b"cleared");
        pith_map_clear_handle(handle);
        assert_eq!(crate::cstring_refcount_for_tests(s), None);
        assert_eq!(pith_map_len_handle(handle), 0);
        pith_map_release_handle(handle);
    }
}

#[test]
fn overwriting_retains_the_new_value_before_releasing_the_old() {
    unsafe {
        let (handle, s) = map_owning_one_value(b"old");
        let t = crate::pith_copy_bytes_to_cstring(b"new");
        pith_map_insert_ikey(handle, 1, t as i64);
        assert_eq!(crate::cstring_refcount_for_tests(s), None);
        assert_eq!(crate::cstring_refcount_for_tests(t), Some(2));

        // storing a key over itself must not free the value in between
        pith_map_insert_ikey(handle, 1, t as i64);
        assert_eq!(crate::cstring_refcount_for_tests(t), Some(2));
        assert_eq!(pith_map_get_ikey(handle, 1), t as i64);

        pith_map_release_handle(handle);
        assert_eq!(crate::cstring_refcount_for_tests(t), Some(1));
        crate::pith_cstring_release(t);
    }
}

#[test]
fn a_second_owner_survives_the_maps_eviction() {
    unsafe {
        let handle = pith_map_new_cstr_val().ptr as i64;
        let s = crate::pith_copy_bytes_to_cstring(b"held elsewhere");
        let key = b"k\0".as_ptr() as *const i8;
        pith_map_insert_cstr(handle, key, s as i64);
        // the caller keeps its own count, as a binding of `m[k]` would
        assert_eq!(crate::cstring_refcount_for_tests(s), Some(2));

        pith_map_remove_cstr(handle, key);
        assert_eq!(crate::cstring_refcount_for_tests(s), Some(1));

        pith_map_release_handle(handle);
        crate::pith_cstring_release(s);
        assert_eq!(crate::cstring_refcount_for_tests(s), None);
    }
}

#[test]
fn values_returns_a_list_that_owns_what_it_hands_back() {
    unsafe {
        use crate::collections::list::{pith_list_get_value, pith_list_release_handle};

        let (handle, s) = map_owning_one_value(b"handed out");
        let vals = pith_map_values_handle(handle);
        // the list took its own count, so the map is free to evict
        assert_eq!(crate::cstring_refcount_for_tests(s), Some(2));

        pith_map_clear_handle(handle);
        assert_eq!(crate::cstring_refcount_for_tests(s), Some(1));
        assert_eq!(pith_list_get_value(
            crate::collections::list::PithList { ptr: vals as *mut () },
            0
        ), s as i64);

        pith_list_release_handle(vals);
        assert_eq!(crate::cstring_refcount_for_tests(s), None);
        pith_map_release_handle(handle);
    }
}

#[test]
fn plain_int_maps_never_touch_their_values() {
    unsafe {
        // 7 is not a pointer; a value-releasing map would dereference it
        let handle = pith_map_new_int().ptr as i64;
        pith_map_insert_ikey(handle, 1, 7);
        pith_map_insert_ikey(handle, 1, 8);
        assert_eq!(pith_map_get_ikey(handle, 1), 8);
        pith_map_remove_ikey(handle, 1);
        pith_map_clear_handle(handle);
        pith_map_release_handle(handle);
    }
}

/// A one-element list, and a liveness probe for it: a freed list has its
/// magic scrubbed, so its length reads back as 0 rather than 1.
unsafe fn one_element_list() -> i64 {
    use crate::collections::list::{pith_list_new_cstr, pith_list_push_value};
    let s = crate::pith_copy_bytes_to_cstring(b"elem");
    let list = unsafe {
        let list = pith_list_new_cstr();
        pith_list_push_value(list, s as i64);
        crate::pith_cstring_release(s);
        list
    };
    list.ptr as i64
}

fn list_is_alive(handle: i64) -> bool {
    use crate::collections::list::{pith_list_len, PithList};
    pith_list_len(PithList {
        ptr: handle as *mut (),
    }) == 1
}

#[test]
fn a_map_learns_its_value_kind_from_a_borrowed_store() {
    unsafe {
        use crate::collections::list::pith_list_release_handle;

        // built with no value flavor at all, the shape an empty `{}` in a
        // position the checker could not type produces
        let handle = pith_map_new_default().ptr as i64;
        let list = one_element_list();
        let key = b"k\0".as_ptr() as *const i8;
        pith_map_insert_cstr_kind(handle, key, list, ListTypeTag::List as i64);

        // the map took a count of its own, so the caller letting go is not
        // the last release
        pith_list_release_handle(list);
        assert!(list_is_alive(list));
        assert_eq!(pith_map_get_cstr(handle, key), list);

        pith_map_release_handle(handle);
        assert!(!list_is_alive(list));
    }
}

#[test]
fn an_owned_value_hands_its_count_to_the_map() {
    unsafe {
        let handle = pith_map_new_default().ptr as i64;
        let list = one_element_list();
        let key = b"k\0".as_ptr() as *const i8;
        // the caller stops tracking the value here and never releases it
        pith_map_insert_cstr_owned_kind(handle, key, list, ListTypeTag::List as i64);
        assert!(list_is_alive(list));

        pith_map_release_handle(handle);
        assert!(!list_is_alive(list));
    }
}

#[test]
fn an_int_keyed_map_leaves_the_scalar_fast_path_to_hold_handles() {
    unsafe {
        use crate::collections::list::pith_list_release_handle;

        let handle = pith_map_new_int().ptr as i64;
        assert!(with_map_ref_handle(handle, MapImpl::uses_int_values8).unwrap());
        let list = one_element_list();
        pith_map_insert_ikey_kind(handle, 7, list, ListTypeTag::List as i64);
        assert!(!with_map_ref_handle(handle, MapImpl::uses_int_values8).unwrap());
        assert_eq!(pith_map_get_ikey(handle, 7), list);
        assert_eq!(pith_map_len_handle(handle), 1);

        pith_list_release_handle(list);
        assert!(list_is_alive(list));
        pith_map_release_handle(handle);
        assert!(!list_is_alive(list));
    }
}

#[test]
fn a_map_that_cannot_adopt_leaks_rather_than_frees() {
    unsafe {
        use crate::collections::list::pith_list_release_handle;

        // a value already stored without a kind leaves the map holding no
        // count on it, so adopting a tag now would start releasing counts
        // the map never took. it must refuse, and take the compensating
        // count itself instead.
        let handle = pith_map_new_default().ptr as i64;
        let first = one_element_list();
        pith_map_insert_cstr(handle, b"a\0".as_ptr() as *const i8, first);

        let second = one_element_list();
        pith_map_insert_cstr_kind(
            handle,
            b"b\0".as_ptr() as *const i8,
            second,
            ListTypeTag::List as i64,
        );
        pith_list_release_handle(second);
        assert!(list_is_alive(second));

        pith_map_release_handle(handle);
        // neither value was freed by the map: `first` is still the
        // caller's, `second` is the leak this trades for the dangle
        assert!(list_is_alive(first));
        assert!(list_is_alive(second));
        pith_list_release_handle(first);
        pith_list_release_handle(second);
    }
}

#[test]
fn values_of_a_list_valued_map_come_back_list_tagged() {
    unsafe {
        use crate::collections::list::{pith_list_len, pith_list_release_handle, PithList};

        let handle = pith_map_new_default().ptr as i64;
        let list = one_element_list();
        pith_map_insert_cstr_owned_kind(
            handle,
            b"k\0".as_ptr() as *const i8,
            list,
            ListTypeTag::List as i64,
        );
        let vals = pith_map_values_handle(handle);
        assert_eq!(
            pith_list_len(PithList {
                ptr: vals as *mut ()
            }),
            1
        );
        // the values list took its own count, so evicting the entry does
        // not free what the caller is now holding
        pith_map_clear_handle(handle);
        assert!(list_is_alive(list));

        pith_list_release_handle(vals);
        assert!(!list_is_alive(list));
        pith_map_release_handle(handle);
    }
}

#[test]
fn released_map_handles_are_rejected() {
    unsafe {
        let map = pith_map_new(0, 8, 0);
        let handle = map.ptr as i64;
        assert_eq!(pith_map_len(map), 0);
        pith_map_release(map);
        assert_eq!(pith_map_len(map), 0);
        assert_eq!(pith_map_len_handle(handle), 0);
        assert_eq!(pith_map_is_empty_handle(handle), 1);
        pith_map_release(map);
    }
}

// this test lives here rather than in cycle.rs because building a map and
// reading its handle needs the private PithMap internals. it serializes
// on the cycle test lock like every test that turns the flag on.
#[test]
#[ignore = "enables the collector flag; run serially via make test-cycle-gc"]
fn buffered_map_dies_into_the_graveyard() {
    let _guard = crate::sync::lock(&crate::cycle::CYCLE_TEST_LOCK);
    crate::cycle::force_enabled_for_tests(true);
    crate::cycle::reset_for_tests();
    unsafe {
        // a struct value observed through a weak reference proves the
        // map's death still cascades into its values.
        let value = crate::runtime_core::pith_struct_alloc(1);
        crate::runtime_core::pith_struct_weak_retain(value);

        let map = pith_map_new_tagged(0, 8, 4); // int keys, struct values
        let handle = map.ptr as i64;
        pith_map_insert_ikey(handle, 1, value); // map retains
        crate::runtime_core::pith_struct_release(value); // map holds the only count
        assert_eq!(crate::runtime_core::pith_struct_weak_upgrade(value), value);
        crate::runtime_core::pith_struct_release(value); // the upgrade's count

        pith_map_retain_handle(handle);
        pith_map_release(map); // 2 -> 1: buffered
        assert_eq!(crate::cycle::suspect_count_for_tests(handle as usize), 1);
        pith_map_release(map); // 1 -> 0: dies buffered

        assert_eq!(crate::cycle::graveyard_count_for_tests(handle as usize), 1);
        assert_eq!(
            crate::runtime_core::pith_struct_weak_upgrade(value),
            0,
            "value was released"
        );
        assert_eq!(
            pith_map_len(map),
            0,
            "magic scrubbed: handle no longer validates"
        );
        pith_map_retain_handle(handle); // must be a no-op, not a revival

        crate::runtime_core::pith_struct_weak_release(value);
        crate::cycle::force_enabled_for_tests(false);
        crate::cycle::reset_for_tests();
    }
}

/// A bytes-keyed map finds an entry by the key's content: a key built
/// from a string and one built from a byte vector are one entry, two keys
/// that share a prefix and a length are two, the empty key is an entry
/// like any other, and the map's own copy of each key means the caller's
/// count on the handle is untouched. The value side keeps the string
/// contract: one count per stored value, dropped on overwrite, remove and
/// free, transferred by take.
#[test]
fn bytes_keyed_map_is_keyed_by_content() {
    unsafe {
        let map = pith_map_new_bytes_cstr_val().ptr as i64;
        let from_vec = crate::bytes::pith_bytes_from_vec(b"key".to_vec());
        let from_str = crate::bytes::pith_bytes_from_string_utf8(c"key".as_ptr());
        let sibling = crate::bytes::pith_bytes_from_vec(b"kex".to_vec());
        let empty = crate::bytes::pith_bytes_from_vec(Vec::new());

        let v1 = crate::pith_copy_bytes_to_cstring(b"one");
        let v2 = crate::pith_copy_bytes_to_cstring(b"two");
        let v3 = crate::pith_copy_bytes_to_cstring(b"three");
        pith_map_insert_bkey(map, from_vec, v1 as i64);
        pith_map_insert_bkey(map, sibling, v2 as i64);
        pith_map_insert_bkey(map, empty, v3 as i64);
        assert_eq!(pith_map_len_handle(map), 3);
        assert_eq!(
            pith_map_get_bkey(map, from_str) as *mut i8,
            v1,
            "found by content"
        );
        assert_eq!(
            pith_map_get_bkey(map, sibling) as *mut i8,
            v2,
            "shared prefix, distinct"
        );
        assert_eq!(
            pith_map_get_bkey(map, 0) as *mut i8,
            v3,
            "null is the empty key"
        );
        assert_eq!(pith_map_contains_bkey(map, from_str), 1);
        assert_eq!(pith_map_get_default_bkey(map, from_str, 7) as *mut i8, v1);
        let absent = crate::bytes::pith_bytes_from_vec(b"ke".to_vec());
        assert_eq!(
            pith_map_contains_bkey(map, absent),
            0,
            "a prefix is not a key"
        );
        assert_eq!(pith_map_get_default_bkey(map, absent, 7), 7);

        // the map holds one count per value; overwriting through the other
        // spelling of the same key releases the displaced value
        assert_eq!(crate::cstring_refcount_for_tests(v1), Some(2));
        let v4 = crate::pith_copy_bytes_to_cstring(b"four");
        pith_map_insert_bkey_owned(map, from_str, v4 as i64);
        assert_eq!(pith_map_len_handle(map), 3, "same key, one entry");
        assert_eq!(crate::cstring_refcount_for_tests(v1), Some(1), "displaced");
        assert_eq!(
            crate::cstring_refcount_for_tests(v4),
            Some(1),
            "owned: transferred"
        );

        // keys() hands out fresh bytes objects the list owns
        let list = pith_map_keys_bkey(map);
        let list_handle = crate::collections::list::PithList {
            ptr: list as *mut (),
        };
        assert_eq!(crate::collections::list::pith_list_len(list_handle), 3);
        let first = crate::collections::list::pith_list_get_value(list_handle, 0);
        assert!(crate::bytes::with_bytes(first, |_| ()).is_some());
        assert!(first != from_vec && first != sibling && first != empty);
        crate::collections::list::pith_list_release_handle(list);

        // take transfers the value's count; remove releases it
        let taken = pith_map_take_bkey(map, sibling) as *mut i8;
        assert_eq!(taken, v2);
        assert_eq!(
            crate::cstring_refcount_for_tests(v2),
            Some(2),
            "count came with it"
        );
        crate::pith_cstring_release(v2);
        pith_map_remove_bkey(map, from_vec);
        assert_eq!(pith_map_contains_bkey(map, from_str), 0);
        assert_eq!(pith_map_len_handle(map), 1);

        for h in [from_vec, from_str, sibling, empty, absent] {
            let rc = crate::bytes::with_bytes_obj(h, |b| {
                b.rc.load(std::sync::atomic::Ordering::Relaxed)
            })
            .expect("the map never took a count");
            assert_eq!(rc, 1);
            crate::bytes::pith_bytes_release(h);
        }
        for v in [v1, v2, v3] {
            crate::pith_cstring_release(v);
        }
        pith_map_release_handle(map);
    }
}

// --- the borrowed-key contract -----------------------------------------

/// The inputs the hash-equality proof runs over. A key that is empty, one
/// byte, or a single ascii word is the ordinary case; the rest are the ones
/// a naive "hash the bytes" would get wrong. A zero byte cannot reach a
/// c-string key, but it can reach a bytes key, and the two `KeyRef` arms
/// share the impl, so it is covered here where it can be.
fn hash_probe_inputs() -> Vec<Vec<u8>> {
    vec![
        Vec::new(),
        b"a".to_vec(),
        b"ab".to_vec(),
        b"region".to_vec(),
        b"user-1000-region".to_vec(),
        // a prefix and the same bytes with more after them: the length has
        // to enter the hash, or these two collide.
        b"abc".to_vec(),
        // embedded zero bytes (bytes keys can hold them)
        vec![0u8],
        vec![b'a', 0, b'b'],
        vec![0, 0, 0, 0],
        // multi-byte utf-8
        "\u{e9}".as_bytes().to_vec(),
        "\u{65e5}\u{672c}\u{8a9e}".as_bytes().to_vec(),
        "\u{1f600}".as_bytes().to_vec(),
        // long: past every small-buffer boundary in sight
        vec![b'x'; 255],
        vec![b'y'; 4096],
        // the same width as an int key's payload, in case a tag were ever
        // dropped
        vec![0u8; 8],
    ]
}

fn hash_with(state: &std::collections::hash_map::RandomState, value: &impl Hash) -> u64 {
    use std::hash::BuildHasher;
    let mut hasher = state.build_hasher();
    value.hash(&mut hasher);
    hasher.finish()
}

/// The one that would fail silently.
///
/// `KeyRef` is what every probe hashes and `MapKey` is what the table
/// stores, so if the two ever wrote different bytes for the same content
/// the table would stop finding keys it holds: no crash, no diagnostic,
/// just every string-keyed map in every program missing entries. This
/// asserts the two agree under the map's own hasher, for every shape in
/// `hash_probe_inputs`, and that the tag keeps the three flavors apart.
#[test]
fn key_ref_hashes_like_map_key() {
    let state = std::collections::hash_map::RandomState::new();
    for bytes in hash_probe_inputs() {
        assert_eq!(
            hash_with(&state, &KeyRef::Str(&bytes)),
            hash_with(&state, &MapKey::String(bytes.clone())),
            "string flavor, {} bytes",
            bytes.len()
        );
        assert_eq!(
            hash_with(&state, &KeyRef::Bytes(&bytes)),
            hash_with(&state, &MapKey::Bytes(bytes.clone())),
            "bytes flavor, {} bytes",
            bytes.len()
        );
        // and the tag has to keep the flavors apart, or a Map[Bytes, V]
        // and a Map[String, V] would share entries
        assert_ne!(
            hash_with(&state, &KeyRef::Str(&bytes)),
            hash_with(&state, &KeyRef::Bytes(&bytes)),
            "flavors must not collide, {} bytes",
            bytes.len()
        );
        assert!(KeyRef::Str(&bytes).equivalent(&MapKey::String(bytes.clone())));
        assert!(KeyRef::Bytes(&bytes).equivalent(&MapKey::Bytes(bytes.clone())));
        assert!(!KeyRef::Str(&bytes).equivalent(&MapKey::Bytes(bytes.clone())));
        assert!(!KeyRef::Bytes(&bytes).equivalent(&MapKey::String(bytes.clone())));
        assert_eq!(
            MapKey::from(&KeyRef::Str(&bytes)),
            MapKey::String(bytes.clone())
        );
        assert_eq!(
            MapKey::from(&KeyRef::Bytes(&bytes)),
            MapKey::Bytes(bytes.clone())
        );
    }
    // a prefix must not hash like the longer key it is a prefix of
    assert_ne!(
        hash_with(&state, &KeyRef::Str(b"ab")),
        hash_with(&state, &KeyRef::Str(b"abc"))
    );
    for n in [i64::MIN, -1, 0, 1, 255, i64::MAX] {
        assert_eq!(
            hash_with(&state, &KeyRef::Int(n)),
            hash_with(&state, &MapKey::Int(n)),
            "int flavor, {n}"
        );
        assert!(KeyRef::Int(n).equivalent(&MapKey::Int(n)));
    }
}

/// The same claim through the table rather than the hasher: a key stored
/// as `MapKey` is found by the `KeyRef` for the same content.
#[test]
fn a_stored_key_is_found_by_its_borrowed_form() {
    for bytes in hash_probe_inputs() {
        let mut table: HashMap<MapKey, Vec<u8>> = HashMap::new();
        table.insert(MapKey::String(bytes.clone()), vec![1]);
        table.insert(MapKey::Bytes(bytes.clone()), vec![2]);
        assert_eq!(table.get(&KeyRef::Str(&bytes)), Some(&vec![1]));
        assert_eq!(table.get(&KeyRef::Bytes(&bytes)), Some(&vec![2]));
    }
}

/// A c-string key one byte longer than the one already stored must not
/// find it. The borrowed probe carries its own length, and this is the
/// shape a length mistake in `with_cstr_key` would show up as.
#[test]
fn a_borrowed_cstr_key_carries_its_length() {
    unsafe {
        let map = pith_map_new_default().ptr as i64;
        let ab = b"ab\0".as_ptr() as *const i8;
        let abc = b"abc\0".as_ptr() as *const i8;
        let empty = b"\0".as_ptr() as *const i8;
        pith_map_insert_cstr(map, ab, 1);
        assert_eq!(pith_map_contains_cstr(map, ab), 1);
        assert_eq!(pith_map_contains_cstr(map, abc), 0);
        assert_eq!(pith_map_contains_cstr(map, empty), 0);
        pith_map_insert_cstr(map, empty, 7);
        assert_eq!(pith_map_get_cstr(map, empty), 7);
        assert_eq!(pith_map_len_handle(map), 2);
        pith_map_release_handle(map);
    }
}

// --- allocation counts -------------------------------------------------
//
// what the borrowed probes and the inline values buy, stated as the only
// number that can show it: an operation on a key the map already holds
// allocates nothing at all, and a new entry allocates once, for the key.

use crate::collections::alloc_probe::allocations_during;

#[test]
fn string_key_hits_allocate_nothing_for_the_key() {
    unsafe {
        let map = pith_map_new_default().ptr as i64;
        let key = b"user-1000-region\0".as_ptr() as *const i8;
        let absent = b"user-1001-region\0".as_ptr() as *const i8;
        pith_map_insert_cstr(map, key, 1);

        assert_eq!(allocations_during(|| { pith_map_get_cstr(map, key); }), 0);
        assert_eq!(
            allocations_during(|| {
                pith_map_get_cstr_strict(map, key);
            }),
            0
        );
        // `.get(k)` returns an optional. A hit is a Some shell, a heap
        // allocation by construction; it is the caller's value, not a copy
        // of the key, so the claim here is that it is the ONLY allocation:
        // one warm-up call to get the struct pool past its own setup, then
        // exactly one per hit. A miss is the null pointer and allocates
        // nothing.
        pith_map_get_cstr_opt(map, key);
        assert_eq!(
            allocations_during(|| {
                pith_map_get_cstr_opt(map, key);
            }),
            1
        );
        assert_eq!(
            allocations_during(|| {
                pith_map_get_cstr_opt(map, absent);
            }),
            0
        );
        assert_eq!(
            allocations_during(|| {
                pith_map_get_default_cstr(map, key, 0);
            }),
            0
        );
        assert_eq!(
            allocations_during(|| {
                pith_map_contains_cstr(map, key);
            }),
            0
        );
        // a miss probes and allocates nothing either
        assert_eq!(
            allocations_during(|| {
                pith_map_contains_cstr(map, absent);
            }),
            0
        );
        assert_eq!(
            allocations_during(|| {
                pith_map_get_cstr(map, absent);
            }),
            0
        );
        // an overwrite of a key the map holds: nothing for the key, which
        // the map already has, and nothing for the value, which goes into
        // the table itself
        assert_eq!(
            allocations_during(|| {
                pith_map_insert_cstr(map, key, 2);
            }),
            0
        );
        assert_eq!(pith_map_get_cstr(map, key), 2);
        // a remove of a key the map holds frees, and allocates nothing
        assert_eq!(
            allocations_during(|| {
                pith_map_remove_cstr(map, key);
            }),
            0
        );
        assert_eq!(pith_map_len_handle(map), 0);

        pith_map_release_handle(map);
    }
}

#[test]
fn a_new_string_key_allocates_the_key_once() {
    unsafe {
        let map = pith_map_new_default().ptr as i64;
        // insert one first, so the table is past its initial allocation
        // and the count below is the entry's own cost
        pith_map_insert_cstr(map, b"seed\0".as_ptr() as *const i8, 0);
        let fresh = b"a-key-the-map-has-never-seen\0".as_ptr() as *const i8;
        // the map's own copy of the key, and nothing else
        assert_eq!(
            allocations_during(|| {
                pith_map_insert_cstr(map, fresh, 5);
            }),
            1
        );
        assert_eq!(pith_map_get_cstr(map, fresh), 5);
        pith_map_release_handle(map);
    }
}

#[test]
fn bytes_key_hits_allocate_nothing_for_the_key() {
    unsafe {
        let map = pith_map_new_bytes().ptr as i64;
        let key = crate::bytes::pith_bytes_from_vec(b"user-1000-region".to_vec());
        let absent = crate::bytes::pith_bytes_from_vec(b"user-1001-region".to_vec());
        pith_map_insert_bkey(map, key, 1);

        assert_eq!(allocations_during(|| { pith_map_get_bkey(map, key); }), 0);
        assert_eq!(
            allocations_during(|| {
                pith_map_get_bkey_strict(map, key);
            }),
            0
        );
        // as in the string test: a hit's Some shell is the one allocation,
        // and it is the returned value rather than a copy of the key; a
        // miss allocates nothing
        pith_map_get_bkey_opt(map, key);
        assert_eq!(
            allocations_during(|| {
                pith_map_get_bkey_opt(map, key);
            }),
            1
        );
        assert_eq!(
            allocations_during(|| {
                pith_map_get_bkey_opt(map, absent);
            }),
            0
        );
        assert_eq!(
            allocations_during(|| {
                pith_map_get_default_bkey(map, key, 0);
            }),
            0
        );
        assert_eq!(
            allocations_during(|| {
                pith_map_contains_bkey(map, key);
            }),
            0
        );
        assert_eq!(
            allocations_during(|| {
                pith_map_contains_bkey(map, absent);
            }),
            0
        );
        assert_eq!(
            allocations_during(|| {
                pith_map_insert_bkey(map, key, 2);
            }),
            0
        );
        assert_eq!(pith_map_get_bkey(map, key), 2);
        // a key the map has never seen: one allocation, the map's own
        // copy of the content, and nothing for the value
        assert_eq!(
            allocations_during(|| {
                pith_map_insert_bkey(map, absent, 9);
            }),
            1
        );
        assert_eq!(pith_map_get_bkey(map, absent), 9);
        assert_eq!(
            allocations_during(|| {
                pith_map_remove_bkey(map, key);
                pith_map_remove_bkey(map, absent);
            }),
            0
        );
        assert_eq!(pith_map_len_handle(map), 0);

        crate::bytes::pith_bytes_release(key);
        crate::bytes::pith_bytes_release(absent);
        pith_map_release_handle(map);
    }
}

/// An int-keyed map that cannot use the scalar fast path (this one owns
/// heap values) goes through the shared table, so it gets the same
/// treatment.
#[test]
fn int_key_hits_allocate_nothing_for_the_key() {
    unsafe {
        let map = pith_map_new_int_cstr_val().ptr as i64;
        let s = crate::pith_copy_bytes_to_cstring(b"value");
        pith_map_insert_ikey(map, 7, s as i64);

        assert_eq!(allocations_during(|| { pith_map_get_ikey(map, 7); }), 0);
        assert_eq!(
            allocations_during(|| {
                pith_map_contains_ikey(map, 7);
            }),
            0
        );
        assert_eq!(
            allocations_during(|| {
                pith_map_contains_ikey(map, 8);
            }),
            0
        );
        // nothing for the key, nothing for the value
        assert_eq!(
            allocations_during(|| {
                pith_map_insert_ikey(map, 7, s as i64);
            }),
            0
        );
        // and an int key the map has never seen allocates nothing at all:
        // there is no key content to copy and the value goes inline
        assert_eq!(
            allocations_during(|| {
                pith_map_insert_ikey(map, 8, s as i64);
            }),
            0
        );
        assert_eq!(
            allocations_during(|| {
                pith_map_remove_ikey(map, 7);
                pith_map_remove_ikey(map, 8);
            }),
            0
        );

        crate::pith_cstring_release(s);
        pith_map_release_handle(map);
    }
}

// --- the fused update ---------------------------------------------------

/// The fused update has to answer exactly what the read-then-store pair it
/// replaces answers, for every key flavor and whatever the values are
/// doing: negative, zero, and up to the ends of the Int range. A sum past
/// either end stops the program, like `+` on an Int (the abort goldens
/// cover that), so the pairs here are the ones that fit.
#[test]
fn a_fused_update_matches_the_pair_it_replaces() {
    unsafe {
        let deltas = [1i64, 0, -1, -40, i64::MAX, i64::MIN];
        let starts = [0i64, 7, -9, i64::MAX, i64::MIN];
        let key = b"user-1000-region\0".as_ptr() as *const i8;
        let bkey = crate::bytes::pith_bytes_from_vec(b"user-1000-region".to_vec());
        for start in starts {
            for delta in deltas {
                let Some(expected) = start.checked_add(delta) else {
                    continue;
                };

                let smap = pith_map_new_default().ptr as i64;
                pith_map_insert_cstr(smap, key, start);
                assert_eq!(pith_map_upsert_add_cstr(smap, key, delta), expected);
                assert_eq!(pith_map_get_cstr_strict(smap, key), expected);
                assert_eq!(pith_map_len_handle(smap), 1);
                pith_map_release_handle(smap);

                let imap = pith_map_new_int().ptr as i64;
                pith_map_insert_ikey(imap, 7, start);
                assert_eq!(pith_map_upsert_add_ikey(imap, 7, delta), expected);
                assert_eq!(pith_map_get_ikey_strict(imap, 7), expected);
                assert_eq!(pith_map_len_handle(imap), 1);
                pith_map_release_handle(imap);

                let bmap = pith_map_new_bytes().ptr as i64;
                pith_map_insert_bkey(bmap, bkey, start);
                assert_eq!(pith_map_upsert_add_bkey(bmap, bkey, delta), expected);
                assert_eq!(pith_map_get_bkey_strict(bmap, bkey), expected);
                assert_eq!(pith_map_len_handle(bmap), 1);
                pith_map_release_handle(bmap);
            }
        }
        crate::bytes::pith_bytes_release(bkey);
    }
}

/// The counting update answers what the `contains_key` branch it replaces
/// answers: a held key gets the delta, wrapping like `add`, and a missing
/// key gets the initial value, for every key flavor. A miss on an int map
/// whose values are heap-free but not in the scalar fast path (a map built
/// before it learned its value size) takes the shared table, so that path
/// is covered by the string and bytes maps here.
#[test]
fn a_counting_update_adds_to_a_held_key_and_inserts_a_missing_one() {
    unsafe {
        let key = b"user-1000-region\0".as_ptr() as *const i8;
        let other = b"user-1001-region\0".as_ptr() as *const i8;
        let bkey = crate::bytes::pith_bytes_from_vec(b"user-1000-region".to_vec());
        let bother = crate::bytes::pith_bytes_from_vec(b"user-1001-region".to_vec());
        for (start, delta) in [(0i64, 1i64), (7, -9), (i64::MAX, -1), (i64::MIN, 1)] {
            let expected = start + delta;

            let smap = pith_map_new_default().ptr as i64;
            assert_eq!(pith_map_add_or_insert_cstr(smap, key, delta, start), start);
            assert_eq!(pith_map_add_or_insert_cstr(smap, key, delta, 99), expected);
            assert_eq!(pith_map_add_or_insert_cstr(smap, other, delta, 5), 5);
            assert_eq!(pith_map_get_cstr_strict(smap, key), expected);
            assert_eq!(pith_map_len_handle(smap), 2);
            pith_map_release_handle(smap);

            let imap = pith_map_new_int().ptr as i64;
            assert_eq!(pith_map_add_or_insert_ikey(imap, 7, delta, start), start);
            assert_eq!(pith_map_add_or_insert_ikey(imap, 7, delta, 99), expected);
            assert_eq!(pith_map_add_or_insert_ikey(imap, 8, delta, 5), 5);
            assert_eq!(pith_map_get_ikey_strict(imap, 7), expected);
            assert_eq!(pith_map_len_handle(imap), 2);
            pith_map_release_handle(imap);

            let bmap = pith_map_new_bytes().ptr as i64;
            assert_eq!(pith_map_add_or_insert_bkey(bmap, bkey, delta, start), start);
            assert_eq!(pith_map_add_or_insert_bkey(bmap, bkey, delta, 99), expected);
            assert_eq!(pith_map_add_or_insert_bkey(bmap, bother, delta, 5), 5);
            assert_eq!(pith_map_get_bkey_strict(bmap, bkey), expected);
            assert_eq!(pith_map_len_handle(bmap), 2);
            pith_map_release_handle(bmap);
        }
        crate::bytes::pith_bytes_release(bkey);
        crate::bytes::pith_bytes_release(bother);
    }
}

/// A hit allocates nothing, and a miss allocates only the map's own copy
/// of a string or bytes key.
#[test]
fn a_counting_update_allocates_only_for_a_new_key() {
    unsafe {
        let key = b"user-1000-region\0".as_ptr() as *const i8;
        let map = pith_map_new_default().ptr as i64;
        pith_map_add_or_insert_cstr(map, key, 1, 1);
        assert_eq!(allocations_during(|| { pith_map_add_or_insert_cstr(map, key, 1, 1); }), 0);
        pith_map_release_handle(map);

        let imap = pith_map_new_int().ptr as i64;
        pith_map_add_or_insert_ikey(imap, 7, 1, 1);
        assert_eq!(allocations_during(|| { pith_map_add_or_insert_ikey(imap, 7, 1, 1); }), 0);
        pith_map_release_handle(imap);

        // the first key also allocates the table, so one goes in first
        let bfirst = crate::bytes::pith_bytes_from_vec(b"user-999-region".to_vec());
        let bkey = crate::bytes::pith_bytes_from_vec(b"user-1000-region".to_vec());
        let bmap = pith_map_new_bytes().ptr as i64;
        pith_map_add_or_insert_bkey(bmap, bfirst, 1, 1);
        assert_eq!(allocations_during(|| { pith_map_add_or_insert_bkey(bmap, bkey, 1, 1); }), 1);
        assert_eq!(allocations_during(|| { pith_map_add_or_insert_bkey(bmap, bkey, 1, 1); }), 0);
        pith_map_release_handle(bmap);
        crate::bytes::pith_bytes_release(bfirst);
        crate::bytes::pith_bytes_release(bkey);
    }
}

/// The point of the entry point: an update of a key the map already holds
/// allocates nothing, the same claim the separate read and store each make
/// on their own.
#[test]
fn a_fused_update_allocates_nothing() {
    unsafe {
        let key = b"user-1000-region\0".as_ptr() as *const i8;
        let map = pith_map_new_default().ptr as i64;
        pith_map_insert_cstr(map, key, 1);
        assert_eq!(
            allocations_during(|| {
                pith_map_upsert_add_cstr(map, key, 1);
            }),
            0
        );
        pith_map_release_handle(map);

        let imap = pith_map_new_int().ptr as i64;
        pith_map_insert_ikey(imap, 7, 1);
        assert_eq!(
            allocations_during(|| {
                pith_map_upsert_add_ikey(imap, 7, 1);
            }),
            0
        );
        pith_map_release_handle(imap);

        let bkey = crate::bytes::pith_bytes_from_vec(b"user-1000-region".to_vec());
        let bmap = pith_map_new_bytes().ptr as i64;
        pith_map_insert_bkey(bmap, bkey, 1);
        assert_eq!(
            allocations_during(|| {
                pith_map_upsert_add_bkey(bmap, bkey, 1);
            }),
            0
        );
        pith_map_release_handle(bmap);
        crate::bytes::pith_bytes_release(bkey);
    }
}

/// The propagating fused update updates existing keys with zero
/// allocations, and reports misses by returning 0 without mutating the map
/// or allocating.
#[test]
fn a_try_fused_update_updates_hits_and_reports_misses_without_mutating() {
    unsafe {
        let hit = b"user-1000-region\0".as_ptr() as *const i8;
        let miss = b"user-9999-region\0".as_ptr() as *const i8;
        let map = pith_map_new_default().ptr as i64;
        pith_map_insert_cstr(map, hit, 10);
        assert_eq!(
            allocations_during(|| {
                assert_eq!(pith_map_try_upsert_add_cstr(map, hit, 5), 1);
                assert_eq!(pith_map_try_upsert_add_cstr(map, miss, 5), 0);
                assert_eq!(pith_map_try_upsert_add_cstr(map, std::ptr::null(), 5), 0);
            }),
            0
        );
        assert_eq!(pith_map_get_cstr_strict(map, hit), 15);
        assert_eq!(pith_map_len_handle(map), 1);
        pith_map_release_handle(map);

        let imap = pith_map_new_int().ptr as i64;
        pith_map_insert_ikey(imap, 7, 10);
        assert_eq!(
            allocations_during(|| {
                assert_eq!(pith_map_try_upsert_add_ikey(imap, 7, -3), 1);
                assert_eq!(pith_map_try_upsert_add_ikey(imap, 99, 5), 0);
            }),
            0
        );
        assert_eq!(pith_map_get_ikey_strict(imap, 7), 7);
        assert_eq!(pith_map_len_handle(imap), 1);
        pith_map_release_handle(imap);

        let bhit = crate::bytes::pith_bytes_from_vec(b"user-1000-region".to_vec());
        let bmiss = crate::bytes::pith_bytes_from_vec(b"user-9999-region".to_vec());
        let bmap = pith_map_new_bytes().ptr as i64;
        pith_map_insert_bkey(bmap, bhit, 20);
        assert_eq!(
            allocations_during(|| {
                assert_eq!(pith_map_try_upsert_add_bkey(bmap, bhit, 4), 1);
                assert_eq!(pith_map_try_upsert_add_bkey(bmap, bmiss, 4), 0);
            }),
            0
        );
        assert_eq!(pith_map_get_bkey_strict(bmap, bhit), 24);
        assert_eq!(pith_map_len_handle(bmap), 1);
        pith_map_release_handle(bmap);
        crate::bytes::pith_bytes_release(bhit);
        crate::bytes::pith_bytes_release(bmiss);
    }
}

// --- what the collector and the wide writer see -------------------------

/// The collector reaches a map's values only through `cycle_map_children`,
/// so a ring through a map's value side is collectable exactly when every
/// value the map owns a count on is reported there once. A value missing
/// from the report leaks its ring and nothing says so; a value reported
/// twice has its count dropped further than the map ever raised it.
#[test]
fn the_collector_sees_every_value_the_map_owns_exactly_once() {
    unsafe {
        // list values, because strings and primitives are not graph nodes
        // and the collector is told nothing about them (cycle_child_code)
        let map = pith_map_new_default().ptr as i64;
        let mut stored: Vec<i64> = Vec::new();
        for i in 0..8 {
            let key = format!("key-{i}\0");
            let value = one_element_list();
            pith_map_insert_cstr_owned_kind(
                map,
                key.as_ptr() as *const i8,
                value,
                ListTypeTag::List as i64,
            );
            stored.push(value);
        }

        let mut seen: Vec<(i64, u8)> = Vec::new();
        cycle_map_children(map, &mut |child, code| seen.push((child, code)));

        assert_eq!(seen.len(), stored.len());
        for value in &stored {
            assert_eq!(seen.iter().filter(|(child, _)| child == value).count(), 1);
        }
        let code = crate::collections::list::cycle_child_code(ListTypeTag::List).unwrap();
        assert!(seen.iter().all(|(_, reported)| *reported == code));

        pith_map_release_handle(map);
        for value in &stored {
            assert!(!list_is_alive(*value));
        }
    }
}

/// `pith_map_insert_int` is the one writer that can hand a map a value
/// that is not a word, and a map constructed for one is the only way to
/// reach the boxed arm. Nothing the emitter produces gets here: every map
/// constructor above passes a value size of 8, and the emitter calls only
/// those. The arm is kept for the raw FFI, so it is tested through it.
#[test]
fn a_map_of_wide_values_reads_its_first_word_back() {
    unsafe {
        let mut map = pith_map_new(0, 16, 0);
        let handle = map.ptr as i64;
        let wide: [u8; 16] = [
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x08, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
            0x01, 0x02,
        ];
        pith_map_insert_int(&mut map, 42, wide.as_ptr(), 16);
        assert_eq!(pith_map_len(map), 1);
        assert_eq!(
            pith_map_get_ikey(handle, 42),
            i64::from_le_bytes(wide[..8].try_into().unwrap())
        );
        // a value of the wrong size is still refused rather than stored
        pith_map_insert_int(&mut map, 43, wide.as_ptr(), 8);
        assert_eq!(pith_map_len(map), 1);
        pith_map_release(map);
    }
}

/// A value too short to carry a word reads back as absent, the way a
/// short box did. Only the raw FFI can build such a map.
#[test]
fn a_value_shorter_than_a_word_carries_none() {
    unsafe {
        let mut map = pith_map_new(0, 4, 0);
        let handle = map.ptr as i64;
        let narrow: [u8; 4] = [0xde, 0xad, 0xbe, 0xef];
        pith_map_insert_int(&mut map, 7, narrow.as_ptr(), 4);
        assert_eq!(pith_map_len(map), 1);
        assert_eq!(pith_map_get_ikey(handle, 7), 0);
        // the Optional says absent (the null pointer) rather than
        // "present, zero"
        assert_eq!(pith_map_get_ikey_opt(handle, 7), 0);
        pith_map_release(map);
    }
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn strict_int_key_miss_is_caught_by_isolate_call() {
    extern "C" fn miss(closure: i64) -> i64 {
        unsafe {
            let handle = crate::pith_closure_get_env(closure, 0);
            pith_map_get_ikey_strict(handle, 99)
        }
    }
    unsafe {
        let map = pith_map_new_int().ptr as i64;
        pith_map_insert_ikey(map, 1, 42);
        let closure = crate::pith_closure_new(miss as *const () as usize as i64);
        crate::pith_closure_set_env(closure, 0, map);
        let msg_ptr = crate::failure::pith_isolate_call(closure);
        let msg = std::ffi::CStr::from_ptr(msg_ptr).to_string_lossy().into_owned();
        crate::pith_cstring_release(msg_ptr);
        crate::pith_closure_release(closure);
        pith_map_release_handle(map);
        assert!(
            msg.contains("map key not found: 99"),
            "expected missing int key diagnostic, got: {msg}"
        );
    }
}

#[test]
fn with_map_mut_and_ref_handles_self_aliasing_and_distinct_maps() {
    unsafe {
        let a = pith_map_new_int();
        let b = pith_map_new_int();
        pith_map_insert_ikey(a.ptr as i64, 1, 10);
        pith_map_insert_ikey(b.ptr as i64, 2, 20);

        // distinct maps merge via `on_distinct`
        let distinct = with_map_mut_and_ref(
            a,
            b,
            |dst, src| {
                if let Some(src_fast) = &src.int_values8 {
                    for (&k, &v) in src_fast {
                        dst.insert_int_value(k, v);
                    }
                }
                "distinct"
            },
            |_| "same",
        );
        assert_eq!(distinct, Some("distinct"));
        assert_eq!(pith_map_len(a), 2);
        assert_eq!(pith_map_get_ikey(a.ptr as i64, 2), 20);

        // self-aliasing takes `on_same` with a single `&mut MapImpl`
        let same = with_map_mut_and_ref(
            a,
            a,
            |_, _| "distinct",
            |map| {
                map.insert_int_value(3, 30);
                "same"
            },
        );
        assert_eq!(same, Some("same"));
        assert_eq!(pith_map_get_ikey(a.ptr as i64, 3), 30);

        // invalid handles return None
        assert!(with_map_mut_and_ref(bogus_map(), a, |_, _| (), |_| ()).is_none());
        assert!(with_map_mut_and_ref(a, bogus_map(), |_, _| (), |_| ()).is_none());

        // shared ref2 handles both distinct and aliased maps
        assert_eq!(with_map_ref2(a, b, |x, y| (x.len(), y.len())), Some((3, 1)));
        assert_eq!(with_map_ref2(a, a, |x, y| x.len() + y.len()), Some(6));
        assert!(with_map_ref2(a, bogus_map(), |_, _| ()).is_none());

        pith_map_release(a);
        pith_map_release(b);
    }
}

#[test]
fn map_clear_empties_storage_before_releasing_values() {
    use std::sync::atomic::{AtomicI64, Ordering};
    static OBSERVED_MAP: AtomicI64 = AtomicI64::new(0);
    static OBSERVED_LEN: AtomicI64 = AtomicI64::new(-1);

    extern "C" fn observe_map_dtor(_ptr: i64) {
        let h = OBSERVED_MAP.load(Ordering::Relaxed);
        let len = unsafe { pith_map_len_handle(h) };
        OBSERVED_LEN.store(len, Ordering::Relaxed);
    }

    unsafe {
        let map = pith_map_new_default().ptr as i64;
        OBSERVED_MAP.store(map, Ordering::Relaxed);
        OBSERVED_LEN.store(-1, Ordering::Relaxed);

        let s = crate::runtime_core::pith_struct_alloc(1);
        crate::runtime_core::pith_struct_set_dtor(
            s,
            observe_map_dtor as *const () as usize as i64,
        );
        let key = b"k\0".as_ptr() as *const i8;
        pith_map_insert_cstr_owned_kind(map, key, s, 4); // Struct
        assert_eq!(pith_map_len_handle(map), 1);

        pith_map_clear_handle(map);
        assert_eq!(
            OBSERVED_LEN.load(Ordering::Relaxed),
            0,
            "map storage must be emptied before value destructors run"
        );
        assert_eq!(pith_map_len_handle(map), 0);
        pith_map_release_handle(map);
    }
}
