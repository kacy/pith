//! Child process construction, spawning, and captured output handles.

use super::{
    command_output, exit_code, pith_store_process_handle, process_output_handles,
    ProcessOutputHandle, NEXT_PROCESS_OUTPUT_HANDLE,
};
use crate::collections::list::PithList;
use crate::ffi_util::{with_cstr_str, with_cstr_str_or_empty};
use crate::handle_registry::{self, HandleKind};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;

unsafe fn pith_optional_cstring(ptr: *const i8) -> String {
    // SAFETY: Caller guarantees `ptr` is null or a valid null-terminated C string.
    unsafe { with_cstr_str_or_empty(ptr, str::to_owned) }
}

unsafe fn pith_required_cstring(ptr: *const i8) -> Option<String> {
    // SAFETY: Caller guarantees `ptr` is null or a valid null-terminated C string.
    unsafe { with_cstr_str(ptr, |text| (!text.is_empty()).then(|| text.to_owned())) }.flatten()
}

unsafe fn pith_string_list_to_vec(list: PithList) -> Vec<String> {
    let len = crate::collections::list::pith_list_len(list);
    let mut values = Vec::with_capacity(len.max(0) as usize);
    let mut i = 0;
    while i < len {
        // SAFETY: `i` is in `0..len` for `list`; caller guarantees elements are valid C strings or null.
        let ptr = unsafe { crate::collections::list::pith_list_get_value(list, i) } as *const i8;
        // SAFETY: Preconditions forwarded from caller.
        values.push(unsafe { pith_optional_cstring(ptr) });
        i += 1;
    }
    values
}

pub(super) fn pith_store_process_output(status: i64, stdout: String, stderr: String) -> i64 {
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
fn build_command_from_parts(
    program_text: String,
    args: Vec<String>,
    cwd_text: String,
    keys: Vec<String>,
    values: Vec<String>,
) -> Command {
    let mut command = Command::new(program_text);
    // before the per-command overrides below, so an override the caller named
    // wins over what the program set process-wide.
    crate::env_overlay::apply(&mut command);

    for arg in args {
        command.arg(arg);
    }

    if !cwd_text.is_empty() {
        command.current_dir(cwd_text);
    }

    for (key, value) in keys.into_iter().zip(values.into_iter()) {
        command.env(key, value);
    }

    command
}

unsafe fn pith_build_command(
    program: *const i8,
    argv: PithList,
    cwd: *const i8,
    env_keys: PithList,
    env_values: PithList,
) -> Option<Command> {
    // SAFETY: Caller guarantees `program`, `cwd`, and all list elements are valid C strings or null.
    let program_text = unsafe { pith_required_cstring(program) }?;
    // SAFETY: Preconditions forwarded from caller.
    let args = unsafe { pith_string_list_to_vec(argv) };
    // SAFETY: Preconditions forwarded from caller.
    let cwd_text = unsafe { pith_optional_cstring(cwd) };
    // SAFETY: Preconditions forwarded from caller.
    let keys = unsafe { pith_string_list_to_vec(env_keys) };
    // SAFETY: Preconditions forwarded from caller.
    let values = unsafe { pith_string_list_to_vec(env_values) };
    Some(build_command_from_parts(program_text, args, cwd_text, keys, values))
}

fn process_spawn_shell(cmd_str: &str) -> i64 {
    let mut command = Command::new("/bin/sh");
    crate::env_overlay::apply(&mut command);
    match command
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

fn spawn_piped_command(mut command: Command) -> i64 {
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

fn run_output_command(command: Command) -> i64 {
    // the child runs to completion on a pool thread (or inline, off green); the
    // pith-visible handle is built here either way.
    match command_output(command) {
        Some(output) => {
            let status = exit_code(output.status);
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            pith_store_process_output(status, stdout, stderr)
        }
        None => 0,
    }
}

/// Spawn a child process and return a process handle
///
/// # Safety
/// cmd must be a valid null-terminated C string
#[no_mangle]
pub unsafe extern "C" fn pith_process_spawn(cmd: *const i8) -> i64 {
    // SAFETY: Caller guarantees `cmd` is null or a valid null-terminated C string.
    unsafe { with_cstr_str(cmd, process_spawn_shell) }.unwrap_or(0)
}

#[no_mangle]
pub unsafe extern "C" fn pith_process_spawn_argv(
    program: *const i8,
    argv: PithList,
    cwd: *const i8,
    env_keys: PithList,
    env_values: PithList,
) -> i64 {
    // SAFETY: Caller guarantees `program`, `cwd`, and all list elements are valid C strings or null.
    let Some(command) = (unsafe { pith_build_command(program, argv, cwd, env_keys, env_values) }) else {
        return 0;
    };
    spawn_piped_command(command)
}

#[no_mangle]
pub unsafe extern "C" fn pith_process_output_argv(
    program: *const i8,
    argv: PithList,
    cwd: *const i8,
    env_keys: PithList,
    env_values: PithList,
) -> i64 {
    // SAFETY: Caller guarantees `program`, `cwd`, and all list elements are valid C strings or null.
    let Some(command) = (unsafe { pith_build_command(program, argv, cwd, env_keys, env_values) }) else {
        return 0;
    };
    run_output_command(command)
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
    crate::pith_strdup_string(&entry.stdout)
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
    crate::pith_strdup_string(&entry.stderr)
}

fn exec_output_impl(cmd_str: &str) -> *mut i8 {
    let parts: Vec<&str> = cmd_str.split_whitespace().collect();
    if parts.is_empty() {
        return std::ptr::null_mut();
    }
    let mut command = std::process::Command::new(parts[0]);
    crate::env_overlay::apply(&mut command);
    command.args(&parts[1..]);
    // only stdout comes back, so there is no status to carry a signal death
    // in. a child that a signal ended fails the call rather than handing back
    // whatever it wrote before it died as if it had finished. an ordinary
    // nonzero exit still returns its stdout, as it always has.
    match crate::process::command_output(command) {
        Some(output) if output.status.code().is_some() => {
            crate::pith_copy_bytes_to_cstring(&output.stdout)
        }
        _ => std::ptr::null_mut(),
    }
}

/// Run a command, split on whitespace with no shell, and return its stdout
/// as a C string, or null when it could not run.
///
/// the child runs to completion via the process pool (see `process`), so a
/// green worker is not held for however long the command takes.
#[no_mangle]
pub unsafe extern "C" fn pith_exec_output(cmd: *const i8) -> *mut i8 {
    // SAFETY: `cmd` is null or a valid null-terminated C string.
    unsafe { with_cstr_str(cmd, exec_output_impl) }.unwrap_or(std::ptr::null_mut())
}
