//! Pure, side-effect-free CLI argument parsing.

use std::process::Command as ProcessCommand;

/// Parsed CLI command ready for execution.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Build { path: String, output: Option<String> },
    BuildIr { ir_path: String, output: String },
    Run { path: String },
    Test(TestOpts),
    Frontend(Vec<String>),
    Parse { path: String },
    Lex { path: String },
    Version,
    Help,
}

/// Options accepted by `pith test`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TestOpts {
    pub(crate) file: String,
    pub(crate) filter: Option<String>,
    pub(crate) selected_tags: Vec<String>,
    pub(crate) excluded_tags: Vec<String>,
    pub(crate) json: bool,
}

impl TestOpts {
    /// Pass the parsed test-selection options to the compiled test binary via
    /// its child environment rather than mutating this process's environment.
    pub(crate) fn apply_env(&self, cmd: &mut ProcessCommand) {
        if let Some(ref filter) = self.filter {
            cmd.env("PITH_TEST_FILTER", filter);
        }
        if self.json {
            cmd.env("PITH_TEST_JSON", "1");
        }
        if !self.selected_tags.is_empty() {
            cmd.env("PITH_TEST_TAGS", self.selected_tags.join(","));
        }
        if !self.excluded_tags.is_empty() {
            cmd.env("PITH_TEST_EXCLUDE_TAGS", self.excluded_tags.join(","));
        }
    }
}

/// Argument parsing failure.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ArgError {
    MissingCommand,
    UnknownCommand(String),
    Usage(String),
    Raw(String),
}

pub(crate) fn parse_args(args: &[String]) -> Result<Command, ArgError> {
    if args.len() < 2 {
        return Err(ArgError::MissingCommand);
    }

    match args[1].as_str() {
        "build" => {
            let (path, output) = file_args("build", &args[2..], true).map_err(ArgError::Usage)?;
            Ok(Command::Build { path, output })
        }
        "build-ir" => {
            if args.len() < 4 {
                return Err(ArgError::Raw(
                    "build-ir requires an IR file and an output path".to_string(),
                ));
            }
            Ok(Command::BuildIr {
                ir_path: args[2].clone(),
                output: args[3].clone(),
            })
        }
        "run" => {
            let (path, _) = file_args("run", &args[2..], false).map_err(ArgError::Usage)?;
            Ok(Command::Run { path })
        }
        "test" => parse_test_args(&args[2..]).map(Command::Test),
        "check" => {
            require_file_arg(args, "check")?;
            // forward every argument, not just args[2]: `check --json <file>`
            // used to hand "--json" to the frontend as the path.
            Ok(Command::Frontend(args[1..].to_vec()))
        }
        "parse" => {
            require_file_arg(args, "parse")?;
            Ok(Command::Parse {
                path: args[2].clone(),
            })
        }
        "lex" => {
            require_file_arg(args, "lex")?;
            Ok(Command::Lex {
                path: args[2].clone(),
            })
        }
        "version" | "--version" => Ok(Command::Version),
        "fmt" | "lint" | "doc" | "new" | "package" | "lsp" => {
            Ok(Command::Frontend(args[1..].to_vec()))
        }
        "help" | "--help" | "-h" => Ok(Command::Help),
        other => Err(ArgError::UnknownCommand(other.to_string())),
    }
}

fn require_file_arg(args: &[String], command: &str) -> Result<(), ArgError> {
    if args.len() < 3 {
        return Err(ArgError::Usage(format!(
            "{} requires a file argument",
            command
        )));
    }
    Ok(())
}

