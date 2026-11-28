use super::closure::*;
use super::cstring::*;
use super::struct_rc::*;
use super::*;
use crate::child_kind::STRUCT as CLOSURE_TAG_STRUCT;
use crate::string;
use std::sync::atomic::Ordering;


unsafe fn heap_refcount(s: *const i8) -> u32 {
    unsafe {
        let base = (s as *const u8).sub(CSTRING_HEADER_SIZE);
        (base.add(4) as *const u32).read()
    }
}

unsafe fn text(s: *const i8) -> String {
    let len = string::pith_cstring_len(s) as usize;
    let bytes = unsafe { std::slice::from_raw_parts(s as *const u8, len) };
    String::from_utf8_lossy(bytes).into_owned()
}

unsafe fn append(base: *const i8, parts: &[*const i8]) -> *mut i8 {
    let mut p = [std::ptr::null(); APPEND_MANY_MAX];
    p[..parts.len()].copy_from_slice(parts);
    unsafe {
        pith_cstring_append_many(parts.len() as i64, base, p[0], p[1], p[2], p[3], p[4], p[5], p[6])
    }
}

#[test]
fn size_classes_cover_the_length_and_grow_geometrically() {
    assert_eq!(cstring_alloc_size(0), 24);
    assert_eq!(cstring_alloc_size(7), 24);
    assert_eq!(cstring_alloc_size(8), 40);
    assert_eq!(cstring_alloc_size(119), 136);
    assert_eq!(cstring_alloc_size(120), 160);
    for len in 0..5000 {
        let size = cstring_alloc_size(len);
        assert!(size > CSTRING_HEADER_SIZE + len);
        if len > 200 {
            assert!(size * 4 <= (CSTRING_HEADER_SIZE + len + 1) * 5);
        }
    }
    assert_eq!(cstring_alloc_size_checked(usize::MAX), None);
    assert_eq!(cstring_alloc_size_checked(usize::MAX - 1), None);
    assert_eq!(cstring_alloc_size_checked(usize::MAX - CSTRING_HEADER_SIZE), None);
    assert_eq!(cstring_alloc_size_checked(isize::MAX as usize), None);
    assert_eq!(cstring_alloc_size_checked((isize::MAX as usize) - CSTRING_HEADER_SIZE), None);
}

#[test]
fn a_sole_owner_appends_in_place() {
    unsafe {
        let s = pith_copy_bytes_to_cstring(b"ab");
        let tail = pith_copy_bytes_to_cstring(b"cd");
        let grown = append(s, &[tail, std::ptr::null()]);
        assert_eq!(grown, s, "still inside the size class");
        assert_eq!(text(grown), "abcd");
        assert_eq!(heap_refcount(grown), 1);
        // past the class: a realloc, same contents, still one count
        let mut acc = grown;
        for _ in 0..100 {
            acc = append(acc, &[tail]);
        }
        assert_eq!(text(acc).len(), 4 + 200);
        assert!(text(acc).ends_with("cdcd"));
        assert_eq!(heap_refcount(acc), 1);
        pith_cstring_release(acc);
        pith_cstring_release(tail);
    }
}

#[test]
fn a_shared_or_literal_base_is_copied() {
    unsafe {
        let s = pith_copy_bytes_to_cstring(b"ab");
        pith_cstring_retain(s); // a second owner
        let tail = pith_copy_bytes_to_cstring(b"!");
        let fresh = append(s, &[tail]);
        assert_ne!(fresh, s as *mut i8);
        assert_eq!(text(s), "ab", "the other owner's value is untouched");
        assert_eq!(text(fresh), "ab!");
        assert_eq!(heap_refcount(s), 1, "the caller's count moved to the result");
        pith_cstring_release(s);
        pith_cstring_release(fresh);

        let literal = b"lit\0".as_ptr() as *const i8;
        let from_literal = append(literal, &[tail]);
        assert_eq!(text(from_literal), "lit!");
        pith_cstring_release(from_literal);
        pith_cstring_release(tail);
    }
}

#[test]
fn the_shared_empty_string_carries_a_literal_header() {
    unsafe {
        let empty = pith_cstring_empty();
        assert_eq!(cstring_header_len(empty), Some(0));
        assert!(cstring_base(empty).is_none(), "never counted or freed");
        pith_cstring_retain(empty);
        pith_cstring_release(empty);
        assert_eq!(crate::string::pith_cstring_len(empty), 0);
    }
}

