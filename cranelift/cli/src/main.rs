//! Forge CLI - native compilation with Cranelift
//!
//! Pipeline: source → self-hosted parse+emit_ir → text IR → ir_consumer.rs → Cranelift → native

use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

/// Offset added to main module's index to avoid collisions with import indices
const MAIN_MODULE_INDEX_OFFSET: usize = 100;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        print_usage();
        return;
    }

    match args[1].as_str() {
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
    println!("  FORGE_DUMP_IR      Path to dump combined IR text (for debugging)");
}

fn dump_ir_if_requested(ir_text: &str) {
    if let Ok(dump_path) = env::var("FORGE_DUMP_IR") {
        let _ = fs::write(&dump_path, ir_text);
        eprintln!("IR dumped to {} ({} bytes)", dump_path, ir_text.len());
    }
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
    if module_index >= MAIN_MODULE_INDEX_OFFSET {
        return Ok(ir.replace("m0s", &format!("m{}s", module_index)));
    }

    // Remove `main` function from imported modules (only main module's main should exist)
    let ir = remove_main_from_import(&ir);
    let prefix = format!("m{}_", module_index);

    // Collect global names from "global NAME ..." lines
    let global_names: Vec<String> = ir.lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 && parts[0] == "global" {
                Some(parts[1].to_string())
            } else {
                None
            }
        })
        .collect();
    let function_names: Vec<String> = ir.lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 && parts[0] == "func" {
                Some(parts[1].to_string())
            } else {
                None
            }
        })
        .collect();

    // Rename string IDs, __init_globals, and globals
    let mut result = String::new();
    for line in ir.lines() {
        let mut new_line = line.replace("m0s", &format!("m{}s", module_index));

        // Rename __init_globals
        new_line = new_line.replace("__init_globals", &format!("__init_globals_{}", module_index));

        // Rename globals in "global NAME", "store NAME", "load R NAME" lines
        let parts: Vec<&str> = new_line.split_whitespace().collect();
        if !parts.is_empty() {
            match parts[0] {
                "func" if parts.len() >= 2 => {
                    let old = parts[1];
                    let renamed = format!("{}{}", prefix, old);
                    let suffix = if parts.len() > 2 {
                        format!(" {}", parts[2..].join(" "))
                    } else {
                        String::new()
                    };
                    new_line = format!("func {}{}", renamed, suffix);
                }
                "global" if parts.len() >= 2 => {
                    let old = parts[1];
                    if !old.starts_with("__for_") { // don't rename loop vars
                        let renamed = format!("{}{}", prefix, old);
                        let suffix = if parts.len() > 2 {
                            format!(" {}", parts[2..].join(" "))
                        } else {
                            String::new()
                        };
                        new_line = format!("global {}{}", renamed, suffix);
                    }
                }
                "store" if parts.len() >= 3 => {
                    let name = parts[1];
                    if global_names.iter().any(|g| g == name) {
                        new_line = new_line.replacen(name, &format!("{}{}", prefix, name), 1);
                    }
                }
                "load" if parts.len() >= 3 => {
                    let name = parts[2];
                    if global_names.iter().any(|g| g == name) {
                        // Replace the global name (second occurrence after "load REG")
                        let pos = new_line.rfind(name).unwrap_or(0);
                        if pos > 0 {
                            new_line = format!("{}{}{}{}", &new_line[..pos], prefix, name, &new_line[pos+name.len()..]);
                        }
                    }
                }
                "call" if parts.len() >= 5 => {
                    let fname = parts[2];
                    if function_names.iter().any(|f| f == fname) {
                        let suffix = if parts.len() > 3 {
                            format!(" {}", parts[3..].join(" "))
                        } else {
                            String::new()
                        };
                        new_line = format!("call {} {}{}", parts[1], format!("{}{}", prefix, fname), suffix);
                    }
                }
                "callv" if parts.len() >= 3 => {
                    let fname = parts[1];
                    if function_names.iter().any(|f| f == fname) {
                        let suffix = if parts.len() > 2 {
                            format!(" {}", parts[2..].join(" "))
                        } else {
                            String::new()
                        };
                        new_line = format!("callv {}{}{}", prefix, fname, suffix);
                    }
                }
                _ => {}
            }
        }

        result.push_str(&new_line);
        result.push('\n');
    }
    Ok(result)
}

