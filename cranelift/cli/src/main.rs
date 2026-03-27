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

/// Get IR text from the self-hosted compiler's ir_emitter.
/// Uses a pre-compiled IR driver binary if available, otherwise
/// generates and compiles one on the fly.
fn get_ir_from_compiler(path: &str) -> Result<String, String> {
    // Check for pre-compiled IR driver
    let driver_bin_candidates = [
        "./self-host/ir_driver",
        "../self-host/ir_driver",
    ];

    for candidate in &driver_bin_candidates {
        if Path::new(candidate).exists() {
            let output = Command::new(candidate)
                .arg(path)
                .output()
                .map_err(|e| format!("run ir_driver: {}", e))?;

            if output.status.success() {
                return Ok(String::from_utf8_lossy(&output.stdout).to_string());
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("IR driver failed: {}", stderr));
        }
    }

    // No pre-compiled driver — generate and compile one on the fly
    // This uses the AST path (parse → compile) for the driver itself
    let driver_source = format!(
        "from lexer import lex_all\nfrom parser import parse\nfrom ast import reset_arena\nfrom ir_emitter import emit_ir\n\nfn main():\n    source := read_file(\"{}\")\n    tokens := lex_all(source)\n    reset_arena()\n    root := parse(tokens)\n    ir := emit_ir(root)\n    print(ir)\n",
        path
    );

    let driver_path = "self-host/_ir_driver.fg";
    fs::write(driver_path, &driver_source)
        .map_err(|e| format!("write driver: {}", e))?;

    // Compile and run the driver using get_ast + compile pipeline
    let ast_text = get_ast_from_compiler(driver_path)?;

    let ir_text = compile_and_run_from_ast(&ast_text, driver_path)?;

    let _ = fs::remove_file(driver_path);

    Ok(ir_text)
}

/// Compile AST text via ir_consumer and run it, returning stdout
fn compile_and_run_from_ast(ast_text: &str, _source_path: &str) -> Result<String, String> {
    // The AST text IS the source of the driver — we need to compile it.
    // But we deleted compiler.rs. So we need another approach.
    // For bootstrapping: use the Forge self-hosted compiler to parse, then ir_emitter to emit IR.
    // But that's circular.
    //
    // Solution: We need a pre-compiled ir_driver binary. If it doesn't exist, we can't compile.
    Err("No pre-compiled IR driver found. Run 'make self-host' to build the IR driver.".to_string())
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
