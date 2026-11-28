//! Pith CLI - native compilation with Cranelift
//!
//! Pipeline: source → self-hosted parse+emit_ir → text IR → ir_consumer → Cranelift → native

mod args;
mod commands;
mod driver;
mod pipeline;
mod temp_run;
mod toolchain;

#[cfg(test)]
mod tests;

fn main() {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(run_cli);
    std::panic::set_hook(previous_hook);

    match result {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
        Err(payload) => {
            eprintln!(
                "internal compiler error: {}",
                panic_message(payload.as_ref())
            );
            std::process::exit(1);
        }
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "unexpected panic".to_string()
}

fn run_cli() -> i32 {
    let raw_args: Vec<String> = std::env::args().collect();
    match args::parse_args(&raw_args) {
        Ok(cmd) => match commands::run(cmd) {
            Ok(code) => code,
            Err(err) => {
                eprintln!("{}", err);
                1
            }
        },
        Err(args::ArgError::MissingCommand) => {
            args::print_usage();
            1
        }
        Err(args::ArgError::UnknownCommand(name)) => {
            eprintln!("Unknown command: {}", name);
            args::print_usage();
            1
        }
        Err(args::ArgError::Usage(message)) => {
            eprintln!("Error: {}", message);
            eprintln!("Run 'pith help' for usage.");
            1
        }
        Err(args::ArgError::Raw(message)) => {
            eprintln!("Error: {}", message);
            1
        }
    }
}
