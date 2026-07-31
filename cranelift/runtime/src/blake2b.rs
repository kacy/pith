//! BLAKE2b (RFC 7693), keyed and unkeyed, with digests up to 64 bytes.
//!
//! Hand-written rather than pulled in as a crate: the runtime keeps its
//! dependency set small on purpose, and BLAKE2b is compact and fully
//! specified. Argon2 (see `argon2.rs`) builds on this module for both its
//! fixed-length hash H and its variable-length hash H'.

/// Largest digest BLAKE2b can produce, in bytes.
pub const MAX_OUT_LEN: usize = 64;

/// Largest key BLAKE2b accepts, in bytes.
pub const MAX_KEY_LEN: usize = 64;

const BLOCK_LEN: usize = 128;

/// Initialization vector from RFC 7693 section 2.6 (the same constants as
/// SHA-512's IV).
const IV: [u64; 8] = [
    0x6a09e667f3bcc908,
    0xbb67ae8584caa73b,
    0x3c6ef372fe94f82b,
    0xa54ff53a5f1d36f1,
    0x510e527fade682d1,
    0x9b05688c2b3e6c1f,
    0x1f83d9abfb41bd6b,
    0x5be0cd19137e2179,
];

/// Message schedule from RFC 7693 section 2.7. BLAKE2b runs 12 rounds; rounds
/// 10 and 11 reuse the first two permutations.
const SIGMA: [[usize; 16]; 12] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
];

/// Incremental BLAKE2b state. Feed input with [`Blake2b::update`] and close
/// with [`Blake2b::finalize`].
pub struct Blake2b {
    h: [u64; 8],
    /// Total bytes compressed so far (the RFC's 128-bit counter; a u128 keeps
    /// the carry into the high word automatic).
    t: u128,
    buf: [u8; BLOCK_LEN],
    buf_len: usize,
    out_len: usize,
}

impl Blake2b {
    /// Start a hash producing `out_len` bytes, keyed when `key` is non-empty.
    /// Returns `None` when `out_len` is 0 or over 64, or the key is over 64
    /// bytes.
    pub fn new(out_len: usize, key: &[u8]) -> Option<Blake2b> {
        if out_len == 0 || out_len > MAX_OUT_LEN || key.len() > MAX_KEY_LEN {
            return None;
        }
        let mut h = IV;
        // parameter block word 0: digest length, key length, fanout 1, depth 1
        h[0] ^= 0x0101_0000 ^ ((key.len() as u64) << 8) ^ out_len as u64;
        let mut state = Blake2b {
            h,
            t: 0,
            buf: [0; BLOCK_LEN],
            buf_len: 0,
            out_len,
        };
        // a key is hashed as a full padded first block before any input
        if !key.is_empty() {
            state.buf[..key.len()].copy_from_slice(key);
            state.buf_len = BLOCK_LEN;
        }
        Some(state)
    }

    /// Absorb `input` into the hash.
    pub fn update(&mut self, mut input: &[u8]) {
        while !input.is_empty() {
            // only flush a full buffer once more input exists; the final
            // block must stay buffered so finalize can flag it as last
            if self.buf_len == BLOCK_LEN {
                self.t += BLOCK_LEN as u128;
                let block = self.buf;
                self.compress(&block, false);
                self.buf_len = 0;
            }
            let take = (BLOCK_LEN - self.buf_len).min(input.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&input[..take]);
            self.buf_len += take;
            input = &input[take..];
        }
    }

    /// Finish the hash and return the digest.
    pub fn finalize(mut self) -> Vec<u8> {
        self.t += self.buf_len as u128;
        self.buf[self.buf_len..].fill(0);
        let block = self.buf;
        self.compress(&block, true);
        let mut out = vec![0u8; self.out_len];
        for (i, chunk) in out.chunks_mut(8).enumerate() {
            chunk.copy_from_slice(&self.h[i].to_le_bytes()[..chunk.len()]);
        }
        out
    }

    /// The compression function F from RFC 7693 section 3.2.
    fn compress(&mut self, block: &[u8; BLOCK_LEN], last: bool) {
        let mut m = [0u64; 16];
        for (i, word) in m.iter_mut().enumerate() {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&block[i * 8..i * 8 + 8]);
            *word = u64::from_le_bytes(bytes);
        }