/// `--filter <substr>` (or `--filter=<substr>`) runs only the tests whose name
/// contains the substring; `--tag`/`--exclude-tag` select and exclude by the
/// labels a `test` block carries, and repeat to name more than one; `--json`
/// reports one record per result rather than prose. Options may come before or
/// after the file.
fn parse_test_args(args: &[String]) -> Result<TestOpts, ArgError> {
    let mut file: Option<String> = None;
    let mut filter: Option<String> = None;
    let mut selected_tags: Vec<String> = Vec::new();
    let mut excluded_tags: Vec<String> = Vec::new();
    let mut json = false;
    let mut i = 0;

    while i < args.len() {
        if args[i] == "--filter" && i + 1 < args.len() {
            filter = Some(args[i + 1].clone());
            i += 2;
        } else if let Some(f) = args[i].strip_prefix("--filter=") {
            filter = Some(f.to_string());
            i += 1;
        } else if args[i] == "--tag" && i + 1 < args.len() {
            selected_tags.push(args[i + 1].clone());
            i += 2;
        } else if let Some(t) = args[i].strip_prefix("--tag=") {
            selected_tags.push(t.to_string());
            i += 1;
        } else if args[i] == "--exclude-tag" && i + 1 < args.len() {
            excluded_tags.push(args[i + 1].clone());
            i += 2;
        } else if let Some(t) = args[i].strip_prefix("--exclude-tag=") {
            excluded_tags.push(t.to_string());
            i += 1;
        } else if args[i] == "--json" {
            json = true;
            i += 1;
        } else if ["--filter", "--tag", "--exclude-tag"].contains(&args[i].as_str()) {
            return Err(ArgError::Usage(format!("{} needs a value", args[i])));
        } else if file.is_none() && !args[i].starts_with('-') {
            file = Some(args[i].clone());
            i += 1;
        } else {
            return Err(ArgError::Usage(unexpected_arg("test", &args[i])));
        }
    }

    let file = file.ok_or_else(|| ArgError::Usage("test requires a file argument".to_string()))?;
    Ok(TestOpts {
        file,
        filter,
        selected_tags,
        excluded_tags,
        json,
    })
}

/// The error for an argument a command does not take: an unknown option,
/// or a second file.
pub(crate) fn unexpected_arg(command: &str, arg: &str) -> String {
    if arg.starts_with('-') && arg.len() > 1 {
        format!("unknown option '{}' for {}", arg, command)
    } else {
        format!("{} takes one file, and '{}' is a second one", command, arg)
    }
}

/// The source file and the `-o` output path of `command`, whose arguments
/// after the command name are `args`. `-o <path>` is accepted before or after
/// the file when `takes_output` is set. Anything else returns an error, so a
/// misspelled option is reported rather than ignored.
pub(crate) fn file_args(
    command: &str,
    args: &[String],
    takes_output: bool,
) -> Result<(String, Option<String>), String> {
    let mut path = None;
    let mut output = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if takes_output && arg == "-o" {
            let value = args.get(i + 1).ok_or("-o needs an output path")?;
            if output.replace(value.clone()).is_some() {
                return Err("-o is given more than once".to_string());
            }
            i += 2;
            continue;
        }
        if path.is_some() || (arg.starts_with('-') && arg.len() > 1) {
            return Err(unexpected_arg(command, arg));
        }
        path = Some(arg.clone());
        i += 1;
    }
    let path = path.ok_or_else(|| format!("{} requires a file argument", command))?;
    Ok((path, output))
}

pub(crate) fn print_usage() {
    println!("Usage: pith <command> [args...]");
    println!();
    println!("Commands:");
    println!("  build <file.pith> [-o <out>]  Compile .pith file to native binary");
    println!("  build-ir <file.ir> <out>  Compile combined IR text to a native binary");
    println!("  run <file.pith>      Compile and run immediately");
    println!("  test <file.pith>     Compile and run tests");
    println!("      [--filter <substr>] [--tag <name>] [--exclude-tag <name>] [--json]");
    println!("  check <file.pith>    Type-check without generating code");
    println!("  fmt [args...]      Format source files");
    println!("  lint [args...]     Lint source files");
    println!("  doc [args...]      Generate or search documentation");
    println!("  package [args...]  Run package-level check/test/lint/doc");
    println!("  new [args...]      Create a new project");
    println!("  lsp                Run the language server over stdio");
    println!("  parse <file.pith>    Parse and display AST");
    println!("  lex <file.pith>      Tokenize and display token stream");
    println!("  version            Display version information");
    println!("  help               Show this help message");
    println!();
    println!("Environment:");
    println!("  PITH_HOME         Toolchain root holding std (default: the directory above bin/pith, or the checkout this pith was built in)");
    println!("  PITH_SELF_HOST    Path to self-hosted compiler (default: self-host/pith_main in this checkout)");
    println!("  PITH_IR_DRIVER    Path to the IR driver (default: self-host/ir_driver in this checkout)");
    println!("  PITH_DUMP_IR      Path to dump combined IR text (for debugging)");
}
