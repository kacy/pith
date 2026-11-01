//! The zstd binding: the bounded decompress on arbitrary frames with an
//! arbitrary cap, and compress then decompress at caps on either side of
//! the true size.
#![no_main]

use pith_runtime::zstd_codec::{pith_zstd_compress, pith_zstd_decompress};
use pith_runtime_fuzz::{fuzz_target, take_bytes, Input, PBytes};

/// The largest cap the harness hands the decoder. A larger one lets a
/// small frame claim gigabytes, which the cap exists to let a program
/// refuse; the harness tests that refusal, not the allocator.
const MAX_CAP: i64 = 1 << 24;

fuzz_target!(|data| {
    let mut input = Input::new(data);
    let op = input.byte();
    unsafe {
        if op & 1 == 0 {
            let mut cap = input.int();
            if cap > MAX_CAP {
                cap = MAX_CAP;
            }
            let frame = PBytes::new(input.rest());
            if let Some(out) = take_bytes(pith_zstd_decompress(frame.0, cap)) {
                assert!(cap > 0 && out.len() as i64 <= cap);
            }
        } else {
            let level = input.int();
            let delta = input.byte() as i64 % 5 - 2;
            let raw = input.rest();
            let plain = PBytes::new(raw);
            let packed = pith_zstd_compress(plain.0, level);
            assert_ne!(packed, 0);
            let packed = PBytes(packed);
            let cap = raw.len() as i64 + delta;
            let out = take_bytes(pith_zstd_decompress(packed.0, cap));
            if cap > 0 && cap >= raw.len() as i64 {
                assert_eq!(out.as_deref(), Some(raw));
            } else {
                assert!(out.is_none(), "a cap below the size refuses the frame");
            }
        }
    }
});
