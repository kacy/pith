//! zstd compression, backed by the reference implementation (the `zstd`
//! crate over libzstd). compression is the one place the repo links a
//! kernel it could in principle write: the reference encoder IS the interop
//! target, and matching its ratios and its edge cases by hand buys nothing
//! a protocol needs. the pith-facing surface (std/compress/zstd.pith)
//! mirrors gzip's, including the bounded decompress.

use crate::bytes::{pith_bytes_from_vec, pith_bytes_ref};
use std::io::Read;

unsafe fn bytes_slice<'a>(handle: i64) -> Option<&'a [u8]> {
    Some(pith_bytes_ref(handle)?.data.as_slice())
}

/// Compress `data` at `level` (1..=22; out-of-range clamps to the default 3).
/// Returns a bytes handle, or 0 on an invalid input handle.
#[no_mangle]
pub unsafe extern "C" fn pith_zstd_compress(data: i64, level: i64) -> i64 {
    let Some(input) = bytes_slice(data) else {
        return 0;
    };
    let level = if (1..=22).contains(&level) { level as i32 } else { 3 };
    match zstd::bulk::compress(input, level) {
        Ok(out) => pith_bytes_from_vec(out),
        Err(_) => 0,
    }
}

/// Decompress a zstd frame, refusing to produce more than `max_out` bytes.
/// The cap is enforced during streaming decode, not after: a decompression
/// bomb fails at the limit without the memory ever being allocated. Returns
/// a bytes handle, or 0 on a malformed frame, an invalid handle, or a
/// too-large output.
#[no_mangle]
pub unsafe extern "C" fn pith_zstd_decompress(data: i64, max_out: i64) -> i64 {
    let Some(input) = bytes_slice(data) else {
        return 0;
    };
    if max_out <= 0 {
        return 0;
    }
    let limit = max_out as usize;
    let Ok(decoder) = zstd::stream::read::Decoder::new(input) else {
        return 0;
    };
    // read up to limit+1: seeing the extra byte is how "exactly at the cap"
    // is told apart from "past it".
    let mut out = Vec::new();
    let mut bounded = decoder.take(limit as u64 + 1);
    if bounded.read_to_end(&mut out).is_err() {
        return 0;
    }
    if out.len() > limit {
        return 0;
    }
    pith_bytes_from_vec(out)
}
