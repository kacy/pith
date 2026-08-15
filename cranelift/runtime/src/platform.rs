use crate::ffi_util::cstr_str;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

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
/// and gives the worker back — a sleeping task must not stall every other task
/// pinned to its worker (`select`'s idle probe sleeps in a loop, so before
/// this two idle selects could occupy a whole two-worker pool). the guard is
/// the same one `dns.rs` uses: `current_task()` is `None` on the main thread,
/// on any plain OS thread, and on the whole os-thread backend, and there
/// blocking the thread is exactly right, so that path is unchanged. on
/// non-linux there is no reactor and `netpoll`'s fallback blocks the worker,
/// consistent with how socket waits degrade there.
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
    if stdin.lock().read_line(&mut line).is_ok() {
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

/// Execute a command and return exit code
///
/// the child runs to completion via the process pool (see `process`), so a
/// green worker is not held for however long the command takes.
///
/// # Safety
/// command must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_exec(command: *const i8) -> i64 {
    use std::process::Command;

    let Some(cmd_str) = cstr_str(command) else {
        return -1;
    };
    let parts: Vec<&str> = cmd_str.split_whitespace().collect();
    if parts.is_empty() {
        return -1;
    }

    let mut cmd = Command::new(parts[0]);
    if parts.len() > 1 {
        cmd.args(&parts[1..]);
    }

    match crate::process::command_status(cmd) {
        Some(status) => status.code().unwrap_or(0) as i64,
        None => -1,
    }
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
    // identical "random" value. relaxed ordering is enough — the value itself
    // is the only payload, nothing else is published through it.
    let prev = RANDOM_SEED
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |s| Some(lcg_step(s)))
        .unwrap_or(0); // unreachable: the closure never returns None
    let new_s = lcg_step(prev);
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
    let range = (max - min + 1) as u64;
    let r = (pith_random_float() * range as f64) as i64;
    min + r
}

/// Format float with given precision
/// Returns C string. Caller must free.
#[no_mangle]
pub unsafe extern "C" fn pith_fmt_float(n: f64, precision: i64) -> *mut i8 {
    let precision = precision.max(0) as usize;
    let s = format!("{:.1$}", n, precision);
    crate::pith_copy_bytes_to_cstring(s.as_bytes())
}

/// Generate random string of given length
/// Returns C string. Caller must free.
#[no_mangle]
pub unsafe extern "C" fn pith_random_string(len: i64) -> *mut i8 {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let n = len.max(0) as usize;

    let ptr = crate::pith_alloc_cstring(n);
    for i in 0..n {
        let idx = (pith_random_float() * CHARSET.len() as f64) as usize % CHARSET.len();
        *ptr.add(i) = CHARSET[idx] as i8;
    }
    ptr
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
