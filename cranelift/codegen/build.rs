use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

fn main() {
    if let Err(err) = generate_runtime_table() {
        eprintln!("failed to generate runtime table: {err}");
        std::process::exit(1);
    }
}

fn generate_runtime_table() -> Result<(), String> {
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").map_err(|e| e.to_string())?);
    let abi_path = manifest_dir.join("../runtime-abi/runtime_functions.txt");
    println!("cargo:rerun-if-changed={}", abi_path.display());
    let contents =
        fs::read_to_string(&abi_path).map_err(|e| format!("{}: {}", abi_path.display(), e))?;

    let mut table = String::from("const ABI_RUNTIME_FUNCTIONS: &[RuntimeDecl] = &[\n");
    let mut param_counts: BTreeMap<&str, usize> = BTreeMap::new();
    // FNV-1a over the entry lines, comments and blank lines left out. the
    // bootstrap seed and a built ir_driver carry it, so the cli can tell
    // when they were made against another table.
    let mut abi_hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (line_no, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = |msg: &str| format!("{}:{}: {}", abi_path.display(), line_no + 1, msg);
        for byte in line.bytes().chain(std::iter::once(b'\n')) {
            abi_hash = (abi_hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
        }
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() != 4 {
            return Err(at("expected 4 pipe-separated columns"));
        }
        let key = parts[0].trim();
        let symbol = parts[1].trim();
        let params = parse_type_list(parts[2].trim()).map_err(|err| at(&err))?;
        let returns = parse_type_list(parts[3].trim()).map_err(|err| at(&err))?;
        // two returns are the register result pair, which the runtime hands
        // back as a two-word `#[repr(C)]` struct. only two integer words are
        // returned in registers the same way by the runtime's C ABI and by a
        // Cranelift signature on every supported target, so nothing else is
        // accepted.
        if returns.len() > 1 && returns != ["I64", "I64"] {
            return Err(at(
                "a runtime function returns nothing, one value, or the pair I64,I64",
            ));
        }
        param_counts.insert(key, params.len());
        table.push_str(&format!(
            "    RuntimeDecl {{ key: \"{key}\", symbol: \"{symbol}\", params: {}, returns: {} }},\n",
            format_type_slice(&params),
            format_type_slice(&returns)
        ));
    }
    table.push_str("];\n");
    let out = format!(
        "{table}\n/// identifies the runtime function table this compiler was built\n\
         /// against: a hash of cranelift/runtime-abi/runtime_functions.txt.\n\
         pub const RUNTIME_ABI: &str = \"{abi_hash:016x}\";\n"
    );

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").map_err(|e| e.to_string())?);
    fs::write(out_dir.join("runtime_table.rs"), out).map_err(|e| e.to_string())?;
    generate_ownership_table(&manifest_dir, &param_counts)
}

// cranelift/runtime-abi/runtime_ownership.txt says what each runtime function
// does with the counts of its arguments and its result, for the count checker.
// every key in runtime_functions.txt needs exactly one row with the same
// number of parameters, so a new runtime function can't be added without
// deciding its ownership.
fn generate_ownership_table(
    manifest_dir: &std::path::Path,
    param_counts: &BTreeMap<&str, usize>,
) -> Result<(), String> {
    let path = manifest_dir.join("../runtime-abi/runtime_ownership.txt");
    println!("cargo:rerun-if-changed={}", path.display());
    let contents = fs::read_to_string(&path).map_err(|e| format!("{}: {}", path.display(), e))?;

    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut table = String::from("const RUNTIME_OWNERSHIP: &[RuntimeOwnership] = &[\n");
    for (line_no, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = |msg: String| format!("{}:{}: {}", path.display(), line_no + 1, msg);
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() < 3 {
            return Err(at(
                "expected key|params|result, then optional evidence and note columns".into(),
            ));
        }
        let key = parts[0].trim();
        let Some(&expected) = param_counts.get(key) else {
            return Err(at(format!("'{key}' is not a key in runtime_functions.txt")));
        };
        if !seen.insert(key.to_string()) {
            return Err(at(format!("'{key}' has more than one row")));
        }
        let params_text = parts[1].trim();
        let params: Vec<&str> = if params_text.is_empty() {
            vec![]
        } else {
            params_text.split(',').map(str::trim).collect()
        };
        if params.len() != expected {
            return Err(at(format!(
                "'{key}' lists {} parameters; runtime_functions.txt has {expected}",
                params.len()
            )));
        }
        let mut arg_items = Vec::new();
        for p in &params {
            arg_items.push(match *p {
                "-" => "ArgOwnership::Plain",
                "b" => "ArgOwnership::Borrow",
                "c" => "ArgOwnership::Consume",
                other => {
                    return Err(at(format!(
                        "unknown parameter token '{other}'; use -, b or c"
                    )))
                }
            });
        }
        let result = parse_result_token(parts[2].trim(), params.len()).map_err(at)?;
        table.push_str(&format!(
            "    RuntimeOwnership {{ key: \"{key}\", args: &[{}], result: {result} }},\n",
            arg_items.join(", ")
        ));
    }
    table.push_str("];\n");
    let missing: Vec<&str> = param_counts
        .keys()
        .copied()
        .filter(|k| !seen.contains(*k))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "{}: no ownership row for {} runtime function(s): {}",
            path.display(),
            missing.len(),
            missing.join(", ")
        ));
    }
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").map_err(|e| e.to_string())?);
    fs::write(out_dir.join("runtime_ownership_table.rs"), table).map_err(|e| e.to_string())
}

// `-`, `own`, `borrow:N`, or one of those after `pair:` for a function that
// returns the (flag, payload) register pair.
fn parse_result_token(text: &str, nparams: usize) -> Result<String, String> {
    let (pair, rest) = match text.strip_prefix("pair:") {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let value =
        match rest {
            "-" => "ResultValue::Plain".to_string(),
            "own" => "ResultValue::Own".to_string(),
            _ => match rest
                .strip_prefix("borrow:")
                .and_then(|n| n.parse::<usize>().ok())
            {
                Some(n) if n < nparams => format!("ResultValue::Borrow({n})"),
                Some(n) => {
                    return Err(format!(
                        "result borrows from argument {n}, but there are {nparams}"
                    ))
                }
                None => return Err(format!(
                    "unknown result token '{text}'; use -, own or borrow:N, optionally after pair:"
                )),
            },
        };
    Ok(format!(
        "ResultOwnership {{ pair: {pair}, value: {value} }}"
    ))
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
    let items: Vec<String> = types.iter().map(|name| format!("types::{name}")).collect();
    format!("&[{}]", items.join(", "))
}
