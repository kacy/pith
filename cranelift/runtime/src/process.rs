//! spawning, waiting on, and killing child processes.
//!
//! ## why none of this parks a green worker
//!
//! a green worker OS thread runs many tasks, so a syscall it sleeps in stalls
//! all of them. waiting on a child is the worst case of that: it is unbounded,
//! since the child decides when it ends.
//!
//! child processes have a nice property here that files do not — everything
//! worth waiting for is pollable. the pipes are pipes, and linux will hand out
//! a `pidfd` that becomes readable exactly when the child exits. so both waits
//! go to the epoll reactor that already serves sockets (`netpoll`), and no
//! thread is parked on a child anywhere in the runtime.
//!
//! that also settles the question of what may run off the task's own thread:
//! nothing does. the reactor never runs pith code — it only marks a task
//! runnable, and the task resumes on the worker it was pinned to — so every
//! handle, `Bytes`, and C string here is built where it always was.

use crate::blocking;
use crate::collections::list::PithList;
use crate::concurrency::scheduler::{backend, Backend};
use crate::fdio;
use crate::ffi_util::{cstr_str, cstr_str_or_empty};
use crate::handle_registry::{self, HandleKind};
use crate::netpoll;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::os::unix::io::{AsRawFd, RawFd};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::OnceLock;

/// which of a child's three pipes a caller wants.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pipe {
    Stdin,
    Stdout,
    Stderr,
}

/// every pipe, for the paths that have to touch all three.
const PIPES: [Pipe; 3] = [Pipe::Stdin, Pipe::Stdout, Pipe::Stderr];

pub(crate) struct ProcessHandle {
    pub(crate) child: Child,
    pub(crate) stdin: Option<ChildStdin>,
    pub(crate) stdout: Option<ChildStdout>,
    pub(crate) stderr: Option<ChildStderr>,
}

impl ProcessHandle {
    /// the raw fd behind one of the child's pipes, or `None` if it was not
    /// captured. `process_io` reads and writes through the fd rather than
    /// through the `ChildStdout`/`ChildStdin` so it can drop the registry lock
    /// before it waits — see the note on `pith_process_close` for what keeps
    /// that fd alive.
    pub(crate) fn pipe(&self, pipe: Pipe) -> Option<RawFd> {
        match pipe {
            Pipe::Stdin => self.stdin.as_ref().map(|s| s.as_raw_fd()),
            Pipe::Stdout => self.stdout.as_ref().map(|s| s.as_raw_fd()),
            Pipe::Stderr => self.stderr.as_ref().map(|s| s.as_raw_fd()),
        }
    }
}

struct ProcessOutputHandle {
    status: i64,
    stdout: String,
    stderr: String,
}

static PROCESS_HANDLES: OnceLock<Mutex<HashMap<i64, ProcessHandle>>> = OnceLock::new();
static NEXT_PROCESS_HANDLE: AtomicI64 = AtomicI64::new(1);
static PROCESS_OUTPUT_HANDLES: OnceLock<Mutex<HashMap<i64, ProcessOutputHandle>>> = OnceLock::new();
static NEXT_PROCESS_OUTPUT_HANDLE: AtomicI64 = AtomicI64::new(1);

pub(crate) fn process_handles() -> &'static Mutex<HashMap<i64, ProcessHandle>> {
    PROCESS_HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn process_output_handles() -> &'static Mutex<HashMap<i64, ProcessOutputHandle>> {
    PROCESS_OUTPUT_HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}

unsafe fn pith_optional_cstring(ptr: *const i8) -> String {
    cstr_str_or_empty(ptr).to_string()
}

unsafe fn pith_required_cstring(ptr: *const i8) -> Option<String> {
    let text = cstr_str(ptr)?;
    if text.is_empty() {
        return None;
    }
    Some(text.to_string())
}