#[test]
fn appending_a_string_to_itself_copies() {
    unsafe {
        let s = pith_copy_bytes_to_cstring(b"xy");
        let doubled = append(s, &[s]);
        assert_eq!(text(doubled), "xyxy");
        pith_cstring_release(doubled);
    }
}

#[test]
fn heap_cstrings_carry_a_refcount() {
    unsafe {
        let s = pith_copy_bytes_to_cstring(b"hello");
        assert_eq!(heap_refcount(s), 1);
        pith_cstring_retain(s);
        assert_eq!(heap_refcount(s), 2);
        pith_cstring_release(s);
        assert_eq!(heap_refcount(s), 1);
        pith_cstring_release(s); // frees; magic is scrubbed first
    }
}

#[test]
fn integers_format_in_decimal() {
    unsafe {
        for n in [0, 7, -7, 10, 99, -100, 1234567890, i64::MAX, i64::MIN] {
            let s = pith_int_to_cstr(n);
            let text = std::ffi::CStr::from_ptr(s).to_str().unwrap().to_owned();
            assert_eq!(text, n.to_string());
            assert_eq!(cstring_header_len(s), Some(text.len() as i64));
            pith_cstring_release(s);
        }
        for n in [0u64, 42, u64::MAX] {
            let s = pith_uint_to_cstr(n as i64);
            assert_eq!(std::ffi::CStr::from_ptr(s).to_str().unwrap(), n.to_string());
            pith_cstring_release(s);
        }
    }
}

#[test]
fn released_cstring_fails_the_header_check() {
    unsafe {
        let s = pith_copy_bytes_to_cstring(b"x");
        pith_cstring_release(s);
        // stale pointer: magic scrubbed, so this must be a no-op,
        // not a double free
        pith_cstring_release(s);
        pith_cstring_retain(s);
    }
}

#[test]
fn literals_and_null_are_no_ops() {
    unsafe {
        let lit = b"static literal\0".as_ptr() as *const i8;
        pith_cstring_retain(lit);
        pith_cstring_release(lit);
        pith_cstring_release(lit);
        pith_cstring_retain(std::ptr::null());
        pith_cstring_release(std::ptr::null());
        // the shared empty string is header-less by design
        let empty = pith_cstring_empty();
        pith_cstring_release(empty);
        assert_eq!(*empty, 0);
    }
}

#[test]
fn concat_and_strdup_produce_owned_cstrings() {
    unsafe {
        let a = pith_copy_bytes_to_cstring(b"ab");
        let b = pith_copy_bytes_to_cstring(b"cd");
        let joined = pith_concat_cstr(a, b);
        assert_eq!(heap_refcount(joined), 1);
        assert_eq!(crate::string::pith_cstring_len(joined), 4);
        let dup = pith_strdup(joined);
        assert_eq!(heap_refcount(dup), 1);
        pith_cstring_release(joined);
        pith_cstring_release(dup);
        pith_cstring_release(a);
        pith_cstring_release(b);
    }
}

unsafe fn struct_counts(ptr: i64) -> (u32, u32) {
    let base = (ptr as usize - STRUCT_HEADER) as *const u8;
    unsafe {
        (
            (base.add(STRUCT_OFF_STRONG) as *const u32).read(),
            (base.add(STRUCT_OFF_WEAK) as *const u32).read(),
        )
    }
}

#[test]
fn struct_without_weak_refs_frees_on_last_strong_release() {
    unsafe {
        let s = pith_struct_alloc(2);
        assert_eq!(struct_counts(s), (1, 1)); // strong 1, implicit weak 1
        pith_struct_retain(s);
        assert_eq!(struct_counts(s).0, 2);
        pith_struct_release(s);
        assert_eq!(struct_counts(s).0, 1);
        pith_struct_release(s); // frees; magic scrubbed. no leak, no crash.
    }
}

#[test]
fn weak_ref_keeps_the_header_alive_past_the_strong_release() {
    unsafe {
        let s = pith_struct_alloc(1);
        pith_struct_weak_retain(s);
        assert_eq!(struct_counts(s), (1, 2)); // implicit + one weak

        // the target is alive: an upgrade returns the pointer with a
        // count of its own
        assert_eq!(pith_struct_weak_upgrade(s), s);
        assert_eq!(struct_counts(s), (2, 2));
        pith_struct_release(s);

        // drop the only strong owner: value dies, header stays (weak > 0)
        pith_struct_release(s);
        assert_eq!(pith_struct_weak_upgrade(s), 0); // dead -> none

        // dropping the last weak frees the header
        pith_struct_weak_release(s);
    }
}

