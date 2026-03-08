//! Forge CLI - native compilation with Cranelift

use std::env;
use std::fs;

fn main() {
    let args: Vec<String> = env::args().collect();
    
    if args.len() < 2 {
        println!("Usage: forge <command> [args...]");
        println!("Commands:");
        println!("  build <file.fg>    Compile to native binary");
        println!("  run <file.fg>      Compile and run");
        println!("  demo               Run a demo compilation");
        println!("  parse <file.fg>    Parse and display AST");
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
            println!("Running {}...", args[2]);
            // TODO: Compile and run
        }
        "demo" => {
            println!("Demo mode - using test_simple.fg");
            build_file("test_simple.fg");
        }
        "parse" => {
            if args.len() < 3 {
                eprintln!("Error: parse requires a file argument");
                return;
            }
            parse_file(&args[2]);
        }
        _ => {
            eprintln!("Unknown command: {}", command);
        }
    }
}

fn build_file(path: &str) {
    use forge_codegen::parser::parse_file;
    use forge_codegen::compiler::compile_module;
    use forge_codegen::create_codegen;
    use forge_codegen::finalize_module;
    use forge_codegen::linker::build_executable;
    
    println!("Building {}...", path);
    
    // Parse the file
    match parse_file(path) {
        Ok(ast_nodes) => {
            println!("Parsed {} top-level declarations", ast_nodes.len());
            
            // Create codegen
            match create_codegen() {
                Ok(mut codegen) => {
                    // Compile all functions with two-pass approach
                    match compile_module(&mut codegen, ast_nodes) {
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
                                                Ok(_) => println!("Created executable: {}", exe_path),
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
        Err(e) => eprintln!("Error parsing file: {}", e),
    }
}

fn parse_file(path: &str) {
    use forge_codegen::parser::parse_file;
    
    println!("Parsing {}...", path);
    
    match parse_file(path) {
        Ok(ast_nodes) => {
            println!("Parsed {} top-level declarations:", ast_nodes.len());
            for (i, node) in ast_nodes.iter().enumerate() {
                println!("  {}: {:?}", i, node);
            }
        }
        Err(e) => eprintln!("Error: {}", e),
    }
}