        let mut v = [0u64; 16];
        v[..8].copy_from_slice(&self.h);
        v[8..].copy_from_slice(&IV);
        v[12] ^= self.t as u64;
        v[13] ^= (self.t >> 64) as u64;
        if last {
            v[14] = !v[14];
        }

        for sigma in &SIGMA {
            // columns
            mix(&mut v, 0, 4, 8, 12, m[sigma[0]], m[sigma[1]]);
            mix(&mut v, 1, 5, 9, 13, m[sigma[2]], m[sigma[3]]);
            mix(&mut v, 2, 6, 10, 14, m[sigma[4]], m[sigma[5]]);
            mix(&mut v, 3, 7, 11, 15, m[sigma[6]], m[sigma[7]]);
            // diagonals
            mix(&mut v, 0, 5, 10, 15, m[sigma[8]], m[sigma[9]]);
            mix(&mut v, 1, 6, 11, 12, m[sigma[10]], m[sigma[11]]);
            mix(&mut v, 2, 7, 8, 13, m[sigma[12]], m[sigma[13]]);
            mix(&mut v, 3, 4, 9, 14, m[sigma[14]], m[sigma[15]]);
        }

        for i in 0..8 {
            self.h[i] ^= v[i] ^ v[i + 8];
        }
    }
}

/// The mixing function G from RFC 7693 section 3.1.
#[inline(always)]
fn mix(v: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize, x: u64, y: u64) {
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
    v[d] = (v[d] ^ v[a]).rotate_right(32);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(24);
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(63);
}

/// One-shot BLAKE2b: hash `data` (keyed when `key` is non-empty) into an
/// `out_len`-byte digest. Returns `None` on an out-of-range digest or key
/// length.
pub fn hash(out_len: usize, key: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    let mut state = Blake2b::new(out_len, key)?;
    state.update(data);
    Some(state.finalize())
}

