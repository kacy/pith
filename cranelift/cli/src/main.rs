//! Forge CLI - native compilation with Cranelift
//!
//! Pipeline: source → self-hosted parse+emit_ir → text IR → ir_consumer.rs → Cranelift → native

use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        print_usage();
        return;
    }

    match args[1].as_str() {
        "build" | "ir-build" => {
            if args.len() < 3 {
                eprintln!("Error: build requires a file argument");
                return;
            }
            build_file(&args[2]);
        }
        "run" | "ir-run" => {
            if args.len() < 3 {
                eprintln!("Error: run requires a file argument");
                return;
            }
            run_file(&args[2]);
        }
        "test" => {
            if args.len() < 3 {
                eprintln!("Error: test requires a file argument");
                return;
            }
            test_file(&args[2]);
        }
        "check" => {
            if args.len() < 3 {
                eprintln!("Error: check requires a file argument");
                return;
            }
            check_file(&args[2]);
        }
        "parse" => {
            if args.len() < 3 {
                eprintln!("Error: parse requires a file argument");
                return;
            }
            parse_file(&args[2]);
        }
        "lex" => {
            if args.len() < 3 {
                eprintln!("Error: lex requires a file argument");
                return;
            }
            lex_file(&args[2]);
        }
        "version" => {
            println!("Forge Cranelift Compiler v0.2.0");
            println!("Using IR path: source → ir_emitter.fg → ir_consumer.rs → native");
        }
        "fmt" | "lint" | "doc" | "new" => {
            delegate_to_frontend(&args[1..]);
        }
        "help" | "--help" | "-h" => {
            print_usage();
        }
        _ => {
            eprintln!("Unknown command: {}", args[1]);
            print_usage();
        }
    }
}

fn print_usage() {
    println!("Usage: forge <command> [args...]");
    println!();
    println!("Commands:");
    println!("  build <file.fg>    Compile .fg file to native binary");
    println!("  run <file.fg>      Compile and run immediately");
    println!("  test <file.fg>     Compile and run tests");
    println!("  check <file.fg>    Type-check without generating code");
    println!("  fmt [args...]      Format source files");
    println!("  lint [args...]     Lint source files");
    println!("  doc [args...]      Generate or search documentation");
    println!("  new [args...]      Create a new project");
    println!("  parse <file.fg>    Parse and display AST");
    println!("  lex <file.fg>      Tokenize and display token stream");
    println!("  version            Display version information");
    println!("  help               Show this help message");
    println!();
    println!("Environment:");
    println!("  FORGE_SELF_HOST    Path to self-hosted compiler (default: ./self-host/forge_main)");
}

/// Find the self-hosted compiler executable
fn find_self_hosted_compiler() -> Option<String> {
    if let Ok(path) = env::var("FORGE_SELF_HOST") {
        if Path::new(&path).exists() {
            return Some(path);
        }
    }

    let candidates = [
        "./self-host/forge_main",
        "../self-host/forge_main",
        "./forge_main",
    ];

    for candidate in &candidates {
        if Path::new(candidate).exists() {
            return Some(candidate.to_string());
        }
    }

    None
}

/// Get AST from self-hosted compiler by running 'forge parse'
fn get_ast_from_compiler(path: &str) -> Result<String, String> {
    let compiler = find_self_hosted_compiler()
        .ok_or("Self-hosted compiler not found. Set FORGE_SELF_HOST or ensure ./self-host/forge_main exists")?;

    let output = Command::new(&compiler)
        .args(["parse", path])
        .output()
        .map_err(|e| format!("Failed to run compiler: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Parse error: {}", stderr));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Get tokens from self-hosted compiler by running 'forge lex'
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
    for candidate in &["./self-host/ir_driver", "../self-host/ir_driver"] {
        if Path::new(candidate).exists() {
            return Some(candidate.to_string());
        }
    }
    None
}

