//! Linking and executable generation
//!
//! Links object files with the runtime library to create executables.

use crate::CompileError;
use std::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeKind {
    Rust,
    Zig,
}

/// Link object files with runtime to create executable
pub fn link_executable(
    obj_file: &str,
    runtime_lib: &str,
    output: &str,
) -> Result<(), CompileError> {
    // Use gcc as the linker
    let mut cmd = Command::new("gcc");

    cmd.arg("-o")
        .arg(output)
        .arg(obj_file)
        .arg(runtime_lib)
        .arg("-lpthread") // Required by our runtime
        .arg("-ldl") // Required by our runtime
        .arg("-lm"); // Math library

    let output_result = cmd
        .output()
        .map_err(|e| CompileError::ModuleError(format!("Failed to run linker: {}", e)))?;

    if !output_result.status.success() {
        let stderr = String::from_utf8_lossy(&output_result.stderr);
        return Err(CompileError::ModuleError(format!(
            "Linking failed: {}",
            stderr
        )));
    }

    Ok(())
}

fn runtime_lib_path_candidates(kind: RuntimeKind) -> [&'static str; 2] {
    match kind {
        RuntimeKind::Rust => [
            "target/release/libforge_runtime.a",
            "../target/release/libforge_runtime.a",
        ],
        RuntimeKind::Zig => [
            "cranelift/runtime-zig/zig-out/lib/libforge_runtime_zig.a",
            "../cranelift/runtime-zig/zig-out/lib/libforge_runtime_zig.a",
        ],
    }
}

/// Get the path to the runtime static library
pub fn get_runtime_lib_path(kind: RuntimeKind) -> String {
    for candidate in runtime_lib_path_candidates(kind) {
        if std::path::Path::new(candidate).exists() {
            return candidate.to_string();
        }
    }
    runtime_lib_path_candidates(kind)[0].to_string()
}

/// Rebuild the rust runtime static library if sources are newer than the .a file.
/// This ensures `forge run` always links against an up-to-date runtime.
fn rebuild_rust_runtime_if_stale(runtime_lib: &str) {
    // Find the forge executable to determine the workspace root
    let exe = std::env::current_exe().unwrap_or_default();
    let workspace_root = exe
        .parent() // target/release
        .and_then(|p| p.parent()) // target
        .and_then(|p| p.parent()) // workspace root
        .map(|p| p.to_path_buf())
        .unwrap_or_default();

    let runtime_src = workspace_root.join("cranelift/runtime/src");

    // Check if runtime lib is older than any source file
    let lib_mtime = std::fs::metadata(runtime_lib)
        .and_then(|m| m.modified())
        .ok();

    let needs_rebuild = if let Some(lib_time) = lib_mtime {
        // Check if any .rs file in runtime/src is newer
        walkdir_check_newer(&runtime_src, lib_time)
    } else {
        true // lib doesn't exist, rebuild
    };

    if needs_rebuild {
        let _ = Command::new("cargo")
            .args(["build", "--release", "-p", "forge-runtime"])
            .current_dir(&workspace_root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

fn rebuild_zig_runtime() {
    let exe = std::env::current_exe().unwrap_or_default();
    let workspace_root = exe
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let runtime_root = workspace_root.join("cranelift/runtime-zig");
    let _ = Command::new("zig")
        .args(["build", "-Doptimize=ReleaseFast"])
        .current_dir(runtime_root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn walkdir_check_newer(dir: &std::path::Path, lib_time: std::time::SystemTime) -> bool {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if walkdir_check_newer(&path, lib_time) {
                    return true;
                }
            } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
                if let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) {
                    if mtime > lib_time {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Complete build: compile and link
pub fn build_executable(
    obj_file: &str,
    output: &str,
    runtime_kind: RuntimeKind,
) -> Result<(), CompileError> {
    let runtime_lib = get_runtime_lib_path(runtime_kind);

    match runtime_kind {
        RuntimeKind::Rust => rebuild_rust_runtime_if_stale(&runtime_lib),
        RuntimeKind::Zig => rebuild_zig_runtime(),
    }

    if !std::path::Path::new(&runtime_lib).exists() {
        return Err(CompileError::ModuleError(format!(
            "Runtime library not found at {}",
            runtime_lib
        )));
    }

    link_executable(obj_file, &runtime_lib, output)?;

    Ok(())
}
