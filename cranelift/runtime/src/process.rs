use crate::collections::list::ForgeList;
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

unsafe fn forge_optional_cstring(ptr: *const i8) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let len = crate::string::forge_cstring_len(ptr) as usize;
    let slice = std::slice::from_raw_parts(ptr as *const u8, len);
    std::str::from_utf8(slice).unwrap_or("").to_string()
}

unsafe fn forge_required_cstring(ptr: *const i8) -> Option<String> {
    let text = forge_optional_cstring(ptr);
    if text.is_empty() {
        return None;
    }
    Some(text)
}

unsafe fn forge_string_list_to_vec(list: ForgeList) -> Vec<String> {
    let len = crate::collections::list::forge_list_len(list);
    let mut values = Vec::with_capacity(len as usize);
    let mut i = 0;
    while i < len {
        let ptr = crate::collections::list::forge_list_get_value(list, i) as *const i8;
        values.push(forge_optional_cstring(ptr));
        i += 1;
    }
    values
}

fn forge_store_process_output(status: i64, stdout: String, stderr: String) -> i64 {
    let handle = NEXT_PROCESS_OUTPUT_HANDLE.fetch_add(1, Ordering::Relaxed);
    let entry = ProcessOutputHandle {
        status,
        stdout,
        stderr,
    };
    process_output_handles().lock().insert(handle, entry);
    handle
}

fn forge_strdup_string(text: &str) -> *mut i8 {
    let owned = format!("{}\0", text);
    unsafe { crate::forge_strdup(owned.as_ptr() as *const i8) }
}

unsafe fn forge_build_command(
    program: *const i8,
    argv: ForgeList,
    cwd: *const i8,
    env_keys: ForgeList,
    env_values: ForgeList,
) -> Option<Command> {
    let program_text = forge_required_cstring(program)?;
    let mut command = Command::new(program_text);

    for arg in forge_string_list_to_vec(argv) {
        command.arg(arg);
    }

    let cwd_text = forge_optional_cstring(cwd);
    if !cwd_text.is_empty() {
        command.current_dir(cwd_text);
    }

    let keys = forge_string_list_to_vec(env_keys);
    let values = forge_string_list_to_vec(env_values);
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
pub unsafe extern "C" fn forge_process_spawn(cmd: *const i8) -> i64 {
    if cmd.is_null() {
        return 0;
    }
    let len = crate::string::forge_cstring_len(cmd) as usize;
    let slice = std::slice::from_raw_parts(cmd as *const u8, len);
    if let Ok(cmd_str) = std::str::from_utf8(slice) {
        match Command::new("/bin/sh")
            .arg("-lc")
            .arg(cmd_str)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                let handle = NEXT_PROCESS_HANDLE.fetch_add(1, Ordering::Relaxed);
                let entry = ProcessHandle {
                    stdin: child.stdin.take(),
                    stdout: child.stdout.take(),
                    stderr: child.stderr.take(),
                    child,
                };
                process_handles().lock().insert(handle, entry);
                handle
            }
            Err(_) => 0,
        }
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_process_spawn_argv(
    program: *const i8,
    argv: ForgeList,
    cwd: *const i8,
    env_keys: ForgeList,
    env_values: ForgeList,
) -> i64 {
    let Some(mut command) = forge_build_command(program, argv, cwd, env_keys, env_values) else {
        return 0;
    };

    match command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(mut child) => {
            let handle = NEXT_PROCESS_HANDLE.fetch_add(1, Ordering::Relaxed);
            let entry = ProcessHandle {
                stdin: child.stdin.take(),
                stdout: child.stdout.take(),
                stderr: child.stderr.take(),
                child,
            };
            process_handles().lock().insert(handle, entry);
            handle
        }
        Err(_) => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_process_output_argv(
    program: *const i8,
    argv: ForgeList,
    cwd: *const i8,
    env_keys: ForgeList,
    env_values: ForgeList,
) -> i64 {
    let Some(mut command) = forge_build_command(program, argv, cwd, env_keys, env_values) else {
        return 0;
    };

    match command.output() {
        Ok(output) => {
            let status = output.status.code().unwrap_or(-1) as i64;
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            forge_store_process_output(status, stdout, stderr)
        }
        Err(_) => 0,
    }
}

#[no_mangle]
pub extern "C" fn forge_process_output_status(handle: i64) -> i64 {
    let outputs = process_output_handles().lock();
    let Some(entry) = outputs.get(&handle) else {
        return -1;
    };
    entry.status
}

#[no_mangle]
pub extern "C" fn forge_process_output_close(handle: i64) {
    process_output_handles().lock().remove(&handle);
}

#[no_mangle]
pub extern "C" fn forge_process_output_stdout(handle: i64) -> *mut i8 {
    let outputs = process_output_handles().lock();
    let Some(entry) = outputs.get(&handle) else {
        return std::ptr::null_mut();
    };
    forge_strdup_string(&entry.stdout)
}

#[no_mangle]
pub extern "C" fn forge_process_output_stderr(handle: i64) -> *mut i8 {
    let outputs = process_output_handles().lock();
    let Some(entry) = outputs.get(&handle) else {
        return std::ptr::null_mut();
    };
    forge_strdup_string(&entry.stderr)
}

/// Wait for a spawned process to finish, returns exit code
#[no_mangle]
pub extern "C" fn forge_process_wait(handle: i64) -> i64 {
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
pub extern "C" fn forge_process_kill(handle: i64) -> i64 {
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
pub extern "C" fn forge_process_close(handle: i64) {
    let mut handles = process_handles().lock();
    handles.remove(&handle);
}
