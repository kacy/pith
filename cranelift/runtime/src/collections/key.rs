//! Shared owned and borrowed key types for `Map` and `Set`.
//!
//! Both hash tables store `OwnedKey` entries and probe with borrowed `KeyRef`
//! values via `hashbrown::HashMap::entry_ref`, so lookups, membership tests,
//! removals, and overwrites never allocate an owned key copy. Only a missing
//! insert/add allocates via `From<&KeyRef> for OwnedKey`.

use hashbrown::Equivalent;
use std::hash::{Hash, Hasher};

/// Owned hash-table key / set element.
///
/// Integer, string, and bytes variants stay distinct so a collection never
/// mixes flavors; the constructor's flavor tag keeps each entry point in its
/// own flavor (see [`require_flavor`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnedKey {
    Int(i64),
    String(Vec<u8>),
    Bytes(Vec<u8>),
}

impl Hash for OwnedKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            OwnedKey::Int(n) => {
                0u8.hash(state);
                n.hash(state);
            }
            OwnedKey::String(bytes) => {
                1u8.hash(state);
                bytes.hash(state);
            }
            OwnedKey::Bytes(bytes) => {
                2u8.hash(state);
                bytes.hash(state);
            }
        }
    }
}

/// Borrowed probe form of [`OwnedKey`].
///
/// The `Hash` implementation writes the exact same tag byte and slice bytes as
/// `OwnedKey`, so `hashbrown` probes match stored entries without allocating.
#[derive(Clone, Copy, Debug)]
pub enum KeyRef<'a> {
    Int(i64),
    Str(&'a [u8]),
    Bytes(&'a [u8]),
}

impl Hash for KeyRef<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match *self {
            KeyRef::Int(n) => {
                0u8.hash(state);
                n.hash(state);
            }
            KeyRef::Str(bytes) => {
                1u8.hash(state);
                bytes.hash(state);
            }
            KeyRef::Bytes(bytes) => {
                2u8.hash(state);
                bytes.hash(state);
            }
        }
    }
}

impl Equivalent<OwnedKey> for KeyRef<'_> {
    fn equivalent(&self, key: &OwnedKey) -> bool {
        match (*self, key) {
            (KeyRef::Int(a), OwnedKey::Int(b)) => a == *b,
            (KeyRef::Str(a), OwnedKey::String(b)) => a == b.as_slice(),
            (KeyRef::Bytes(a), OwnedKey::Bytes(b)) => a == b.as_slice(),
            _ => false,
        }
    }
}

/// Allocate the table's own copy of a borrowed key. Reached only when an insert
/// or set add misses.
impl From<&KeyRef<'_>> for OwnedKey {
    fn from(key: &KeyRef<'_>) -> OwnedKey {
        match *key {
            KeyRef::Int(n) => OwnedKey::Int(n),
            KeyRef::Str(bytes) => OwnedKey::String(bytes.to_vec()),
            KeyRef::Bytes(bytes) => OwnedKey::Bytes(bytes.to_vec()),
        }
    }
}

impl KeyRef<'_> {
    /// Format the key as text for traces and strict-miss diagnostics.
    pub(crate) fn display(&self) -> String {
        match *self {
            KeyRef::Int(n) => n.to_string(),
            KeyRef::Str(b) => format!("{:?}", String::from_utf8_lossy(b)),
            KeyRef::Bytes(b) => format!("bytes{:?}", b),
        }
    }
}

impl OwnedKey {
    /// Format the key as text for traces and strict-miss diagnostics.
    pub(crate) fn display(&self) -> String {
        match self {
            OwnedKey::Int(n) => n.to_string(),
            OwnedKey::String(b) => format!("{:?}", String::from_utf8_lossy(b)),
            OwnedKey::Bytes(b) => format!("bytes{:?}", b),
        }
    }
}

/// Key / element flavor tag stored on a `MapImpl` or `SetImpl`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyFlavor {
    Int,
    String,
    Bytes,
}

impl KeyFlavor {
    pub(crate) fn from_code(code: i32) -> KeyFlavor {
        match code {
            1 => KeyFlavor::String,
            2 => KeyFlavor::Bytes,
            _ => KeyFlavor::Int,
        }
    }
}

/// Guard against reaching a bytes-flavored container through a non-bytes entry
/// point (or the reverse): mixing those flavors would reinterpret a bytes
/// handle as a c-string or vice versa (E254, issues #920 and #955).
pub(crate) fn require_flavor(
    container_noun: &str,
    field_noun: &str,
    actual: KeyFlavor,
    wanted: KeyFlavor,
    entry: &str,
) {
    if actual == wanted || (actual != KeyFlavor::Bytes && wanted != KeyFlavor::Bytes) {
        return;
    }
    runtime_fatal!(
        "{} called on a {} whose {} are {:?}, not {:?}",
        entry,
        container_noun,
        field_noun,
        actual,
        wanted
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;

    fn hash_of<T: Hash>(value: &T) -> u64 {
        let mut h = DefaultHasher::new();
        value.hash(&mut h);
        h.finish()
    }

    #[test]
    fn key_ref_hashes_and_matches_owned_key() {
        let pairs: &[(OwnedKey, KeyRef<'_>)] = &[
            (OwnedKey::Int(0), KeyRef::Int(0)),
            (OwnedKey::Int(-42), KeyRef::Int(-42)),
            (OwnedKey::Int(i64::MAX), KeyRef::Int(i64::MAX)),
            (OwnedKey::String(Vec::new()), KeyRef::Str(b"")),
            (OwnedKey::String(b"hello".to_vec()), KeyRef::Str(b"hello")),
            (OwnedKey::Bytes(Vec::new()), KeyRef::Bytes(b"")),
            (OwnedKey::Bytes(b"\x00\xff".to_vec()), KeyRef::Bytes(b"\x00\xff")),
        ];
        for (owned, borrowed) in pairs {
            assert_eq!(hash_of(owned), hash_of(borrowed));
            assert!(borrowed.equivalent(owned));
            assert_eq!(&OwnedKey::from(borrowed), owned);
        }
        assert!(!KeyRef::Str(b"a").equivalent(&OwnedKey::Bytes(b"a".to_vec())));
        assert!(!KeyRef::Bytes(b"a").equivalent(&OwnedKey::String(b"a".to_vec())));
    }
}
