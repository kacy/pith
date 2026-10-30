//! Hardware block loops for the checksum and digest code in std.
//!
//! std.checksum and std.hash are written in Pith: padding, buffering, the
//! tail and a portable block loop all live there. On CPUs with the right
//! instructions, the bulk of the input goes through one of the functions
//! below instead. Each takes a whole run of blocks per call and reports
//! when it cannot help, so the Pith loop then does the work. Cranelift has
//! no carry-less multiply, CRC or SHA instructions, so Pith code cannot emit
//! them itself.

use crate::bytes::pith_bytes_ref;
use crate::collections::list::primitive_words_mut;

/// Fold `len` bytes from the start of `data` into the CRC-32 (IEEE)
/// register `crc`, as std.checksum keeps it: inverted, before the final
/// xor. `len` must be a multiple of 16 and at least 64. Returns the new
/// register, or -1 when the CPU lacks the instructions or an argument is
/// out of range, and the caller then runs its own loop.
#[no_mangle]
pub unsafe extern "C" fn pith_crc32_fold(crc: i64, data: i64, len: i64) -> i64 {
    let Some(bytes) = pith_bytes_ref(data) else {
        return -1;
    };
    if len < 64 || len % 16 != 0 || len as u64 > bytes.data.len() as u64 {
        return -1;
    }
    match crc32_hw(crc as u32, &bytes.data[..len as usize]) {
        Some(folded) => folded as i64,
        None => -1,
    }
}

/// Run the SHA-256 compression function over `blocks` 64-byte blocks from
/// the start of `data`. `state` is a list of eight Ints holding the 32-bit
/// chaining words, updated in place. Returns 1 when it did, or 0 when the
/// CPU lacks the instructions or an argument is out of range, and the
/// caller then runs its own loop with the state unchanged.
#[no_mangle]
pub unsafe extern "C" fn pith_sha256_blocks(state: i64, data: i64, blocks: i64) -> i64 {
    let Some(bytes) = pith_bytes_ref(data) else {
        return 0;
    };
    let Some(words) = primitive_words_mut(state) else {
        return 0;
    };
    if words.len() != 8 || blocks < 0 || (blocks as u64).saturating_mul(64) > bytes.data.len() as u64 {
        return 0;
    }
    let mut h = [0u32; 8];
    for (out, &word) in h.iter_mut().zip(words.iter()) {
        *out = word as u32;
    }
    if !sha256_hw(&mut h, &bytes.data[..blocks as usize * 64]) {
        return 0;
    }
    for (word, &out) in words.iter_mut().zip(h.iter()) {
        *word = out as i64;
    }
    1
}

/// Fold `data` (a multiple of 16 bytes, at least 64) into the CRC-32
/// register, or None when the CPU lacks the instructions.
fn crc32_hw(crc: u32, data: &[u8]) -> Option<u32> {
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("pclmulqdq")
            && std::arch::is_x86_feature_detected!("sse4.1")
        {
            return Some(x86::crc32_fold(crc, data));
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("crc") {
            return Some(arm::crc32_fold(crc, data));
        }
    }
    let _ = (crc, data);
    None
}