// structs travel between tasks, and tasks run on different worker threads,
// so the last strong owner can die on a thread the weak reader never runs
// on. the dead flag is an atomic for exactly this shape; the join gives the
// load a happens-after edge, so it must observe the death.
#[test]
fn weak_upgrade_sees_a_release_from_another_thread() {
    unsafe {
        let s = pith_struct_alloc(1);
        pith_struct_weak_retain(s);
        assert_eq!(pith_struct_weak_upgrade(s), s);
        pith_struct_release(s);

        std::thread::spawn(move || pith_struct_release(s))
            .join()
            .expect("releasing thread panicked");

        assert_eq!(pith_struct_weak_upgrade(s), 0);
        pith_struct_weak_release(s);
    }
}

// a weak field store or read on a holder nothing has marked shared skips
// the field lock, since only the task that built the holder can reach it.
// on a shared holder both wait for the lock, which is what keeps a store
// in another task from freeing the old target's header between a read's
// load and its upgrade.
#[test]
fn weak_field_calls_lock_only_a_shared_holder() {
    unsafe {
        let holder = pith_struct_alloc(1);
        let target = pith_struct_alloc(1);
        for shared in [false, true] {
            if shared {
                struct_mark_shared(holder);
            }
            let guard = WeakFieldGuard::lock(holder);
            let (done, finished) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                pith_struct_weak_retain(target);
                pith_struct_weak_store_field(holder, 0, target);
                done.send(pith_struct_weak_load_field(holder, 0)).expect("send");
            });
            let wait = std::time::Duration::from_millis(if shared { 50 } else { 10_000 });
            let early = finished.recv_timeout(wait);
            assert_eq!(early.is_ok(), !shared, "shared: {shared}");
            drop(guard);
            let read = early.or_else(|_| finished.recv()).expect("worker result");
            worker.join().expect("worker panicked");
            assert_eq!(read, target);
            // the upgrade's count, and the field's weak count on the
            // target stored before
            assert_eq!(struct_counts(target), (2, 2));
            pith_struct_release(read);
        }
        pith_struct_weak_store_field(holder, 0, 0);
        assert_eq!(struct_counts(target), (1, 1));
        pith_struct_release(target);
        pith_struct_release(holder);
    }
}

#[test]
fn ord_reads_a_byte_from_0_to_255() {
    let high = b"\xc3\xa9\0";
    let ascii = b"A\0";
    unsafe {
        assert_eq!(pith_ord_cstr(high.as_ptr() as *const i8), 195);
        assert_eq!(pith_ord_cstr(ascii.as_ptr() as *const i8), 65);
        assert_eq!(pith_ord_cstr(b"\0".as_ptr() as *const i8), 0);
        assert_eq!(pith_ord_cstr(std::ptr::null()), 0);
    }
}

/// a compiled literal's header gives its length, and retain and release
/// leave it alone.
#[test]
fn a_literal_header_gives_the_length_and_is_never_counted() {
    let mut words = [0u64; 4];
    let text = b"literal\0";
    unsafe {
        let base = words.as_mut_ptr() as *mut u8;
        (base as *mut u32).write(CSTRING_STATIC_MAGIC);
        (base.add(8) as *mut u64).write(7);
        std::ptr::copy_nonoverlapping(text.as_ptr(), base.add(16), text.len());
        let s = base.add(16) as *const i8;
        assert_eq!(cstring_header_len(s), Some(7));
        assert_eq!(crate::string::pith_cstring_len(s), 7);
        assert!(cstring_base(s).is_none());
        pith_cstring_retain(s);
        pith_cstring_release(s);
        assert_eq!((base.add(4) as *const u32).read(), 0);
        assert_eq!((base as *const u32).read(), CSTRING_STATIC_MAGIC);
    }
}

