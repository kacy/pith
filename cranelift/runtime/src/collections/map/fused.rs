//! Fused integer arithmetic map updates (`upsert_add`, `try_upsert_add`, `add_or_insert`).

use super::ops::{invalid_map_handle_exit, null_map_key_exit, strict_miss};
use super::{
    require_key_flavor, with_bytes_key, with_cstr_key, with_map_mut_handle, KeyRef, KeyType,
    MapImpl, MapVal,
};
use hashbrown::hash_map::EntryRef;

impl MapImpl {
    /// `word + delta`, or the integer-overflow runtime error when the sum does
    /// not fit. The fused update stands in for `m[k] + d` on an Int value,
    /// which traps on overflow like any other Int `+`.
    fn checked_word_add(word: i64, delta: i64) -> i64 {
        match word.checked_add(delta) {
            Some(sum) => sum,
            None => unsafe {
                crate::runtime_core::pith_runtime_error(7);
                0
            },
        }
    }

    /// Add `delta` to the word stored under `key` and return the new word, or
    /// `None` when the map does not hold the key.
    ///
    /// One probe does the read and the write. The shape the emitter fuses into
    /// this, `m.insert(k, m[k] + d)`, hashed the key three times and probed
    /// three times for the one slot; the fused form hashes and probes once.
    /// Ownership does not enter into it: the fused path is only taken for a
    /// map of integer values, on which the map holds no counts.
    fn upsert_add(&mut self, key: &KeyRef<'_>, delta: i64) -> Option<i64> {
        if let KeyRef::Int(n) = key {
            if let Some(data) = &mut self.int_values8 {
                let slot = data.get_mut(n)?;
                *slot = Self::checked_word_add(*slot, delta);
                return Some(*slot);
            }
        }
        match self.data.get_mut(key)? {
            MapVal::Word(word) => {
                *word = Self::checked_word_add(*word, delta);
                Some(*word)
            }
            MapVal::Wide(_) => {
                eprintln!(
                    "pith runtime error: map update on a map whose values are not word-sized"
                );
                // panic-guard: the fused update is emitted only for an integer-valued map, so a wide value here is a compiler bug with no correct answer.
                std::process::exit(1);
            }
        }
    }

    /// Add `delta` to the word under `key` when the map holds it, or store
    /// `init` there when it does not, and return the word now stored. This is
    /// the counting shape `if m.contains_key(k): m[k] += d else: m[k] = e` in
    /// one hash and one probe, where the unfused form paid two calls and two
    /// probes. Like `upsert_add`, it is only reached for integer values.
    fn add_or_insert(&mut self, key: &KeyRef<'_>, delta: i64, init: i64) -> i64 {
        if let KeyRef::Int(n) = key {
            if let Some(data) = &mut self.int_values8 {
                let slot = data.entry(*n).and_modify(|v| *v = Self::checked_word_add(*v, delta)).or_insert(init);
                return *slot;
            }
        }
        match self.data.entry_ref(key) {
            EntryRef::Occupied(mut entry) => match entry.get_mut() {
                MapVal::Word(word) => {
                    *word = Self::checked_word_add(*word, delta);
                    *word
                }
                MapVal::Wide(_) => {
                    eprintln!(
                        "pith runtime error: map update on a map whose values are not word-sized"
                    );
                    // panic-guard: the fused update is emitted only for an integer-valued map, so a wide value here is a compiler bug with no correct answer.
                    std::process::exit(1);
                }
            },
            EntryRef::Vacant(entry) => {
                entry.insert(MapVal::Word(init));
                init
            }
        }
    }
}

/// Body of the fused update: add `delta` to the value under a borrowed key and
/// hand back the new value. A key the map does not hold takes the same exit
/// `m[k]` takes, because the strict get this replaces is what would have run.
#[inline(always)]
fn upsert_add_keyed(impl_ref: &mut MapImpl, map_key: &KeyRef<'_>, delta: i64) -> i64 {
    if impl_ref.val_is_heap() {
        eprintln!("pith runtime error: map update on a map whose values are reference-counted");
        // panic-guard: the fused update is emitted only for an integer-valued map, so a counted value here is a compiler bug with no correct answer.
        std::process::exit(1);
    }
    match impl_ref.upsert_add(map_key, delta) {
        Some(v) => v,
        None => strict_miss(map_key.display()),
    }
}