/// Run the IR driver on a single file, returning its IR text.
/// Renames string IDs to avoid collisions: m0sN → m{module_index}sN
fn run_ir_driver(driver: &str, path: &str, module_index: usize) -> Result<String, String> {
    let output = Command::new(driver)
        .arg(path)
        .output()
        .map_err(|e| format!("run ir_driver: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("IR driver failed on {}: {}", path, stderr));
    }
    let ir = String::from_utf8_lossy(&output.stdout).to_string();
    if module_index == 0 {
        return Ok(ir);
    }
    // Rename m0sN → m{module_index}sN to avoid string ID collisions
    let ir = ir.replace("m0s", &format!("m{}s", module_index));
    // Rename __init_globals → __init_globals_N to avoid duplicate definitions
    let ir = ir.replace("__init_globals", &format!("__init_globals_{}", module_index));
    Ok(ir)
}

/// Find the stdlib root directory by walking up from the source file and CWD
fn find_stdlib_root(source_path: &str) -> Option<String> {
    let check = |dir: &Path| -> Option<String> {
        if dir.join("std/math.fg").exists() {
            let s = dir.to_string_lossy().to_string();
            return Some(if s.is_empty() { ".".to_string() } else { s });
        }
        None
    };
    // Try from source file directory
    let mut dir = Path::new(source_path).parent().unwrap_or(Path::new(".")).to_path_buf();
    for _ in 0..10 {
        if let Some(root) = check(&dir) { return Some(root); }
        match dir.parent() {
            Some(p) if p != dir => dir = p.to_path_buf(),
            _ => break,
        }
    }
    // Try from current working directory
    if let Ok(cwd) = env::current_dir() {
        let mut dir = cwd;
        for _ in 0..10 {
            if let Some(root) = check(&dir) { return Some(root); }
            match dir.parent() {
                Some(p) if p != dir => dir = p.to_path_buf(),
                _ => break,
            }
        }
    }
    None
}

/// Extract import module paths from a source file (simple text scan)
fn extract_imports(source: &str) -> Vec<String> {
    let mut imports = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("from ") && trimmed.contains(" import ") {
            // "from std.csv import parse, encode" → "std.csv"
            if let Some(rest) = trimmed.strip_prefix("from ") {
                if let Some(mod_path) = rest.split_whitespace().next() {
                    imports.push(mod_path.to_string());
                }
            }
        }
    }
    imports
}

/// Resolve a module path to a file path
fn resolve_module_path(mod_path: &str, source_dir: &str, stdlib_root: Option<&str>) -> Option<String> {
    let rel_path = mod_path.replace('.', "/") + ".fg";
    if mod_path.starts_with("std.") {
        if let Some(root) = stdlib_root {
            let path = format!("{}/{}", root, rel_path);
            if Path::new(&path).exists() {
                return Some(path);
            }
        }
    }
    let path = if source_dir == "." { rel_path.clone() } else { format!("{}/{}", source_dir, rel_path) };
    if Path::new(&path).exists() {
        return Some(path);
    }
    None
}

/// Get IR text for a file and all its imports (recursive)
fn get_ir_from_compiler(path: &str) -> Result<String, String> {
    let driver = find_ir_driver()
        .ok_or("No IR driver found. Ensure ./self-host/ir_driver exists.")?;

    let source = fs::read_to_string(path)
        .map_err(|e| format!("read {}: {}", path, e))?;

    let source_dir = Path::new(path).parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());
    let stdlib_root = find_stdlib_root(path);

    // Collect all imported modules (DFS, dependency order)
    let mut visited = std::collections::HashSet::new();
    let mut module_files = Vec::new();
    visited.insert(path.to_string());
    collect_imports_recursive(
        &source, &source_dir, stdlib_root.as_deref(), &mut visited, &mut module_files
    );

    // Generate IR: imported modules first, then main file
    let mut all_ir = String::new();
    for (i, mod_file) in module_files.iter().enumerate() {
        let ir = run_ir_driver(&driver, mod_file, i)?;
        if !ir.is_empty() {
            all_ir.push_str(&ir);
            all_ir.push('\n');
        }
    }
    let main_ir = run_ir_driver(&driver, path, module_files.len())?;
    all_ir.push_str(&main_ir);

    Ok(all_ir)
}

/// Recursively collect imported module file paths in dependency order
fn collect_imports_recursive(
    source: &str,
    source_dir: &str,
    stdlib_root: Option<&str>,
    visited: &mut std::collections::HashSet<String>,
    module_files: &mut Vec<String>,
) {
    // Modules whose functions are handled as runtime builtins — skip importing
    // their Forge implementations to avoid shadowing with incompatible versions
    let builtin_modules = [
        "std.json", "std.toml", "std.encoding", "std.hash", "std.math",
        "std.fmt", "std.log", "std.fs", "std.net.tcp", "std.net.dns",
        "std.net.url", "std.os.path", "std.os.process",
    ];
    for mod_path in extract_imports(source) {
        if builtin_modules.contains(&mod_path.as_str()) {
            continue; // handled as runtime builtins
        }
        if let Some(file_path) = resolve_module_path(&mod_path, source_dir, stdlib_root) {
            if visited.contains(&file_path) {
                continue;
            }
            visited.insert(file_path.clone());
            // Recurse into this module's imports
            if let Ok(mod_source) = fs::read_to_string(&file_path) {
                let mod_dir = Path::new(&file_path).parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| ".".to_string());
                collect_imports_recursive(&mod_source, &mod_dir, stdlib_root, visited, module_files);
            }
            module_files.push(file_path);
        }
    }
}

