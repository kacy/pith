use std::sync::atomic::{AtomicU64, Ordering};

static RANDOM_SEED: AtomicU64 = AtomicU64::new(123456789);

/// Exit the program with given status code
#[no_mangle]
pub extern "C" fn forge_exit(code: i64) {
    std::process::exit(code as i32);
}

/// Sleep for given number of milliseconds
#[no_mangle]
pub extern "C" fn forge_sleep(ms: i64) {
    std::thread::sleep(std::time::Duration::from_millis(ms as u64));
}

/// Get current time in milliseconds since epoch
#[no_mangle]
pub extern "C" fn forge_time() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Read a line from stdin
/// Returns C string. Caller must free with forge_free.
#[no_mangle]
pub unsafe extern "C" fn forge_input() -> *mut i8 {
    use std::alloc::{alloc, Layout};
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

        let len = line.len();
        let layout = Layout::from_size_align(len + 1, 1).unwrap();
        let ptr = alloc(layout) as *mut i8;

        if !ptr.is_null() {
            std::ptr::copy_nonoverlapping(line.as_ptr(), ptr as *mut u8, len);
            *ptr.add(len) = 0;
        }
        return ptr;
    }
    std::ptr::null_mut()
}

/// Execute a command and return exit code
///
/// # Safety
/// command must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn forge_exec(command: *const i8) -> i64 {
    use std::process::Command;

    if command.is_null() {
        return -1;
    }

    let len = crate::string::forge_cstring_len(command) as usize;
    let slice = std::slice::from_raw_parts(command as *const u8, len);
    if let Ok(cmd_str) = std::str::from_utf8(slice) {
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
    }
    -1
}

/// Random float between 0.0 and 1.0
#[no_mangle]
pub extern "C" fn forge_random_float() -> f64 {
    use std::num::Wrapping;

    let s = RANDOM_SEED.load(Ordering::Relaxed);
    let new_s = Wrapping(s) * Wrapping(6364136223846793005) + Wrapping(1);
    RANDOM_SEED.store(new_s.0, Ordering::Relaxed);
    (new_s.0 >> 11) as f64 / (1u64 << 53) as f64
}

/// Seed the random number generator
#[no_mangle]
pub extern "C" fn forge_random_seed(seed: i64) {
    RANDOM_SEED.store(seed as u64, Ordering::Relaxed);
}

/// Random integer in range [min, max]
#[no_mangle]
pub extern "C" fn forge_random_int(min: i64, max: i64) -> i64 {
    if min >= max {
        return min;
    }
    let range = (max - min + 1) as u64;
    let r = (forge_random_float() * range as f64) as i64;
    min + r
}

/// Format float with given precision
/// Returns C string. Caller must free.
#[no_mangle]
pub unsafe extern "C" fn forge_fmt_float(n: f64, precision: i64) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    let s = format!("{:.1$}", n, precision as usize);
    let len = s.len();
    let layout = Layout::from_size_align(len + 1, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;

    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(s.as_ptr(), ptr as *mut u8, len);
        *ptr.add(len) = 0;
    }
    ptr
}

/// Generate random string of given length
/// Returns C string. Caller must free.
#[no_mangle]
pub unsafe extern "C" fn forge_random_string(len: i64) -> *mut i8 {
    use std::alloc::{alloc, Layout};

    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let n = len.max(0) as usize;

    let layout = Layout::from_size_align(n + 1, 1).unwrap();
    let ptr = alloc(layout) as *mut i8;

    if !ptr.is_null() {
        for i in 0..n {
            let idx = (forge_random_float() * CHARSET.len() as f64) as usize % CHARSET.len();
            *ptr.add(i) = CHARSET[idx] as i8;
        }
        *ptr.add(n) = 0;
    }
    ptr
}