#[cfg(test)]
mod tests {
    use super::hash;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }

    fn hash_hex(out_len: usize, key: &[u8], data: &[u8]) -> Option<String> {
        hash(out_len, key, data).map(|digest| hex(&digest))
    }

    #[test]
    fn rfc_7693_appendix_a_abc() {
        assert_eq!(
            hash_hex(64, b"", b"abc").as_deref(),
            Some(
                "ba80a53f981c4d0d6a2797b69f12f6e94c212f14685ac4b74b12bb6fdbffa2d1\
                 7d87c5392aab792dc252d5de4533cc9518d38aa8dbf1925ab92386edd4009923"
            )
        );
    }

    #[test]
    fn unkeyed_empty_input() {
        assert_eq!(
            hash_hex(64, b"", b"").as_deref(),
            Some(
                "786a02f742015903c6c6fd852552d272912f4740e15847618a86e217f71f5419\
                 d25e1031afee585313896444934eb04b903a685b1448b755d56f701afe9be2ce"
            )
        );
    }

    #[test]
    fn truncated_digest_lengths() {
        let fox: &[u8] = b"The quick brown fox jumps over the lazy dog";
        assert_eq!(
            hash_hex(20, b"", fox).as_deref(),
            Some("3c523ed102ab45a37d54f5610d5a983162fde84f")
        );
        assert_eq!(
            hash_hex(32, b"", fox).as_deref(),
            Some("01718cec35cd3d796dd00020e0bfecb473ad23457d063b75eff29c0ffa2e58a9")
        );
        assert_eq!(
            hash_hex(48, b"", fox).as_deref(),
            Some(
                "b7c81b228b6bd912930e8f0b5387989691c1cee1e65aade4da3b86a3\
                 c9f678fc8018f6ed9e2906720c8d2a3aeda9c03d"
            )
        );
    }

    // the official blake2b-kat vectors hash the byte sequence 00 01 02 ...
    // with the 64-byte key 00 01 02 ... 3f
    fn kat_input(len: usize) -> Vec<u8> {
        (0..len).map(|i| i as u8).collect()
    }

    fn kat_key() -> Vec<u8> {
        kat_input(64)
    }

    #[test]
    fn official_kat_keyed_empty() {
        assert_eq!(
            hash_hex(64, &kat_key(), b"").as_deref(),
            Some(
                "10ebb67700b1868efb4417987acf4690ae9d972fb7a590c2f02871799aaa4786\
                 b5e996e8f0f4eb981fc214b005f42d2ff4233499391653df7aefcbc13fc51568"
            )
        );
    }

    #[test]
    fn official_kat_keyed_one_byte() {
        assert_eq!(
            hash_hex(64, &kat_key(), &kat_input(1)).as_deref(),
            Some(
                "961f6dd1e4dd30f63901690c512e78e4b45e4742ed197c3c5e45c549fd25f2e4\
                 187b0bc9fe30492b16b0d0bc4ef9b0f34c7003fac09a5ef1532e69430234cebd"
            )
        );
    }

    #[test]
    fn official_kat_keyed_block_boundaries() {
        assert_eq!(
            hash_hex(64, &kat_key(), &kat_input(64)).as_deref(),
            Some(
                "65676d800617972fbd87e4b9514e1c67402b7a331096d3bfac22f1abb95374ab\
                 c942f16e9ab0ead33b87c91968a6e509e119ff07787b3ef483e1dcdccf6e3022"
            )
        );
        assert_eq!(
            hash_hex(64, &kat_key(), &kat_input(128)).as_deref(),
            Some(
                "72065ee4dd91c2d8509fa1fc28a37c7fc9fa7d5b3f8ad3d0d7a25626b57b1b44\
                 788d4caf806290425f9890a3a2a35a905ab4b37acfd0da6e4517b2525c9651e4"
            )
        );
        assert_eq!(
            hash_hex(64, &kat_key(), &kat_input(129)).as_deref(),
            Some(
                "64475dfe7600d7171bea0b394e27c9b00d8e74dd1e416a79473682ad3dfdbb70\
                 6631558055cfc8a40e07bd015a4540dcdea15883cbbf31412df1de1cd4152b91"
            )
        );
    }

    #[test]
    fn official_kat_keyed_255_bytes() {
        assert_eq!(
            hash_hex(64, &kat_key(), &kat_input(255)).as_deref(),
            Some(
                "142709d62e28fcccd0af97fad0f8465b971e82201dc51070faa0372aa43e9248\
                 4be1c1e73ba10906d5d1853db6a4106e0a7bf9800d373d6dee2d46d62ef2a461"
            )
        );
        // keyed and truncated at the same time
        assert_eq!(
            hash_hex(32, &kat_key(), &kat_input(255)).as_deref(),
            Some("fe7b76a61787c089141f9e10fca1e5092488d89c62ea793fb2c5b1f849b4f2cb")
        );
    }

    #[test]
    fn unkeyed_block_boundaries() {
        assert_eq!(
            hash_hex(64, b"", &kat_input(128)).as_deref(),
            Some(
                "2319e3789c47e2daa5fe807f61bec2a1a6537fa03f19ff32e87eecbfd64b7e0e\
                 8ccff439ac333b040f19b0c4ddd11a61e24ac1fe0f10a039806c5dcc0da3d115"
            )
        );
        assert_eq!(
            hash_hex(64, b"", &kat_input(129)).as_deref(),
            Some(
                "f59711d44a031d5f97a9413c065d1e614c417ede998590325f49bad2fd444d3e\
                 4418be19aec4e11449ac1a57207898bc57d76a1bcf3566292c20c683a5c4648f"
            )
        );
    }

    #[test]
    fn incremental_matches_one_shot() {
        let data = kat_input(300);
        let Some(whole) = hash(64, b"", &data) else {
            assert!(false, "one-shot hash failed");
            return;
        };
        let Some(mut state) = super::Blake2b::new(64, b"") else {
            assert!(false, "state construction failed");
            return;
        };
        for chunk in data.chunks(7) {
            state.update(chunk);
        }
        assert_eq!(state.finalize(), whole);
    }

    #[test]
    fn rejects_out_of_range_lengths() {
        assert!(hash(0, b"", b"data").is_none());
        assert!(hash(65, b"", b"data").is_none());
        assert!(hash(64, &[0u8; 65], b"data").is_none());
        assert!(hash(64, &[0u8; 64], b"data").is_some());
        assert!(hash(1, b"", b"").is_some());
    }
}