/// Add `delta` to the value under a C-string key, returning the new value.
///
/// The emitter calls this for `m.insert(k, m[k] + d)` and `m[k] = m[k] + d`
/// when both key expressions are the same side-effect-free expression and the
/// map's values are integers. The unfused shape hashes the key twice here,
/// once for the strict get and once for the store; this hashes it once. The
/// store counter is the one that moves, because one call is one store.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_map_upsert_add_cstr(
    map_handle: i64,
    key: *const i8,
    delta: i64,
) -> i64 {
    if key.is_null() {
        null_map_key_exit();
    }
    let run = |impl_ref: &mut MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::String, "map_upsert_add");
        crate::perf_stats!(PERF_MAP_STRING_INSERTS += 1);
        // SAFETY: `key` is non-null and a valid C string per caller contract.
        unsafe { with_cstr_key(key, |map_key| upsert_add_keyed(impl_ref, map_key, delta)) }
            .unwrap_or(0)
    };
    // SAFETY: `with_map_mut_handle` validates `map_handle` before mutating it.
    match unsafe { with_map_mut_handle(map_handle, run) } {
        Some(v) => v,
        None => invalid_map_handle_exit(),
    }
}

/// Add `delta` to the value under an integer key, returning the new value.
/// The int-keyed twin of `pith_map_upsert_add_cstr`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_upsert_add_ikey(map_handle: i64, key: i64, delta: i64) -> i64 {
    let run = |impl_ref: &mut MapImpl| {
        crate::perf_stats!(PERF_MAP_INT_INSERTS += 1);
        upsert_add_keyed(impl_ref, &KeyRef::Int(key), delta)
    };
    // SAFETY: `with_map_mut_handle` validates `map_handle` before mutating it.
    match unsafe { with_map_mut_handle(map_handle, run) } {
        Some(v) => v,
        None => invalid_map_handle_exit(),
    }
}

/// Add `delta` to the value under a bytes key, returning the new value. The
/// bytes-keyed twin of `pith_map_upsert_add_cstr`; the key is borrowed from
/// the caller's bytes object for the length of the call, the way every other
/// bytes entry point borrows it.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_upsert_add_bkey(map_handle: i64, key: i64, delta: i64) -> i64 {
    let run = |impl_ref: &mut MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_upsert_add_bkey");
        // SAFETY: `with_bytes_key` validates `key` and scopes its borrowed slice.
        match unsafe { with_bytes_key(key, |map_key| upsert_add_keyed(impl_ref, map_key, delta)) } {
            Some(v) => v,
            None => strict_miss(String::from("<not a bytes value>")),
        }
    };
    // SAFETY: `with_map_mut_handle` validates `map_handle` before mutating it.
    match unsafe { with_map_mut_handle(map_handle, run) } {
        Some(v) => v,
        None => invalid_map_handle_exit(),
    }
}

/// Body of the propagating fused update: add `delta` to the value under a
/// borrowed key and return 1 when the map held the key, or 0 without modifying
/// the map when the key is absent. Used inside `-> T!` functions so an
/// unguarded `m[k] = m[k] + d` / `m[k] += d` hashes and probes once on a hit
/// and still propagates a miss as an error instead of aborting.
#[inline(always)]
fn try_upsert_add_keyed(impl_ref: &mut MapImpl, map_key: &KeyRef<'_>, delta: i64) -> i64 {
    if impl_ref.val_is_heap() {
        eprintln!("pith runtime error: map update on a map whose values are reference-counted");
        // panic-guard: the fused update is emitted only for an integer-valued map, so a counted value here is a compiler bug with no correct answer.
        std::process::exit(1);
    }
    i64::from(impl_ref.upsert_add(map_key, delta).is_some())
}

/// Add `delta` to the value under a C-string key when present, returning 1 on
/// hit and 0 on miss without modifying the map.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a valid null-terminated C string (or null, which returns 0).
#[no_mangle]
pub unsafe extern "C" fn pith_map_try_upsert_add_cstr(
    map_handle: i64,
    key: *const i8,
    delta: i64,
) -> i64 {
    if key.is_null() {
        return 0;
    }
    let run = |impl_ref: &mut MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::String, "map_try_upsert_add");
        crate::perf_stats!(PERF_MAP_STRING_INSERTS += 1);
        // SAFETY: `key` is non-null and a valid C string per caller contract.
        unsafe { with_cstr_key(key, |map_key| try_upsert_add_keyed(impl_ref, map_key, delta)) }
            .unwrap_or(0)
    };
    // SAFETY: `with_map_mut_handle` validates `map_handle` before mutating it.
    unsafe { with_map_mut_handle(map_handle, run) }.unwrap_or(0)
}

/// Add `delta` to the value under an integer key when present, returning 1 on
/// hit and 0 on miss without modifying the map.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_try_upsert_add_ikey(
    map_handle: i64,
    key: i64,
    delta: i64,
) -> i64 {
    let run = |impl_ref: &mut MapImpl| {
        crate::perf_stats!(PERF_MAP_INT_INSERTS += 1);
        try_upsert_add_keyed(impl_ref, &KeyRef::Int(key), delta)
    };
    // SAFETY: `with_map_mut_handle` validates `map_handle` before mutating it.
    unsafe { with_map_mut_handle(map_handle, run) }.unwrap_or(0)
}

