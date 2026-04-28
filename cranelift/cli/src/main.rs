//! Pith CLI - native compilation with Cranelift
//!
//! Pipeline: source → self-hosted parse+emit_ir → text IR → ir_consumer.rs → Cranelift → native

use std::env;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        print_usage();
        std::process::exit(1);
    }

    match args[1].as_str() {
        "build" => {
            if args.len() < 3 {
                eprintln!("Error: build requires a file argument");
                std::process::exit(1);
            }
            build_file(&args[2]);
        }
        "run" => {
            if args.len() < 3 {
                eprintln!("Error: run requires a file argument");
                std::process::exit(1);
            }
            run_file(&args[2]);
        }
        "test" => {
            if args.len() < 3 {
                eprintln!("Error: test requires a file argument");
                std::process::exit(1);
            }
            test_file(&args[2]);
        }
        "check" => {
            if args.len() < 3 {
                eprintln!("Error: check requires a file argument");
                std::process::exit(1);
            }
            check_file(&args[2]);
        }
        "parse" => {
            if args.len() < 3 {
                eprintln!("Error: parse requires a file argument");
                std::process::exit(1);
            }
            parse_file(&args[2]);
        }
        "lex" => {
            if args.len() < 3 {
                eprintln!("Error: lex requires a file argument");
                std::process::exit(1);
            }
            lex_file(&args[2]);
        }
        "version" => {
            println!("Pith Cranelift Compiler v0.2.2");
            println!("Using IR path: source → ir_emitter.pith → ir_consumer.rs → native");
        }
        "fmt" | "lint" | "doc" | "new" | "package" => {
            delegate_to_frontend(&args[1..]);
        }
        "help" | "--help" | "-h" => {
            print_usage();
        }
        _ => {
            eprintln!("Unknown command: {}", args[1]);
            print_usage();
            std::process::exit(1);
        }
    }
}

fn print_usage() {
    println!("Usage: pith <command> [args...]");
    println!();
    println!("Commands:");
    println!("  build <file.pith>    Compile .pith file to native binary");
    println!("  run <file.pith>      Compile and run immediately");
    println!("  test <file.pith>     Compile and run tests");
    println!("  check <file.pith>    Type-check without generating code");
    println!("  fmt [args...]      Format source files");
    println!("  lint [args...]     Lint source files");
    println!("  doc [args...]      Generate or search documentation");
    println!("  package [args...]  Run package-level check/test/lint/doc");
    println!("  new [args...]      Create a new project");
    println!("  parse <file.pith>    Parse and display AST");
    println!("  lex <file.pith>      Tokenize and display token stream");
    println!("  version            Display version information");
    println!("  help               Show this help message");
    println!();
    println!("Environment:");
    println!("  PITH_SELF_HOST    Path to self-hosted compiler (default: ./self-host/pith_main)");
    println!("  PITH_DUMP_IR      Path to dump combined IR text (for debugging)");
}

fn dump_ir_if_requested(ir_text: &str) {
    if let Ok(dump_path) = env::var("PITH_DUMP_IR") {
        let _ = fs::write(&dump_path, ir_text);
        eprintln!("IR dumped to {} ({} bytes)", dump_path, ir_text.len());
    }
}

fn combined_output(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("{}{}", stdout, stderr)
}

/// Find the self-hosted compiler executable
fn find_self_hosted_compiler() -> Option<String> {
    if let Ok(path) = env::var("PITH_SELF_HOST") {
        if Path::new(&path).exists() {
            return Some(path);
        }
    }

    let candidates = [
        "./self-host/pith_main",
        "../self-host/pith_main",
        "./pith_main",
    ];

    for candidate in &candidates {
        if Path::new(candidate).exists() {
            return Some(candidate.to_string());
        }
    }

    None
}

