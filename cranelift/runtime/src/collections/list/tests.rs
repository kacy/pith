use super::lifecycle::*;
use super::read::*;
use super::write::*;
use super::*;

fn bogus_list() -> PithList {
    PithList {
        ptr: 12345usize as *mut (),
    }
}

#[test]
fn a_live_list_is_a_list_and_a_freed_one_is_not() {
    unsafe {
        let list = pith_list_new(8, 0);
        assert!(is_list_ptr(list.ptr));
        pith_list_push_value(list, 7);
        assert_eq!(pith_auto_len(list.ptr as i64), 1);
        pith_list_release(list);
        assert!(!is_list_ptr(list.ptr));
    }
}

#[test]
fn a_list_literal_call_takes_over_the_counts_on_its_values() {
    unsafe {
        let count_of = |s: *mut i8| crate::runtime_core::cstring_refcount_for_tests(s);
        let s = crate::pith_copy_bytes_to_cstring(b"x");
        // one count for each of the two elements, and one kept here
        crate::pith_cstring_retain(s);
        crate::pith_cstring_retain(s);
        // the value past the count is not an element
        let strings = pith_list_of(1, 2, s as i64, s as i64, 99, 0, 0, 0, 0, 0);
        assert_eq!(pith_auto_len(strings.ptr as i64), 2);
        assert_eq!(pith_list_get_value(strings, 1), s as i64);
        assert_eq!(count_of(s), Some(3));
        pith_list_release(strings);
        assert_eq!(count_of(s), Some(1));
        crate::pith_cstring_release(s);

        let ints = pith_list_of(0, 8, 1, 2, 3, 4, 5, 6, 7, 8);
        pith_list_push_value(ints, 9);
        assert_eq!(pith_auto_len(ints.ptr as i64), 9);
        assert_eq!(pith_list_get_value(ints, 7), 8);
        assert_eq!(pith_list_get_value(ints, 8), 9);
        pith_list_release(ints);

        let empty = pith_list_of(0, 0, 0, 0, 0, 0, 0, 0, 0, 0);
        assert_eq!(pith_auto_len(empty.ptr as i64), 0);
        pith_list_push_value(empty, 5);
        assert_eq!(pith_list_get_value(empty, 0), 5);
        pith_list_release(empty);
    }
}

#[test]
fn a_string_that_starts_with_the_list_magic_is_measured_as_a_string() {
    let mut text = LIST_MAGIC.to_le_bytes().to_vec();
    text.resize(300, b'x');
    unsafe {
        let s = crate::pith_copy_bytes_to_cstring(&text);
        assert_eq!(pith_auto_len(s as i64), 300);
        crate::pith_cstring_release(s);
    }
}

/// a page-aligned buffer to lay fake list structs out in, so a test can
/// choose whether one crosses a page boundary.
fn two_pages() -> Vec<u64> {
    vec![0u64; 3 * 4096 / 8]
}

fn page_start(buf: &mut [u64]) -> *mut u8 {
    let addr = buf.as_mut_ptr() as usize;
    ((addr + 4095) & !4095) as *mut u8
}

#[test]
fn a_magic_match_without_the_identity_word_is_not_a_list() {
    let mut buf = two_pages();
    let base = page_start(&mut buf);
    unsafe {
        (base as *mut u32).write(LIST_MAGIC);
        assert!(!crosses_page(base as *const ()));
        assert!(!is_list_ptr(base as *const ()));
        // the address alone is not the identity word either
        let fake = base as *mut ListImpl;
        std::ptr::addr_of_mut!((*fake).identity).write(base as u64);
        assert!(!is_list_ptr(base as *const ()));
        std::ptr::addr_of_mut!((*fake).identity).write(identity_tag(base as *const ()));
        assert!(is_list_ptr(base as *const ()));
    }
}

#[test]
fn an_identity_word_across_a_page_is_answered_by_the_registry() {
    let mut buf = two_pages();
    let at = unsafe { page_start(&mut buf).add(4096 - 8) };
    assert!(crosses_page(at as *const ()));
    unsafe {
        (at as *mut u32).write(LIST_MAGIC);
        let fake = at as *mut ListImpl;
        std::ptr::addr_of_mut!((*fake).identity).write(identity_tag(at as *const ()));
    }
    // not registered, so not a list, whatever its words say
    assert!(!is_list_ptr(at as *const ()));
    handle_registry::register(at as *const (), HandleKind::List);
    assert!(is_list_ptr(at as *const ()));
    handle_registry::unregister(at as *const (), HandleKind::List);
    assert!(!is_list_ptr(at as *const ()));
}