/// Remove `func main ... endfunc` from imported module IR
fn remove_main_from_import(ir: &str) -> String {
    let mut result = String::new();
    let mut skip = false;
    for line in ir.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("func main ") {
            skip = true;
            continue;
        }
        if skip && trimmed == "endfunc" {
            skip = false;
            continue;
        }
        if !skip {
            result.push_str(line);
            result.push('\n');
        }
    }
    result
}

fn rewrite_calls_with_function_map(
    ir: &str,
    function_map: &std::collections::HashMap<String, String>,
) -> String {
    let mut rewritten = String::new();
    for line in ir.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let mut handled = false;
        if parts.len() >= 5 && parts[0] == "call" {
            let fname = parts[2];
            if let Some(prefixed) = function_map.get(fname) {
                rewritten.push_str(&format!(
                    "call {} {} {}",
                    parts[1],
                    prefixed,
                    parts[3..].join(" ")
                ));
                rewritten.push('\n');
                handled = true;
            }
        } else if parts.len() >= 3 && parts[0] == "callv" {
            let fname = parts[1];
            if let Some(prefixed) = function_map.get(fname) {
                rewritten.push_str(&format!(
                    "callv {} {}",
                    prefixed,
                    parts[2..].join(" ")
                ));
                rewritten.push('\n');
                handled = true;
            }
        }
        if !handled {
            rewritten.push_str(line);
            rewritten.push('\n');
        }
    }
    rewritten
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

