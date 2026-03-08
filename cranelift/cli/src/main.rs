//! Forge CLI - native compilation with Cranelift

use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();
    
    if args.len() < 2 {
        println!("Usage: forge <command> [args...]");
        println!("Commands:");
        println!("  build <file.fg>    Compile to native binary");
        println!("  run <file.fg>      Compile and run");
        println!("  demo               Run a demo compilation");
        return;
    }
    
    let command = &args[1];
    
    match command.as_str() {
        "build" => {
            if args.len() < 3 {
                eprintln!("Error: build requires a file argument");
                return;
            }
            println!("Building {}...", args[2]);
            // TODO: Implement build
        }
        "run" => {
            if args.len() < 3 {
                eprintln!("Error: run requires a file argument");
                return;
            }
            println!("Running {}...", args[2]);
            // TODO: Implement run
        }
        "demo" => {
            run_demo();
        }
        _ => {
            eprintln!("Unknown command: {}", command);
        }
    }
}

fn run_demo() {
    use forge_codegen::ast::{AstNode, BinaryOp, compile_function};
    use forge_codegen::create_codegen;
    
    println!("=== Forge Cranelift Demo ===");
    println!();
    
    // Create a simple function: fn add(a: Int, b: Int) -> Int { a + b }
    let body = AstNode::BinaryOp {
        op: BinaryOp::Add,
        left: Box::new(AstNode::Identifier("a".to_string())),
        right: Box::new(AstNode::Identifier("b".to_string())),
    };
    
    let params = vec![
        ("a".to_string(), "Int".to_string()),
        ("b".to_string(), "Int".to_string()),
    ];
    
    println!("Compiling: fn add(a: Int, b: Int) -> Int {{ a + b }}");
    
    match create_codegen() {
        Ok(mut codegen) => {
            match compile_function(&mut codegen, "add", &params, "Int", &body) {
                Ok(func_id) => {
                    println!("Successfully compiled function 'add' (ID: {:?})", func_id);
                    
                    // Finalize the module
                    match forge_codegen::finalize_module(codegen.module) {
                        Ok(bytes) => {
                            println!("Generated {} bytes of object code", bytes.len());
                            println!("First 16 bytes: {:02x?}", &bytes[..16.min(bytes.len())]);
                        }
                        Err(e) => eprintln!("Error finalizing module: {}", e),
                    }
                }
                Err(e) => eprintln!("Error compiling function: {}", e),
            }
        }
        Err(e) => eprintln!("Error creating codegen: {}", e),
    }
    
    println!();
    println!("Demo complete!");
}