#[test]
fn a_sixteen_aligned_list_never_crosses_a_page() {
    for offset in (0..4096usize).step_by(16) {
        assert!(!crosses_page((0x10_0000 + offset) as *const ()));
    }
}

#[test]
fn identity_tags_are_never_zero() {
    for addr in [8usize, 4096, 0x7fff_ffff_f000] {
        assert_ne!(identity_tag(addr as *const ()), 0);
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
fn owned_set_outside_the_list_releases_the_value() {
    unsafe {
        let list = pith_list_new(8, 1); // ListTypeTag::String
        let kept = crate::pith_copy_bytes_to_cstring(b"kept");
        pith_list_push_value_owned(list, kept as i64);

        // past the end and negative: nothing is stored, and the count
        // the caller handed over is dropped instead of stranded
        for index in [1, 7, -1] {
            let s = crate::pith_copy_bytes_to_cstring(b"dropped");
            crate::pith_cstring_retain(s);
            pith_list_set_value_owned(list, index, s as i64);
            assert_eq!(crate::cstring_refcount_for_tests(s), Some(1));
            crate::pith_cstring_release(s);
        }
        assert_eq!(pith_list_len(list), 1);
        assert_eq!(pith_list_get_value(list, 0), kept as i64);

        // the checked store reports a miss and leaves the value alone,
        // since the caller's miss arm releases it
        let s = crate::pith_copy_bytes_to_cstring(b"caller");
        assert_eq!(pith_list_set_value_owned_checked(list, 1, s as i64), 0);
        assert_eq!(crate::cstring_refcount_for_tests(s), Some(1));
        assert_eq!(pith_list_set_value_owned_checked(list, 0, s as i64), 1);
        assert_eq!(crate::cstring_refcount_for_tests(kept), None);
        assert_eq!(pith_list_get_value(list, 0), s as i64);
        pith_list_release(list);
        assert_eq!(crate::cstring_refcount_for_tests(s), None);
    }
}

#[test]
fn checked_scalar_store_reports_the_bounds() {
    unsafe {
        let list = pith_list_new(8, 0);
        pith_list_push_value(list, 1);
        pith_list_push_value(list, 2);
        assert_eq!(pith_list_set_scalar_checked(list, 1, 9), 1);
        assert_eq!(pith_list_set_scalar_checked(list, 2, 9), 0);
        assert_eq!(pith_list_set_scalar_checked(list, -1, 9), 0);
        assert_eq!(pith_list_set_value_checked(list, 0, 8), 1);
        assert_eq!(pith_list_get_value(list, 0), 8);
        assert_eq!(pith_list_get_value(list, 1), 9);
        pith_list_release(list);
        // a dead handle is a miss, not a crash
        assert_eq!(pith_list_set_scalar_checked(list, 0, 1), 0);
    }
}

#[test]
fn string_list_pop_transfers_ownership() {
    unsafe {
        let list = pith_list_new(8, 1);
        let s = crate::pith_copy_bytes_to_cstring(b"moved");
        pith_list_push_value(list, s as i64);
        crate::pith_cstring_release(s); // caller keeps nothing

        let popped = pith_list_pop(list) as *const i8;
        assert_eq!(popped, s);
        // the list's count moved out with the element
        assert_eq!(crate::cstring_refcount_for_tests(popped), Some(1));
        // empty list and bogus handle return 0 safely
        assert_eq!(pith_list_pop(list), 0);
        assert_eq!(pith_list_pop(bogus_list()), 0);
        crate::pith_cstring_release(popped);
        pith_list_release(list);
    }
}

#[test]
fn shared_list_push_and_set_propagate_shared_flag() {
    unsafe {
        let mut parent = pith_list_new(8, 2); // ListTypeTag::List
        crate::share::share_value(parent.ptr as i64, ListTypeTag::List);

        let child_pushed = pith_list_new(8, 0);
        let pushed_word = (child_pushed.ptr as i64).to_ne_bytes();
        pith_list_push(&mut parent, pushed_word.as_ptr(), 8);
        let pushed_shared = with_list_ref(child_pushed, |r| {
            r.cycle_flags.load(std::sync::atomic::Ordering::Relaxed)
                & crate::share::CONTAINER_SHARED_FLAG
                != 0
        });
        assert_eq!(
            pushed_shared,
            Some(true),
            "ListImpl::push must trigger share_stored on shared lists"
        );

        let child_set = pith_list_new(8, 0);
        let set_word = (child_set.ptr as i64).to_ne_bytes();
        assert!(pith_list_set(parent, 0, set_word.as_ptr(), 8));
        let set_shared = with_list_ref(child_set, |r| {
            r.cycle_flags.load(std::sync::atomic::Ordering::Relaxed)
                & crate::share::CONTAINER_SHARED_FLAG
                != 0
        });
        assert_eq!(
            set_shared,
            Some(true),
            "ListImpl::set must trigger share_stored on shared lists"
        );

        pith_list_release(child_pushed);
        pith_list_release(child_set);
        pith_list_release(parent);
    }
}

#[test]
fn a_kind_store_takes_exactly_one_count_on_a_tagged_list() {
    unsafe {
        let list = pith_list_new(8, 1); // ListTypeTag::String
        let s = crate::pith_copy_bytes_to_cstring(b"borrowed");
        pith_list_push_value_kind(list, s as i64, 1);
        // one count for the caller's local, one for the list, and no
        // third one from a compensating retain
        assert_eq!(crate::cstring_refcount_for_tests(s), Some(2));

        crate::pith_cstring_release(s);
        assert_eq!(crate::cstring_refcount_for_tests(s), Some(1));
        pith_list_release(list);
        assert_eq!(crate::cstring_refcount_for_tests(s), None);
    }
}

#[test]
fn a_kind_store_adopts_into_an_empty_untagged_list() {
    unsafe {
        let list = pith_list_new_default(); // ListTypeTag::Primitive
        let s = crate::pith_copy_bytes_to_cstring(b"adopted");
        pith_list_push_value_kind(list, s as i64, 1);
        assert_eq!(crate::cstring_refcount_for_tests(s), Some(2));

        // the list learned the kind at the store, so its free-time
        // cascade reclaims what it holds
        crate::pith_cstring_release(s);
        pith_list_release(list);
        assert_eq!(crate::cstring_refcount_for_tests(s), None);
    }
}

#[test]
fn a_kind_store_that_cannot_adopt_leaks_rather_than_frees() {
    unsafe {
        // already holding a primitive, so the tag cannot change without
        // making the list release something it never counted
        let list = pith_list_new_default();
        pith_list_push_value(list, 41);
        let s = crate::pith_copy_bytes_to_cstring(b"stranded");
        pith_list_push_value_kind(list, s as i64, 1);
        // the compensating count the emitter used to add, added here
        assert_eq!(crate::cstring_refcount_for_tests(s), Some(2));

        crate::pith_cstring_release(s);
        pith_list_release(list);
        // outlives the list rather than being freed under it
        assert_eq!(crate::cstring_refcount_for_tests(s), Some(1));
        crate::pith_cstring_release(s);
    }
}

#[test]
fn a_kind_insert_and_overwrite_own_one_count_each() {
    unsafe {
        let list = pith_list_new(8, 1);
        let first = crate::pith_copy_bytes_to_cstring(b"first");
        pith_list_insert_value_kind(list, 0, first as i64, 1);
        assert_eq!(crate::cstring_refcount_for_tests(first), Some(2));
        crate::pith_cstring_release(first);

        let second = crate::pith_copy_bytes_to_cstring(b"second");
        pith_list_set_value_kind(list, 0, second as i64, 1);
        // the displaced element loses the list's count, which was its last
        assert_eq!(crate::cstring_refcount_for_tests(first), None);
        assert_eq!(crate::cstring_refcount_for_tests(second), Some(2));

        crate::pith_cstring_release(second);
        pith_list_release(list);
        assert_eq!(crate::cstring_refcount_for_tests(second), None);
    }
}

/// A string list holding one element the caller has already let go of,
/// so the list's count is the only one left.
unsafe fn list_owning_one_element(text: &[u8]) -> (PithList, *mut i8) {
    let s = crate::pith_copy_bytes_to_cstring(text);
    let (list, rc) = unsafe {
        let list = pith_list_new(8, 1);
        pith_list_push_value(list, s as i64);
        crate::pith_cstring_release(s);
        let rc = crate::cstring_refcount_for_tests(s);
        (list, rc)
    };
    assert_eq!(rc, Some(1));
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

#[test]
fn narrow_elements_read_without_overread() {
    unsafe {
        let mut b_list = pith_list_new(1, 0);
        let byte: u8 = 0x5a;
        pith_list_push(&mut b_list, &byte as *const u8, 1);
        with_list_ref(b_list, |b_ref| {
            assert_eq!(b_ref.get_value(0), Some(0x5a));
            assert_eq!(b_ref.get_value_unchecked(0), 0x5a);
        })
        .unwrap();
        let updated: u8 = 0xa5;
        assert!(pith_list_set(b_list, 0, &updated as *const u8, 1));
        assert_eq!(
            with_list_ref(b_list, |b_ref| b_ref.get_value(0)).unwrap(),
            Some(0xa5)
        );
        pith_list_release(b_list);

        let mut w_list = pith_list_new(4, 0);
        let word: u32 = 0x1234_5678;
        let bytes = word.to_ne_bytes();
        pith_list_push(&mut w_list, bytes.as_ptr(), 4);
        let expected = i64::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3], 0, 0, 0, 0]);
        with_list_ref(w_list, |w_ref| {
            assert_eq!(w_ref.get_value(0), Some(expected));
            assert_eq!(w_ref.get_value_unchecked(0), expected);
        })
        .unwrap();
        pith_list_release(w_list);
    }
}

