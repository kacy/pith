//! process-level builtins: exit, sleep, clocks, reading a stdin line, `exec`,
//! the shared random number generator, and float formatting.

use crate::ffi_util::{with_cstr_bytes, with_cstr_str};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

/// the state of the process-wide LCG behind `pith_random_float`, reseeded by
/// `pith_random_seed`.
static RANDOM_SEED: AtomicU64 = AtomicU64::new(123456789);

/// Process-start reference point for the monotonic clock.
///
/// `Instant` is guaranteed monotonic (never runs backwards), so measuring
/// against a fixed start gives us a stable nanosecond timeline for the life
/// of the process.
static MONO_START: OnceLock<Instant> = OnceLock::new();

/// Exit the program with given status code
#[no_mangle]
pub extern "C" fn pith_exit(code: i64) {
    // panic-guard: this is pith's own exit builtin, not a trap.
    std::process::exit(code as i32);
}

/// Sleep for given number of milliseconds.
///
/// from inside a green task this parks the *task* on the reactor's timer heap
/// and gives the worker back: a sleeping task must not stall every other task
/// queued on its worker (`select`'s idle probe sleeps in a loop, so two idle
/// selects blocking their workers could occupy a whole two-worker pool).
/// `current_task()` is `None` on the main thread, on any plain OS thread, and
/// on the whole os-thread backend, and there blocking the thread is exactly
/// right. on non-linux there is no reactor and `netpoll`'s fallback blocks the
/// worker, consistent with how socket waits degrade there.
#[no_mangle]
pub extern "C" fn pith_sleep(ms: i64) {
    // a negative duration is not a very long one: `ms as u64` turns -5 into
    // about 584 million years, which is indistinguishable from a hang. clamp
    // first, so both paths below see a duration that means what it says.
    let ms = ms.max(0);
    if let Some(task) = crate::concurrency::green::current_task() {
        crate::netpoll::sleep_task(ms, task);
        return;
    }
    // the blocking arm sits in the kernel for the whole duration touching no
    // heap handle, so it is a native window for the cycle collector; the
    // bracket's exit re-checks the stop request before pith code resumes.
    let _native = crate::cycle::native_bracket();
    std::thread::sleep(std::time::Duration::from_millis(ms as u64));
}

/// Get current time in milliseconds since epoch
#[no_mangle]
pub extern "C" fn pith_time() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Get a monotonic timestamp in nanoseconds.
///
/// Unlike `pith_time`, this is not wall-clock time: it only ever increases and
/// is unaffected by clock adjustments, so it is safe for measuring durations.
/// The value is relative to a fixed process-start point, not any epoch.
#[no_mangle]
pub extern "C" fn pith_time_nanos() -> i64 {
    let start = MONO_START.get_or_init(Instant::now);
    start.elapsed().as_nanos() as i64
}

/// Read a line from stdin
/// Returns C string. Caller must free with pith_free.
#[no_mangle]
pub unsafe extern "C" fn pith_input() -> *mut i8 {
    use std::io::{self, BufRead};

    let stdin = io::stdin();
    let mut line = String::new();
    let read_ok = {
        let _native = crate::cycle::native_bracket();
        stdin.lock().read_line(&mut line).is_ok()
    };
    if read_ok {
        if line.ends_with('\n') {
            line.pop();
        }
        if line.ends_with('\r') {
            line.pop();
        }

        return crate::pith_copy_bytes_to_cstring(line.as_bytes());
    }
    std::ptr::null_mut()
}

fn exec_command_str(cmd_str: &str) -> i64 {
    use std::process::Command;

    let parts: Vec<&str> = cmd_str.split_whitespace().collect();
    if parts.is_empty() {
        return -1;
    }

    let mut cmd = Command::new(parts[0]);
    crate::env_overlay::apply(&mut cmd);
    if parts.len() > 1 {
        cmd.args(&parts[1..]);
    }

    match crate::process::command_status(cmd) {
        Some(status) => crate::process::exit_code(status),
        None => -1,
    }
}