/// Get AST from self-hosted compiler by running 'pith parse'
fn get_ast_from_compiler(path: &str) -> Result<String, String> {
    let compiler = find_self_hosted_compiler()
        .ok_or("Self-hosted compiler not found. Set PITH_SELF_HOST or ensure ./self-host/pith_main exists")?;

    let output = Command::new(&compiler)
        .args(["parse", path])
        .output()
        .map_err(|e| format!("Failed to run compiler: {}", e))?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{}{}", stdout, stderr);
        if combined.trim().is_empty() {
            return Err("Parse error".to_string());
        }
        return Err(combined);
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Get tokens from self-hosted compiler by running 'pith lex'
fn get_tokens_from_compiler(path: &str) -> Result<String, String> {
    let compiler = find_self_hosted_compiler().ok_or("Self-hosted compiler not found")?;

    let output = Command::new(&compiler)
        .args(["lex", path])
        .output()
        .map_err(|e| format!("Failed to run compiler: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Lex error: {}", stderr));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Find the pre-compiled IR driver binary
fn find_ir_driver() -> Option<String> {
    if let Ok(path) = env::var("PITH_IR_DRIVER") {
        if Path::new(&path).exists() {
            return Some(path);
        }
    }

    for candidate in &["./self-host/ir_driver", "../self-host/ir_driver"] {
        if Path::new(candidate).exists() {
            return Some(candidate.to_string());
        }
    }
    None
}

fn get_ir_from_compiler(path: &str, emit_tests: bool) -> Result<String, String> {
    let driver =
        find_ir_driver().ok_or("No IR driver found. Ensure ./self-host/ir_driver exists.")?;
    let mut command = Command::new(&driver);
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
        if combined.trim().is_empty() {
            return Err(format!("IR driver failed on {}", path));
        }
        return Err(format!("IR driver failed on {}: {}", path, combined));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn validate_combined_ir_contract(ir_text: &str) -> Result<(), String> {
    for (index, line) in ir_text.lines().enumerate() {
        let line_no = index + 1;
        if line.starts_with("call ") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 5 {
                return Err(format!(
                    "IR contract error on line {}: malformed call",
                    line_no
                ));
            }
            if parts[3].parse::<usize>().is_ok() || parts[4].parse::<usize>().is_err() {
                return Err(format!(
                    "IR contract error on line {}: call instructions must include an explicit return kind",
                    line_no
                ));
            }
        } else if line.starts_with("field ") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 4 || parts[3].parse::<i32>().is_err() {
                return Err(format!(
                    "IR contract error on line {}: field instructions must include an explicit offset",
                    line_no
                ));
            }
        }
    }
    Ok(())
}

fn compile_to_object(path: &str, emit_tests: bool) -> Result<(Vec<u8>, usize), String> {
    use pith_codegen::create_codegen;
    use pith_codegen::finalize_module;
    use pith_codegen::ir_consumer::compile_from_ir;

    let ir_text =
        get_ir_from_compiler(path, emit_tests).map_err(|e| format!("Error getting IR: {}", e))?;
    validate_combined_ir_contract(&ir_text)?;

    dump_ir_if_requested(&ir_text);

    let mut codegen = create_codegen().map_err(|e| format!("Error creating codegen: {}", e))?;
    let runtime_funcs = pith_codegen::declare_runtime_functions(&mut codegen.module)
        .map_err(|e| format!("Error declaring runtime: {}", e))?;
    let funcs = compile_from_ir(&mut codegen, &ir_text, &runtime_funcs)
        .map_err(|e| format!("Error compiling: {}", e))?;
    let bytes = finalize_module(codegen.module).map_err(|e| format!("Error finalizing: {}", e))?;
    Ok((bytes, funcs.len()))
}

fn build_file(path: &str) {
    use pith_codegen::linker::build_executable;

    let (bytes, func_count) = match compile_to_object(path, false) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };

    eprintln!("Compiled {} functions", func_count);
    let obj_path = path.replace(".pith", ".o");
    let exe_path = path.replace(".pith", "");
    if let Err(e) = fs::write(&obj_path, &bytes) {
        eprintln!("Error writing object: {}", e);
        std::process::exit(1);
    }
    match build_executable(&obj_path, &exe_path) {
        Ok(_) => eprintln!("Created: {}", exe_path),
        Err(e) => {
            eprintln!("Error linking: {}", e);
            std::process::exit(1);
        }
    }
}

fn unique_run_artifact_paths() -> (std::path::PathBuf, std::path::PathBuf) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_else(|_| std::time::Duration::from_secs(0))
        .as_nanos();
    let exe_path = env::temp_dir().join(format!("pith_ir_{}_{}", std::process::id(), stamp));
    let obj_path = env::temp_dir().join(format!("pith_ir_{}_{}.o", std::process::id(), stamp));
    (obj_path, exe_path)
}

