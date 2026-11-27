//! Hardware block loops for the checksum and digest code in std.
//!
//! std.checksum and std.hash are written in Pith: padding, buffering, the
//! tail and a portable block loop all live there. On CPUs with the right
//! instructions, the bulk of the input goes through one of the functions
//! below instead. Each takes a whole run of blocks per call and reports
//! when it cannot help, so the Pith loop then does the work. Cranelift has
//! no carry-less multiply, CRC or SHA instructions, so Pith code cannot emit
//! them itself.

use crate::bytes::with_bytes;
use crate::collections::list::with_primitive_words_mut;

#[inline(always)]
fn crc32_fold_slice(crc: i64, bytes: &[u8], len: i64) -> i64 {
    if len < 64 || len % 16 != 0 || len as u64 > bytes.len() as u64 {
        return -1;
    }
    match crc32_hw(crc as u32, &bytes[..len as usize]) {
        Some(folded) => folded as i64,
        None => -1,
    }
}

#[inline(always)]
fn sha256_blocks_slice(words: &mut [i64], bytes: &[u8], blocks: i64) -> i64 {
    if words.len() != 8 || blocks < 0 || (blocks as u64).saturating_mul(64) > bytes.len() as u64 {
        return 0;
    }
    let mut h = [0u32; 8];
    for (out, &word) in h.iter_mut().zip(words.iter()) {
        *out = word as u32;
    }
    if !sha256_hw(&mut h, &bytes[..blocks as usize * 64]) {
        return 0;
    }
    for (word, &out) in words.iter_mut().zip(h.iter()) {
        *word = out as i64;
    }
    1
}

#[inline(always)]
fn sha512_blocks_slice(words: &mut [i64], bytes: &[u8], blocks: i64) -> i64 {
    if words.len() != 8 || blocks < 0 || (blocks as u64).saturating_mul(128) > bytes.len() as u64 {
        return 0;
    }
    let mut h = [0u64; 8];
    for (out, &word) in h.iter_mut().zip(words.iter()) {
        *out = word as u64;
    }
    sha512_blocks_portable(&mut h, &bytes[..blocks as usize * 128]);
    for (word, &out) in words.iter_mut().zip(h.iter()) {
        *word = out as i64;
    }
    1
}

/// Fold `len` bytes from the start of `data` into the CRC-32 (IEEE)
/// register `crc`, as std.checksum keeps it: inverted, before the final
/// xor. `len` must be a multiple of 16 and at least 64. Returns the new
/// register, or -1 when the CPU lacks the instructions or an argument is
/// out of range, and the caller then runs its own loop.
#[no_mangle]
pub unsafe extern "C" fn pith_crc32_fold(crc: i64, data: i64, len: i64) -> i64 {
    // SAFETY: `data` is validated by `with_bytes` before borrowing its slice.
    unsafe { with_bytes(data, |bytes| crc32_fold_slice(crc, bytes, len)) }.unwrap_or(-1)
}

/// Run the SHA-256 compression function over `blocks` 64-byte blocks from
/// the start of `data`. `state` is a list of eight Ints holding the 32-bit
/// chaining words, updated in place. Returns 1 when it did, or 0 when the
/// CPU lacks the instructions or an argument is out of range, and the
/// caller then runs its own loop with the state unchanged.
#[no_mangle]
pub unsafe extern "C" fn pith_sha256_blocks(state: i64, data: i64, blocks: i64) -> i64 {
    // SAFETY: `data` (`PithBytes`) and `state` (`ListImpl`) are distinct handle types
    // validated before borrowing their slices.
    unsafe {
        with_bytes(data, |bytes| {
            with_primitive_words_mut(state, |words| sha256_blocks_slice(words, bytes, blocks))
        })
    }
    .flatten()
    .unwrap_or(0)
}

