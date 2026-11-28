//! Source → text IR → contract validation → Cranelift object → linked binary.

use crate::driver::{bootstrap_ir_driver_from_seed, find_ir_driver, ir_runtime_abi, IR_ABI_PREFIX};
use crate::temp_run::describe_exit;
use crate::toolchain::{apply_pith_home, combined_output};
use pith_codegen::runtime_imports::RUNTIME_ABI;
use std::env;
use std::fs;
use std::process::Command;

pub(crate) fn dump_ir_if_requested(ir_text: &str) {
    if let Ok(dump_path) = env::var("PITH_DUMP_IR") {
        let stamped;
        let ir_text = if ir_runtime_abi(ir_text).is_some() {
            ir_text
        } else {
            stamped = format!("{}{}\n{}", IR_ABI_PREFIX, RUNTIME_ABI, ir_text);
            &stamped
        };
        let _ = fs::write(&dump_path, ir_text);
        eprintln!("IR dumped to {} ({} bytes)", dump_path, ir_text.len());
    }
}

pub(crate) fn get_ir_from_compiler(path: &str, emit_tests: bool) -> Result<String, String> {
    let driver = match find_ir_driver(path)? {
        Some(driver) => driver,
        None => bootstrap_ir_driver_from_seed()?,
    };
    let mut command = Command::new(&driver);
    apply_pith_home(&mut command);
    command.arg("--combined");
    if emit_tests {
        command.arg("--tests");
    }
    let output = command
        .arg(path)
        .output()
        .map_err(|e| format!("run ir_driver: {}", e))?;
    if !output.status.success() {
        let combined = combined_output(&output);
        let status = describe_exit(&output.status);
        if combined.trim().is_empty() {
            return Err(format!(
                "IR driver {} on {} with no diagnostics — this is a compiler bug, not a problem in your program",
                status, path
            ));
        }
        return Err(format!("IR driver failed on {}: {}", path, combined));
    }
    let ir = String::from_utf8_lossy(&output.stdout).to_string();
    // a clean exit with no emitted functions means the driver died
    // silently (an emitter bug); linking would fail later with a
    // baffling "undefined reference to main"
    if !ir.lines().any(|l| l.starts_with("func ")) {
        return Err(format!(
            "IR driver exited cleanly but emitted no functions for {} ({} bytes of IR) — this is a compiler bug, not a problem in your program",
            path,
            ir.len()
        ));
    }
    Ok(ir)
}

/// Validate combined IR call/field contract without allocating per-line token vectors.
pub(crate) fn validate_combined_ir_contract(ir_text: &str) -> Result<(), String> {
    for (index, raw_line) in ir_text.lines().enumerate() {
        let line_no = index + 1;
        let line = raw_line.trim();
        if line.starts_with("call ") {
            let mut parts = line.split_whitespace();
            let _op = parts.next();
            let _dst = parts.next();
            let _callee = parts.next();
            let Some(retkind) = parts.next() else {
                return Err(format!(
                    "IR contract error on line {}: malformed call",
                    line_no
                ));
            };
            let Some(nargs) = parts.next() else {
                return Err(format!(
                    "IR contract error on line {}: malformed call",
                    line_no
                ));
            };
            if retkind.parse::<usize>().is_ok() || nargs.parse::<usize>().is_err() {
                return Err(format!(
                    "IR contract error on line {}: call instructions must include an explicit return kind",
                    line_no
                ));
            }
        } else if line.starts_with("field ") {
            let mut parts = line.split_whitespace();
            let _op = parts.next();
            let _dst = parts.next();
            let _obj = parts.next();
            let offset = parts.next();
            if offset.is_none_or(|tok| tok.parse::<i32>().is_err()) {
                return Err(format!(
                    "IR contract error on line {}: field instructions must include an explicit offset",
                    line_no
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn compile_to_object(path: &str, emit_tests: bool) -> Result<(Vec<u8>, usize), String> {
    // the ir driver says nothing when it cannot read its input, which would
    // be reported as a compiler bug, so a missing file is caught here
    readable_source(path)?;
    let ir_text =
        get_ir_from_compiler(path, emit_tests).map_err(|e| format!("Error getting IR: {}", e))?;
    object_from_ir(&ir_text)
}

/// Ok when `path` is a regular file this process can open for reading.
/// `check_readable_file` with an error that names the file.
pub(crate) fn readable_source(path: &str) -> Result<(), String> {
    check_readable_file(path).map_err(|e| format!("Error reading {}: {}", path, e))
}

pub(crate) fn check_readable_file(path: &str) -> std::io::Result<()> {
    if fs::File::open(path)?.metadata()?.is_dir() {
        return Err(std::io::ErrorKind::IsADirectory.into());
    }
    Ok(())
}

pub(crate) fn object_from_ir(ir_text: &str) -> Result<(Vec<u8>, usize), String> {
    use pith_codegen::create_codegen;
    use pith_codegen::finalize_module;
    use pith_codegen::ir_consumer::compile_from_ir;

    // dump before validating: the dump exists to debug bad IR, so it must
    // fire even when (especially when) the contract check rejects it
    dump_ir_if_requested(ir_text);

    if let Some(abi) = ir_runtime_abi(ir_text) {
        if abi != RUNTIME_ABI {
            return Err(format!(
                "this IR was compiled against runtime ABI {}, and this pith was built against {}: \
                 rebuild pith from the checkout the IR came from (cargo build --release)",
                abi, RUNTIME_ABI
            ));
        }
    }
    validate_combined_ir_contract(ir_text)?;

    let mut codegen = create_codegen().map_err(|e| format!("Error creating codegen: {}", e))?;
    let runtime_funcs = pith_codegen::declare_runtime_functions(&mut codegen.module)
        .map_err(|e| format!("Error declaring runtime: {}", e))?;
    let funcs = compile_from_ir(&mut codegen, ir_text, &runtime_funcs)
        .map_err(|e| format!("Error compiling: {}", e))?;
    let bytes = finalize_module(codegen.module).map_err(|e| format!("Error finalizing: {}", e))?;
    Ok((bytes, funcs.len()))
}

/// The executable `pith build` writes for `path`: the path without its
/// trailing `.pith`. Only the last extension goes, so a directory named
/// `foo.pith` keeps its name. None when nothing would be left of the file
/// name, or when there is no `.pith` to strip and the output would
/// overwrite the source.
pub(crate) fn default_exe_path(path: &str) -> Option<&str> {
    path.strip_suffix(".pith")
        .filter(|stem| !stem.is_empty() && !stem.ends_with('/'))
}

/// Write an object file and link it into `exe_path`.
pub(crate) fn write_and_link(bytes: &[u8], obj_path: &str, exe_path: &str) -> Result<(), String> {
    use pith_codegen::linker::build_executable;

    fs::write(obj_path, bytes).map_err(|e| format!("Error writing object: {}", e))?;
    build_executable(obj_path, exe_path).map_err(|e| format!("Error linking: {}", e))?;
    eprintln!("Created: {}", exe_path);
    Ok(())
}