#[test]
fn a_join_writes_its_int_parts_in_decimal() {
    unsafe {
        let a = crate::pith_copy_bytes_to_cstring(b"id=");
        let b = crate::pith_copy_bytes_to_cstring(b";");
        let null = std::ptr::null();
        let int = |n: i64| n as *const i8;
        let ints = |bits: i64| bits << JOIN_INT_SHIFT;
        for n in [0, 7, 10, 99, 100, -1, -10, 1234567890123, i64::MAX, i64::MIN] {
            assert_eq!(int_text_len(n), n.to_string().len());
            let joined = pith_concat_cstr_many(3 | ints(0b010), a, int(n), b, null, null, null, null, null);
            assert_eq!(text(joined), format!("id={};", n));
            assert_eq!(cstring_header_len(joined), Some(n.to_string().len() as i64 + 4));
            pith_cstring_release(joined);
        }
        // an int part of 0 is text, where a null string part is empty
        let zero = pith_concat_cstr_many(1 | ints(0b1), int(0), null, null, null, null, null, null, null);
        assert_eq!(text(zero), "0");
        // a bit past the count is ignored
        let one = pith_concat_cstr_many(1 | ints(0b10), a, int(5), null, null, null, null, null, null);
        assert_eq!(text(one), "id=");

        // appended in place, and through the copy a shared base takes
        let s = pith_copy_bytes_to_cstring(b"n");
        let grown = pith_cstring_append_many(2 | ints(0b01), s, int(-42), b, null, null, null, null, null);
        assert_eq!(grown as *const i8, s as *const i8, "still inside the size class");
        assert_eq!(text(grown), "n-42;");
        pith_cstring_retain(grown);
        let copy = pith_cstring_append_many(2 | ints(0b10), grown, b, int(8), null, null, null, null, null);
        assert_ne!(copy as *const i8, grown as *const i8);
        assert_eq!(text(copy), "n-42;;8");
        assert_eq!(text(grown), "n-42;");
        for s in [zero, one, copy, grown, a, b] {
            pith_cstring_release(s);
        }
    }
}

#[test]
fn concat_many_joins_its_parts_like_a_chain_of_concats() {
    unsafe {
        let a = crate::pith_copy_bytes_to_cstring(b"ab");
        let b = crate::pith_copy_bytes_to_cstring(b"");
        let c = crate::pith_copy_bytes_to_cstring(b"cde");
        let null = std::ptr::null();
        let joined = pith_concat_cstr_many(4, a, null, b, c, a, a, a, a);
        assert_eq!(std::ffi::CStr::from_ptr(joined).to_bytes(), b"abcde");
        assert_eq!(cstring_header_len(joined), Some(5));
        assert_eq!(heap_refcount(joined), 1);
        let all = pith_concat_cstr_many(8, a, a, a, a, a, a, a, c);
        assert_eq!(std::ffi::CStr::from_ptr(all).to_bytes(), b"abababababababcde");
        assert!(pith_concat_cstr_many(2, null, null, a, a, a, a, a, a).is_null());
        // the parts keep their own counts
        assert_eq!(heap_refcount(a), 1);
        for s in [joined, all, a, b, c] {
            pith_cstring_release(s);
        }
    }
}

#[test]
fn chr_hands_out_a_static_one_byte_string() {
    unsafe {
        for n in [0i64, 10, 34, 65, 127, 200, 255, 256 + 65] {
            let s = pith_chr_cstr(n);
            assert_eq!(cstring_header_len(s), Some(1));
            assert_eq!(*s as u8, n as u8);
            assert_eq!(*s.add(1), 0);
            assert!(cstring_base(s).is_none());
            pith_cstring_retain(s);
            pith_cstring_release(s);
            pith_cstring_release(s);
            assert_eq!(*s as u8, n as u8);
        }
        assert_eq!(pith_chr_cstr(65), pith_chr_cstr(65));
    }
}

#[test]
fn count_release_returns_the_count_before() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let count = AtomicU32::new(3);
    assert_eq!(count_release(&count), 3);
    assert_eq!(count_release(&count), 2);
    assert_eq!(count_release(&count), 1);
    assert_eq!(count.load(Ordering::Relaxed), 0);
}

static DTOR_RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

unsafe extern "C" fn counting_dtor(_ptr: i64) {
    DTOR_RUNS.fetch_add(1, Ordering::SeqCst);
}

/// a destructor that keeps a weak reference to the value it tears down.
unsafe extern "C" fn weak_taking_dtor(ptr: i64) {
    unsafe { pith_struct_weak_retain(ptr) };
    WEAK_TAKEN.store(ptr, Ordering::SeqCst);
}

