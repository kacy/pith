//! Forge CLI - native compilation with Cranelift
//!
//! This CLI integrates with the self-hosted Forge compiler to provide
//! native code generation via Cranelift backend.

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

    let command = &args[1];

    match command.as_str() {
        "build" => {
            if args.len() < 3 {
                eprintln!("Error: build requires a file argument");
                return;
            }
            build_file(&args[2]);
        }
        "run" => {
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
            println!("Forge Cranelift Compiler v0.1.0");
            println!("Using Cranelift backend for native code generation");
        }
        "help" | "--help" | "-h" => {
            print_usage();
        }
        _ => {
            eprintln!("Unknown command: {}", command);
            print_usage();
        }
    }
}

fn print_usage() {
    println!("Usage: forge-cranelift <command> [args...]");
    println!();
    println!("Commands:");
    println!("  build <file.fg>    Compile .fg file to native binary (via Cranelift)");
    println!("  run <file.fg>      Compile and run immediately");
    println!("  test <file.fg>     Compile and run tests");
    println!("  check <file.fg>    Type-check without generating code");
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
    // Check environment variable first
    if let Ok(path) = env::var("FORGE_SELF_HOST") {
        if Path::new(&path).exists() {
            return Some(path);
        }
    }

    // Try common locations
    let candidates = [
        "./self-host/forge_main",
        "../self-host/forge_main",
        "./forge_main",
        "./zig-out/bin/forge_main",
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
        .args(&["parse", path])
        .output()
        .map_err(|e| format!("Failed to run compiler: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Parse error: {}", stderr));
    }

    let ast = String::from_utf8_lossy(&output.stdout);
    Ok(ast.to_string())
}

/// Get tokens from self-hosted compiler by running 'forge lex'
fn get_tokens_from_compiler(path: &str) -> Result<String, String> {
    let compiler = find_self_hosted_compiler().ok_or("Self-hosted compiler not found")?;

    let output = Command::new(&compiler)
        .args(&["lex", path])
        .output()
        .map_err(|e| format!("Failed to run compiler: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Lex error: {}", stderr));
    }

    let tokens = String::from_utf8_lossy(&output.stdout);
    Ok(tokens.to_string())
}

fn build_file(path: &str) {
    use forge_codegen::compiler::compile_module_from_text_with_imports;
    use forge_codegen::create_codegen;
    use forge_codegen::finalize_module;
    use forge_codegen::linker::build_executable;
    use forge_codegen::CompileError;

    println!("Building {} with Cranelift backend...", path);

    // First, get AST from self-hosted compiler
    let ast_text = match get_ast_from_compiler(path) {
        Ok(ast) => ast,
        Err(e) => {
            eprintln!("Error getting AST: {}", e);
            return;
        }
    };

    // Create callback function for resolving imports
    let get_ast_callback = |file_path: &str| -> Result<String, CompileError> {
        get_ast_from_compiler(file_path).map_err(|e| {
            CompileError::ModuleError(format!("Failed to get AST for {}: {}", file_path, e))
        })
    };

    // Create codegen
    match create_codegen() {
        Ok(mut codegen) => {
            // Parse AST text and compile all functions with import resolution
            match compile_module_from_text_with_imports(
                &mut codegen,
                &ast_text,
                path,
                &get_ast_callback,
            ) {
                Ok(funcs) => {
                    println!("Compiled {} functions", funcs.len());

                    // Finalize and write object file
                    match finalize_module(codegen.module) {
                        Ok(bytes) => {
                            let obj_path = path.replace(".fg", ".o");
                            match fs::write(&obj_path, &bytes) {
                                Ok(_) => {
                                    println!("Written {} ({} bytes)", obj_path, bytes.len());

                                    // Link to create executable
                                    let exe_path = path.replace(".fg", "");
                                    match build_executable(&obj_path, &exe_path) {
                                        Ok(_) => {
                                            println!("Created executable: {}", exe_path)
                                        }
                                        Err(e) => eprintln!("Error linking: {}", e),
                                    }
                                }
                                Err(e) => eprintln!("Error writing object file: {}", e),
                            }
                        }
                        Err(e) => eprintln!("Error finalizing module: {}", e),
                    }
                }
                Err(e) => eprintln!("Error compiling module: {}", e),
            }
        }
        Err(e) => eprintln!("Error creating codegen: {}", e),
    }
}