unsafe fn pith_string_list_to_vec(list: PithList) -> Vec<String> {
    let len = crate::collections::list::pith_list_len(list);
    let mut values = Vec::with_capacity(len as usize);
    let mut i = 0;
    while i < len {
        let ptr = crate::collections::list::pith_list_get_value(list, i) as *const i8;
        values.push(pith_optional_cstring(ptr));
        i += 1;
    }
    values
}

fn pith_store_process_output(status: i64, stdout: String, stderr: String) -> i64 {
    let handle = NEXT_PROCESS_OUTPUT_HANDLE.fetch_add(1, Ordering::Relaxed);
    let entry = ProcessOutputHandle {
        status,
        stdout,
        stderr,
    };
    process_output_handles().lock().insert(handle, entry);
    handle_registry::register_id(handle, HandleKind::ProcessOutput);
    handle
}

fn pith_store_process_handle(mut child: Child) -> i64 {
    let handle = NEXT_PROCESS_HANDLE.fetch_add(1, Ordering::Relaxed);
    let entry = ProcessHandle {
        stdin: child.stdin.take(),
        stdout: child.stdout.take(),
        stderr: child.stderr.take(),
        child,
    };
    // under green the pipes have to be non-blocking, or a read with nothing on
    // the other end sleeps the worker instead of yielding to the reactor. this
    // is our end of each pipe only — the child's end is a separate open file
    // description and keeps the blocking semantics it expects. the os-thread
    // backend leaves them alone: there is no worker to protect and a blocking
    // read is exactly what it wants.
    if backend() == Backend::Green {
        for pipe in PIPES {
            if let Some(fd) = entry.pipe(pipe) {
                fdio::set_nonblocking(fd);
            }
        }
    }
    process_handles().lock().insert(handle, entry);
    handle_registry::register_id(handle, HandleKind::Process);
    handle
}

fn pith_strdup_string(text: &str) -> *mut i8 {
    // the shared helper allocates from the known length without probing the
    // rust buffer for a header it cannot have (see runtime_core).
    crate::runtime_core::pith_strdup_string(text)
}

unsafe fn pith_build_command(
    program: *const i8,
    argv: PithList,
    cwd: *const i8,
    env_keys: PithList,
    env_values: PithList,
) -> Option<Command> {
    let program_text = pith_required_cstring(program)?;
    let mut command = Command::new(program_text);

    for arg in pith_string_list_to_vec(argv) {
        command.arg(arg);
    }

    let cwd_text = pith_optional_cstring(cwd);
    if !cwd_text.is_empty() {
        command.current_dir(cwd_text);
    }

    let keys = pith_string_list_to_vec(env_keys);
    let values = pith_string_list_to_vec(env_values);
    for (key, value) in keys.into_iter().zip(values.into_iter()) {
        command.env(key, value);
    }

    Some(command)
}