#[test]
fn with_list_mut_and_ref_handles_self_aliasing_and_distinct_lists() {
    unsafe {
        let a = pith_list_new(8, 0);
        let b = pith_list_new(8, 0);
        pith_list_push_value(a, 1);
        pith_list_push_value(a, 2);
        pith_list_push_value(b, 3);

        // distinct lists extend without aliasing
        let distinct = with_list_mut_and_ref(
            a,
            b,
            |dst, src| {
                for i in 0..src.len() {
                    if let Some(v) = src.get_value(i) {
                        dst.push_value(v);
                    }
                }
                "distinct"
            },
            |_| "same",
        );
        assert_eq!(distinct, Some("distinct"));
        assert_eq!(pith_list_len(a), 3);
        assert_eq!(pith_list_get_value(a, 2), 3);

        // self-aliasing extends a list with itself via `on_same` (single `&mut`)
        let same = with_list_mut_and_ref(
            a,
            a,
            |_, _| "distinct",
            |list| {
                let snapshot: Vec<i64> = list.values8.clone();
                for v in snapshot {
                    list.push_value(v);
                }
                "same"
            },
        );
        assert_eq!(same, Some("same"));
        assert_eq!(pith_list_len(a), 6);
        assert_eq!(pith_list_get_value(a, 3), 1);
        assert_eq!(pith_list_get_value(a, 4), 2);
        assert_eq!(pith_list_get_value(a, 5), 3);

        // invalid handles return None
        assert!(with_list_mut_and_ref(bogus_list(), a, |_, _| (), |_| ()).is_none());
        assert!(with_list_mut_and_ref(a, bogus_list(), |_, _| (), |_| ()).is_none());

        // shared ref2 handles both distinct and aliased lists
        assert_eq!(
            with_list_ref2_handle(a.ptr as i64, b.ptr as i64, |x, y| (x.len(), y.len())),
            Some((6, 1))
        );
        assert_eq!(
            with_list_ref2_handle(a.ptr as i64, a.ptr as i64, |x, y| x.len() + y.len()),
            Some(12)
        );
        assert!(with_list_ref2_handle(a.ptr as i64, 12345, |_, _| ()).is_none());

        pith_list_release(a);
        pith_list_release(b);
    }
}

