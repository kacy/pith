use crate::ffi_util::cstr_str;
use std::alloc::Layout;
use std::sync::atomic::{AtomicU64, Ordering};

static RANDOM_SEED: AtomicU64 = AtomicU64::new(123456789);
const RANDOM_MULTIPLIER: u64 = 6364136223846793005;

fn next_random_seed() -> u64 {
    loop {
        let current = RANDOM_SEED.load(Ordering::Relaxed);
        let next = current.wrapping_mul(RANDOM_MULTIPLIER).wrapping_add(1);
        if RANDOM_SEED
            .compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return next;
        }
    }
}

/// Exit the program with given status code
#[no_mangle]
pub extern "C" fn pith_exit(code: i64) {
    std::process::exit(code as i32);
}

/// Sleep for given number of milliseconds
#[no_mangle]
pub extern "C" fn pith_sleep(ms: i64) {
    if ms <= 0 {
        return;
    }
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

    if let Ok(status) = cmd.status() {
        if let Some(code) = status.code() {
            return code as i64;
        }
        return 0;
    }
    -1
}

/// Random float between 0.0 and 1.0
#[no_mangle]
pub extern "C" fn pith_random_float() -> f64 {
    (next_random_seed() >> 11) as f64 / (1u64 << 53) as f64
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

    let range = max as i128 - min as i128 + 1;
    let offset = (pith_random_float() * range as f64) as i128;
    (min as i128 + offset.min(range - 1)) as i64
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
    let Some(size) = n.checked_add(1) else {
        eprintln!("pith runtime error: random string allocation size overflow");
        return std::ptr::null_mut();
    };
    let Ok(layout) = Layout::from_size_align(size, 1) else {
        eprintln!("pith runtime error: invalid random string allocation layout");
        return std::ptr::null_mut();
    };

    let ptr = crate::runtime_core::pith_try_alloc(layout) as *mut i8;
    if ptr.is_null() {
        eprintln!("pith runtime error: allocation failed");
        return std::ptr::null_mut();
    }
    for i in 0..n {
        let idx = (pith_random_float() * CHARSET.len() as f64) as usize % CHARSET.len();
        *ptr.add(i) = CHARSET[idx] as i8;
    }
    *ptr.add(n) = 0;
    ptr
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_rejects_null_and_invalid_utf8() {
        let invalid = [0xffu8, 0x00];

        unsafe {
            assert_eq!(pith_exec(std::ptr::null()), -1);
            assert_eq!(pith_exec(invalid.as_ptr() as *const i8), -1);
        }
    }

    #[test]
    fn random_int_handles_the_full_i64_range() {
        pith_random_seed(1);
        let value = pith_random_int(i64::MIN, i64::MAX);
        assert!((i64::MIN..=i64::MAX).contains(&value));
    }

    #[test]
    fn random_seed_updates_are_atomic_across_threads() {
        pith_random_seed(1);

        let mut threads = Vec::new();
        for _ in 0..8 {
            threads.push(std::thread::spawn(|| {
                for _ in 0..1000 {
                    let value = pith_random_float();
                    assert!((0.0..1.0).contains(&value));
                }
            }));
        }

        for thread in threads {
            assert!(thread.join().is_ok());
        }
    }

    #[test]
    fn random_string_rejects_size_overflow_without_exiting() {
        unsafe {
            assert!(pith_random_string(i64::MAX).is_null());
        }
    }
}