fn rewrite_call_retkinds(ir: &str) -> String {
    let mut func_kinds: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for line in ir.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 4 && parts[0] == "func" {
            func_kinds.insert(parts[1].to_string(), parts[3].to_string());
        }
    }

    let builtin_kinds: std::collections::HashMap<&str, &str> = [
        ("parse_int", "result_int"),
        ("tcp_connect", "result_int"),
        ("tcp_listen", "result_int"),
        ("tcp_accept", "result_int"),
        ("tcp_write", "result_int"),
        ("process_spawn", "result_int"),
        ("process_write", "result_int"),
        ("file_open_read", "result_int"),
        ("file_open_write", "result_int"),
        ("file_open_append", "result_int"),
        ("file_write", "result_int"),
        ("write_file", "result_bool"),
        ("append_file", "result_bool"),
    ]
    .into_iter()
    .collect();

    let mut rewritten = String::new();
    for line in ir.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 5 && parts[0] == "call" {
            let expected = func_kinds
                .get(parts[2])
                .map(|s| s.as_str())
                .or_else(|| builtin_kinds.get(parts[2]).copied());
            let current = parts[3];
            let should_rewrite = matches!(
                (current, expected),
                ("unknown", Some(_))
                    | ("int", Some("result_int"))
                    | ("bool", Some("result_bool"))
            );
            if should_rewrite {
                let retkind = expected.unwrap();
                rewritten.push_str(&format!("call {} {} {} {}", parts[1], parts[2], retkind, parts[4]));
                if parts.len() > 5 {
                    rewritten.push(' ');
                    rewritten.push_str(&parts[5..].join(" "));
                }
                rewritten.push('\n');
                continue;
            }
        }
        rewritten.push_str(line);
        rewritten.push('\n');
    }

    rewritten
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
    let mut global_renames: Vec<(String, String)> = Vec::new(); // (bare, prefixed)
    let mut function_renames: Vec<(String, String)> = Vec::new(); // (bare, prefixed)
    let mut imported_init_funcs: Vec<String> = Vec::new();
    let mut imported_function_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for (i, mod_file) in module_files.iter().enumerate() {
        let mut ir = run_ir_driver(&driver, mod_file, i)?;
        if !imported_function_map.is_empty() {
            ir = rewrite_calls_with_function_map(&ir, &imported_function_map);
        }
        if !ir.is_empty() {
            // Collect global name mappings for main module rewriting
            let prefix = format!("m{}_", i);
            for line in ir.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 && parts[0] == "global" {
                    let prefixed = parts[1].to_string();
                    if let Some(bare) = prefixed.strip_prefix(&prefix) {
                        global_renames.push((bare.to_string(), prefixed.clone()));
                    }
                } else if parts.len() >= 2 && parts[0] == "func" {
                    let prefixed = parts[1].to_string();
                    if let Some(bare) = prefixed.strip_prefix(&prefix) {
                        imported_function_map.insert(bare.to_string(), prefixed.clone());
                        function_renames.push((bare.to_string(), prefixed.clone()));
                        if bare.starts_with("__init_globals") {
                            imported_init_funcs.push(prefixed.clone());
                        }
                    }
                }
            }
            all_ir.push_str(&ir);
            all_ir.push('\n');
        }
    }
    let main_ir_raw = run_ir_driver(&driver, path, module_files.len() + MAIN_MODULE_INDEX_OFFSET)?;
    // Rewrite main module's load/store references to imported globals
    let mut main_ir = String::new();
    let global_map: std::collections::HashMap<&str, &str> = global_renames.iter()
        .map(|(bare, prefixed)| (bare.as_str(), prefixed.as_str()))
        .collect();
    let function_map: std::collections::HashMap<&str, &str> = function_renames.iter()
        .map(|(bare, prefixed)| (bare.as_str(), prefixed.as_str()))
        .collect();
    let main_func_names: std::collections::HashSet<String> = main_ir_raw.lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 && parts[0] == "func" {
                Some(parts[1].to_string())
            } else {
                None
            }
        })
        .collect();
    for line in main_ir_raw.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let mut rewritten = false;
        if parts.len() >= 3 {
            match parts[0] {
                "load" => {
                    // load REG NAME — rewrite NAME if it's an imported global
                    if let Some(&prefixed) = global_map.get(parts[2]) {
                        main_ir.push_str(&format!("load {} {}", parts[1], prefixed));
                        rewritten = true;
                    }
                }
                "store" => {
                    // store NAME REG — rewrite NAME if it's an imported global
                    if let Some(&prefixed) = global_map.get(parts[1]) {
                        main_ir.push_str(&format!("store {} {}", prefixed, parts[2]));
                        rewritten = true;
                    }
                }
                "call" if parts.len() >= 5 => {
                    let fname = parts[2];
                    if !main_func_names.contains(fname) {
                        if let Some(&prefixed) = function_map.get(fname) {
                            main_ir.push_str(&format!("call {} {} {}", parts[1], prefixed, parts[3..].join(" ")));
                            rewritten = true;
                        }
                    }
                }
                "callv" if parts.len() >= 3 => {
                    let fname = parts[1];
                    if !main_func_names.contains(fname) {
                        if let Some(&prefixed) = function_map.get(fname) {
                            main_ir.push_str(&format!("callv {} {}", prefixed, parts[2..].join(" ")));
                            rewritten = true;
                        }
                    }
                }
                _ => {}
            }
        }
        if !rewritten {
            main_ir.push_str(line);
        }
        main_ir.push('\n');
        if parts.len() >= 2 && parts[0] == "func" && parts[1] == "main" {
            for (idx, init_func) in imported_init_funcs.iter().enumerate() {
                main_ir.push_str(&format!("call {} {} int 0\n", 900000 + idx, init_func));
            }
        }
    }
    all_ir.push_str(&main_ir);

    Ok(rewrite_call_retkinds(&all_ir))
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
        "std.encoding", "std.hash",
        "std.fmt", "std.net.tcp", "std.net.dns",
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

    dump_ir_if_requested(&ir_text);

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

fn unique_run_artifact_paths() -> (std::path::PathBuf, std::path::PathBuf) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_else(|_| std::time::Duration::from_secs(0))
        .as_nanos();
    let exe_path = env::temp_dir().join(format!("forge_ir_{}_{}", std::process::id(), stamp));
    let obj_path = env::temp_dir().join(format!("forge_ir_{}_{}.o", std::process::id(), stamp));
    (obj_path, exe_path)
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

    dump_ir_if_requested(&ir_text);

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
                        let (obj_path, exe_path) = unique_run_artifact_paths();
                        let keep_artifacts = std::env::var("FORGE_KEEP_RUN_ARTIFACTS").is_ok();
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
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
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
