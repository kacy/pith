//! Linking and executable generation
//!
//! Links object files with the runtime library to create executables.

use crate::CompileError;
use std::process::Command;

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

/// Get the path to the runtime static library
pub fn get_runtime_lib_path() -> String {
    // In development, use target/release
    if std::path::Path::new("target/release/libforge_runtime.a").exists() {
        "target/release/libforge_runtime.a".to_string()
    } else if std::path::Path::new("../target/release/libforge_runtime.a").exists() {
        "../target/release/libforge_runtime.a".to_string()
    } else {
        // Try to find it
        "target/release/libforge_runtime.a".to_string()
    }
}

/// Complete build: compile and link
pub fn build_executable(obj_file: &str, output: &str) -> Result<(), CompileError> {
    let runtime_lib = get_runtime_lib_path();

    if !std::path::Path::new(&runtime_lib).exists() {
        return Err(CompileError::ModuleError(format!(
            "Runtime library not found at {}",
            runtime_lib
        )));
    }

    link_executable(obj_file, &runtime_lib, output)?;

    Ok(())
}
