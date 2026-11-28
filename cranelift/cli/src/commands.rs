//! Top-level CLI command handlers returning structured exit codes / errors
//! so RAII guards always clean up before process exit.

use crate::args::{print_usage, Command as CliCommand, TestOpts};
use crate::driver::{resolve_driver_for_frontend, stamp_if_ir_driver};
use crate::pipeline::{
    compile_to_object, default_exe_path, object_from_ir, readable_source, write_and_link,
};
use crate::temp_run::{child_exit_code, run_temp_paths, TempBuildDir};
use crate::toolchain::{apply_pith_home, find_self_hosted_compiler, run_frontend_capture};
use std::env;
use std::fs;
use std::process::Command;

pub(crate) fn run(command: CliCommand) -> Result<i32, String> {
    match command {
        CliCommand::Build { path, output } => {
            cmd_build(&path, output.as_deref())?;
            Ok(0)
        }
        CliCommand::BuildIr { ir_path, output } => {
            cmd_build_ir(&ir_path, &output)?;
            Ok(0)
        }
        CliCommand::Run { path } => compile_and_run(&path, None),
        CliCommand::Test(opts) => compile_and_run(&opts.file, Some(&opts)),
        CliCommand::Frontend(args) => delegate_to_frontend(&args),
        CliCommand::Parse { path } => {
            let ast = readable_source(&path).and_then(|_| run_frontend_capture("parse", &path))?;
            println!("{}", ast);
            Ok(0)
        }
        CliCommand::Lex { path } => {
            let tokens = readable_source(&path).and_then(|_| run_frontend_capture("lex", &path))?;
            println!("{}", tokens);
            Ok(0)
        }
        CliCommand::Version => {
            println!("pith {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        CliCommand::Help => {
            print_usage();
            Ok(0)
        }
    }
}

fn finish_build(bytes: &[u8], func_count: usize, exe_path: &str) -> Result<(), String> {
    eprintln!("Compiled {} functions", func_count);
    write_and_link(bytes, &format!("{}.o", exe_path), exe_path)?;
    stamp_if_ir_driver(exe_path);
    Ok(())
}

/// Compile `path` to an executable at `output`, or next to the source
/// without its `.pith` when no output is given. The object file is written
/// beside the executable as `<exe>.o`.
fn cmd_build(path: &str, output: Option<&str>) -> Result<(), String> {
    let exe_path = output.or_else(|| default_exe_path(path)).ok_or_else(|| {
        format!(
            "Error: {} does not end in .pith, so there is no default output name; pass -o <out>",
            path
        )
    })?;
    let (bytes, func_count) = compile_to_object(path, false)?;
    finish_build(&bytes, func_count, exe_path)
}

/// Compile a combined IR text file straight to a native binary. Pairs with
/// PITH_DUMP_IR (which writes such a file) and backs the bootstrap seed.
fn cmd_build_ir(ir_path: &str, exe_path: &str) -> Result<(), String> {
    let ir_text =
        fs::read_to_string(ir_path).map_err(|e| format!("Error reading {}: {}", ir_path, e))?;
    let (bytes, func_count) = object_from_ir(&ir_text)?;
    finish_build(&bytes, func_count, exe_path)
}

/// Compile `path` (with its tests when `test_opts` is `Some`) to a temporary
/// binary, run it, and return its exit status. The binary and object are built
/// in a private directory removed automatically on drop unless
/// `PITH_KEEP_RUN_ARTIFACTS` is set.
fn compile_and_run(path: &str, test_opts: Option<&TestOpts>) -> Result<i32, String> {
    use pith_codegen::linker::build_executable;

    let (bytes, _) = compile_to_object(path, test_opts.is_some())?;
    let keep_artifacts = env::var("PITH_KEEP_RUN_ARTIFACTS").is_ok();
    let temp_root = env::temp_dir();
    let dir = TempBuildDir::create(&temp_root, keep_artifacts).map_err(|e| {
        format!(
            "Error creating a build directory in {}: {}",
            temp_root.display(),
            e
        )
    })?;

    let (exe_path, obj_path) = run_temp_paths(dir.path(), path);
    fs::write(&obj_path, &bytes).map_err(|e| format!("Error writing object: {}", e))?;
    build_executable(&obj_path.to_string_lossy(), &exe_path.to_string_lossy())
        .map_err(|e| format!("Error linking {}: {}", exe_path.display(), e))?;

    let mut cmd = Command::new(&exe_path);
    apply_pith_home(&mut cmd);
    if let Some(opts) = test_opts {
        opts.apply_env(&mut cmd);
    }
    let status = match cmd.spawn() {
        Ok(mut child) => {
            // Enter IgnoreSigint strictly AFTER spawn so the child inherits SIG_DFL.
            #[cfg(unix)]
            let _sig_guard = crate::temp_run::unix_sig::IgnoreSigint::enter();
            child.wait()
        }
        Err(e) => Err(e),
    };
    drop(dir);

    match status {
        Ok(s) if s.success() => Ok(0),
        Ok(s) => Ok(child_exit_code(path, &s)),
        Err(e) => Err(format!("Error running {}: {}", exe_path.display(), e)),
    }
}

fn delegate_to_frontend(args: &[String]) -> Result<i32, String> {
    let compiler = find_self_hosted_compiler().map_err(|e| format!("Error: {}", e))?;

    let mut command = Command::new(&compiler);
    apply_pith_home(&mut command);
    command.args(args);
    if let Ok(current_exe) = env::current_exe() {
        if let Some(driver) = resolve_driver_for_frontend() {
            command.env("PITH_IR_DRIVER", driver);
        }
        command.env("PITH_NATIVE", current_exe);
    }
    let status = command
        .status()
        .map_err(|e| format!("Failed to run self-hosted compiler: {}", e))?;

    if status.success() {
        Ok(0)
    } else {
        Ok(status.code().unwrap_or(1))
    }
}