/// Compress the 64-byte blocks of `data` into `state`, or return false,
/// with `state` untouched, when the CPU lacks the instructions.
fn sha256_hw(state: &mut [u32; 8], data: &[u8]) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("sha")
            && std::arch::is_x86_feature_detected!("sse4.1")
        {
            x86::sha256_blocks(state, data);
            return true;
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("sha2") {
            arm::sha256_blocks(state, data);
            return true;
        }
    }
    let _ = (state, data);
    false
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
const K256: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::K256;
    use std::arch::x86_64::*;

    // CRC-32 folding constants for the reflected IEEE polynomial: x^n mod P
    // for the fold distances (512 and 128 bits), the 64-bit step, then P and
    // its Barrett quotient.
    const K1: i64 = 0x1_5444_2bd4;
    const K2: i64 = 0x1_c6e4_1596;
    const K3: i64 = 0x1_7519_97d0;
    const K4: i64 = 0x0_ccaa_009e;
    const K5: i64 = 0x1_63cd_6124;
    const P_X: i64 = 0x1_db71_0641;
    const U_PRIME: i64 = 0x1_f701_1641;

    /// Fold `data` (a multiple of 16 bytes, at least 64) into `crc`. The
    /// caller has checked for pclmulqdq and sse4.1.
    pub(super) fn crc32_fold(crc: u32, data: &[u8]) -> u32 {
        // SAFETY: the caller checked the CPU features.
        unsafe { crc32_fold_impl(crc, data) }
    }

    #[target_feature(enable = "pclmulqdq,sse2,sse4.1")]
    unsafe fn crc32_fold_impl(crc: u32, data: &[u8]) -> u32 {
        let mut chunks = data.chunks_exact(16).map(|c| _mm_loadu_si128(c.as_ptr() as *const __m128i));
        let mut next = || chunks.next().unwrap_or_else(|| _mm_setzero_si128());
        let blocks = data.len() / 16;

        // four lanes of 128 bits, each folded forward by 512 bits per step
        let mut x3 = _mm_xor_si128(next(), _mm_cvtsi32_si128(crc as i32));
        let mut x2 = next();
        let mut x1 = next();
        let mut x0 = next();
        let k1k2 = _mm_set_epi64x(K2, K1);
        let mut left = blocks - 4;
        while left >= 4 {
            x3 = fold(x3, next(), k1k2);
            x2 = fold(x2, next(), k1k2);
            x1 = fold(x1, next(), k1k2);
            x0 = fold(x0, next(), k1k2);
            left -= 4;
        }

        // the four lanes into one, then the leftover 16-byte blocks
        let k3k4 = _mm_set_epi64x(K4, K3);
        let mut x = fold(x3, x2, k3k4);
        x = fold(x, x1, k3k4);
        x = fold(x, x0, k3k4);
        while left > 0 {
            x = fold(x, next(), k3k4);
            left -= 1;
        }

        // 128 bits down to 64, then 32 with a Barrett reduction
        let low32 = _mm_set_epi32(0, 0, 0, -1);
        let x = _mm_xor_si128(_mm_clmulepi64_si128(x, k3k4, 0x10), _mm_srli_si128(x, 8));
        let x = _mm_xor_si128(
            _mm_clmulepi64_si128(_mm_and_si128(x, low32), _mm_set_epi64x(0, K5), 0x00),
            _mm_srli_si128(x, 4),
        );
        let pu = _mm_set_epi64x(U_PRIME, P_X);
        let t1 = _mm_clmulepi64_si128(_mm_and_si128(x, low32), pu, 0x10);
        let t2 = _mm_clmulepi64_si128(_mm_and_si128(t1, low32), pu, 0x00);
        _mm_extract_epi32(_mm_xor_si128(x, t2), 1) as u32
    }

    #[target_feature(enable = "pclmulqdq,sse2")]
    unsafe fn fold(a: __m128i, b: __m128i, keys: __m128i) -> __m128i {
        let lo = _mm_clmulepi64_si128(a, keys, 0x00);
        let hi = _mm_clmulepi64_si128(a, keys, 0x11);
        _mm_xor_si128(_mm_xor_si128(b, lo), hi)
    }

    /// Compress the 64-byte blocks of `data` into `state`. The caller has
    /// checked for sha and sse4.1.
    pub(super) fn sha256_blocks(state: &mut [u32; 8], data: &[u8]) {
        // SAFETY: the caller checked the CPU features.
        unsafe { sha256_blocks_impl(state, data) }
    }

    #[target_feature(enable = "sha,sse2,ssse3,sse4.1")]
    unsafe fn sha256_blocks_impl(state: &mut [u32; 8], data: &[u8]) {
        // big-endian words: reverse the bytes of each 32-bit lane
        let swap = _mm_set_epi64x(0x0c0d_0e0f_0809_0a0b, 0x0405_0607_0001_0203);
        let dcba = _mm_loadu_si128(state.as_ptr() as *const __m128i);
        let hgfe = _mm_loadu_si128(state.as_ptr().add(4) as *const __m128i);

        // sha256rnds2 wants the state as (a, b, e, f) and (c, d, g, h)
        let cdab = _mm_shuffle_epi32(dcba, 0xb1);
        let efgh = _mm_shuffle_epi32(hgfe, 0x1b);
        let mut abef = _mm_alignr_epi8(cdab, efgh, 8);
        let mut cdgh = _mm_blend_epi16(efgh, cdab, 0xf0);

        for block in data.chunks_exact(64) {
            let (abef_in, cdgh_in) = (abef, cdgh);
            let at = block.as_ptr() as *const __m128i;
            let mut w = [
                _mm_shuffle_epi8(_mm_loadu_si128(at), swap),
                _mm_shuffle_epi8(_mm_loadu_si128(at.add(1)), swap),
                _mm_shuffle_epi8(_mm_loadu_si128(at.add(2)), swap),
                _mm_shuffle_epi8(_mm_loadu_si128(at.add(3)), swap),
            ];
            for i in 0..16 {
                // w[i % 4] holds words 4i - 16 .. 4i - 13 until it is replaced
                // by words 4i .. 4i + 3
                if i >= 4 {
                    let t = _mm_add_epi32(
                        _mm_sha256msg1_epu32(w[i % 4], w[(i + 1) % 4]),
                        _mm_alignr_epi8(w[(i + 3) % 4], w[(i + 2) % 4], 4),
                    );
                    w[i % 4] = _mm_sha256msg2_epu32(t, w[(i + 3) % 4]);
                }
                let k = _mm_loadu_si128(K256.as_ptr().add(i * 4) as *const __m128i);
                let wk = _mm_add_epi32(w[i % 4], k);
                cdgh = _mm_sha256rnds2_epu32(cdgh, abef, wk);
                abef = _mm_sha256rnds2_epu32(abef, cdgh, _mm_shuffle_epi32(wk, 0x0e));
            }
            abef = _mm_add_epi32(abef, abef_in);
            cdgh = _mm_add_epi32(cdgh, cdgh_in);
        }

        let feba = _mm_shuffle_epi32(abef, 0x1b);
        let dchg = _mm_shuffle_epi32(cdgh, 0xb1);
        let dcba = _mm_blend_epi16(feba, dchg, 0xf0);
        let hgef = _mm_alignr_epi8(dchg, feba, 8);
        _mm_storeu_si128(state.as_mut_ptr() as *mut __m128i, dcba);
        _mm_storeu_si128(state.as_mut_ptr().add(4) as *mut __m128i, hgef);
    }
}