/// Add `delta` to the value under a bytes key when present, returning 1 on
/// hit and 0 on miss without modifying the map.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_try_upsert_add_bkey(
    map_handle: i64,
    key: i64,
    delta: i64,
) -> i64 {
    let run = |impl_ref: &mut MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_try_upsert_add_bkey");
        // SAFETY: `with_bytes_key` validates `key` and scopes its borrowed slice.
        unsafe { with_bytes_key(key, |map_key| try_upsert_add_keyed(impl_ref, map_key, delta)) }
            .unwrap_or(0)
    };
    // SAFETY: `with_map_mut_handle` validates `map_handle` before mutating it.
    unsafe { with_map_mut_handle(map_handle, run) }.unwrap_or(0)
}

#[inline(always)]
fn add_or_insert_keyed(impl_ref: &mut MapImpl, map_key: &KeyRef<'_>, delta: i64, init: i64) -> i64 {
    if impl_ref.val_is_heap() {
        eprintln!("pith runtime error: map update on a map whose values are reference-counted");
        // panic-guard: the fused update is emitted only for an integer-valued map, so a counted value here is a compiler bug with no correct answer.
        std::process::exit(1);
    }
    impl_ref.add_or_insert(map_key, delta, init)
}

/// Add `delta` to the value under a C-string key, or store `init` when the
/// map does not hold the key, returning the value now stored.
///
/// The emitter calls this for `if m.contains_key(k): m[k] += d else: m[k] = e`
/// (and the `insert` and `m[k] = m[k] + d` spellings of either arm) when the
/// map, key, delta and initial value are side-effect-free reads and the
/// map's values are integers.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
/// * `key` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_map_add_or_insert_cstr(
    map_handle: i64,
    key: *const i8,
    delta: i64,
    init: i64,
) -> i64 {
    if key.is_null() {
        null_map_key_exit();
    }
    let run = |impl_ref: &mut MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::String, "map_add_or_insert");
        crate::perf_stats!(PERF_MAP_STRING_INSERTS += 1);
        // SAFETY: `key` is non-null and a valid C string per caller contract.
        unsafe {
            with_cstr_key(key, |map_key| {
                add_or_insert_keyed(impl_ref, map_key, delta, init)
            })
        }
        .unwrap_or(0)
    };
    // SAFETY: `with_map_mut_handle` validates `map_handle` before mutating it.
    match unsafe { with_map_mut_handle(map_handle, run) } {
        Some(v) => v,
        None => invalid_map_handle_exit(),
    }
}

/// The int-keyed twin of `pith_map_add_or_insert_cstr`.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_add_or_insert_ikey(
    map_handle: i64,
    key: i64,
    delta: i64,
    init: i64,
) -> i64 {
    let run = |impl_ref: &mut MapImpl| {
        crate::perf_stats!(PERF_MAP_INT_INSERTS += 1);
        add_or_insert_keyed(impl_ref, &KeyRef::Int(key), delta, init)
    };
    // SAFETY: `with_map_mut_handle` validates `map_handle` before mutating it.
    match unsafe { with_map_mut_handle(map_handle, run) } {
        Some(v) => v,
        None => invalid_map_handle_exit(),
    }
}

/// The bytes-keyed twin of `pith_map_add_or_insert_cstr`. The key is borrowed
/// for the call and copied only when it is inserted.
///
/// # Safety
/// * `map_handle` must be a valid `MapImpl` pointer cast to i64.
#[no_mangle]
pub unsafe extern "C" fn pith_map_add_or_insert_bkey(
    map_handle: i64,
    key: i64,
    delta: i64,
    init: i64,
) -> i64 {
    let run = |impl_ref: &mut MapImpl| {
        require_key_flavor(impl_ref.key_type, KeyType::Bytes, "map_add_or_insert_bkey");
        // SAFETY: `with_bytes_key` validates `key` and scopes its borrowed slice.
        match unsafe {
            with_bytes_key(key, |map_key| {
                add_or_insert_keyed(impl_ref, map_key, delta, init)
            })
        } {
            Some(v) => v,
            None => strict_miss(String::from("<not a bytes value>")),
        }
    };
    // SAFETY: `with_map_mut_handle` validates `map_handle` before mutating it.
    match unsafe { with_map_mut_handle(map_handle, run) } {
        Some(v) => v,
        None => invalid_map_handle_exit(),
    }
}