static WEAK_TAKEN: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// a destructor that tries to upgrade a weak reference to its own value.
unsafe extern "C" fn self_upgrading_dtor(ptr: i64) {
    SELF_UPGRADE.store(unsafe { pith_struct_weak_upgrade(ptr) }, Ordering::SeqCst);
}

static SELF_UPGRADE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-1);

#[test]
fn an_upgrade_during_the_destructor_reads_none() {
    unsafe {
        let s = pith_struct_alloc(1);
        pith_struct_weak_retain(s);
        pith_struct_set_dtor(s, self_upgrading_dtor as *const () as usize as i64);
        pith_struct_release(s);
        assert_eq!(SELF_UPGRADE.load(Ordering::SeqCst), 0, "a dying value is not revived");
        assert_eq!(pith_struct_weak_upgrade(s), 0);
        pith_struct_weak_release(s);
    }
}

#[test]
fn a_retain_of_a_dead_struct_is_refused() {
    unsafe {
        let s = pith_struct_alloc(1);
        pith_struct_weak_retain(s);
        pith_struct_release(s); // dead; the weak reference keeps the header
        let counts = struct_counts(s);
        pith_struct_retain(s);
        assert_eq!(struct_counts(s), counts, "no count taken on a dead value");
        // the stray retain's release lands on the over-release guard
        pith_struct_release(s);
        assert_eq!(struct_counts(s), counts);
        pith_struct_weak_release(s);
    }
}

#[test]
fn a_sole_owner_release_runs_the_destructor_once() {
    unsafe {
        let before = DTOR_RUNS.load(Ordering::SeqCst);
        let s = pith_struct_alloc(2);
        pith_struct_set_dtor(s, counting_dtor as *const () as usize as i64);
        pith_struct_release(s);
        assert_eq!(DTOR_RUNS.load(Ordering::SeqCst), before + 1);
        assert_eq!(pith_struct_strong_count(s), 0); // freed: magic scrubbed
    }
}

#[test]
fn a_weak_reference_taken_in_the_destructor_sees_the_value_dead() {
    unsafe {
        let s = pith_struct_alloc(1);
        pith_struct_set_dtor(s, weak_taking_dtor as *const () as usize as i64);
        pith_struct_release(s);
        assert_eq!(WEAK_TAKEN.load(Ordering::SeqCst), s);
        // the header outlives the value for the weak reference
        assert_eq!(struct_counts(s).1, 1);
        assert_eq!(pith_struct_weak_upgrade(s), 0);
        pith_struct_weak_release(s);
    }
}

#[test]
fn strong_count_follows_retain_and_release() {
    unsafe {
        assert_eq!(pith_struct_strong_count(0), 0);
        assert_eq!(pith_struct_strong_count(12345), 0);
        let s = pith_struct_alloc(3);
        assert_eq!(pith_struct_strong_count(s), 1);
        pith_struct_retain(s);
        assert_eq!(pith_struct_strong_count(s), 2);
        pith_struct_release(s);
        assert_eq!(pith_struct_strong_count(s), 1);
        pith_struct_release(s); // fully freed
        assert_eq!(pith_struct_strong_count(s), 0); // magic scrubbed
    }
}

#[test]
fn weak_upgrade_on_a_dead_or_invalid_pointer_is_none() {
    unsafe {
        assert_eq!(pith_struct_weak_upgrade(0), 0);
        assert_eq!(pith_struct_weak_upgrade(12345), 0);
        let s = pith_struct_alloc(1);
        pith_struct_release(s); // fully freed, no weak refs
        assert_eq!(pith_struct_weak_upgrade(s), 0); // magic scrubbed -> none
    }
}

#[test]
fn weak_ops_on_invalid_pointers_are_no_ops() {
    unsafe {
        pith_struct_weak_retain(0);
        pith_struct_weak_release(0);
        pith_struct_weak_retain(12345);
        pith_struct_weak_release(12345);
    }
}

#[test]
fn invalid_closure_handles_return_safe_defaults() {
    unsafe {
        assert_eq!(pith_closure_get_fn(12345), 0);
        assert_eq!(pith_closure_get_env(12345, 0), 0);
        pith_closure_set_env(12345, 0, 99);
    }
}