/// Run the SHA-512 compression function (shared by SHA-512 and SHA-384) over
/// `blocks` 128-byte blocks from the start of `data`. `state` is a list of
/// eight Ints holding the 64-bit chaining words, updated in place. Returns 1
/// when it did, or 0 when an argument is out of range, leaving `state`
/// untouched so the caller can fall back.
#[no_mangle]
pub unsafe extern "C" fn pith_sha512_blocks(state: i64, data: i64, blocks: i64) -> i64 {
    // SAFETY: `data` (`PithBytes`) and `state` (`ListImpl`) are distinct handle types
    // validated before borrowing their slices.
    unsafe {
        with_bytes(data, |bytes| {
            with_primitive_words_mut(state, |words| sha512_blocks_slice(words, bytes, blocks))
        })
    }
    .flatten()
    .unwrap_or(0)
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

/// FIPS 180-4 §4.2.3 SHA-512 round constants.
const K512: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
    0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
    0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694,
    0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70,
    0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30,
    0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b,
    0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178,
    0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

/// Native Rust SHA-512 block compression loop (also used by SHA-384).
/// Compiled by LLVM with full 64-bit rotate codegen and zero per-word bounds
/// checks, keeping the 16-word rolling schedule and 8 state words in registers.
fn sha512_blocks_portable(state: &mut [u64; 8], data: &[u8]) {
    macro_rules! sha512_round {
        ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident, $k:expr, $w:expr) => {{
            let s1 = $e.rotate_right(14) ^ $e.rotate_right(18) ^ $e.rotate_right(41);
            let ch = $g ^ ($e & ($f ^ $g));
            let t1 = $h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add($k)
                .wrapping_add($w);
            let s0 = $a.rotate_right(28) ^ $a.rotate_right(34) ^ $a.rotate_right(39);
            let maj = ($a & $b) | ($c & ($a | $b));
            $d = $d.wrapping_add(t1);
            $h = t1.wrapping_add(s0.wrapping_add(maj));
        }};
    }
    macro_rules! sha512_schedule {
        ($w:ident, $i:expr) => {{
            let w1 = $w[($i + 1) & 15];
            let w9 = $w[($i + 9) & 15];
            let w14 = $w[($i + 14) & 15];
            let s0 = w1.rotate_right(1) ^ w1.rotate_right(8) ^ (w1 >> 7);
            let s1 = w14.rotate_right(19) ^ w14.rotate_right(61) ^ (w14 >> 6);
            $w[$i] = $w[$i].wrapping_add(s0).wrapping_add(w9).wrapping_add(s1);
            $w[$i]
        }};
    }

    for block in data.chunks_exact(128) {
        let mut w = [0u64; 16];
        for (slot, chunk) in w.iter_mut().zip(block.chunks_exact(8)) {
            *slot = u64::from_be_bytes([
                chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
            ]);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;

        sha512_round!(a, b, c, d, e, f, g, h, K512[0], w[0]);
        sha512_round!(h, a, b, c, d, e, f, g, K512[1], w[1]);
        sha512_round!(g, h, a, b, c, d, e, f, K512[2], w[2]);
        sha512_round!(f, g, h, a, b, c, d, e, K512[3], w[3]);
        sha512_round!(e, f, g, h, a, b, c, d, K512[4], w[4]);
        sha512_round!(d, e, f, g, h, a, b, c, K512[5], w[5]);
        sha512_round!(c, d, e, f, g, h, a, b, K512[6], w[6]);
        sha512_round!(b, c, d, e, f, g, h, a, K512[7], w[7]);
        sha512_round!(a, b, c, d, e, f, g, h, K512[8], w[8]);
        sha512_round!(h, a, b, c, d, e, f, g, K512[9], w[9]);
        sha512_round!(g, h, a, b, c, d, e, f, K512[10], w[10]);
        sha512_round!(f, g, h, a, b, c, d, e, K512[11], w[11]);
        sha512_round!(e, f, g, h, a, b, c, d, K512[12], w[12]);
        sha512_round!(d, e, f, g, h, a, b, c, K512[13], w[13]);
        sha512_round!(c, d, e, f, g, h, a, b, K512[14], w[14]);
        sha512_round!(b, c, d, e, f, g, h, a, K512[15], w[15]);

        let mut base = 16;
        while base < 80 {
            let k = &K512[base..base + 16];
            let w0 = sha512_schedule!(w, 0);
            sha512_round!(a, b, c, d, e, f, g, h, k[0], w0);
            let w1 = sha512_schedule!(w, 1);
            sha512_round!(h, a, b, c, d, e, f, g, k[1], w1);
            let w2 = sha512_schedule!(w, 2);
            sha512_round!(g, h, a, b, c, d, e, f, k[2], w2);
            let w3 = sha512_schedule!(w, 3);
            sha512_round!(f, g, h, a, b, c, d, e, k[3], w3);
            let w4 = sha512_schedule!(w, 4);
            sha512_round!(e, f, g, h, a, b, c, d, k[4], w4);
            let w5 = sha512_schedule!(w, 5);
            sha512_round!(d, e, f, g, h, a, b, c, k[5], w5);
            let w6 = sha512_schedule!(w, 6);
            sha512_round!(c, d, e, f, g, h, a, b, k[6], w6);
            let w7 = sha512_schedule!(w, 7);
            sha512_round!(b, c, d, e, f, g, h, a, k[7], w7);
            let w8 = sha512_schedule!(w, 8);
            sha512_round!(a, b, c, d, e, f, g, h, k[8], w8);
            let w9 = sha512_schedule!(w, 9);
            sha512_round!(h, a, b, c, d, e, f, g, k[9], w9);
            let w10 = sha512_schedule!(w, 10);
            sha512_round!(g, h, a, b, c, d, e, f, k[10], w10);
            let w11 = sha512_schedule!(w, 11);
            sha512_round!(f, g, h, a, b, c, d, e, k[11], w11);
            let w12 = sha512_schedule!(w, 12);
            sha512_round!(e, f, g, h, a, b, c, d, k[12], w12);
            let w13 = sha512_schedule!(w, 13);
            sha512_round!(d, e, f, g, h, a, b, c, k[13], w13);
            let w14 = sha512_schedule!(w, 14);
            sha512_round!(c, d, e, f, g, h, a, b, k[14], w14);
            let w15 = sha512_schedule!(w, 15);
            sha512_round!(b, c, d, e, f, g, h, a, k[15], w15);
            base += 16;
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }
}

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
    fn crc32_fold_impl(crc: u32, data: &[u8]) -> u32 {
        let mut chunks = data.chunks_exact(16).map(|c| {
            // SAFETY: `c` is a 16-byte slice; unaligned load reads within it.
            unsafe { _mm_loadu_si128(c.as_ptr() as *const __m128i) }
        });
        let zero = _mm_setzero_si128();
        let mut next = || chunks.next().unwrap_or(zero);
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
    fn fold(a: __m128i, b: __m128i, keys: __m128i) -> __m128i {
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
    fn sha256_blocks_impl(state: &mut [u32; 8], data: &[u8]) {
        // big-endian words: reverse the bytes of each 32-bit lane
        let swap = _mm_set_epi64x(0x0c0d_0e0f_0809_0a0b, 0x0405_0607_0001_0203);
        // SAFETY: `state` has 8 `u32` elements (two 16-byte unaligned loads).
        let (dcba, hgfe) = unsafe {
            (
                _mm_loadu_si128(state.as_ptr() as *const __m128i),
                _mm_loadu_si128(state.as_ptr().add(4) as *const __m128i),
            )
        };

        // sha256rnds2 wants the state as (a, b, e, f) and (c, d, g, h)
        let cdab = _mm_shuffle_epi32(dcba, 0xb1);
        let efgh = _mm_shuffle_epi32(hgfe, 0x1b);
        let mut abef = _mm_alignr_epi8(cdab, efgh, 8);
        let mut cdgh = _mm_blend_epi16(efgh, cdab, 0xf0);

        for block in data.chunks_exact(64) {
            let (abef_in, cdgh_in) = (abef, cdgh);
            // SAFETY: `block` is 64 bytes (four 16-byte unaligned loads).
            let raw = unsafe {
                let at = block.as_ptr() as *const __m128i;
                [
                    _mm_loadu_si128(at),
                    _mm_loadu_si128(at.add(1)),
                    _mm_loadu_si128(at.add(2)),
                    _mm_loadu_si128(at.add(3)),
                ]
            };
            let mut w = [
                _mm_shuffle_epi8(raw[0], swap),
                _mm_shuffle_epi8(raw[1], swap),
                _mm_shuffle_epi8(raw[2], swap),
                _mm_shuffle_epi8(raw[3], swap),
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
                // SAFETY: `K256` has 64 `u32` elements and `i * 4 + 4 <= 64`.
                let k = unsafe { _mm_loadu_si128(K256.as_ptr().add(i * 4) as *const __m128i) };
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
        // SAFETY: `state` has 8 `u32` elements (two 16-byte unaligned stores).
        unsafe {
            _mm_storeu_si128(state.as_mut_ptr() as *mut __m128i, dcba);
            _mm_storeu_si128(state.as_mut_ptr().add(4) as *mut __m128i, hgef);
        }
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
    fn crc32_fold_impl(mut crc: u32, data: &[u8]) -> u32 {
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
        // SAFETY: the caller checked the CPU feature (NEON is baseline on aarch64).
        unsafe { sha256_blocks_impl(state, data) }
    }

    #[target_feature(enable = "neon,sha2")]
    fn sha256_blocks_impl(state: &mut [u32; 8], data: &[u8]) {
        // SAFETY: `state` has 8 `u32` elements (two 16-byte loads).
        let (mut abcd, mut efgh) = unsafe {
            (
                vld1q_u32(state.as_ptr()),
                vld1q_u32(state.as_ptr().add(4)),
            )
        };
        for block in data.chunks_exact(64) {
            let (abcd_in, efgh_in) = (abcd, efgh);
            // SAFETY: `block` is 64 bytes (four 16-byte loads).
            let raw = unsafe {
                let at = block.as_ptr();
                [
                    vld1q_u8(at),
                    vld1q_u8(at.add(16)),
                    vld1q_u8(at.add(32)),
                    vld1q_u8(at.add(48)),
                ]
            };
            // big-endian words: reverse the bytes of each 32-bit lane
            let mut w = [
                vreinterpretq_u32_u8(vrev32q_u8(raw[0])),
                vreinterpretq_u32_u8(vrev32q_u8(raw[1])),
                vreinterpretq_u32_u8(vrev32q_u8(raw[2])),
                vreinterpretq_u32_u8(vrev32q_u8(raw[3])),
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
                // SAFETY: `K256` has 64 `u32` elements and `i * 4 + 4 <= 64`.
                let k = unsafe { vld1q_u32(K256.as_ptr().add(i * 4)) };
                let wk = vaddq_u32(w[i % 4], k);
                let abcd_before = abcd;
                abcd = vsha256hq_u32(abcd, efgh, wk);
                efgh = vsha256h2q_u32(efgh, abcd_before, wk);
            }
            abcd = vaddq_u32(abcd, abcd_in);
            efgh = vaddq_u32(efgh, efgh_in);
        }
        // SAFETY: `state` has 8 `u32` elements (two 16-byte stores).
        unsafe {
            vst1q_u32(state.as_mut_ptr(), abcd);
            vst1q_u32(state.as_mut_ptr().add(4), efgh);
        }
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

    #[test]
    fn sha512_and_sha384_blocks_match_fips_180_4_vectors() {
        // FIPS 180-4 SHA-512("abc"), one 128-byte block
        let mut block = [0u8; 128];
        block[..3].copy_from_slice(b"abc");
        block[3] = 0x80;
        block[127] = 24;
        let mut state512 = [
            0x6a09e667f3bcc908u64,
            0xbb67ae8584caa73b,
            0x3c6ef372fe94f82b,
            0xa54ff53a5f1d36f1,
            0x510e527fade682d1,
            0x9b05688c2b3e6c1f,
            0x1f83d9abfb41bd6b,
            0x5be0cd19137e2179,
        ];
        sha512_blocks_portable(&mut state512, &block);
        assert_eq!(
            state512,
            [
                0xddaf35a193617aba,
                0xcc417349ae204131,
                0x12e6fa4e89a97ea2,
                0x0a9eeee64b55d39a,
                0x2192992a274fc1a8,
                0x36ba3c23a3feebbd,
                0x454d4423643ce80e,
                0x2a9ac94fa54ca49f,
            ]
        );

        // FIPS 180-4 SHA-384("abc"), same compression with SHA-384 IV
        let mut state384 = [
            0xcbbb9d5dc1059ed8u64,
            0x629a292a367cd507,
            0x9159015a3070dd17,
            0x152fecd8f70e5939,
            0x67332667ffc00b31,
            0x8eb44a8768581511,
            0xdb0c2e0d64f98fa7,
            0x47b5481dbefa4fa4,
        ];
        sha512_blocks_portable(&mut state384, &block);
        assert_eq!(
            &state384[..6],
            &[
                0xcb00753f45a35e8b,
                0xb5a03d699ac65007,
                0x272c32ab0eded163,
                0x1a8b605a43ff5bed,
                0x8086072ba1e7cc23,
                0x58baeca134c825a7,
            ]
        );

        // FIPS 180-4 112-byte two-block message for SHA-512
        let msg = b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu";
        let mut blocks = [0u8; 256];
        blocks[..112].copy_from_slice(msg);
        blocks[112] = 0x80;
        blocks[254] = 0x03;
        blocks[255] = 0x80; // 112 * 8 = 896 = 0x0380
        let mut words: [i64; 8] = [
            0x6a09e667f3bcc908u64 as i64,
            0xbb67ae8584caa73bu64 as i64,
            0x3c6ef372fe94f82bu64 as i64,
            0xa54ff53a5f1d36f1u64 as i64,
            0x510e527fade682d1u64 as i64,
            0x9b05688c2b3e6c1fu64 as i64,
            0x1f83d9abfb41bd6bu64 as i64,
            0x5be0cd19137e2179u64 as i64,
        ];
        assert_eq!(sha512_blocks_slice(&mut words, &blocks, 2), 1);
        let got: Vec<u64> = words.iter().map(|&w| w as u64).collect();
        assert_eq!(
            got,
            vec![
                0x8e959b75dae313da,
                0x8cf4f72814fc143f,
                0x8f7779c6eb9f7fa1,
                0x7299aeadb6889018,
                0x501d289e4900f7e4,
                0x331b99dec4b5433a,
                0xc7d329eeb6dd2654,
                0x5e96e55b874be909,
            ]
        );

        // Out-of-range arguments fail closed and leave words untouched
        let snapshot = words;
        assert_eq!(sha512_blocks_slice(&mut words, &blocks, -1), 0);
        assert_eq!(sha512_blocks_slice(&mut words, &blocks, 3), 0);
        assert_eq!(sha512_blocks_slice(&mut words[..7], &blocks, 1), 0);
        assert_eq!(words, snapshot);
    }
}