/// Spawn a child process and return a process handle
///
/// # Safety
/// cmd must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_process_spawn(cmd: *const i8) -> i64 {
    let Some(cmd_str) = cstr_str(cmd) else {
        return 0;
    };
    match Command::new("/bin/sh")
        .arg("-lc")
        .arg(cmd_str)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => pith_store_process_handle(child),
        Err(_) => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_process_spawn_argv(
    program: *const i8,
    argv: PithList,
    cwd: *const i8,
    env_keys: PithList,
    env_values: PithList,
) -> i64 {
    let Some(mut command) = pith_build_command(program, argv, cwd, env_keys, env_values) else {
        return 0;
    };

    match command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => pith_store_process_handle(child),
        Err(_) => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn pith_process_output_argv(
    program: *const i8,
    argv: PithList,
    cwd: *const i8,
    env_keys: PithList,
    env_values: PithList,
) -> i64 {
    let Some(mut command) = pith_build_command(program, argv, cwd, env_keys, env_values) else {
        return 0;
    };

    match command.output() {
        Ok(output) => {
            let status = output.status.code().unwrap_or(-1) as i64;
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            pith_store_process_output(status, stdout, stderr)
        }
        Err(_) => 0,
    }
}

#[no_mangle]
pub extern "C" fn pith_process_output_status(handle: i64) -> i64 {
    if !handle_registry::is_valid_id(handle, HandleKind::ProcessOutput) {
        return -1;
    }
    let outputs = process_output_handles().lock();
    let Some(entry) = outputs.get(&handle) else {
        return -1;
    };
    entry.status
}

#[no_mangle]
pub extern "C" fn pith_process_output_close(handle: i64) {
    process_output_handles().lock().remove(&handle);
    handle_registry::unregister_id(handle, HandleKind::ProcessOutput);
}

#[no_mangle]
pub extern "C" fn pith_process_output_stdout(handle: i64) -> *mut i8 {
    if !handle_registry::is_valid_id(handle, HandleKind::ProcessOutput) {
        return std::ptr::null_mut();
    }
    let outputs = process_output_handles().lock();
    let Some(entry) = outputs.get(&handle) else {
        return std::ptr::null_mut();
    };
    pith_strdup_string(&entry.stdout)
}

#[no_mangle]
pub extern "C" fn pith_process_output_stderr(handle: i64) -> *mut i8 {
    if !handle_registry::is_valid_id(handle, HandleKind::ProcessOutput) {
        return std::ptr::null_mut();
    }
    let outputs = process_output_handles().lock();
    let Some(entry) = outputs.get(&handle) else {
        return std::ptr::null_mut();
    };
    pith_strdup_string(&entry.stderr)
}

/// what a first, non-blocking look at a child found.
enum ExitState {
    /// it is already over; this is its exit code.
    Finished(i64),
    /// still running, and this fd becomes readable when that changes.
    Watch(RawFd),
    /// still running with nothing to watch it by — the caller has to block.
    Unwatchable,
    /// the handle is not ours (closed under us, or never existed).
    Unknown,
}

/// an fd that becomes readable when the process `pid` exits, or `None` when we
/// cannot get one.
///
/// `pidfd_open` is linux 5.3 and newer. an older kernel answers ENOSYS and we
/// fall back to blocking in `wait`, which is what every backend did before.
#[cfg(target_os = "linux")]
fn watch_pid(pid: u32) -> Option<RawFd> {
    // SAFETY: pidfd_open takes a pid and a flags word and returns an fd or -1.
    // the pid is live: the caller holds the registry lock and has just seen the
    // child unreaped, so the number cannot have been recycled.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0 as libc::c_uint) };
    if fd < 0 {
        return None;
    }
    Some(fd as RawFd)
}

#[cfg(not(target_os = "linux"))]
fn watch_pid(_pid: u32) -> Option<RawFd> {
    // no pidfd off linux, and no epoll reactor either (see `netpoll_fallback`),
    // so there would be nothing to hand the wait to even if there were.
    None
}

/// look at a child without blocking, and set up a way to be told when it ends.
///
/// this is one lock hold on purpose: the pidfd has to be opened while the child
/// is known to be unreaped, or the pid it names could already belong to someone
/// else. the lock is released before the wait itself, which is the part that
/// must not hold it — a task parked with the registry locked would wedge every
/// other process call on that worker, and at one worker that is a deadlock.
fn exit_state(handle: i64) -> ExitState {
    let mut handles = process_handles().lock();
    let Some(entry) = handles.get_mut(&handle) else {
        return ExitState::Unknown;
    };
    match entry.child.try_wait() {
        // already exited, or already waited for once: `try_wait` remembers the
        // status, so a second wait on the same handle costs nothing.
        Ok(Some(status)) => ExitState::Finished(status.code().unwrap_or(-1) as i64),
        Ok(None) => match watch_pid(entry.child.id()) {
            Some(fd) => ExitState::Watch(fd),
            None => ExitState::Unwatchable,
        },
        Err(_) => ExitState::Unwatchable,
    }
}

/// collect the exit status of a child that has already ended. blocks if it has
/// not, which is why every caller checks first.
fn reap(handle: i64) -> i64 {
    let mut handles = process_handles().lock();
    let Some(entry) = handles.get_mut(&handle) else {
        return -1;
    };
    match entry.child.wait() {
        Ok(status) => status.code().unwrap_or(-1) as i64,
        Err(_) => -1,
    }
}

/// Wait for a spawned process to finish, returns exit code
///
/// Off a green task this is the plain blocking wait it always was. On one, the
/// task parks on a pidfd in the epoll reactor and its worker runs everything
/// else until the child exits.
#[no_mangle]
pub extern "C" fn pith_process_wait(handle: i64) -> i64 {
    if !handle_registry::is_valid_id(handle, HandleKind::Process) {
        return -1;
    }
    // off a green task there is no worker to keep free and blocking is what an
    // OS thread is for, so take the direct wait untouched.
    if !blocking::offloads() {
        return reap(handle);
    }
    let watch = match exit_state(handle) {
        ExitState::Finished(code) => return code,
        ExitState::Unknown => return -1,
        ExitState::Unwatchable => return reap(handle),
        ExitState::Watch(fd) => fd,
    };

    // suspend the coroutine until the child exits. a wait that ends any other
    // way (an epoll error) just falls through to a blocking reap, which is
    // still correct, only slower than we wanted.
    fdio::wait_ready(watch as i64, true, -1);
    // drop the registration before the fd goes away, so the reactor is not left
    // holding a number the kernel can hand to the next socket we open.
    netpoll::on_close(watch);
    // SAFETY: closing the pidfd we opened above and have not shared.
    unsafe { libc::close(watch) };

    reap(handle)
}

/// Send a kill signal to a process
#[no_mangle]
pub extern "C" fn pith_process_kill(handle: i64) -> i64 {
    if !handle_registry::is_valid_id(handle, HandleKind::Process) {
        return 0;
    }
    let mut handles = process_handles().lock();
    let Some(entry) = handles.get_mut(&handle) else {
        return 0;
    };
    match entry.child.kill() {
        Ok(_) => 1,
        Err(_) => 0,
    }
}

/// Close and forget a process handle
#[no_mangle]
pub extern "C" fn pith_process_close(handle: i64) {
    // take the entry out under the lock but drop it after, so the pipes close
    // outside the lock and a task parked on one of them can be woken first.
    let entry = process_handles().lock().remove(&handle);
    handle_registry::unregister_id(handle, HandleKind::Process);

    // a task suspended in the reactor on one of these pipes has to come back
    // before the fd is closed. otherwise it stays parked on a number the kernel
    // is free to hand to the next socket we open, and waits on that instead.
    // `on_close` hands it a closed-fd outcome, which its read reports as an
    // error; this is the same teardown a socket gets in `pith_tcp_close`.
    if let Some(entry) = &entry {
        for pipe in PIPES {
            if let Some(fd) = entry.pipe(pipe) {
                netpoll::on_close(fd);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_process_handles_return_safe_defaults() {
        assert_eq!(pith_process_wait(12345), -1);
        assert_eq!(pith_process_kill(12345), 0);
        pith_process_close(12345);
    }

    #[test]
    fn process_spawn_rejects_null_and_invalid_utf8() {
        let invalid = [0xffu8, 0x00];

        unsafe {
            assert_eq!(pith_process_spawn(std::ptr::null()), 0);
            assert_eq!(pith_process_spawn(invalid.as_ptr() as *const i8), 0);
        }
    }

    #[test]
    fn closed_process_output_handles_are_rejected() {
        let handle = pith_store_process_output(7, "out".to_string(), "err".to_string());
        assert_eq!(pith_process_output_status(handle), 7);
        assert!(!pith_process_output_stdout(handle).is_null());
        assert!(!pith_process_output_stderr(handle).is_null());

        pith_process_output_close(handle);
        assert_eq!(pith_process_output_status(handle), -1);
        assert!(pith_process_output_stdout(handle).is_null());
        assert!(pith_process_output_stderr(handle).is_null());
        pith_process_output_close(handle);
    }
}