#[test]
fn closure_env_access_requires_valid_slot() {
    let handle = pith_closure_new(77);
    unsafe {
        pith_closure_set_env(handle, 0, 42);
        pith_closure_set_env(handle, -1, 99);
        assert_eq!(pith_closure_get_fn(handle), 77);
        assert_eq!(pith_closure_get_env(handle, 0), 42);
        assert_eq!(pith_closure_get_env(handle, -1), 0);
    }
}

// slots past the inline block spill to the heap extension instead of
// being silently dropped. the old behavior ignored the store and
// answered 0, which made a 17-capture closure compute a wrong answer
// with no diagnostic. an unwritten spill slot still answers 0.
#[test]
fn closure_env_slots_past_the_inline_block_spill() {
    let handle = pith_closure_new(77);
    let first_spill = PITH_CLOSURE_ENV_SLOTS as i64;
    unsafe {
        // the boundary pair: the last inline slot and the first spill
        // slot are adjacent indices in different storage, so a rebase
        // off by one would alias them or lose one of the writes.
        pith_closure_set_env(handle, first_spill - 1, 41);
        pith_closure_set_env(handle, first_spill, 99);
        pith_closure_set_env(handle, first_spill + 5, 123);
        assert_eq!(pith_closure_get_env(handle, first_spill - 1), 41);
        assert_eq!(pith_closure_get_env(handle, first_spill), 99);
        assert_eq!(pith_closure_get_env(handle, first_spill + 5), 123);
        // the gap between written spill slots reads as 0, as does a
        // slot past everything written
        assert_eq!(pith_closure_get_env(handle, first_spill + 1), 0);
        assert_eq!(pith_closure_get_env(handle, first_spill + 100), 0);
        // a slot at or past the hard cap is ignored like a negative
        // one. before the cap, this store attempted a terabyte-scale
        // resize and aborted the process
        pith_closure_set_env(handle, PITH_CLOSURE_MAX_ENV_SLOTS as i64, 7);
        pith_closure_set_env(handle, 1 << 40, 7);
        assert_eq!(pith_closure_get_env(handle, PITH_CLOSURE_MAX_ENV_SLOTS as i64), 0);
        assert_eq!(pith_closure_get_env(handle, 1 << 40), 0);
        pith_closure_release(handle);
    }
}

// a spilled slot's tag must drive the same release the inline tags
// do: the closure below captures a struct past the inline block, and
// the last closure release has to drop the count the caller moved
// into that spill slot.
#[test]
fn spilled_rc_capture_is_released_with_the_closure() {
    unsafe {
        let s = pith_struct_alloc(1);
        pith_struct_retain(s); // the count the closure will own
        assert_eq!(struct_counts(s).0, 2);
        let handle = pith_closure_new(77);
        pith_closure_set_env_rc(
            handle,
            PITH_CLOSURE_ENV_SLOTS as i64 + 3,
            s,
            CLOSURE_TAG_STRUCT as i64,
        );
        pith_closure_release(handle);
        assert_eq!(struct_counts(s).0, 1, "spilled capture was not released");
        pith_struct_release(s);
    }
}

// a recycled closure shell keeps stale bytes in `env` (we skip zeroing
// 128 B on construction), but zeroes `env_tags` so an overwritten or
// untouched slot can never double-release an older closure's RC capture.
#[test]
fn recycled_closure_clears_tags_without_zeroing_env() {
    unsafe {
        let s = pith_struct_alloc(1);
        pith_struct_retain(s); // owned by first closure
        assert_eq!(struct_counts(s).0, 2);

        let first = pith_closure_new(11);
        pith_closure_set_env(first, 0, 100);
        pith_closure_set_env_rc(first, 1, s, CLOSURE_TAG_STRUCT as i64);
        pith_closure_release(first);
        assert_eq!(struct_counts(s).0, 1, "first closure released its capture");

        // allocate a second closure (recycles `first`'s shell when pooled),
        // write only slot 0, and release: slot 1's tag must be 0 so `s` is
        // not released a second time.
        let second = pith_closure_new(22);
        pith_closure_set_env(second, 0, 200);
        assert_eq!(pith_closure_get_fn(second), 22);
        assert_eq!(pith_closure_get_env(second, 0), 200);
        pith_closure_release(second);
        assert_eq!(struct_counts(s).0, 1, "stale slot tag must not double-release");
        pith_struct_release(s);
    }
}
