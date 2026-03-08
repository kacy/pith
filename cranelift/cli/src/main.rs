//! Forge CLI - native compilation with Cranelift

use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();
    
    if args.len() < 2 {
        println!("Usage: forge <command> [args...]");
        println!("Commands:");
        println!("  build <file.fg>    Compile to native binary");
        println!("  run <file.fg>      Compile and run");
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
        _ => {
            eprintln!("Unknown command: {}", command);
        }
    }
}