#[test]
fn list_clear_empties_storage_before_releasing_elements() {
    use std::sync::atomic::{AtomicI64, Ordering};
    static OBSERVED_LIST: AtomicI64 = AtomicI64::new(0);
    static OBSERVED_LEN: AtomicI64 = AtomicI64::new(-1);

    extern "C" fn observe_list_dtor(_ptr: i64) {
        let h = OBSERVED_LIST.load(Ordering::Relaxed);
        let len = pith_list_len(PithList { ptr: h as *mut () });
        OBSERVED_LEN.store(len, Ordering::Relaxed);
    }

    unsafe {
        let list = pith_list_new(8, 4); // ListTypeTag::Struct
        OBSERVED_LIST.store(list.ptr as i64, Ordering::Relaxed);
        OBSERVED_LEN.store(-1, Ordering::Relaxed);

        let s = crate::runtime_core::pith_struct_alloc(1);
        crate::runtime_core::pith_struct_set_dtor(
            s,
            observe_list_dtor as *const () as usize as i64,
        );
        pith_list_push_value_owned(list, s);
        assert_eq!(pith_list_len(list), 1);

        pith_list_clear_value(list);
        assert_eq!(
            OBSERVED_LEN.load(Ordering::Relaxed),
            0,
            "list storage must be emptied before element destructors run"
        );
        assert_eq!(pith_list_len(list), 0);
        pith_list_release(list);
    }
}