fn run_file(path: &str) {
    use pith_codegen::linker::build_executable;

    let (bytes, _) = match compile_to_object(path, false) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };

    let (obj_path, exe_path) = unique_run_artifact_paths();
    let keep_artifacts = std::env::var("PITH_KEEP_RUN_ARTIFACTS").is_ok();
    if let Err(e) = fs::write(&obj_path, &bytes) {
        eprintln!("Error writing object: {}", e);
        std::process::exit(1);
    }
    match build_executable(&obj_path.to_string_lossy(), &exe_path.to_string_lossy()) {
        Ok(_) => {
            let status = Command::new(&exe_path).status();
            if !keep_artifacts {
                let _ = fs::remove_file(&obj_path);
                let _ = fs::remove_file(&exe_path);
            }
            match status {
                Ok(s) => {
                    if !s.success() {
                        std::process::exit(s.code().unwrap_or(1));
                    }
                }
                Err(e) => {
                    eprintln!("Error running {}: {}", exe_path.display(), e);
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("Error linking {}: {}", exe_path.display(), e);
            std::process::exit(1);
        }
    }
}

fn test_file(path: &str) {
    use pith_codegen::linker::build_executable;

    let (bytes, _) = match compile_to_object(path, true) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };

    let (obj_path, exe_path) = unique_run_artifact_paths();
    let keep_artifacts = std::env::var("PITH_KEEP_RUN_ARTIFACTS").is_ok();
    if let Err(e) = fs::write(&obj_path, &bytes) {
        eprintln!("Error writing object: {}", e);
        std::process::exit(1);
    }
    match build_executable(&obj_path.to_string_lossy(), &exe_path.to_string_lossy()) {
        Ok(_) => {
            let status = Command::new(&exe_path).status();
            if !keep_artifacts {
                let _ = fs::remove_file(&obj_path);
                let _ = fs::remove_file(&exe_path);
            }
            match status {
                Ok(s) => {
                    if !s.success() {
                        std::process::exit(s.code().unwrap_or(1));
                    }
                }
                Err(e) => {
                    eprintln!("Error running {}: {}", exe_path.display(), e);
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("Error linking {}: {}", exe_path.display(), e);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::validate_combined_ir_contract;

    #[test]
    fn accepts_explicit_call_and_field_contracts() {
        let ir = "func main 0 unknown\ncall 1 greet string 1 0\nfield 2 1 0 Point name\n";
        assert!(validate_combined_ir_contract(ir).is_ok());
    }

    #[test]
    fn rejects_legacy_call_without_retkind() {
        let ir = "call 1 greet 1 0\n";
        assert!(validate_combined_ir_contract(ir).is_err());
    }

    #[test]
    fn rejects_field_without_offset() {
        let ir = "field 2 1 Point name\n";
        assert!(validate_combined_ir_contract(ir).is_err());
    }
}

fn check_file(path: &str) {
    let compiler = match find_self_hosted_compiler() {
        Some(c) => c,
        None => {
            eprintln!("Self-hosted compiler not found");
            std::process::exit(1);
        }
    };

    let output = Command::new(&compiler)
        .args(["check", path])
        .output()
        .unwrap_or_else(|e| {
            eprintln!("Failed to run self-hosted compiler: {}", e);
            std::process::exit(1);
        });

    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        std::process::exit(output.status.code().unwrap_or(1));
    }
}

fn parse_file(path: &str) {
    match get_ast_from_compiler(path) {
        Ok(ast) => println!("{}", ast),
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    }
}

fn lex_file(path: &str) {
    match get_tokens_from_compiler(path) {
        Ok(tokens) => println!("{}", tokens),
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    }
}

fn delegate_to_frontend(args: &[String]) {
    let compiler = match find_self_hosted_compiler() {
        Some(c) => c,
        None => {
            eprintln!("Self-hosted compiler not found");
            std::process::exit(1);
        }
    };

    let mut command = Command::new(&compiler);
    command.args(args);
    if let Ok(current_exe) = env::current_exe() {
        if env::var("PITH_IR_DRIVER").is_err() {
            if let Some(root) = current_exe
                .parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.parent())
            {
                let ir_driver = root.join("self-host").join("ir_driver");
                if ir_driver.exists() {
                    command.env("PITH_IR_DRIVER", ir_driver);
                }
            }
        }
        command.env("PITH_NATIVE", current_exe);
    }
    let status = command.status().unwrap_or_else(|e| {
        eprintln!("Failed to run self-hosted compiler: {}", e);
        std::process::exit(1);
    });

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
}