fn build_file(path: &str) {
    use forge_codegen::create_codegen;
    use forge_codegen::finalize_module;
    use forge_codegen::ir_consumer::compile_from_ir;
    use forge_codegen::linker::build_executable;

    let ir_text = match get_ir_from_compiler(path) {
        Ok(ir) => ir,
        Err(e) => {
            eprintln!("Error getting IR: {}", e);
            return;
        }
    };

    match create_codegen() {
        Ok(mut codegen) => {
            let runtime_funcs = match forge_codegen::declare_runtime_functions(&mut codegen.module) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("Error declaring runtime: {}", e);
                    return;
                }
            };
            match compile_from_ir(&mut codegen, &ir_text, &runtime_funcs) {
                Ok(funcs) => {
                    eprintln!("Compiled {} functions", funcs.len());
                    match finalize_module(codegen.module) {
                        Ok(bytes) => {
                            let obj_path = path.replace(".fg", ".o");
                            let exe_path = path.replace(".fg", "");
                            if let Err(e) = fs::write(&obj_path, &bytes) {
                                eprintln!("Error writing object: {}", e);
                                return;
                            }
                            match build_executable(&obj_path, &exe_path) {
                                Ok(_) => eprintln!("Created: {}", exe_path),
                                Err(e) => eprintln!("Error linking: {}", e),
                            }
                        }
                        Err(e) => eprintln!("Error finalizing: {}", e),
                    }
                }
                Err(e) => eprintln!("Error compiling: {}", e),
            }
        }
        Err(e) => eprintln!("Error creating codegen: {}", e),
    }
}

fn run_file(path: &str) {
    use forge_codegen::create_codegen;
    use forge_codegen::finalize_module;
    use forge_codegen::ir_consumer::compile_from_ir;
    use forge_codegen::linker::build_executable;

    let ir_text = match get_ir_from_compiler(path) {
        Ok(ir) => ir,
        Err(e) => {
            eprintln!("Error getting IR: {}", e);
            return;
        }
    };

    match create_codegen() {
        Ok(mut codegen) => {
            let runtime_funcs = match forge_codegen::declare_runtime_functions(&mut codegen.module) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("Error: {}", e);
                    return;
                }
            };
            match compile_from_ir(&mut codegen, &ir_text, &runtime_funcs) {
                Ok(_) => match finalize_module(codegen.module) {
                    Ok(bytes) => {
                        let obj_path = format!("/tmp/forge_ir_{}.o", std::process::id());
                        let exe_path = format!("/tmp/forge_ir_{}", std::process::id());
                        if fs::write(&obj_path, &bytes).is_ok() {
                            if build_executable(&obj_path, &exe_path).is_ok() {
                                let status = Command::new(&exe_path).status();
                                let _ = fs::remove_file(&obj_path);
                                let _ = fs::remove_file(&exe_path);
                                if let Ok(s) = status {
                                    if !s.success() {
                                        std::process::exit(s.code().unwrap_or(1));
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => eprintln!("Error: {}", e),
                },
                Err(e) => eprintln!("Error: {}", e),
            }
        }
        Err(e) => eprintln!("Error: {}", e),
    }
}

fn test_file(path: &str) {
    eprintln!("Test mode not yet implemented for Cranelift backend");
    eprintln!("Use 'forge run' for now");
    let _ = path;
}

fn check_file(path: &str) {
    let compiler = match find_self_hosted_compiler() {
        Some(c) => c,
        None => {
            eprintln!("Self-hosted compiler not found");
            return;
        }
    };

    let output = Command::new(&compiler)
        .args(["check", path])
        .output()
        .expect("Failed to run compiler");

    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        std::process::exit(1);
    }
}

fn parse_file(path: &str) {
    match get_ast_from_compiler(path) {
        Ok(ast) => println!("{}", ast),
        Err(e) => eprintln!("{}", e),
    }
}

fn lex_file(path: &str) {
    match get_tokens_from_compiler(path) {
        Ok(tokens) => println!("{}", tokens),
        Err(e) => eprintln!("{}", e),
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

    let status = Command::new(&compiler)
        .args(args)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("Failed to run self-hosted compiler: {}", e);
            std::process::exit(1);
        });

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
}