/// Execute a command and return exit code
///
/// the child runs to completion via the process pool (see `process`), so a
/// green worker is not held for however long the command takes.
///
/// # Safety
/// command must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_exec(command: *const i8) -> i64 {
    // SAFETY: Caller guarantees `command` is null or a valid null-terminated C string.
    unsafe { with_cstr_str(command, exec_command_str) }.unwrap_or(-1)
}

/// the multiplier/increment of the LCG behind `pith_random_float` (knuth's
/// MMIX constants).
const LCG_MUL: u64 = 6364136223846793005;
const LCG_INC: u64 = 1;

fn lcg_step(s: u64) -> u64 {
    s.wrapping_mul(LCG_MUL).wrapping_add(LCG_INC)
}

/// Random float between 0.0 and 1.0
#[no_mangle]
pub extern "C" fn pith_random_float() -> f64 {
    // step the seed with a compare-exchange rather than a load/store pair: two
    // tasks on different workers drawing at once must each claim a distinct
    // step of the sequence, not both advance from the same seed and return the
    // identical "random" value. relaxed ordering is enough: the value itself
    // is the only payload, nothing else is published through it.
    let mut prev = RANDOM_SEED.load(Ordering::Relaxed);
    let new_s = loop {
        let next = lcg_step(prev);
        match RANDOM_SEED.compare_exchange_weak(
            prev,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break next,
            Err(actual) => prev = actual,
        }
    };
    (new_s >> 11) as f64 / (1u64 << 53) as f64
}

/// Seed the random number generator
#[no_mangle]
pub extern "C" fn pith_random_seed(seed: i64) {
    RANDOM_SEED.store(seed as u64, Ordering::Relaxed);
}

/// Random integer in range [min, max]
#[no_mangle]
pub extern "C" fn pith_random_int(min: i64, max: i64) -> i64 {
    if min >= max {
        return min;
    }
    let span = max.abs_diff(min);
    let range = span.saturating_add(1);
    let offset = ((pith_random_float() * range as f64) as u64).min(span);
    min.wrapping_add(offset as i64)
}

/// Maximum decimal precision accepted by `pith_fmt_float` / `pith_float_fixed`.
const MAX_FLOAT_PRECISION: i64 = 1024;

/// Format float with given precision
/// Returns C string. Caller must free.
#[no_mangle]
pub unsafe extern "C" fn pith_fmt_float(n: f64, precision: i64) -> *mut i8 {
    let precision = precision.clamp(0, MAX_FLOAT_PRECISION) as usize;
    let s = format!("{:.1$}", n, precision);
    crate::pith_copy_bytes_to_cstring(s.as_bytes())
}

/// Generate random string of given length
/// Returns C string. Caller must free.
#[no_mangle]
pub unsafe extern "C" fn pith_random_string(len: i64) -> *mut i8 {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    if len <= 0 {
        return crate::pith_cstring_empty();
    }
    let n = len as usize;
    if !crate::runtime_core::cstring_len_valid(n) {
        return crate::pith_cstring_empty();
    }

    // SAFETY: Allocates a non-null cstring buffer of `n` writable bytes plus trailing NUL.
    let ptr = unsafe { crate::pith_alloc_cstring(n) };
    // SAFETY: `ptr` is non-null and uniquely owned for `n` uninitialized bytes.
    let buf = unsafe { std::slice::from_raw_parts_mut(ptr.cast::<std::mem::MaybeUninit<u8>>(), n) };
    for byte in buf {
        let idx = (pith_random_float() * CHARSET.len() as f64) as usize % CHARSET.len();
        byte.write(CHARSET[idx]);
    }
    ptr
}

/// The `format_time` builtin. It formats a timestamp, given in milliseconds
/// since the unix epoch, in UTC using a subset of strftime: `%Y` (year),
/// `%m` `%d` `%H` `%M` `%S` (two digits each), `%F` (`%Y-%m-%d`), `%T`
/// (`%H:%M:%S`), `%3f` (milliseconds, three digits) and `%%`. Any other
/// `%` sequence is copied as written.
///
/// # Safety
/// fmt must be null or a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn pith_format_time_fmt(timestamp_ms: i64, fmt: *const i8) -> *mut i8 {
    // SAFETY: `fmt` is null or a valid null-terminated C string for this call.
    let s = unsafe { with_cstr_bytes(fmt, |bytes| format_time_utc(timestamp_ms, bytes)) }
        .unwrap_or_default();
    crate::pith_copy_bytes_to_cstring(s.as_bytes())
}

