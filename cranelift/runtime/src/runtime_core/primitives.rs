//! Print/exit, bitwise, checked/wrapping integer casts, math, float bitcasts,
//! and container display helpers.

use super::cstring::{format_float, pith_copy_bytes_to_cstring, preview_lossy};

#[no_mangle]
pub unsafe extern "C" fn pith_print(s: *const i8) {
    // SAFETY: Caller guarantees `s` is null or a valid NUL-terminated C string.
    unsafe { pith_print_cstr(s) };
}
fn print_utf8_bytes(bytes: &[u8]) {
    if let Ok(str_ref) = std::str::from_utf8(bytes) {
        println!("{}", str_ref);
    } else {
        println!();
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_print_cstr(ptr: *const i8) {
    // SAFETY: Caller guarantees `ptr` is null or a valid NUL-terminated C string.
    if unsafe { crate::ffi_util::with_cstr_bytes(ptr, print_utf8_bytes) }.is_none() {
        println!();
    }
}

fn eprint_utf8_bytes(bytes: &[u8]) {
    if let Ok(str_ref) = std::str::from_utf8(bytes) {
        eprintln!("{}", str_ref);
    } else {
        eprintln!();
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_print_err(ptr: *const i8) {
    // SAFETY: Caller guarantees `ptr` is null or a valid NUL-terminated C string.
    if unsafe { crate::ffi_util::with_cstr_bytes(ptr, eprint_utf8_bytes) }.is_none() {
        eprintln!();
    }
}

fn report_main_error_msg(msg: &str) {
    eprintln!("error: {}", msg);
}

/// Report an error escaping `main` and exit nonzero. The emitter calls this on
/// main's error-return paths, because the entry glue pins main's process exit
/// code to 0. Without this, a program that failed out of main looked like it
/// succeeded. `is_string` says whether `err` is a string handle; a typed error
/// gets a generic line rather than a read through a non-string pointer.
#[no_mangle]
pub unsafe extern "C" fn pith_main_error_exit(err: i64, is_string: i64) -> i64 {
    let printed = if is_string != 0 && err != 0 {
        // SAFETY: When `is_string != 0` and `err != 0`, emitter passes a valid C string handle.
        unsafe { crate::ffi_util::with_cstr_str(err as *const i8, report_main_error_msg) }.is_some()
    } else {
        false
    };
    if !printed {
        eprintln!("error: main returned an error");
    }
    // panic-guard: main returned an error; this is the process exit status for it.
    std::process::exit(1);
}
#[no_mangle]
pub extern "C" fn pith_bit_and(a: i64, b: i64) -> i64 {
    a & b
}

#[no_mangle]
pub extern "C" fn pith_bit_or(a: i64, b: i64) -> i64 {
    a | b
}

#[no_mangle]
pub extern "C" fn pith_bit_xor(a: i64, b: i64) -> i64 {
    a ^ b
}

#[no_mangle]
pub extern "C" fn pith_bit_not(a: i64) -> i64 {
    !a
}

// the error an integer that does not fit raises, the same one the code
// generator's overflow trap reports (overflow_trap.rs).
fn integer_overflow() -> ! {
    crate::failure::fail("integer overflow".to_string())
}

// a shift count outside 0..63 stops the program rather than being taken mod
// 64. codegen lowers calls to these to the same check and one shift
// instruction; these are the fallback with the same result.
#[no_mangle]
pub extern "C" fn pith_bit_shl(a: i64, b: i64) -> i64 {
    if !(0..64).contains(&b) {
        integer_overflow();
    }
    a << b
}

#[no_mangle]
pub extern "C" fn pith_bit_shr(a: i64, b: i64) -> i64 {
    if !(0..64).contains(&b) {
        integer_overflow();
    }
    ((a as u64) >> b) as i64
}

// the rotates work on the low 32 or all 64 bits of `a`, with the amount taken
// mod the width; a 32-bit result is zero-extended. codegen lowers calls to
// these to one rotate instruction, and these are the fallback with the same
// result.
#[no_mangle]
pub extern "C" fn pith_bit_rotl32(a: i64, b: i64) -> i64 {
    (a as u32).rotate_left(b as u32 & 31) as i64
}

#[no_mangle]
pub extern "C" fn pith_bit_rotr32(a: i64, b: i64) -> i64 {
    (a as u32).rotate_right(b as u32 & 31) as i64
}

#[no_mangle]
pub extern "C" fn pith_bit_rotl64(a: i64, b: i64) -> i64 {
    (a as u64).rotate_left(b as u32 & 63) as i64
}

#[no_mangle]
pub extern "C" fn pith_bit_rotr64(a: i64, b: i64) -> i64 {
    (a as u64).rotate_right(b as u32 & 63) as i64
}

// the conversions from Int. `UInt8(x)` and the rest stop the program when x
// does not fit the target type, and the `wrapping_*` forms keep the low bits
// of the two's complement value. a narrow value is held sign or zero extended
// to 64 bits and an unsigned word as the bits of an i64. codegen inlines all
// of these, and these are the fallback with the same result.
fn fit(n: i64, lo: i64, hi: i64) -> i64 {
    if n < lo || n > hi {
        integer_overflow();
    }
    n
}

#[no_mangle]
pub extern "C" fn pith_uint(n: i64) -> i64 {
    fit(n, 0, i64::MAX)
}

#[no_mangle]
pub extern "C" fn pith_int8(n: i64) -> i64 {
    fit(n, i8::MIN as i64, i8::MAX as i64)
}

#[no_mangle]
pub extern "C" fn pith_int16(n: i64) -> i64 {
    fit(n, i16::MIN as i64, i16::MAX as i64)
}

#[no_mangle]
pub extern "C" fn pith_int32(n: i64) -> i64 {
    fit(n, i32::MIN as i64, i32::MAX as i64)
}

#[no_mangle]
pub extern "C" fn pith_int64(n: i64) -> i64 {
    n
}

#[no_mangle]
pub extern "C" fn pith_uint8(n: i64) -> i64 {
    fit(n, 0, u8::MAX as i64)
}

#[no_mangle]
pub extern "C" fn pith_uint16(n: i64) -> i64 {
    fit(n, 0, u16::MAX as i64)
}

#[no_mangle]
pub extern "C" fn pith_uint32(n: i64) -> i64 {
    fit(n, 0, u32::MAX as i64)
}

#[no_mangle]
pub extern "C" fn pith_uint64(n: i64) -> i64 {
    fit(n, 0, i64::MAX)
}

#[no_mangle]
pub extern "C" fn pith_wrapping_int8(n: i64) -> i64 {
    (n as i8) as i64
}

#[no_mangle]
pub extern "C" fn pith_wrapping_int16(n: i64) -> i64 {
    (n as i16) as i64
}

#[no_mangle]
pub extern "C" fn pith_wrapping_int32(n: i64) -> i64 {
    (n as i32) as i64
}

#[no_mangle]
pub extern "C" fn pith_wrapping_uint8(n: i64) -> i64 {
    (n as u8) as i64
}

#[no_mangle]
pub extern "C" fn pith_wrapping_uint16(n: i64) -> i64 {
    (n as u16) as i64
}

#[no_mangle]
pub extern "C" fn pith_wrapping_uint32(n: i64) -> i64 {
    (n as u32) as i64
}

#[no_mangle]
pub extern "C" fn pith_wrapping_uint64(n: i64) -> i64 {
    n
}

#[no_mangle]
pub extern "C" fn pith_abs(n: i64) -> i64 {
    match n.checked_abs() {
        Some(v) => v,
        None => integer_overflow(),
    }
}

#[no_mangle]
pub extern "C" fn pith_min(a: i64, b: i64) -> i64 {
    if a < b {
        a
    } else {
        b
    }
}

#[no_mangle]
pub extern "C" fn pith_max(a: i64, b: i64) -> i64 {
    if a > b {
        a
    } else {
        b
    }
}

#[no_mangle]
pub extern "C" fn pith_clamp(n: i64, min: i64, max: i64) -> i64 {
    if n < min {
        min
    } else if n > max {
        max
    } else {
        n
    }
}

#[no_mangle]
pub extern "C" fn pith_pow(a: f64, b: f64) -> f64 {
    a.powf(b)
}

#[no_mangle]
pub extern "C" fn pith_sqrt(n: f64) -> f64 {
    n.sqrt()
}

#[no_mangle]
pub extern "C" fn pith_floor(n: f64) -> f64 {
    n.floor()
}

#[no_mangle]
pub extern "C" fn pith_ceil(n: f64) -> f64 {
    n.ceil()
}

#[no_mangle]
pub extern "C" fn pith_round(n: f64) -> f64 {
    n.round()
}

#[no_mangle]
pub extern "C" fn pith_sin(n: f64) -> f64 {
    n.sin()
}

#[no_mangle]
pub extern "C" fn pith_cos(n: f64) -> f64 {
    n.cos()
}

#[no_mangle]
pub extern "C" fn pith_tan(n: f64) -> f64 {
    n.tan()
}

#[no_mangle]
pub extern "C" fn pith_asin(n: f64) -> f64 {
    n.asin()
}

#[no_mangle]
pub extern "C" fn pith_acos(n: f64) -> f64 {
    n.acos()
}

#[no_mangle]
pub extern "C" fn pith_atan(n: f64) -> f64 {
    n.atan()
}

#[no_mangle]
pub extern "C" fn pith_atan2(y: f64, x: f64) -> f64 {
    y.atan2(x)
}
// --- container display -------------------------------------------------
//
// interpolating a list or map renders it through these. the emitter passes
// an element-kind code because the runtime cannot tell an int list from a
// float list (both store 8-byte values): 0=int, 1=float, 2=bool, 3=string.

pub(crate) unsafe fn display_value_for_map(out: &mut String, raw: i64, kind: i64) {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { display_value(out, raw, kind) }
}

unsafe fn display_value(out: &mut String, raw: i64, kind: i64) {
    match kind {
        1 => out.push_str(&format_float(f64::from_bits(raw as u64))),
        2 => out.push_str(if raw != 0 { "true" } else { "false" }),
        // SAFETY: Caller guarantees `raw` is a valid C string handle when `kind == 3`.
        3 => out.push_str(&unsafe { cstr_to_display(raw as *const i8) }),
        _ => out.push_str(&raw.to_string()),
    }
}

pub(super) unsafe fn cstr_to_display(ptr: *const i8) -> String {
    // SAFETY: Preconditions forwarded from caller.
    unsafe { crate::ffi_util::with_cstr_bytes(ptr, preview_lossy) }.unwrap_or_default()
}

fn collect_list_values(list: &crate::collections::list::ListImpl) -> Vec<i64> {
    let mut values = Vec::with_capacity(list.len());
    for i in 0..list.len() {
        if let Some(raw) = list.get_value(i) {
            values.push(raw);
        }
    }
    values
}

/// Render a list as "[a, b, c]" for interpolation. `sorted` is set when the
/// source is a set: hash iteration order is not part of the language, so a
/// displayed set sorts its elements.
#[no_mangle]
pub unsafe extern "C" fn pith_display_list(handle: i64, elem_kind: i64, sorted: i64) -> *mut i8 {
    // SAFETY: `handle` is validated by `with_list_ref_handle` before reading elements.
    let mut values = unsafe { crate::collections::list::with_list_ref_handle(handle, collect_list_values) }
        .unwrap_or_default();
    if sorted != 0 {
        match elem_kind {
            1 => values.sort_by(|a, b| {
                f64::from_bits(*a as u64)
                    .partial_cmp(&f64::from_bits(*b as u64))
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            3 => values.sort_by_key(|v| {
                // SAFETY: Caller guarantees elements are valid C strings when `elem_kind == 3`.
                unsafe { cstr_to_display(*v as *const i8) }
            }),
            _ => values.sort(),
        }
    }
    let mut out = String::from("[");
    for (i, raw) in values.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        // SAFETY: Caller guarantees elements match `elem_kind`.
        unsafe { display_value(&mut out, *raw, elem_kind) };
    }
    out.push(']');
    pith_copy_bytes_to_cstring(out.as_bytes())
}

pub use pith_ceil as pith_math_ceil;
pub use pith_floor as pith_math_floor;
pub use pith_pow as pith_math_pow;
pub use pith_round as pith_math_round;
pub use pith_sqrt as pith_math_sqrt;
#[no_mangle]
pub extern "C" fn pith_int_to_float(n: i64) -> f64 {
    n as f64
}

#[no_mangle]
pub extern "C" fn pith_float_to_int(n: f64) -> i64 {
    // truncates toward zero. NaN and a value outside Int's range stop the
    // program instead of turning into 0 or the nearest end of the range.
    // -2^63 converts exactly, and 2^63 is the first value past the top.
    if n.is_nan() {
        crate::failure::fail("NaN converted to an integer".to_string());
    }
    if !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&n) {
        integer_overflow();
    }
    n as i64
}

// Reinterpret the raw bits, not the numeric value: the bit pattern of a float
// read as an integer and vice versa. `to_float`/`to_int` above convert values
// (5 -> 5.0); these preserve bits, which is what binary wire formats need.
#[no_mangle]
pub extern "C" fn pith_float_from_bits(bits: i64) -> f64 {
    f64::from_bits(bits as u64)
}

#[no_mangle]
pub extern "C" fn pith_float_to_bits(value: f64) -> i64 {
    value.to_bits() as i64
}
