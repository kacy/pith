//! substring search shared by the c-string primitives.
//!
//! `contains`, `index_of`, `replace` and `split` used to compare a
//! needle-length slice at every position of the haystack, which is a
//! `bcmp` call per byte of haystack whether or not that byte could start a
//! match. on the std pipeline that made `pith_cstring_contains` alone 10.5
//! percent of the workload by self instruction count, and on the event
//! ledger `pith_string_split_to_list` was 10.9 percent (#1099).
//!
//! the search is libc's `memmem`. two candidates were measured against the
//! naive loop on `bench/substring_search.pith`, the std pipeline and the
//! event ledger: `memchr` for the needle's first byte followed by a compare
//! of the rest, and `memmem`. on the shapes the workloads have — a one-byte
//! delimiter or quote in a field of a few dozen bytes — the two are within
//! 40 instructions of each other per call and both are 3 to 20 times
//! cheaper than the naive loop; on the std pipeline as a whole `memmem`
//! reads 0.6 percent fewer instructions. where they part is the long
//! haystack: a 7-byte needle absent from 4000 bytes costs `memmem` 8.4k
//! instructions and the first-byte scan 25.6k, and a haystack made of the
//! needle's first byte costs it 72k against 262k, since the first-byte
//! scan then compares the needle at every position, exactly as the naive
//! loop did. `memmem` has no such cliff, and it is the one that stays.
//! docs/performance.md has the per-cell numbers.

use std::ffi::c_void;

/// the first position at or after `start` where `needle` occurs in
/// `haystack`. an empty needle matches at `start` when `start` is within
/// the haystack (its end included), which is what the callers' old loops
/// did; a needle longer than the remaining haystack never matches.
pub fn find_from(haystack: &[u8], needle: &[u8], start: usize) -> Option<usize> {
    let h_len = haystack.len();
    let n_len = needle.len();
    if start > h_len {
        return None;
    }
    if n_len == 0 {
        return Some(start);
    }
    if h_len - start < n_len {
        return None;
    }
    let rest = &haystack[start..];
    // memmem reads only within the lengths it is given, which are exactly
    // the two slices; neither is empty here.
    let found = unsafe {
        libc::memmem(
            rest.as_ptr() as *const c_void,
            rest.len(),
            needle.as_ptr() as *const c_void,
            n_len,
        )
    };
    if found.is_null() {
        None
    } else {
        Some(start + (found as usize - rest.as_ptr() as usize))
    }
}

/// the first position where `needle` occurs in `haystack`.
pub fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    find_from(haystack, needle, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive(haystack: &[u8], needle: &[u8], start: usize) -> Option<usize> {
        if start > haystack.len() {
            return None;
        }
        if needle.is_empty() {
            return Some(start);
        }
        if haystack.len() - start < needle.len() {
            return None;
        }
        (start..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
    }

    #[test]
    fn empty_needle_matches_at_start() {
        assert_eq!(find(b"", b""), Some(0));
        assert_eq!(find(b"abc", b""), Some(0));
        assert_eq!(find_from(b"abc", b"", 3), Some(3));
        assert_eq!(find_from(b"abc", b"", 4), None);
    }

    #[test]
    fn needle_longer_than_haystack_never_matches() {
        assert_eq!(find(b"", b"a"), None);
        assert_eq!(find(b"ab", b"abc"), None);
        assert_eq!(find_from(b"abcd", b"cde", 2), None);
    }

    #[test]
    fn needle_at_either_end() {
        assert_eq!(find(b"needle in a haystack", b"needle"), Some(0));
        assert_eq!(find(b"a haystack ends in a needle", b"needle"), Some(21));
        assert_eq!(find(b"x", b"x"), Some(0));
        assert_eq!(find(b"abc", b"abc"), Some(0));
        assert_eq!(find(b"abc", b"c"), Some(2));
    }

    #[test]
    fn repeated_first_bytes() {
        assert_eq!(find(b"aaaaaaab", b"ab"), Some(6));
        assert_eq!(find(b"aaaaaaaa", b"ab"), None);
        assert_eq!(find(b"aaaa", b"aa"), Some(0));
        assert_eq!(find_from(b"aaaa", b"aa", 1), Some(1));
        assert_eq!(find_from(b"aaaa", b"aa", 3), None);
    }

    #[test]
    fn shared_prefix_with_an_earlier_non_match() {
        assert_eq!(find(b"abcabd", b"abd"), Some(3));
        assert_eq!(find(b"ababac", b"abac"), Some(2));
        assert_eq!(find(b"ababab", b"abac"), None);
        assert_eq!(find(b"xxabcxxabcdxx", b"abcd"), Some(7));
    }

    #[test]
    fn non_ascii_bytes() {
        let hay = "héllo wörld".as_bytes();
        assert_eq!(find(hay, "ö".as_bytes()), Some(8));
        assert_eq!(find(hay, "wörld".as_bytes()), Some(7));
        assert_eq!(find(hay, &[0xff]), None);
        assert_eq!(find(&[0x00, 0xff, 0xfe], &[0xff, 0xfe]), Some(1));
    }

    #[test]
    fn start_past_the_end_is_none() {
        assert_eq!(find_from(b"abc", b"a", 3), None);
        assert_eq!(find_from(b"abc", b"a", 100), None);
        assert_eq!(find_from(b"abca", b"a", 1), Some(3));
    }

    // a small generator with no dependency: xorshift over a seed, so the
    // pairs are the same on every run and a failure names its inputs.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }

        fn below(&mut self, n: u64) -> usize {
            (self.next() % n) as usize
        }
    }

    #[test]
    fn matches_the_naive_search_on_generated_pairs() {
        // a small alphabet so repeats and shared prefixes are common, with
        // bytes above 0x7f in it so the search is exercised on non-ascii.
        let alphabet: [u8; 6] = [b'a', b'b', b'c', 0xc3, 0xa9, 0xff];
        let mut rng = Rng(0x9e3779b97f4a7c15);
        let mut pairs = 0usize;
        let mut hits = 0usize;
        for _ in 0..4000 {
            let h_len = rng.below(64);
            let haystack: Vec<u8> = (0..h_len).map(|_| alphabet[rng.below(6)]).collect();
            let n_len = rng.below(9);
            // half the needles are cut from the haystack so most of them hit
            let needle: Vec<u8> = if rng.below(2) == 0 && n_len <= h_len && h_len > 0 {
                let at = rng.below((h_len - n_len + 1) as u64);
                haystack[at..at + n_len].to_vec()
            } else {
                (0..n_len).map(|_| alphabet[rng.below(6)]).collect()
            };
            let start = rng.below((h_len + 2) as u64);
            let expected = naive(&haystack, &needle, start);
            let actual = find_from(&haystack, &needle, start);
            assert_eq!(
                actual, expected,
                "haystack {haystack:?} needle {needle:?} start {start}"
            );
            assert_eq!(find(&haystack, &needle), naive(&haystack, &needle, 0));
            pairs += 1;
            if expected.is_some() {
                hits += 1;
            }
        }
        assert_eq!(pairs, 4000);
        assert!(
            hits > 1000,
            "only {hits} of {pairs} pairs matched; the generator is off"
        );
    }
}
