//! A sampling profiler that needs no outside tool. `PITH_PROF=<file>` makes
//! the program record the interrupted instruction every 250 us of cpu time
//! (`ITIMER_PROF`, which counts every thread) and write the samples at exit,
//! after the executable's load address and the process's memory map.
//! `tooling/prof_report.py` turns that into a flat profile by function. perf
//! is not installed everywhere this runs, and a ptrace sampler cannot attach
//! to a process whose threads block the signals it relies on.

use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::OnceLock;

const MAX_SAMPLES: usize = 1 << 22;
const INTERVAL_US: libc::suseconds_t = 250;

static SAMPLES: AtomicPtr<u64> = AtomicPtr::new(std::ptr::null_mut());
static NEXT: AtomicUsize = AtomicUsize::new(0);
static OUT_PATH: OnceLock<std::ffi::OsString> = OnceLock::new();

// runs before main, the way a C constructor would, so the first sample can
// land in the module initializers
#[used]
#[link_section = ".init_array"]
static START: extern "C" fn() = start;

extern "C" fn start() {
    let Some(path) = std::env::var_os("PITH_PROF") else {
        return;
    };
    if path.is_empty() || !cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
        return;
    }
    let buffer = vec![0u64; MAX_SAMPLES].into_boxed_slice();
    SAMPLES.store(Box::leak(buffer).as_mut_ptr(), Ordering::Release);
    let _ = OUT_PATH.set(path);
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_sample as *const () as usize;
        action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        if libc::sigaction(libc::SIGPROF, &action, std::ptr::null_mut()) != 0 {
            return;
        }
        libc::atexit(write_samples);
        set_timer(INTERVAL_US);
    }
}

unsafe fn set_timer(interval_us: libc::suseconds_t) {
    let tick = libc::timeval {
        tv_sec: 0,
        tv_usec: interval_us,
    };
    let timer = libc::itimerval {
        it_interval: tick,
        it_value: tick,
    };
    libc::setitimer(libc::ITIMER_PROF, &timer, std::ptr::null_mut());
}

extern "C" fn on_sample(_sig: libc::c_int, _info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    let samples = SAMPLES.load(Ordering::Relaxed);
    if samples.is_null() || context.is_null() {
        return;
    }
    let pc = unsafe { interrupted_pc(context as *const libc::ucontext_t) };
    let slot = NEXT.fetch_add(1, Ordering::Relaxed);
    if slot < MAX_SAMPLES {
        unsafe { samples.add(slot).write(pc) };
    }
}

#[cfg(target_arch = "x86_64")]
unsafe fn interrupted_pc(context: *const libc::ucontext_t) -> u64 {
    (*context).uc_mcontext.gregs[libc::REG_RIP as usize] as u64
}

#[cfg(target_arch = "aarch64")]
unsafe fn interrupted_pc(context: *const libc::ucontext_t) -> u64 {
    (*context).uc_mcontext.pc
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
unsafe fn interrupted_pc(_context: *const libc::ucontext_t) -> u64 {
    0
}

extern "C" fn write_samples() {
    unsafe { set_timer(0) };
    let Some(path) = OUT_PATH.get() else {
        return;
    };
    let samples = SAMPLES.load(Ordering::Acquire);
    if samples.is_null() {
        return;
    }
    let count = NEXT.load(Ordering::Relaxed).min(MAX_SAMPLES);
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap_or_default();
    let exe = std::fs::read_link("/proc/self/exe")
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    // the first mapping of the executable file starts at its load address
    let base = maps
        .lines()
        .find(|line| !exe.is_empty() && line.ends_with(exe.as_str()))
        .and_then(|line| line.split('-').next())
        .and_then(|start| u64::from_str_radix(start, 16).ok())
        .unwrap_or(0);
    let mut text = String::with_capacity(count * 13 + maps.len() * 2);
    text.push_str(&format!("base {base:x}\n"));
    for line in maps.lines() {
        text.push_str("map ");
        text.push_str(line);
        text.push('\n');
    }
    for i in 0..count {
        let pc = unsafe { samples.add(i).read() };
        text.push_str(&format!("{pc:x}\n"));
    }
    if let Err(err) = std::fs::write(path, text) {
        eprintln!("pith: PITH_PROF could not write {}: {err}", path.to_string_lossy());
    }
}
