use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(forge_cranelift_new_api)");

    let Some(lockfile) = workspace_lockfile() else {
        return;
    };
    let Ok(contents) = fs::read_to_string(&lockfile) else {
        return;
    };
    if uses_new_cranelift_api(&contents) {
        println!("cargo:rustc-cfg=forge_cranelift_new_api");
    }

    if let Err(err) = generate_runtime_table() {
        panic!("failed to generate runtime table: {err}");
    }
}

fn workspace_lockfile() -> Option<PathBuf> {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").ok()?);
    for ancestor in out_dir.ancestors() {
        if ancestor.file_name().and_then(|name| name.to_str()) == Some("target") {
            let lockfile = ancestor.parent()?.join("Cargo.lock");
            if lockfile.exists() {
                return Some(lockfile);
            }
        }
    }
    None
}

fn uses_new_cranelift_api(lockfile: &str) -> bool {
    let mut in_frontend = false;
    for line in lockfile.lines() {
        let trimmed = line.trim();
        if trimmed == "[[package]]" {
            in_frontend = false;
            continue;
        }
        if trimmed == "name = \"cranelift-frontend\"" {
            in_frontend = true;
            continue;
        }
        if in_frontend && trimmed.starts_with("version = ") {
            let version = trimmed.trim_start_matches("version = ").trim_matches('"');
            return version_is_new_api(version);
        }
    }
    false
}

fn version_is_new_api(version: &str) -> bool {
    let mut parts = version.split('.');
    let major = parts.next().and_then(|part| part.parse::<u32>().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|part| part.parse::<u32>().ok()).unwrap_or(0);
    major > 0 || minor >= 130
}

fn generate_runtime_table() -> Result<(), String> {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").map_err(|e| e.to_string())?);
    let abi_path = manifest_dir.join("../runtime-abi/runtime_functions.txt");
    println!("cargo:rerun-if-changed={}", abi_path.display());
    let contents = fs::read_to_string(&abi_path)
        .map_err(|e| format!("{}: {}", abi_path.display(), e))?;

    let mut out = String::new();
    out.push_str("const RUNTIME_FUNCTIONS: &[RuntimeDecl] = &[\n");
    for (line_no, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() != 4 {
            return Err(format!("{}:{}: expected 4 pipe-separated columns", abi_path.display(), line_no + 1));
        }
        let key = parts[0].trim();
        let symbol = parts[1].trim();
        let params = parse_type_list(parts[2].trim())
            .map_err(|err| format!("{}:{}: {}", abi_path.display(), line_no + 1, err))?;
        let returns = parse_type_list(parts[3].trim())
            .map_err(|err| format!("{}:{}: {}", abi_path.display(), line_no + 1, err))?;
        out.push_str("    RuntimeDecl { key: \"");
        out.push_str(key);
        out.push_str("\", symbol: \"");
        out.push_str(symbol);
        out.push_str("\", params: ");
        out.push_str(&format_type_slice(&params));
        out.push_str(", returns: ");
        out.push_str(&format_type_slice(&returns));
        out.push_str(" },\n");
    }
    out.push_str("];\n");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").map_err(|e| e.to_string())?);
    fs::write(out_dir.join("runtime_table.rs"), out).map_err(|e| e.to_string())
}

fn parse_type_list(text: &str) -> Result<Vec<&str>, String> {
    if text.is_empty() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for part in text.split(',') {
        let name = part.trim();
        match name {
            "I64" | "I32" | "F64" => out.push(name),
            _ => return Err(format!("unknown type token '{name}'")),
        }
    }
    Ok(out)
}

fn format_type_slice(types: &[&str]) -> String {
    if types.is_empty() {
        return "&[]".to_string();
    }
    let items: Vec<String> = types
        .iter()
        .map(|name| format!("types::{name}"))
        .collect();
    format!("&[{}]", items.join(", "))
}
