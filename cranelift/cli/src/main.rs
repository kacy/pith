//! Pith CLI - native compilation with Cranelift
//!
//! Pipeline: source → self-hosted parse+emit_ir → text IR → ir_consumer.rs → Cranelift → native

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use pith_codegen::runtime_imports::RUNTIME_ABI;

fn main() {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(run_cli);
    std::panic::set_hook(previous_hook);

    if let Err(payload) = result {
        eprintln!(
            "internal compiler error: {}",
            panic_message(payload.as_ref())
        );
        std::process::exit(1);
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

fn run_cli() {
    let args: Vec<String> = env::args().collect();

    // the compiler and ir driver this spawns find std through PITH_HOME when
    // the source file is outside a checkout
    if env::var_os("PITH_HOME").is_none() {
        if let Some(home) = install_home() {
            env::set_var("PITH_HOME", home);
        }
    }

    if args.len() < 2 {
        print_usage();
        std::process::exit(1);
    }

    match args[1].as_str() {
        "build" => {
            let (path, output) = parse_file_args("build", &args[2..], true);
            build_file(&path, output.as_deref());
        }
        "build-ir" => {
            if args.len() < 4 {
                eprintln!("Error: build-ir requires an IR file and an output path");
                std::process::exit(1);
            }
            build_ir_file(&args[2], &args[3]);
        }
        "run" => {
            let (path, _) = parse_file_args("run", &args[2..], false);
            run_file(&path);
        }
        "test" => {
            require_file_arg(&args, "test");
            // `--filter <substr>` (or `--filter=<substr>`) runs only the tests
            // whose name contains the substring; `--tag`/`--exclude-tag` select
            // and exclude by the labels a `test` block carries, and repeat to
            // name more than one; `--json` reports one record per result rather
            // than prose. every one of them reaches the test binary through the
            // environment, which the subprocess inherits.
            let mut selected_tags: Vec<String> = Vec::new();
            let mut excluded_tags: Vec<String> = Vec::new();
            let mut i = 3;
            while i < args.len() {
                if args[i] == "--filter" && i + 1 < args.len() {
                    env::set_var("PITH_TEST_FILTER", &args[i + 1]);
                    i += 2;
                } else if let Some(f) = args[i].strip_prefix("--filter=") {
                    env::set_var("PITH_TEST_FILTER", f);
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
                    env::set_var("PITH_TEST_JSON", "1");
                    i += 1;
                } else if ["--filter", "--tag", "--exclude-tag"].contains(&args[i].as_str()) {
                    usage_error(&format!("{} needs a value", args[i]));
                } else {
                    usage_error(&unexpected_arg("test", &args[i]));
                }
            }
            if !selected_tags.is_empty() {
                env::set_var("PITH_TEST_TAGS", selected_tags.join(","));
            }
            if !excluded_tags.is_empty() {
                env::set_var("PITH_TEST_EXCLUDE_TAGS", excluded_tags.join(","));
            }
            test_file(&args[2]);
        }
        "check" => {
            require_file_arg(&args, "check");
            // forward every argument, not just args[2]: `check --json <file>`
            // used to hand "--json" to the frontend as the path.
            delegate_to_frontend(&args[1..]);
        }
        "parse" => {
            require_file_arg(&args, "parse");
            parse_file(&args[2]);
        }
        "lex" => {
            require_file_arg(&args, "lex");
            lex_file(&args[2]);
        }
        "version" | "--version" => {
            println!("pith {}", env!("CARGO_PKG_VERSION"));
        }
        "fmt" | "lint" | "doc" | "new" | "package" | "lsp" => {
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

/// Exit with a usage error unless `args` names a file after the command.
fn require_file_arg(args: &[String], command: &str) {
    if args.len() < 3 {
        usage_error(&format!("{} requires a file argument", command));
    }
}

/// Print a usage error and exit with status 1.
fn usage_error(message: &str) -> ! {
    eprintln!("Error: {}", message);
    eprintln!("Run 'pith help' for usage.");
    std::process::exit(1);
}

/// The error for an argument a command does not take: an unknown option,
/// or a second file.
fn unexpected_arg(command: &str, arg: &str) -> String {
    if arg.starts_with('-') && arg.len() > 1 {
        format!("unknown option '{}' for {}", arg, command)
    } else {
        format!("{} takes one file, and '{}' is a second one", command, arg)
    }
}

/// The source file and the `-o` output path of `command`, whose arguments
/// after the command name are `args`. `-o <path>` is accepted before or after
/// the file when `takes_output` is set. Anything else exits with a usage
/// error, so a misspelled option is reported rather than ignored.
fn parse_file_args(command: &str, args: &[String], takes_output: bool) -> (String, Option<String>) {
    match file_args(command, args, takes_output) {
        Ok(parsed) => parsed,
        Err(message) => usage_error(&message),
    }
}

fn file_args(
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

fn print_usage() {
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
    println!("  PITH_HOME         Installed toolchain root (default: the directory above bin/pith)");
    println!("  PITH_SELF_HOST    Path to self-hosted compiler (default: ./self-host/pith_main)");
    println!("  PITH_DUMP_IR      Path to dump combined IR text (for debugging)");
}

fn dump_ir_if_requested(ir_text: &str) {
    if let Ok(dump_path) = env::var("PITH_DUMP_IR") {
        let stamped;
        let ir_text = if ir_runtime_abi(ir_text).is_some() {
            ir_text
        } else {
            stamped = format!("{}{}\n{}", IR_ABI_PREFIX, RUNTIME_ABI, ir_text);
            &stamped
        };
        let _ = fs::write(&dump_path, ir_text);
        eprintln!("IR dumped to {} ({} bytes)", dump_path, ir_text.len());
    }
}

fn combined_output(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("{}{}", stdout, stderr)
}

/// The root of an installed toolchain: `PITH_HOME` if set, otherwise the
/// directory above this binary when it holds `lib/pith_main`, as in
/// `<home>/bin/pith`. A cargo build inside a checkout has no such directory.
fn install_home() -> Option<PathBuf> {
    if let Some(home) = env::var_os("PITH_HOME").filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(home));
    }
    let exe = fs::canonicalize(env::current_exe().ok()?).ok()?;
    let home = exe.parent()?.parent()?;
    home.join("lib/pith_main")
        .exists()
        .then(|| home.to_path_buf())
}

/// A file under the install's `lib/`, if there is an install and it has one.
fn installed_lib(name: &str) -> Option<String> {
    let path = install_home()?.join("lib").join(name);
    path.exists().then(|| path.to_string_lossy().into_owned())
}

/// Find the self-hosted compiler executable
fn find_self_hosted_compiler() -> Option<String> {
    if let Ok(path) = env::var("PITH_SELF_HOST") {
        if Path::new(&path).exists() {
            return Some(path);
        }
    }
    if let Some(path) = installed_lib("pith_main") {
        return Some(path);
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
    let compiler = find_self_hosted_compiler().ok_or(
        "Self-hosted compiler not found. Set PITH_SELF_HOST or ensure ./self-host/pith_main exists",
    )?;

    let output = Command::new(&compiler)
        .args(["parse", path])
        .output()
        .map_err(|e| format!("Failed to run compiler: {}", e))?;

    if !output.status.success() {
        let combined = combined_output(&output);
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

/// The first line of the IR the cli dumps, and so of the bootstrap seed:
/// `runtime_abi <hash>`, the runtime function table it was compiled against.
/// The consumer skips the line like any other it does not know.
const IR_ABI_PREFIX: &str = "runtime_abi ";

/// The runtime ABI an IR text says it was compiled against, if it says.
fn ir_runtime_abi(ir_text: &str) -> Option<&str> {
    ir_text.lines().next()?.strip_prefix(IR_ABI_PREFIX).map(str::trim)
}

/// The runtime ABI of the seed under `dir`, read from its first line only.
fn seed_runtime_abi(dir: &str) -> Option<String> {
    use std::io::BufRead;
    let file = fs::File::open(format!("{}/bootstrap/ir_driver.ir", dir)).ok()?;
    let mut first = String::new();
    std::io::BufReader::new(file).read_line(&mut first).ok()?;
    ir_runtime_abi(&first).map(str::to_string)
}

/// Where the runtime ABI a driver binary was built against is recorded.
fn driver_abi_path(driver: &str) -> String {
    format!("{}.abi", driver)
}

/// Record that a freshly built ir_driver matches this cli's runtime table.
/// Any other executable is left alone.
fn stamp_if_ir_driver(exe_path: &str) {
    if Path::new(exe_path).file_name().and_then(|n| n.to_str()) == Some("ir_driver") {
        if let Err(e) = fs::write(driver_abi_path(exe_path), RUNTIME_ABI) {
            eprintln!("warning: could not record the runtime ABI of {}: {}", exe_path, e);
        }
    }
}

/// Why a driver under `dir` must not be used as it is, or None when it can.
/// A driver built against another runtime table emits calls this cli may
/// lower to the wrong function or not at all, and a driver older than the
/// seed predates the emitter the checkout holds.
fn ir_driver_staleness(dir: &str, driver: &str) -> Option<String> {
    match fs::read_to_string(driver_abi_path(driver)) {
        Ok(abi) if abi.trim() == RUNTIME_ABI => {}
        Ok(abi) => {
            return Some(format!(
                "{} was built against runtime ABI {}, and this pith has {}",
                driver,
                abi.trim(),
                RUNTIME_ABI
            ))
        }
        Err(_) => return Some(format!("{} has no runtime ABI record", driver)),
    }
    if ir_driver_older_than_seed(dir, driver) {
        return Some(format!(
            "{} is older than {}/bootstrap/ir_driver.ir",
            driver, dir
        ));
    }
    None
}

/// The refusal for a seed made against another runtime table: building it
/// would bake calls this cli cannot resolve, or resolves differently, into
/// the driver that compiles everything else.
fn seed_abi_mismatch(dir: &str, seed_abi: Option<&str>) -> String {
    format!(
        "{}/bootstrap/ir_driver.ir was generated against runtime ABI {}, and this pith was built against {}. \
         rebuild pith from this checkout (cargo build --release). after changing \
         cranelift/runtime-abi/runtime_functions.txt, rebuild the driver from source \
         (pith build self-host/ir_driver.pith) and run make refresh-bootstrap-seed",
        dir,
        seed_abi.unwrap_or("(none recorded)"),
        RUNTIME_ABI
    )
}

/// Find the IR driver to compile `source` with. Ok(None) means none was
/// found and the seed should build one.
fn find_ir_driver(source: &str) -> Result<Option<String>, String> {
    if let Ok(path) = env::var("PITH_IR_DRIVER") {
        if Path::new(&path).exists() {
            return Ok(Some(path));
        }
    }
    if let Some(path) = installed_lib("ir_driver") {
        return Ok(Some(path));
    }

    for dir in &["./self-host", "../self-host"] {
        let candidate = format!("{}/ir_driver", dir);
        if !Path::new(&candidate).exists() {
            continue;
        }
        let Some(reason) = ir_driver_staleness(dir, &candidate) else {
            return Ok(Some(candidate));
        };
        let seed_abi = seed_runtime_abi(dir);
        if seed_abi.as_deref() == Some(RUNTIME_ABI) {
            eprintln!("{}; rebuilding it from the seed", reason);
            return Ok(None);
        }
        // building the driver from source is how a changed runtime table
        // reaches a new driver and then the seed, so the old driver may do
        // that one job
        if Path::new(source).file_name().and_then(|n| n.to_str()) == Some("ir_driver.pith") {
            eprintln!("{}; using it to build the new driver", reason);
            return Ok(Some(candidate));
        }
        return Err(format!("{}. {}", reason, seed_abi_mismatch(dir, seed_abi.as_deref())));
    }
    Ok(None)
}

/// Every emitter change regenerates the seed, so a seed newer than the binary
/// means a pull or checkout moved the compiler under it. The old binary still
/// emits the old IR, which the current backend can miscompile (a none optional
/// read as a heap shell, for one), so it is rebuilt from the seed instead.
fn ir_driver_older_than_seed(dir: &str, driver: &str) -> bool {
    let modified = |path: &str| fs::metadata(path).and_then(|m| m.modified()).ok();
    match (
        modified(&format!("{}/bootstrap/ir_driver.ir", dir)),
        modified(driver),
    ) {
        (Some(seed), Some(binary)) => seed > binary,
        _ => false,
    }
}

/// The ir_driver binary is itself compiled pith, so a fresh clone has no way
/// to build it: pith build needs ir_driver to get IR. The tracked IR seed
/// breaks that cycle: build ir_driver once from the seed, then everything
/// else builds from source as usual. A stale binary is rebuilt the same way
/// (see ir_driver_staleness), and a seed made against another runtime table
/// is refused.
fn bootstrap_ir_driver_from_seed() -> Result<String, String> {
    use pith_codegen::linker::build_executable;

    for dir in &["./self-host", "../self-host"] {
        let seed = format!("{}/bootstrap/ir_driver.ir", dir);
        if !Path::new(&seed).exists() {
            continue;
        }
        let seed_abi = seed_runtime_abi(dir);
        if seed_abi.as_deref() != Some(RUNTIME_ABI) {
            return Err(seed_abi_mismatch(dir, seed_abi.as_deref()));
        }
        eprintln!("building ir_driver from {}", seed);
        let ir_text = fs::read_to_string(&seed).map_err(|e| format!("read {}: {}", seed, e))?;
        let (bytes, _) = object_from_ir(&ir_text).map_err(|e| format!("seed build failed: {}", e))?;
        // link under a per-process name and rename into place, so parallel
        // builds never run a half-written binary
        let tmp = format!("{}/ir_driver.{}", dir, std::process::id());
        let obj_path = format!("{}.o", tmp);
        let exe_path = format!("{}/ir_driver", dir);
        fs::write(&obj_path, &bytes).map_err(|e| format!("seed object: {}", e))?;
        let linked = build_executable(&obj_path, &tmp);
        let _ = fs::remove_file(&obj_path);
        linked.map_err(|e| format!("seed link failed: {}", e))?;
        fs::rename(&tmp, &exe_path).map_err(|e| format!("seed install failed: {}", e))?;
        stamp_if_ir_driver(&exe_path);
        return Ok(exe_path);
    }
    Err("No IR driver found. Ensure ./self-host/ir_driver exists.".to_string())
}

fn describe_exit(status: &std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    if let Some(sig) = status.signal() {
        let name = match sig {
            4 => " (SIGILL, illegal instruction)",
            6 => " (SIGABRT, abort)",
            7 => " (SIGBUS, bus error)",
            8 => " (SIGFPE, arithmetic exception)",
            9 => " (SIGKILL, killed)",
            11 => " (SIGSEGV, segmentation fault)",
            13 => " (SIGPIPE, broken pipe)",
            15 => " (SIGTERM, terminated)",
            _ => "",
        };
        return format!("crashed with signal {}{}", sig, name);
    }
    match status.code() {
        Some(c) => format!("failed with exit code {}", c),
        None => "failed".to_string(),
    }
}

/// The status `pith run` and `pith test` exit with when the program failed.
/// A program that exited passes its code through. One killed by a signal
/// has no code and would otherwise leave a bare exit 1 with nothing said,
/// so this names the signal on stderr and exits with 128 plus its number,
/// the status a shell reports for it.
fn child_exit_code(path: &str, status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    if let Some(code) = status.code() {
        return code;
    }
    eprintln!("pith: {} {}", path, describe_exit(status));
    128 + status.signal().unwrap_or(0)
}

fn get_ir_from_compiler(path: &str, emit_tests: bool) -> Result<String, String> {
    let driver = match find_ir_driver(path)? {
        Some(driver) => driver,
        None => bootstrap_ir_driver_from_seed()?,
    };
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
        let status = describe_exit(&output.status);
        if combined.trim().is_empty() {
            return Err(format!(
                "IR driver {} on {} with no diagnostics — this is a compiler bug, not a problem in your program",
                status, path
            ));
        }
        return Err(format!("IR driver failed on {}: {}", path, combined));
    }
    let ir = String::from_utf8_lossy(&output.stdout).to_string();
    // a clean exit with no emitted functions means the driver died
    // silently (an emitter bug); linking would fail later with a
    // baffling "undefined reference to main"
    if !ir.lines().any(|l| l.starts_with("func ")) {
        return Err(format!(
            "IR driver exited cleanly but emitted no functions for {} ({} bytes of IR) — this is a compiler bug, not a problem in your program",
            path,
            ir.len()
        ));
    }
    Ok(ir)
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
    // the ir driver says nothing when it cannot read its input, which would
    // be reported as a compiler bug, so a missing file is caught here
    check_readable_file(path).map_err(|e| format!("Error reading {}: {}", path, e))?;
    let ir_text =
        get_ir_from_compiler(path, emit_tests).map_err(|e| format!("Error getting IR: {}", e))?;
    object_from_ir(&ir_text)
}

/// Ok when `path` is a regular file this process can open for reading.
fn check_readable_file(path: &str) -> std::io::Result<()> {
    if fs::File::open(path)?.metadata()?.is_dir() {
        return Err(std::io::ErrorKind::IsADirectory.into());
    }
    Ok(())
}

fn object_from_ir(ir_text: &str) -> Result<(Vec<u8>, usize), String> {
    use pith_codegen::create_codegen;
    use pith_codegen::finalize_module;
    use pith_codegen::ir_consumer::compile_from_ir;

    // dump before validating: the dump exists to debug bad IR, so it must
    // fire even when (especially when) the contract check rejects it
    dump_ir_if_requested(ir_text);

    if let Some(abi) = ir_runtime_abi(ir_text) {
        if abi != RUNTIME_ABI {
            return Err(format!(
                "this IR was compiled against runtime ABI {}, and this pith was built against {}: \
                 rebuild pith from the checkout the IR came from (cargo build --release)",
                abi, RUNTIME_ABI
            ));
        }
    }
    validate_combined_ir_contract(ir_text)?;

    let mut codegen = create_codegen().map_err(|e| format!("Error creating codegen: {}", e))?;
    let runtime_funcs = pith_codegen::declare_runtime_functions(&mut codegen.module)
        .map_err(|e| format!("Error declaring runtime: {}", e))?;
    let funcs = compile_from_ir(&mut codegen, ir_text, &runtime_funcs)
        .map_err(|e| format!("Error compiling: {}", e))?;
    let bytes = finalize_module(codegen.module).map_err(|e| format!("Error finalizing: {}", e))?;
    Ok((bytes, funcs.len()))
}

/// Compile `path` to an executable at `output`, or next to the source
/// without its `.pith` when no output is given. The object file is written
/// beside the executable as `<exe>.o`.
fn build_file(path: &str, output: Option<&str>) {
    let Some(exe_path) = output.or_else(|| default_exe_path(path)) else {
        eprintln!(
            "Error: {} does not end in .pith, so there is no default output name; pass -o <out>",
            path
        );
        std::process::exit(1);
    };
    let (bytes, func_count) = match compile_to_object(path, false) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };

    eprintln!("Compiled {} functions", func_count);
    write_and_link(&bytes, &format!("{}.o", exe_path), exe_path);
    stamp_if_ir_driver(exe_path);
}

/// The executable `pith build` writes for `path`: the path without its
/// trailing `.pith`. Only the last extension goes, so a directory named
/// `foo.pith` keeps its name. None when nothing would be left of the file
/// name, or when there is no `.pith` to strip and the output would
/// overwrite the source.
fn default_exe_path(path: &str) -> Option<&str> {
    path.strip_suffix(".pith")
        .filter(|stem| !stem.is_empty() && !stem.ends_with('/'))
}

/// Write an object file and link it into `exe_path`, exiting on failure.
fn write_and_link(bytes: &[u8], obj_path: &str, exe_path: &str) {
    use pith_codegen::linker::build_executable;

    if let Err(e) = fs::write(obj_path, bytes) {
        eprintln!("Error writing object: {}", e);
        std::process::exit(1);
    }
    match build_executable(obj_path, exe_path) {
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

/// Compile a combined IR text file straight to a native binary. Pairs with
/// PITH_DUMP_IR (which writes such a file) and backs the bootstrap seed.
fn build_ir_file(ir_path: &str, exe_path: &str) {
    let ir_text = match fs::read_to_string(ir_path) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("Error reading {}: {}", ir_path, e);
            std::process::exit(1);
        }
    };
    let (bytes, func_count) = match object_from_ir(&ir_text) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };

    eprintln!("Compiled {} functions", func_count);
    write_and_link(&bytes, &format!("{}.o", exe_path), exe_path);
}

fn run_file(path: &str) {
    compile_and_run(path, false);
}

fn test_file(path: &str) {
    compile_and_run(path, true);
}

/// Compile `path` (with its tests when `emit_tests`) to a temporary binary,
/// run it, and exit with its status when it fails. The binary and object
/// are removed afterwards unless PITH_KEEP_RUN_ARTIFACTS is set.
fn compile_and_run(path: &str, emit_tests: bool) {
    use pith_codegen::linker::build_executable;

    let (bytes, _) = match compile_to_object(path, emit_tests) {
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
                        std::process::exit(child_exit_code(path, &s));
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

#[cfg(test)]
mod tests {
    use super::{
        check_readable_file, default_exe_path, driver_abi_path, file_args, ir_driver_older_than_seed, ir_driver_staleness, ir_runtime_abi,
        object_from_ir, seed_runtime_abi, validate_combined_ir_contract, RUNTIME_ABI,
    };
    use std::fs;
    use std::time::{Duration, SystemTime};

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

    #[test]
    fn a_driver_older_than_its_seed_is_stale() {
        let dir = std::env::temp_dir().join(format!("pith-seed-age-{}", std::process::id()));
        fs::create_dir_all(dir.join("bootstrap")).unwrap();
        let seed = dir.join("bootstrap/ir_driver.ir");
        let driver = dir.join("ir_driver");
        fs::write(&seed, "").unwrap();
        fs::write(&driver, "").unwrap();
        let set = |path: &std::path::Path, secs: u64| {
            let file = fs::File::options().write(true).open(path).unwrap();
            file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
                .unwrap();
        };
        let (d, b) = (dir.to_str().unwrap(), driver.to_str().unwrap());

        set(&seed, 2_000);
        set(&driver, 1_000);
        assert!(ir_driver_older_than_seed(d, b));

        set(&driver, 3_000);
        assert!(!ir_driver_older_than_seed(d, b));

        fs::remove_file(&seed).unwrap();
        assert!(!ir_driver_older_than_seed(d, b));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_driver_is_stale_without_a_matching_abi_record() {
        let dir = std::env::temp_dir().join(format!("pith-driver-abi-{}", std::process::id()));
        fs::create_dir_all(dir.join("bootstrap")).unwrap();
        let driver = dir.join("ir_driver");
        fs::write(&driver, "").unwrap();
        let (d, b) = (dir.to_str().unwrap(), driver.to_str().unwrap());

        let reason = ir_driver_staleness(d, b).expect("no record is stale");
        assert!(reason.contains("has no runtime ABI record"), "{reason}");

        fs::write(driver_abi_path(b), "0123456789abcdef").unwrap();
        let reason = ir_driver_staleness(d, b).expect("another table is stale");
        assert!(reason.contains("built against runtime ABI 0123456789abcdef"), "{reason}");

        fs::write(driver_abi_path(b), RUNTIME_ABI).unwrap();
        assert_eq!(ir_driver_staleness(d, b), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_seed_records_its_runtime_abi_on_its_first_line() {
        let dir = std::env::temp_dir().join(format!("pith-seed-abi-{}", std::process::id()));
        fs::create_dir_all(dir.join("bootstrap")).unwrap();
        let seed = dir.join("bootstrap/ir_driver.ir");
        let d = dir.to_str().unwrap();
        fs::write(&seed, "runtime_abi feedface00000000\nfunc main 0 int\n").unwrap();
        assert_eq!(seed_runtime_abi(d).as_deref(), Some("feedface00000000"));
        fs::write(&seed, "func main 0 int\nruntime_abi feedface00000000\n").unwrap();
        assert_eq!(seed_runtime_abi(d), None);
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(ir_runtime_abi(""), None);
    }

    #[test]
    fn ir_for_another_runtime_abi_is_refused() {
        let ir = "runtime_abi 0000000000000000\nfunc main 0 int\niconst 1 0\nret 1\nendfunc\n";
        let err = object_from_ir(ir).expect_err("another table must be refused");
        assert!(err.contains("compiled against runtime ABI 0000000000000000"), "{err}");
        let ir = format!("runtime_abi {}\nfunc main 0 int\niconst 1 0\nret 1\nendfunc\n", RUNTIME_ABI);
        assert!(object_from_ir(&ir).is_ok());
    }

    #[test]
    fn the_default_output_strips_only_the_trailing_extension() {
        assert_eq!(default_exe_path("hello.pith"), Some("hello"));
        assert_eq!(default_exe_path("foo.pith/hello.pith"), Some("foo.pith/hello"));
        assert_eq!(default_exe_path("a.pith.pith"), Some("a.pith"));
        assert_eq!(default_exe_path("hello"), None);
        assert_eq!(default_exe_path("foo.pith/hello"), None);
        assert_eq!(default_exe_path(".pith"), None);
        assert_eq!(default_exe_path("dir/.pith"), None);
    }

    #[test]
    fn build_takes_one_file_and_an_output_path() {
        let args = |list: &[&str]| list.iter().map(|a| a.to_string()).collect::<Vec<_>>();
        let parsed = |list: &[&str], output: bool| file_args("build", &args(list), output);
        let ok = |path: &str, out: Option<&str>| Ok((path.to_string(), out.map(str::to_string)));

        assert_eq!(parsed(&["a.pith"], true), ok("a.pith", None));
        assert_eq!(parsed(&["a.pith", "-o", "out"], true), ok("a.pith", Some("out")));
        assert_eq!(parsed(&["-o", "out", "a.pith"], true), ok("a.pith", Some("out")));
        assert_eq!(parsed(&["-"], true), ok("-", None));

        let err = |list: &[&str], output: bool| parsed(list, output).unwrap_err();
        assert_eq!(err(&[], true), "build requires a file argument");
        assert_eq!(err(&["a.pith", "-o"], true), "-o needs an output path");
        assert_eq!(err(&["a.pith", "-o", "x", "-o", "y"], true), "-o is given more than once");
        assert_eq!(err(&["a.pith", "--release"], true), "unknown option '--release' for build");
        assert_eq!(err(&["a.pith", "-o", "x"], false), "unknown option '-o' for build");
        assert_eq!(err(&["a.pith", "b.pith"], true), "build takes one file, and 'b.pith' is a second one");
    }

    #[test]
    fn a_missing_or_directory_source_is_not_readable() {
        let dir = std::env::temp_dir().join(format!("pith-source-read-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.pith");
        fs::write(&file, "fn main():\n    return\n").unwrap();
        assert!(check_readable_file(file.to_str().unwrap()).is_ok());
        let missing = check_readable_file(dir.join("none.pith").to_str().unwrap()).unwrap_err();
        assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
        let is_dir = check_readable_file(dir.to_str().unwrap()).unwrap_err();
        assert_eq!(is_dir.kind(), std::io::ErrorKind::IsADirectory);
        fs::remove_dir_all(&dir).unwrap();
    }
}
