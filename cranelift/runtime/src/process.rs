use crate::collections::list::PithList;
use crate::ffi_util::{cstr_str, cstr_str_or_empty};
use crate::handle_registry::{self, HandleKind};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::OnceLock;

pub(crate) struct ProcessHandle {
    pub(crate) child: Child,
    pub(crate) stdin: Option<ChildStdin>,
    pub(crate) stdout: Option<ChildStdout>,
    pub(crate) stderr: Option<ChildStderr>,
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
    process_handles().lock().insert(handle, entry);
    handle_registry::register_id(handle, HandleKind::Process);
    handle
}

fn pith_strdup_string(text: &str) -> *mut i8 {
    let owned = format!("{}\0", text);
    unsafe { crate::pith_strdup(owned.as_ptr() as *const i8) }
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

/// Wait for a spawned process to finish, returns exit code
#[no_mangle]
pub extern "C" fn pith_process_wait(handle: i64) -> i64 {
    if !handle_registry::is_valid_id(handle, HandleKind::Process) {
        return -1;
    }
    let mut handles = process_handles().lock();
    let Some(entry) = handles.get_mut(&handle) else {
        return -1;
    };
    match entry.child.wait() {
        Ok(status) => status.code().unwrap_or(-1) as i64,
        Err(_) => -1,
    }
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
    let mut handles = process_handles().lock();
    handles.remove(&handle);
    handle_registry::unregister_id(handle, HandleKind::Process);
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