fn run_file(path: &str) {
    use forge_codegen::compiler::compile_module_from_text;
    use forge_codegen::create_codegen;
    use forge_codegen::finalize_module;
    use forge_codegen::linker::build_executable;

    println!("Running {} with Cranelift backend...", path);

    // Get AST from self-hosted compiler
    let ast_text = match get_ast_from_compiler(path) {
        Ok(ast) => ast,
        Err(e) => {
            eprintln!("Error getting AST: {}", e);
            return;
        }
    };

    match create_codegen() {
        Ok(mut codegen) => {
            match compile_module_from_text(&mut codegen, &ast_text) {
                Ok(_funcs) => {
                    match finalize_module(codegen.module) {
                        Ok(bytes) => {
                            // Write object file to a temp location
                            let obj_path = format!("/tmp/forge_run_{}.o", std::process::id());
                            let exe_path = format!("/tmp/forge_run_{}", std::process::id());
                            match std::fs::write(&obj_path, &bytes) {
                                Ok(_) => {
                                    match build_executable(&obj_path, &exe_path) {
                                        Ok(_) => {
                                            // Run the executable
                                            let status =
                                                std::process::Command::new(&exe_path).status();
                                            match status {
                                                Ok(s) => {
                                                    // Clean up temp files
                                                    let _ = std::fs::remove_file(&obj_path);
                                                    let _ = std::fs::remove_file(&exe_path);
                                                    if !s.success() {
                                                        std::process::exit(s.code().unwrap_or(1));
                                                    }
                                                }
                                                Err(e) => {
                                                    eprintln!("Error running executable: {}", e);
                                                    let _ = std::fs::remove_file(&obj_path);
                                                    let _ = std::fs::remove_file(&exe_path);
                                                }
                                            }
                                        }
                                        Err(e) => eprintln!("Error linking: {}", e),
                                    }
                                }
                                Err(e) => eprintln!("Error writing object file: {}", e),
                            }
                        }
                        Err(e) => eprintln!("Error finalizing module: {}", e),
                    }
                }
                Err(e) => eprintln!("Error compiling module: {}", e),
            }
        }
        Err(e) => eprintln!("Error creating codegen: {}", e),
    }
}

fn test_file(path: &str) {
    // For now, same as run but with test mode
    println!("Test mode not yet implemented for Cranelift backend");
    println!("Use 'forge run' for now, or the C transpilation backend for full test support");
}

fn check_file(path: &str) {
    // Type-check by running self-hosted compiler's check command
    let compiler = match find_self_hosted_compiler() {
        Some(c) => c,
        None => {
            eprintln!("Self-hosted compiler not found");
            return;
        }
    };

    let output = Command::new(&compiler)
        .args(&["check", path])
        .output()
        .expect("Failed to run compiler");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    print!("{}", stdout);
    eprint!("{}", stderr);

    if !output.status.success() {
        std::process::exit(1);
    }
}

fn parse_file(path: &str) {
    // Parse by running self-hosted compiler's parse command
    match get_ast_from_compiler(path) {
        Ok(ast) => println!("{}", ast),
        Err(e) => eprintln!("{}", e),
    }
}

fn lex_file(path: &str) {
    // Lex by running self-hosted compiler's lex command
    match get_tokens_from_compiler(path) {
        Ok(tokens) => println!("{}", tokens),
        Err(e) => eprintln!("{}", e),
    }
}