#[cfg(target_arch = "aarch64")]
mod arm {
    use super::K256;
    use std::arch::aarch64::*;

    /// Run the CRC-32 register over `data` eight bytes at a time. The
    /// caller has checked for the crc extension, whose crc32x instruction
    /// uses the IEEE polynomial.
    pub(super) fn crc32_fold(crc: u32, data: &[u8]) -> u32 {
        // SAFETY: the caller checked the CPU feature.
        unsafe { crc32_fold_impl(crc, data) }
    }

    #[target_feature(enable = "crc")]
    unsafe fn crc32_fold_impl(mut crc: u32, data: &[u8]) -> u32 {
        for word in data.chunks_exact(8) {
            let mut le = [0u8; 8];
            le.copy_from_slice(word);
            crc = __crc32d(crc, u64::from_le_bytes(le));
        }
        crc
    }

    /// Compress the 64-byte blocks of `data` into `state`. The caller has
    /// checked for the sha2 extension.
    pub(super) fn sha256_blocks(state: &mut [u32; 8], data: &[u8]) {
        // SAFETY: the caller checked the CPU feature.
        unsafe { sha256_blocks_impl(state, data) }
    }

    #[target_feature(enable = "sha2")]
    unsafe fn sha256_blocks_impl(state: &mut [u32; 8], data: &[u8]) {
        let mut abcd = vld1q_u32(state.as_ptr());
        let mut efgh = vld1q_u32(state.as_ptr().add(4));
        for block in data.chunks_exact(64) {
            let (abcd_in, efgh_in) = (abcd, efgh);
            let at = block.as_ptr();
            // big-endian words: reverse the bytes of each 32-bit lane
            let mut w = [
                vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(at))),
                vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(at.add(16)))),
                vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(at.add(32)))),
                vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(at.add(48)))),
            ];
            for i in 0..16 {
                // w[i % 4] holds words 4i - 16 .. 4i - 13 until it is replaced
                // by words 4i .. 4i + 3
                if i >= 4 {
                    w[i % 4] = vsha256su1q_u32(
                        vsha256su0q_u32(w[i % 4], w[(i + 1) % 4]),
                        w[(i + 2) % 4],
                        w[(i + 3) % 4],
                    );
                }
                let wk = vaddq_u32(w[i % 4], vld1q_u32(K256.as_ptr().add(i * 4)));
                let abcd_before = abcd;
                abcd = vsha256hq_u32(abcd, efgh, wk);
                efgh = vsha256h2q_u32(efgh, abcd_before, wk);
            }
            abcd = vaddq_u32(abcd, abcd_in);
            efgh = vaddq_u32(efgh, efgh_in);
        }
        vst1q_u32(state.as_mut_ptr(), abcd);
        vst1q_u32(state.as_mut_ptr().add(4), efgh);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crc32_bitwise(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 == 1 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }

    fn sample(len: usize, seed: u32) -> Vec<u8> {
        let mut s = seed;
        (0..len)
            .map(|_| {
                s = s.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (s >> 16) as u8
            })
            .collect()
    }

    #[test]
    fn crc32_fold_matches_the_bitwise_definition() {
        for len in (64..=1024).step_by(16) {
            let data = sample(len, len as u32);
            let Some(folded) = crc32_hw(!0, &data) else {
                return;
            };
            let folded = !folded;
            assert_eq!(folded, crc32_bitwise(&data), "len {len}");
        }
    }

    #[test]
    fn sha256_blocks_matches_the_known_digest() {
        // "abc" padded to one block
        let mut block = [0u8; 64];
        block[..3].copy_from_slice(b"abc");
        block[3] = 0x80;
        block[63] = 24;
        let mut state = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        if !sha256_hw(&mut state, &block) {
            return;
        }
        assert_eq!(
            state,
            [
                0xba7816bf, 0x8f01cfea, 0x414140de, 0x5dae2223, 0xb00361a3, 0x96177a9c, 0xb410ff61,
                0xf20015ad
            ]
        );

        // a 56-byte message, which pads to two blocks
        let msg = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        let mut blocks = [0u8; 128];
        blocks[..56].copy_from_slice(msg);
        blocks[56] = 0x80;
        blocks[126] = 0x01;
        blocks[127] = 0xc0;
        let mut state = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        assert!(sha256_hw(&mut state, &blocks));
        assert_eq!(
            state,
            [
                0x248d6a61, 0xd20638b8, 0xe5c02693, 0x0c3e6039, 0xa33ce459, 0x64ff2167, 0xf6ecedd4,
                0x19db06c1
            ]
        );
    }
}
