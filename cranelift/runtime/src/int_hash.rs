//! A multiplicative hasher for the runtime's internal tables keyed by
//! integers: handle addresses, fd numbers, threadlocal slots and sequence
//! numbers. The keys come from the runtime, not from program input, so the
//! default siphash spends time on flooding protection these tables do not
//! need, on paths an http server takes several times per request.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

#[derive(Default)]
pub(crate) struct IntHasher(u64);

impl Hasher for IntHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(b as u64);
        }
    }

    fn write_u8(&mut self, n: u8) {
        self.write_u64(n as u64);
    }

    fn write_u32(&mut self, n: u32) {
        self.write_u64(n as u64);
    }

    fn write_i32(&mut self, n: i32) {
        self.write_u64(n as u32 as u64);
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0 ^ n).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }

    fn write_i64(&mut self, n: i64) {
        self.write_u64(n as u64);
    }

    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }

    fn write_isize(&mut self, n: isize) {
        self.write_u64(n as u64);
    }

    fn finish(&self) -> u64 {
        // the product's low bits are its weakest, and zero for an aligned
        // address, while the table indexes buckets by the low bits. rotating
        // brings the well-mixed middle bits down, as rustc's fxhash does.
        self.0.rotate_left(26)
    }
}

pub(crate) type IntBuildHasher = BuildHasherDefault<IntHasher>;
pub(crate) type IntMap<K, V> = HashMap<K, V, IntBuildHasher>;
pub(crate) type IntSet<K> = HashSet<K, IntBuildHasher>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::BuildHasher;

    #[test]
    fn small_sequential_keys_spread_across_low_bits() {
        let build = IntBuildHasher::default();
        let mut buckets = std::collections::HashSet::new();
        for key in 0..64i64 {
            buckets.insert(build.hash_one(key) & 63);
        }
        // a hasher that left small keys in the high bits would put them all
        // in a handful of buckets.
        assert!(
            buckets.len() > 32,
            "only {} of 64 buckets used",
            buckets.len()
        );
    }

    #[test]
    fn aligned_addresses_spread_across_low_bits() {
        let build = IntBuildHasher::default();
        let mut buckets = std::collections::HashSet::new();
        for i in 0..64usize {
            buckets.insert(build.hash_one(0x7f00_0000_0000usize + i * 64) & 63);
        }
        assert!(
            buckets.len() > 32,
            "only {} of 64 buckets used",
            buckets.len()
        );
    }

    #[test]
    fn maps_and_sets_behave_like_the_std_ones() {
        let mut map: IntMap<(i32, u8), i64> = IntMap::default();
        for fd in 0..1000 {
            map.insert((fd, (fd % 2) as u8), fd as i64 * 3);
        }
        for fd in 0..1000 {
            assert_eq!(map.get(&(fd, (fd % 2) as u8)), Some(&(fd as i64 * 3)));
            assert_eq!(map.get(&(fd, ((fd + 1) % 2) as u8)), None);
        }
        let mut set: IntSet<usize> = IntSet::default();
        assert!(set.insert(8));
        assert!(!set.insert(8));
        assert!(set.remove(&8));
        assert!(set.is_empty());
    }
}