/// Converts days since 1970-01-01 to a (year, month, day) civil date in the
/// proleptic gregorian calendar.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

pub(crate) fn format_time_utc(timestamp_ms: i64, fmt: &[u8]) -> String {
    let secs = timestamp_ms.div_euclid(1000);
    let millis = timestamp_ms.rem_euclid(1000);
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let tod = secs.rem_euclid(86_400);
    let (hour, minute, second) = (tod / 3600, tod / 60 % 60, tod % 60);
    let mut out = String::with_capacity(fmt.len() + 16);
    let mut i = 0;
    while i < fmt.len() {
        let rest = &fmt[i..];
        let (text, used) = match rest {
            [b'%', b'Y', ..] => (format!("{year:04}"), 2),
            [b'%', b'm', ..] => (format!("{month:02}"), 2),
            [b'%', b'd', ..] => (format!("{day:02}"), 2),
            [b'%', b'H', ..] => (format!("{hour:02}"), 2),
            [b'%', b'M', ..] => (format!("{minute:02}"), 2),
            [b'%', b'S', ..] => (format!("{second:02}"), 2),
            [b'%', b'F', ..] => (format!("{year:04}-{month:02}-{day:02}"), 2),
            [b'%', b'T', ..] => (format!("{hour:02}:{minute:02}:{second:02}"), 2),
            [b'%', b'3', b'f', ..] => (format!("{millis:03}"), 3),
            [b'%', b'%', ..] => ("%".to_string(), 2),
            _ => {
                // copy plain text up to the next '%' (or one '%' that starts
                // no known sequence) unchanged, keeping utf-8 intact.
                let end = rest[1..]
                    .iter()
                    .position(|&b| b == b'%')
                    .map_or(rest.len(), |p| p + 1);
                out.push_str(&String::from_utf8_lossy(&rest[..end]));
                i += end;
                continue;
            }
        };
        out.push_str(&text);
        i += used;
    }
    out
}


#[cfg(test)]
mod tests {
    use super::*;

    // both random tests reseed the one process-global sequence, so they must
    // not interleave with each other (cargo runs tests concurrently).
    static RANDOM_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // seeding pins the whole sequence: same seed, same draws. this is the
    // contract the CAS step must preserve for single-threaded callers.
    #[test]
    fn seeded_draws_are_reproducible() {
        let _guard = RANDOM_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        pith_random_seed(42);
        let first: Vec<u64> = (0..8).map(|_| pith_random_float().to_bits()).collect();
        pith_random_seed(42);
        let second: Vec<u64> = (0..8).map(|_| pith_random_float().to_bits()).collect();
        assert_eq!(first, second);
    }

    // the race this guards against: with a load/store seed update, two threads
    // can both step from the same seed and return the identical draw. the
    // compare-exchange gives every draw its own step of the sequence, so
    // concurrent draws are distinct (up to the astronomically unlikely 53-bit
    // output collision between different seeds).
    #[test]
    fn concurrent_draws_do_not_repeat() {
        let _guard = RANDOM_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        pith_random_seed(7);
        let threads: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    (0..2000)
                        .map(|_| pith_random_float().to_bits())
                        .collect::<Vec<u64>>()
                })
            })
            .collect();
        let mut all: Vec<u64> = threads
            .into_iter()
            .flat_map(|t| t.join().expect("drawing thread panicked"))
            .collect();
        let total = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), total, "two concurrent draws returned the same value");
    }

    #[test]
    fn exec_rejects_null_and_invalid_utf8() {
        let invalid = [0xffu8, 0x00];

        unsafe {
            assert_eq!(pith_exec(std::ptr::null()), -1);
            assert_eq!(pith_exec(invalid.as_ptr() as *const i8), -1);
        }
    }
}
