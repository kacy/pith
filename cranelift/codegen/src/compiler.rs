//! AST to Cranelift IR translation with two-pass compilation
//!
//! First pass: Declare all functions
//! Second pass: Compile all function bodies

use crate::ast::{AstNode, BinaryOp, UnaryOp};
use crate::{forge_type_to_cranelift, CodeGen, CompileError};
use cranelift::prelude::*;
use cranelift_codegen::ir::GlobalValue;
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use std::collections::HashMap;

/// Local variable slot using Cranelift's Variable system for SSA
#[derive(Debug)]
pub struct LocalVar {
    pub var: Variable,
    pub ty: Type,
    pub kind: ValueKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    Unknown,
    String,
    Int,
    Bool,
    ListString,
    ListUnknown,
    Map,
    MapIntKey,
    Set,
    Struct,
}

/// Variable counter for generating unique variable indices
static mut VAR_COUNTER: u32 = 0;

fn next_variable() -> Variable {
    unsafe {
        let var = Variable::from_u32(VAR_COUNTER);
        VAR_COUNTER += 1;
        var
    }
}

/// Lambda counter for generating unique lambda function names
static mut LAMBDA_COUNTER: u32 = 0;

fn next_lambda_name() -> String {
    unsafe {
        let id = LAMBDA_COUNTER;
        LAMBDA_COUNTER += 1;
        format!("__lambda_{}", id)
    }
}

/// Global map from lambda node pointer to its captured variable names.
/// Populated during the pre-pass and read-only during body compilation.
/// Single-threaded access only.
static LAMBDA_CAPTURES_MAP: std::sync::OnceLock<std::sync::Mutex<HashMap<usize, Vec<String>>>> =
    std::sync::OnceLock::new();

fn get_lambda_captures(ptr: usize) -> Vec<String> {
    if let Some(mutex) = LAMBDA_CAPTURES_MAP.get() {
        if let Ok(map) = mutex.lock() {
            return map.get(&ptr).cloned().unwrap_or_default();
        }
    }
    Vec::new()
}

fn set_lambda_captures(captures_map: HashMap<usize, Vec<String>>) {
    let mutex = LAMBDA_CAPTURES_MAP.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Ok(mut map) = mutex.lock() {
        *map = captures_map;
    }
}

/// Information about a compiled lambda
#[derive(Debug, Clone)]
pub struct LambdaInfo {
    pub func_id: FuncId,
    pub lambda_params: Vec<(String, String)>, // (name, type)
    pub capture_vars: Vec<String>,
    pub name: String,
}

/// Find all identifier names referenced in a node that are NOT declared within it.
/// This is used to discover free variables (captures) for lambdas.
fn find_free_vars(
    node: &AstNode,
    bound: &std::collections::HashSet<String>,
    free: &mut Vec<String>,
) {
    match node {
        AstNode::Identifier(name) => {
            if !bound.contains(name) && !free.contains(name) {
                free.push(name.clone());
            }
        }
        AstNode::BinaryOp { left, right, .. } => {
            find_free_vars(left, bound, free);
            find_free_vars(right, bound, free);
        }
        AstNode::UnaryOp { operand, .. } => find_free_vars(operand, bound, free),
        AstNode::Call { args, .. } => {
            for a in args {
                find_free_vars(a, bound, free);
            }
        }
        AstNode::Block(stmts) => {
            let mut bound2 = bound.clone();
            for s in stmts {
                if let AstNode::Let { name, value, .. } = s {
                    find_free_vars(value, &bound2, free);
                    bound2.insert(name.clone());
                } else {
                    find_free_vars(s, &bound2, free);
                }
            }
        }
        AstNode::If {
            cond,
            then_branch,
            else_branch,
        } => {
            find_free_vars(cond, bound, free);
            find_free_vars(then_branch, bound, free);
            if let Some(e) = else_branch {
                find_free_vars(e, bound, free);
            }
        }
        AstNode::Return(Some(e)) => find_free_vars(e, bound, free),
        _ => {}
    }
}

/// Walk AST and collect all lambdas, using pointer identity to assign unique names.
/// Returns a HashMap from lambda node pointer to (name, func_params_including_captures)
fn collect_lambda_ids(
    node: &AstNode,
    out: &mut HashMap<usize, (String, Vec<(String, String)>, Vec<String>)>,
) {
    match node {
        AstNode::Lambda { params, body, .. } => {
            let ptr = node as *const AstNode as usize;
            if !out.contains_key(&ptr) {
                let name = next_lambda_name();
                // Find free variables (captures) by analyzing the body
                let mut bound: std::collections::HashSet<String> =
                    params.iter().map(|(n, _)| n.clone()).collect();
                let mut captures: Vec<String> = Vec::new();
                find_free_vars(body, &bound, &mut captures);
                // Filter out built-in function names that shouldn't be captured
                let runtime_names = [
                    "print",
                    "to_string",
                    "len",
                    "push",
                    "pop",
                    "join",
                    "split",
                    "contains",
                    "starts_with",
                    "ends_with",
                    "trim",
                    "upper",
                    "lower",
                    "abs",
                    "min",
                    "max",
                    "floor",
                    "ceil",
                    "sqrt",
                    "pow",
                    "str",
                    "int",
                    "float",
                    "bool",
                    "range",
                    "input",
                ];
                captures.retain(|c| !runtime_names.contains(&c.as_str()));
                // Lambda function signature: only declared params (I64 each).
                // Captured variables are accessed via forge_closure_get_env() in the body.
                let full_params = params.clone();
                out.insert(ptr, (name, full_params, captures));
            }
            collect_lambda_ids(body, out);
        }
        AstNode::Block(stmts) => {
            for s in stmts {
                collect_lambda_ids(s, out);
            }
        }
        AstNode::Let { value, .. } => collect_lambda_ids(value, out),
        AstNode::Assign { value, .. } => collect_lambda_ids(value, out),
        AstNode::Return(Some(e)) => collect_lambda_ids(e, out),
        AstNode::Call { args, .. } => {
            for a in args {
                collect_lambda_ids(a, out);
            }
        }
        AstNode::If {
            cond,
            then_branch,
            else_branch,
        } => {
            collect_lambda_ids(cond, out);
            collect_lambda_ids(then_branch, out);
            if let Some(e) = else_branch {
                collect_lambda_ids(e, out);
            }
        }
        AstNode::While { cond, body } => {
            collect_lambda_ids(cond, out);
            collect_lambda_ids(body, out);
        }
        AstNode::BinaryOp { left, right, .. } => {
            collect_lambda_ids(left, out);
            collect_lambda_ids(right, out);
        }
        AstNode::For { body, .. } => collect_lambda_ids(body, out),
        _ => {}
    }
}

/// Try to find a field offset by searching all known struct layouts.
/// Used as a heuristic when the struct type is unknown.
fn find_field_in_any_struct(field_name: &str) -> Option<usize> {
    // Check all registered structs for this field name
    // If found in exactly one struct, use that offset
    let layouts = crate::STRUCT_LAYOUTS.get()?;
    let map = layouts.lock().ok()?;
    let mut found: Option<usize> = None;
    for (_sname, fields) in map.iter() {
        for (fname, offset) in fields {
            if fname == field_name {
                found = Some(*offset);
                // Don't break - continue to check if ambiguous
                // For now, just use the first match
                return Some(*offset);
            }
        }
    }
    found
}

fn infer_kind_from_type_name(ty: &str) -> ValueKind {
    match ty {
        "String" => ValueKind::String,
        "Bool" => ValueKind::Bool,
        "Int" | "Float" => ValueKind::Int,
        _ if ty.starts_with("List[String]") => ValueKind::ListString,
        _ if ty.starts_with("List[") => ValueKind::ListUnknown,
        _ if ty.starts_with("Map[Int,") || ty.starts_with("Map[Int]") => ValueKind::MapIntKey,
        _ if ty.starts_with("Map[") => ValueKind::Map,
        _ if ty.starts_with("Set[") => ValueKind::Set,
        _ if crate::get_struct_layout(ty).is_some() => ValueKind::Struct,
        _ if ty
            .chars()
            .next()
            .map(|c| c.is_ascii_uppercase())
            .unwrap_or(false)
            && !ty.contains('[') =>
        {
            ValueKind::Struct
        }
        _ => ValueKind::Unknown,
    }
}

fn infer_value_kind(node: &AstNode, variables: &HashMap<String, LocalVar>) -> ValueKind {
    match node {
        AstNode::StringLiteral(_) | AstNode::StringInterp { .. } => ValueKind::String,
        AstNode::BoolLiteral(_) => ValueKind::Bool,
        AstNode::IntLiteral(_) | AstNode::FloatLiteral(_) => ValueKind::Int,
        AstNode::BinaryOp { op, left, right } => {
            // String concatenation returns a String
            if matches!(op, BinaryOp::Add) {
                let lk = infer_value_kind(left, variables);
                let rk = infer_value_kind(right, variables);
                if matches!(lk, ValueKind::String)
                    || matches!(rk, ValueKind::String)
                    || matches!(
                        left.as_ref(),
                        AstNode::StringLiteral(_) | AstNode::StringInterp { .. }
                    )
                    || matches!(
                        right.as_ref(),
                        AstNode::StringLiteral(_) | AstNode::StringInterp { .. }
                    )
                {
                    return ValueKind::String;
                }
            }
            // Comparison and logical operators return Bool
            if matches!(
                op,
                BinaryOp::Eq
                    | BinaryOp::Neq
                    | BinaryOp::Lt
                    | BinaryOp::Gt
                    | BinaryOp::Lte
                    | BinaryOp::Gte
                    | BinaryOp::And
                    | BinaryOp::Or
            ) {
                return ValueKind::Bool;
            }
            ValueKind::Int
        }
        AstNode::ListLiteral { elements, .. } => {
            if elements
                .first()
                .map(|e| matches!(e, AstNode::StringLiteral(_) | AstNode::StringInterp { .. }))
                .unwrap_or(false)
            {
                ValueKind::ListString
            } else {
                ValueKind::ListUnknown
            }
        }
        AstNode::MapLiteral { .. } => ValueKind::Map,
        AstNode::SetLiteral { .. } => ValueKind::Set,
        AstNode::StructInit { .. } => ValueKind::Struct,
        AstNode::Identifier(name) => variables.get(name).map(|v| v.kind).unwrap_or_else(|| {
            // Check if it's a global variable with a known type
            if let Some(gtype) = crate::get_global_var_type(name) {
                infer_kind_from_type_name(&gtype)
            } else {
                ValueKind::Unknown
            }
        }),
        AstNode::FieldAccess { obj, field } => {
            let field_name = field.strip_prefix('.').unwrap_or(field);
            // Try to determine from struct type info (local vars, then globals)
            let struct_type = match obj.as_ref() {
                AstNode::Identifier(name) => crate::get_var_struct_type(name).or_else(|| {
                    // Check if global var has a struct type
                    crate::get_global_var_type(name).and_then(|gtype| {
                        if crate::get_struct_layout(&gtype).is_some() {
                            Some(gtype)
                        } else {
                            None
                        }
                    })
                }),
                _ => None,
            };
            if let Some(ref stype) = struct_type {
                if let Some(ftype) = crate::get_struct_field_type(stype, field_name) {
                    return infer_kind_from_type_name(&ftype);
                }
            }
            // Fallback heuristics for common struct field names
            match field.as_str() {
                ".children" | ".param_types" | ".items" | ".fields" | ".params" | ".args"
                | ".entries" | ".elements" => ValueKind::ListUnknown,
                ".value" | ".kind" | ".name" | ".doc" | ".sig" | ".path" | ".type_name"
                | ".message" | ".text" | ".label" | ".key" | ".module_name" | ".return_type" => {
                    ValueKind::String
                }
                ".line" | ".column" | ".count" | ".index" | ".offset" | ".length" | ".size" => {
                    ValueKind::Int
                }
                ".is_mutable" | ".is_public" | ".is_optional" | ".has_default" | ".exported"
                | ".resolved" => ValueKind::Bool,
                _ => ValueKind::Unknown,
            }
        }
        AstNode::Index { expr, .. } => match infer_value_kind(expr, variables) {
            ValueKind::String | ValueKind::ListString => ValueKind::String,
            ValueKind::Map | ValueKind::MapIntKey => {
                // Infer map value type from variable type annotation
                if let AstNode::Identifier(name) = expr.as_ref() {
                    let map_type = crate::get_global_var_type(name).or_else(|| {
                        crate::get_var_struct_type(&format!("__map_val_{}", name))
                            .map(|s| s.to_string())
                    });
                    if let Some(ref ty) = map_type {
                        if let Some(comma) = ty.find(',') {
                            let val_part = ty[comma + 1..].trim().trim_end_matches(']');
                            return infer_kind_from_type_name(val_part);
                        }
                    }
                }
                ValueKind::Unknown
            }
            ValueKind::ListUnknown => {
                // Check if the list holds structs (e.g., List[ScopeEntry])
                if let AstNode::Identifier(name) = expr.as_ref() {
                    let elem_type = crate::get_var_struct_type(&format!("__list_elem_{}", name))
                        .map(|s| s.to_string())
                        .or_else(|| {
                            crate::get_global_var_type(name).and_then(|gtype| {
                                if gtype.starts_with("List[") && gtype.ends_with(']') {
                                    Some(gtype[5..gtype.len() - 1].to_string())
                                } else {
                                    None
                                }
                            })
                        });
                    if let Some(ref elem) = elem_type {
                        if crate::get_struct_layout(elem).is_some() {
                            return ValueKind::Struct;
                        }
                        return infer_kind_from_type_name(elem);
                    }
                }
                ValueKind::Unknown
            }
            _ => ValueKind::Unknown,
        },
        AstNode::Call { func, args } => match func.as_str() {
            "substring"
            | "trim"
            | "trim_left"
            | "trim_whitespace"
            | "join"
            | "read_file"
            | "input"
            | "env"
            | "d_trim_left"
            | "d_trim_right"
            | "get_type_name"
            | "convert_path_to_module"
            | "to_upper"
            | "to_lower"
            | "reverse"
            | "replace"
            | "repeat"
            | "pad_left"
            | "pad_right"
            | "to_string"
            | "char_at"
            | "sha256"
            | "hex_encode"
            | "hex_decode"
            | "base64_encode"
            | "base64_decode"
            | "path_join"
            | "path_dir"
            | "path_base"
            | "path_ext"
            | "path_stem"
            | "scheme"
            | "host"
            | "query"
            | "fragment"
            | "decode"
            | "type_of"
            | "get_string"
            | "encode" => ValueKind::String,
            "port" => ValueKind::Int,
            "fnv1a" => ValueKind::Int,
            "split" | "args" | "keys" | "values" | "list_dir" | "chars" | "object_keys" => {
                ValueKind::ListString
            }
            "sort" | "slice" => {
                // Preserve element type from the source list
                if let Some(first_arg) = args.first() {
                    infer_value_kind(first_arg, variables)
                } else {
                    ValueKind::ListUnknown
                }
            }
            "len" | "time" | "random_int" | "ord" | "index_of" | "last_index_of"
            | "get_int" | "array_len" => ValueKind::Int,
            "contains" | "contains_key" | "starts_with" | "ends_with" | "string_starts_with"
            | "dir_exists" | "file_exists" | "is_empty" | "object_has" | "get_bool" | "has" => ValueKind::Bool,
            _ => {
                // Check registered function return types
                if let Some(ret_type) = crate::get_func_return_type(func) {
                    infer_kind_from_type_name(&ret_type)
                } else {
                    ValueKind::Unknown
                }
            }
        },
        _ => ValueKind::Unknown,
    }
}

/// Collect all string literals from AST
/// Strip surrounding/edge double-quotes from string literals (zig parser includes them)
fn strip_string_quotes(s: &str) -> String {
    // String literals from the zig lexer include surrounding double-quotes as characters.
    // StringInterp literal parts may have a leading " (first part) or trailing " (last part).
    // Strip leading " and/or trailing " from the content.
    let s = if s.starts_with('"') { &s[1..] } else { s };
    let s = if s.ends_with('"') && !s.ends_with("\\\"") {
        &s[..s.len() - 1]
    } else {
        s
    };
    process_escape_sequences(s)
}

/// Process escape sequences in string literals: \n, \t, \\, \", \r, \0
fn process_escape_sequences(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => result.push('\n'),
                Some('t') => result.push('\t'),
                Some('r') => result.push('\r'),
                Some('0') => result.push('\0'),
                Some('\\') => result.push('\\'),
                Some('"') => result.push('"'),
                Some('\'') => result.push('\''),
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(c);
        }
    }
    result
}

fn collect_strings(node: &AstNode, strings: &mut Vec<String>) {
    match node {
        AstNode::StringLiteral(s) => strings.push(strip_string_quotes(s)),
        AstNode::StringInterp { parts } => {
            for part in parts {
                match part {
                    crate::ast::StringInterpPart::Literal(s) => {
                        strings.push(strip_string_quotes(s))
                    }
                    crate::ast::StringInterpPart::Expr(expr) => collect_strings(expr, strings),
                }
            }
        }
        AstNode::BinaryOp { left, right, .. } => {
            collect_strings(left, strings);
            collect_strings(right, strings);
        }
        AstNode::UnaryOp { operand, .. } => collect_strings(operand, strings),
        AstNode::Call { args, .. } => {
            for arg in args {
                collect_strings(arg, strings);
            }
        }

        AstNode::ListLiteral { elements, .. } => {
            for elem in elements {
                collect_strings(elem, strings);
            }
        }
        AstNode::Let { value, .. } => collect_strings(value, strings),
        AstNode::Block(stmts) => {
            for stmt in stmts {
                collect_strings(stmt, strings);
            }
        }
        AstNode::If {
            cond,
            then_branch,
            else_branch,
        } => {
            collect_strings(cond, strings);
            collect_strings(then_branch, strings);
            if let Some(else_) = else_branch {
                collect_strings(else_, strings);
            }
        }
        AstNode::While { cond, body } => {
            collect_strings(cond, strings);
            collect_strings(body, strings);
        }
        AstNode::Return(expr) => {
            if let Some(e) = expr {
                collect_strings(e, strings);
            }
        }
        AstNode::For { iterable, body, .. } => {
            collect_strings(iterable, strings);
            collect_strings(body, strings);
        }
        AstNode::MapLiteral { entries, .. } => {
            for (key, value) in entries {
                collect_strings(key, strings);
                collect_strings(value, strings);
            }
        }
        AstNode::SetLiteral { elements, .. } => {
            for elem in elements {
                collect_strings(elem, strings);
            }
        }
        AstNode::Assign { value, .. } => {
            collect_strings(value, strings);
        }
        AstNode::Spawn { expr } => collect_strings(expr, strings),
        AstNode::Await { expr } => collect_strings(expr, strings),
        _ => {}
    }
}

/// Collect generic types used in a node and register them
fn collect_generic_types(node: &AstNode, registry: &mut crate::GenericRegistry) {
    match node {
        AstNode::Let {
            value,
            type_annotation,
            ..
        } => {
            // Register the type annotation if it contains generics
            if let Some(ty) = type_annotation {
                registry.register(ty);
            }
            collect_generic_types(value, registry);
        }
        AstNode::ListLiteral { .. } => {
            // Register List with element type if available
            registry.register("List");
        }
        AstNode::MapLiteral { .. } => {
            // Register Map with key/value types if available
            registry.register("Map");
        }
        AstNode::Call { args, .. } => {
            for arg in args {
                collect_generic_types(arg, registry);
            }
        }
        AstNode::Block(stmts) => {
            for stmt in stmts {
                collect_generic_types(stmt, registry);
            }
        }
        _ => {}
    }
}

/// Parse a file with automatic import resolution
/// Returns merged AST from main file and all imported modules
pub fn parse_file_with_imports(path: &str) -> Result<Vec<AstNode>, CompileError> {
    use crate::parser::parse_file;
    use std::collections::HashSet;

    let mut all_nodes = Vec::new();
    let mut parsed_files = HashSet::new();
    let mut files_to_parse = vec![path.to_string()];

    while let Some(file_path) = files_to_parse.pop() {
        // Skip if already parsed (handles circular imports)
        if parsed_files.contains(&file_path) {
            continue;
        }

        eprintln!("Parsing: {}", file_path);

        // Parse the file
        let mut nodes = parse_file(&file_path)?;

        // Imported helper modules may contain their own standalone `main` entrypoints.
        // Keep only the root file's main to avoid duplicate definitions.
        if file_path != path {
            nodes.retain(|node| {
                !matches!(
                    node,
                    AstNode::Function { name, .. } if name == "main"
                )
            });
        }

        // Collect imports
        for node in &nodes {
            if let AstNode::Import { module, .. } = node {
                // Convert module name to file path
                // e.g., "lexer" -> "lexer.fg"
                let import_path = format!("{}.fg", module);

                // Check if file exists in same directory as main file
                let base_dir = std::path::Path::new(path)
                    .parent()
                    .map(|p| p.to_str().unwrap_or("."))
                    .unwrap_or(".");
                let full_path = format!("{}/{}", base_dir, import_path);

                if std::path::Path::new(&full_path).exists() && !parsed_files.contains(&full_path) {
                    files_to_parse.push(full_path);
                } else if std::path::Path::new(&import_path).exists()
                    && !parsed_files.contains(&import_path)
                {
                    files_to_parse.push(import_path);
                }
            }
        }

        // Add nodes to collection
        all_nodes.extend(nodes);
        parsed_files.insert(file_path);
    }

    eprintln!("Total files parsed: {}", parsed_files.len());
    eprintln!("Total AST nodes: {}", all_nodes.len());

    Ok(all_nodes)
}

/// Compile a module from AST text (output from forge parse command)
/// This is the entry point for the CLI integration
pub fn compile_module_from_text(
    codegen: &mut CodeGen,
    ast_text: &str,
) -> Result<HashMap<String, FuncId>, CompileError> {
    // Parse the AST text
    let ast_nodes = crate::parser::TextAstParser::parse(ast_text)?;

    // Compile using the existing compile_module function
    compile_module(codegen, ast_nodes)
}

/// Compile a module from AST text with import resolution
/// This resolves imports and compiles all dependent modules
pub fn compile_module_from_text_with_imports(
    codegen: &mut CodeGen,
    ast_text: &str,
    base_path: &str,
    get_ast_for_file: &dyn Fn(&str) -> Result<String, CompileError>,
) -> Result<HashMap<String, FuncId>, CompileError> {
    use std::collections::HashSet;

    // Parse the main AST
    let mut all_nodes = crate::parser::TextAstParser::parse(ast_text)?;

    // Track which files we've already processed to avoid cycles
    let mut processed_files: HashSet<String> = HashSet::new();
    processed_files.insert(base_path.to_string());

    // Collect imports from the main file
    let mut imports_to_resolve: Vec<(String, String)> = Vec::new(); // (module_path, import_name)

    for node in &all_nodes {
        if let AstNode::Import { module, names: _ } = node {
            // Get the directory of the base file
            let base_dir = std::path::Path::new(base_path)
                .parent()
                .map(|p| p.to_str().unwrap_or("."))
                .unwrap_or(".");

            // Convert module path to file path
            // e.g., "types" -> "self-host/types.fg"
            // Handle both "types" and "self-host/types" style imports
            let module_file = if module.contains('/') {
                // Already has path separators, use as-is
                format!("{}.fg", module)
            } else {
                // Simple module name, look in same directory
                format!("{}/{}.fg", base_dir, module)
            };

            // Also check in std/ directory for standard library modules
            let std_module_file = format!("std/{}.fg", module);

            if !processed_files.contains(&module_file) {
                // Check if file exists in base directory first
                if std::path::Path::new(&module_file).exists() {
                    imports_to_resolve.push((module_file.clone(), module.clone()));
                    processed_files.insert(module_file);
                } else if std::path::Path::new(&std_module_file).exists() {
                    // Try std/ directory
                    imports_to_resolve.push((std_module_file.clone(), module.clone()));
                    processed_files.insert(std_module_file);
                } else {
                    // Fallback to base directory path (will fail later if not found)
                    imports_to_resolve.push((module_file.clone(), module.clone()));
                    processed_files.insert(module_file);
                }
            }
        }
    }

    // Resolve imports (breadth-first to handle dependencies)
    while let Some((module_path, module_name)) = imports_to_resolve.pop() {
        eprintln!("Resolving import: {} (from {})", module_name, module_path);

        // Get AST for imported module
        match get_ast_for_file(&module_path) {
            Ok(import_ast_text) => {
                match crate::parser::TextAstParser::parse(&import_ast_text) {
                    Ok(mut import_nodes) => {
                        // Remove main function from imported modules to avoid conflicts
                        import_nodes.retain(|node| {
                            !matches!(
                                node,
                                AstNode::Function { name, .. } if name == "main"
                            )
                        });

                        // Collect more imports from this file
                        for node in &import_nodes {
                            if let AstNode::Import { module, .. } = node {
                                let base_dir = std::path::Path::new(&module_path)
                                    .parent()
                                    .map(|p| p.to_str().unwrap_or("."))
                                    .unwrap_or(".");

                                let next_module_file = if module.contains('/') {
                                    format!("{}.fg", module)
                                } else {
                                    format!("{}/{}.fg", base_dir, module)
                                };

                                // Also check in std/ directory
                                let std_module_file = format!("std/{}.fg", module);

                                if !processed_files.contains(&next_module_file) {
                                    if std::path::Path::new(&next_module_file).exists() {
                                        imports_to_resolve
                                            .push((next_module_file.clone(), module.clone()));
                                        processed_files.insert(next_module_file);
                                    } else if std::path::Path::new(&std_module_file).exists() {
                                        imports_to_resolve
                                            .push((std_module_file.clone(), module.clone()));
                                        processed_files.insert(std_module_file);
                                    } else {
                                        imports_to_resolve
                                            .push((next_module_file.clone(), module.clone()));
                                        processed_files.insert(next_module_file);
                                    }
                                }
                            }
                        }

                        // Add import nodes to collection
                        all_nodes.extend(import_nodes);
                    }
                    Err(e) => {
                        eprintln!("Warning: Failed to parse {}: {:?}", module_path, e);
                    }
                }
            }
            Err(e) => {
                eprintln!("Warning: Failed to get AST for {}: {:?}", module_path, e);
            }
        }
    }

    eprintln!("Total modules resolved: {}", processed_files.len());
    eprintln!("Total AST nodes: {}", all_nodes.len());

    // Compile the merged AST
    compile_module(codegen, all_nodes)
}

/// Compile all functions from AST with two-pass approach
pub fn compile_module(
    codegen: &mut CodeGen,
    ast_nodes: Vec<AstNode>,
) -> Result<HashMap<String, FuncId>, CompileError> {
    // Collect all string literals first
    let mut all_strings = Vec::new();
    for node in &ast_nodes {
        match node {
            AstNode::Function { body, .. } => collect_strings(body, &mut all_strings),
            AstNode::Test { body, .. } => collect_strings(body, &mut all_strings),
            AstNode::Let { value, .. } => collect_strings(value, &mut all_strings),
            _ => {}
        }
    }
    // Deduplicate (preserving order)
    let mut seen = std::collections::HashSet::new();
    all_strings.retain(|s| seen.insert(s.clone()));

    // Declare string data (strings are already quote-stripped by collect_strings)
    let mut string_funcs = HashMap::new();
    for (i, s) in all_strings.iter().enumerate() {
        let name = format!("str_{}", i);
        match crate::declare_string_data(&mut codegen.module, &name, s) {
            Ok(func_id) => {
                string_funcs.insert(s.clone(), func_id);
            }
            Err(_) => {}
        }
    }

    // Pre-register generic types used in the code
    for node in &ast_nodes {
        if let AstNode::Function { body, .. } = node {
            collect_generic_types(body, &mut codegen.generic_registry);
        }
    }

    // Pass 0: Register all struct layouts
    crate::clear_struct_state();
    for node in &ast_nodes {
        if let AstNode::StructDecl { name, fields, .. } = node {
            crate::register_struct_layout(name, fields);
            eprintln!("Registered struct '{}' with {} fields", name, fields.len());
        }
    }

    // Pass 0.1: Register type aliases
    for node in &ast_nodes {
        if let AstNode::TypeAlias { name, target } = node {
            // If the target is a struct, register the alias as pointing to the same layout
            if crate::get_struct_layout(target).is_some() {
                crate::register_struct_alias(name, target);
            }
        }
    }

    // Pass 0.5: Generic monomorphization
    // Collect generic function declarations and instantiations
    let mut monomorphizer = crate::monomorphize::Monomorphizer::new();
    let mut all_nodes = ast_nodes;

    // First, register all generic function declarations
    // Generic declarations have [T] format, instantiations have _Type format
    for node in &all_nodes {
        if let AstNode::Function {
            name,
            params,
            return_type,
            body,
        } = node
        {
            // Check if this is a generic function declaration (has [T] in name)
            if name.contains('[') && name.contains(']') {
                // Extract base name and type parameters
                if let Some(start) = name.find('[') {
                    let base_name = name[..start].to_string();
                    let end = name.rfind(']').unwrap_or(name.len());
                    let type_params_str = &name[start + 1..end];
                    let type_params: Vec<String> = type_params_str
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .collect();

                    let decl = crate::monomorphize::GenericFunctionDecl {
                        name: base_name.clone(),
                        type_params,
                        params: params.clone(),
                        return_type: return_type.clone(),
                        body: (**body).clone(),
                    };
                    monomorphizer.register_generic_decl(&base_name, decl);
                    eprintln!("Registered generic function: {}", name);
                }
            }
        }
    }

    // Pre-scan variable types for monomorphization inference
    crate::monomorphize::prescan_variable_types(&all_nodes);

    // Collect all generic instantiations from the AST
    let instantiations =
        crate::monomorphize::collect_generic_instantiations(&all_nodes, &monomorphizer);
    eprintln!("Found {} generic instantiations", instantiations.len());

    // Generate monomorphized function variants
    let mut monomorphized_funcs: Vec<AstNode> = Vec::new();
    for (inst_name, base_name, type_args) in instantiations {
        if let Some(decl) = monomorphizer.generic_decls.get(&base_name) {
            if let Some(func_node) = monomorphizer.monomorphize_function(decl, &type_args) {
                eprintln!("Generated monomorphized function: {}", inst_name);
                monomorphized_funcs.push(func_node);
            }
        }
    }

    // Add monomorphized functions to the AST
    all_nodes.extend(monomorphized_funcs);

    // Pass 1: Declare all functions and tests
    let mut declared_funcs = HashMap::new();
    let mut func_signatures: HashMap<String, Vec<Type>> = HashMap::new();

    for node in &all_nodes {
        match node {
            AstNode::Function {
                name,
                params,
                return_type,
                ..
            } => {
                let mut sig = codegen.module.make_signature();

                for (_, ty) in params {
                    let cl_ty = forge_type_to_cranelift(ty);
                    sig.params.push(AbiParam::new(cl_ty));
                }

                let ret_ty = forge_type_to_cranelift(return_type);
                sig.returns.push(AbiParam::new(ret_ty));

                let func_id = codegen
                    .module
                    .declare_function(name, Linkage::Export, &sig)
                    .map_err(|e| CompileError::ModuleError(e.to_string()))?;

                declared_funcs.insert(name.clone(), func_id);
                func_signatures.insert(
                    name.clone(),
                    params
                        .iter()
                        .map(|(_, ty)| forge_type_to_cranelift(ty))
                        .collect(),
                );
                crate::set_func_return_type(name, return_type);
            }
            AstNode::Test { name, .. } => {
                // Declare test functions with no params, void return
                let mut sig = codegen.module.make_signature();
                sig.returns.push(AbiParam::new(types::I64));

                let func_id = codegen
                    .module
                    .declare_function(name, Linkage::Export, &sig)
                    .map_err(|e| CompileError::ModuleError(e.to_string()))?;

                declared_funcs.insert(name.clone(), func_id);
            }
            AstNode::ImplBlock {
                target_type,
                methods,
                ..
            } => {
                // Register impl methods as regular functions
                // Methods are named either "method_name" or "TypeName_method_name"
                for method in methods {
                    if let AstNode::Function {
                        name,
                        params,
                        return_type,
                        ..
                    } = method
                    {
                        let mut sig = codegen.module.make_signature();

                        // Impl methods take the struct as first param (self as i64/pointer)
                        sig.params.push(AbiParam::new(types::I64));
                        for (_, ty) in params {
                            let cl_ty = forge_type_to_cranelift(ty);
                            sig.params.push(AbiParam::new(cl_ty));
                        }

                        let ret_ty = forge_type_to_cranelift(return_type);
                        sig.returns.push(AbiParam::new(ret_ty));

                        // Register under both plain name and TypeName_method_name
                        let plain_name = name.clone();
                        let qualified_name = if !target_type.is_empty() {
                            format!("{}_{}", target_type, name)
                        } else {
                            name.clone()
                        };

                        for reg_name in [&plain_name, &qualified_name] {
                            if !declared_funcs.contains_key(reg_name) {
                                if let Ok(func_id) =
                                    codegen
                                        .module
                                        .declare_function(reg_name, Linkage::Export, &sig)
                                {
                                    declared_funcs.insert(reg_name.clone(), func_id);
                                    let mut param_types: Vec<Type> = vec![types::I64]; // self
                                    param_types.extend(
                                        params.iter().map(|(_, ty)| forge_type_to_cranelift(ty)),
                                    );
                                    func_signatures.insert(reg_name.clone(), param_types);
                                    crate::set_func_return_type(reg_name, return_type);
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Collect test names before consuming all_nodes
    let test_names: Vec<String> = all_nodes
        .iter()
        .filter_map(|node| {
            if let AstNode::Test { name, .. } = node {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect();

    // Collect global variable info (top-level bind/Let nodes)
    let global_vars: Vec<(String, Option<String>, AstNode)> = all_nodes
        .iter()
        .filter_map(|node| {
            if let AstNode::Let {
                name,
                type_annotation,
                value,
            } = node
            {
                Some((name.clone(), type_annotation.clone(), *value.clone()))
            } else {
                None
            }
        })
        .collect();

    // Declare Cranelift data objects for each global (8-byte slots)
    let mut global_data_ids: HashMap<String, DataId> = HashMap::new();
    for (gname, gtype, gvalue) in &global_vars {
        let data_name = format!("__global_{}", gname);
        // Skip duplicate globals (same name from different modules)
        if global_data_ids.contains_key(gname) {
            continue;
        }
        let data_id = codegen
            .module
            .declare_data(&data_name, Linkage::Local, true, false)
            .map_err(|e| CompileError::ModuleError(e.to_string()))?;
        // Define as 8 bytes of zero
        let mut data_desc = DataDescription::new();
        data_desc.define_zeroinit(8);
        codegen
            .module
            .define_data(data_id, &data_desc)
            .map_err(|e| CompileError::ModuleError(e.to_string()))?;
        global_data_ids.insert(gname.clone(), data_id);

        // Store the global's type for type-directed dispatch
        if let Some(ty) = gtype {
            crate::set_global_var_type(gname, ty);
        } else {
            // Infer type name from the initial value expression
            let inferred_type = match gvalue {
                AstNode::IntLiteral(_) => "Int",
                AstNode::FloatLiteral(_) => "Float",
                AstNode::BoolLiteral(_) => "Bool",
                AstNode::StringLiteral(_) | AstNode::StringInterp { .. } => "String",
                AstNode::ListLiteral { .. } => "List",
                AstNode::MapLiteral { .. } => "Map",
                _ => "Unknown",
            };
            crate::set_global_var_type(gname, inferred_type);
        }
    }

    // Pass 2: Compile all function and test bodies
    let runtime_funcs = crate::declare_runtime_functions(&mut codegen.module)?;

    // Lambda pre-pass: collect all lambdas and declare them as functions
    // lambda_funcs: node pointer -> (name, FuncId, params, capture_vars)
    let mut lambda_map: HashMap<usize, (String, Vec<(String, String)>, Vec<String>)> =
        HashMap::new();
    for node in &all_nodes {
        match node {
            AstNode::Function { body, .. } | AstNode::Test { body, .. } => {
                collect_lambda_ids(body, &mut lambda_map);
            }
            _ => {}
        }
    }

    // Declare each lambda as a Cranelift function (Internal linkage)
    let mut lambda_funcs: HashMap<usize, FuncId> = HashMap::new();
    for (&ptr, (name, full_params, _capture_vars)) in &lambda_map {
        let mut sig = codegen.module.make_signature();
        for (_pname, pty) in full_params {
            sig.params.push(AbiParam::new(forge_type_to_cranelift(pty)));
        }
        sig.returns.push(AbiParam::new(types::I64));
        if let Ok(func_id) = codegen.module.declare_function(name, Linkage::Local, &sig) {
            lambda_funcs.insert(ptr, func_id);
            declared_funcs.insert(name.clone(), func_id);
            func_signatures.insert(
                name.clone(),
                full_params
                    .iter()
                    .map(|(_, ty)| forge_type_to_cranelift(ty))
                    .collect(),
            );
        }
    }

    // Store captures in global map so compile_expr can access them for set_env emission
    {
        let captures_map: HashMap<usize, Vec<String>> = lambda_map
            .iter()
            .map(|(&ptr, (_, _, captures))| (ptr, captures.clone()))
            .collect();
        set_lambda_captures(captures_map);
    }

    // Compile lambda bodies as separate functions
    // We need to walk the original AST nodes to find lambda nodes and compile them
    for node in &all_nodes {
        match node {
            AstNode::Function { body, .. } | AstNode::Test { body, .. } => {
                compile_lambda_bodies_in_node(
                    body,
                    codegen,
                    &lambda_funcs,
                    &lambda_map,
                    &runtime_funcs,
                    &declared_funcs,
                    &string_funcs,
                    &func_signatures,
                )?;
            }
            _ => {}
        }
    }

    // Declare and compile __init_globals if there are any global variables
    let init_globals_id = if !global_vars.is_empty() {
        let mut sig = codegen.module.make_signature();
        sig.returns.push(AbiParam::new(types::I64));
        let func_id = codegen
            .module
            .declare_function("__init_globals", Linkage::Local, &sig)
            .map_err(|e| CompileError::ModuleError(e.to_string()))?;
        declared_funcs.insert("__init_globals".to_string(), func_id);

        // Build the init function body: evaluate each global's initial value and store it
        {
            let init_body_stmts: Vec<AstNode> = global_vars
                .iter()
                .map(|(name, _ty, value)| AstNode::Assign {
                    name: name.clone(),
                    value: Box::new(value.clone()),
                })
                .collect();
            let init_body = AstNode::Block(init_body_stmts);
            compile_function_body(
                codegen,
                func_id,
                "__init_globals",
                &[],
                "Void",
                &init_body,
                &runtime_funcs,
                &declared_funcs,
                &string_funcs,
                &global_data_ids,
                &func_signatures,
                &lambda_funcs,
            )?;
        }

        Some(func_id)
    } else {
        None
    };

    for node in &all_nodes {
        if let AstNode::Function {
            name,
            params,
            return_type,
            body,
        } = node
        {
            if let Some(&func_id) = declared_funcs.get(name) {
                compile_function_body(
                    codegen,
                    func_id,
                    name,
                    params,
                    return_type,
                    body,
                    &runtime_funcs,
                    &declared_funcs,
                    &string_funcs,
                    &global_data_ids,
                    &func_signatures,
                    &lambda_funcs,
                )?;
            }
        }
        if let AstNode::Test { name, body } = node {
            if let Some(&func_id) = declared_funcs.get(name) {
                compile_test_body(
                    codegen,
                    func_id,
                    body,
                    &runtime_funcs,
                    &declared_funcs,
                    &string_funcs,
                    &func_signatures,
                    &lambda_funcs,
                    &global_data_ids,
                )?;
            }
        }
        // Compile impl block methods
        if let AstNode::ImplBlock {
            target_type,
            methods,
            ..
        } = node
        {
            for method in methods {
                if let AstNode::Function {
                    name,
                    params,
                    return_type,
                    body,
                } = method
                {
                    // Methods take `self` as first param — use target_type so struct tracking works
                    let mut method_params = vec![("self".to_string(), target_type.clone())];
                    method_params.extend(params.clone());

                    let plain_name = name.clone();
                    let qualified_name = if !target_type.is_empty() {
                        format!("{}_{}", target_type, name)
                    } else {
                        name.clone()
                    };

                    for reg_name in [&plain_name, &qualified_name] {
                        if let Some(&func_id) = declared_funcs.get(reg_name) {
                            compile_function_body(
                                codegen,
                                func_id,
                                reg_name,
                                &method_params,
                                return_type,
                                body,
                                &runtime_funcs,
                                &declared_funcs,
                                &string_funcs,
                                &global_data_ids,
                                &func_signatures,
                                &lambda_funcs,
                            )?;
                            break; // Only compile once (plain_name and qualified_name share same func_id if different)
                        }
                    }
                }
            }
        }
    }

    // Check if we need to generate a test runner
    if !declared_funcs.contains_key("main") && !test_names.is_empty() {
        // Generate test runner main()
        generate_test_runner(codegen, &runtime_funcs, &declared_funcs, &test_names)?;
    }

    Ok(declared_funcs)
}

/// Compile lambda bodies found inside a node as separate Cranelift functions
#[allow(clippy::too_many_arguments)]
fn compile_lambda_bodies_in_node(
    node: &AstNode,
    codegen: &mut CodeGen,
    lambda_funcs: &HashMap<usize, FuncId>,
    lambda_map: &HashMap<usize, (String, Vec<(String, String)>, Vec<String>)>,
    runtime_funcs: &HashMap<String, FuncId>,
    declared_funcs: &HashMap<String, FuncId>,
    string_funcs: &HashMap<String, FuncId>,
    func_signatures: &HashMap<String, Vec<Type>>,
) -> Result<(), CompileError> {
    match node {
        AstNode::Lambda {
            params,
            body,
            capture_vars,
            ..
        } => {
            let ptr = node as *const AstNode as usize;
            if let (Some(&func_id), Some((name, full_params, captures))) =
                (lambda_funcs.get(&ptr), lambda_map.get(&ptr))
            {
                // Compile the lambda as a function body with its params.
                // Captured vars are accessed via CLOSURE_ENV slots.
                eprintln!(
                    "DEBUG: Compiling lambda body for '{}' with {} params",
                    name,
                    full_params.len()
                );
                compile_function_body_with_captures(
                    codegen,
                    func_id,
                    name,
                    full_params,
                    "Int", // lambdas return I64
                    body,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    &HashMap::new(), // no global vars in lambda context
                    func_signatures,
                    lambda_funcs,
                    captures, // captured variable names
                )?;
            }
            // Recurse into lambda body for nested lambdas
            compile_lambda_bodies_in_node(
                body,
                codegen,
                lambda_funcs,
                lambda_map,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                func_signatures,
            )?;
        }
        AstNode::Block(stmts) => {
            for s in stmts {
                compile_lambda_bodies_in_node(
                    s,
                    codegen,
                    lambda_funcs,
                    lambda_map,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    func_signatures,
                )?;
            }
        }
        AstNode::Let { value, .. } | AstNode::Assign { value, .. } => {
            compile_lambda_bodies_in_node(
                value,
                codegen,
                lambda_funcs,
                lambda_map,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                func_signatures,
            )?;
        }
        AstNode::Return(Some(e)) => {
            compile_lambda_bodies_in_node(
                e,
                codegen,
                lambda_funcs,
                lambda_map,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                func_signatures,
            )?;
        }
        AstNode::Call { args, .. } => {
            for a in args {
                compile_lambda_bodies_in_node(
                    a,
                    codegen,
                    lambda_funcs,
                    lambda_map,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    func_signatures,
                )?;
            }
        }
        AstNode::If {
            cond,
            then_branch,
            else_branch,
        } => {
            compile_lambda_bodies_in_node(
                cond,
                codegen,
                lambda_funcs,
                lambda_map,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                func_signatures,
            )?;
            compile_lambda_bodies_in_node(
                then_branch,
                codegen,
                lambda_funcs,
                lambda_map,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                func_signatures,
            )?;
            if let Some(e) = else_branch {
                compile_lambda_bodies_in_node(
                    e,
                    codegen,
                    lambda_funcs,
                    lambda_map,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    func_signatures,
                )?;
            }
        }
        AstNode::While { cond, body } => {
            compile_lambda_bodies_in_node(
                cond,
                codegen,
                lambda_funcs,
                lambda_map,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                func_signatures,
            )?;
            compile_lambda_bodies_in_node(
                body,
                codegen,
                lambda_funcs,
                lambda_map,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                func_signatures,
            )?;
        }
        AstNode::BinaryOp { left, right, .. } => {
            compile_lambda_bodies_in_node(
                left,
                codegen,
                lambda_funcs,
                lambda_map,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                func_signatures,
            )?;
            compile_lambda_bodies_in_node(
                right,
                codegen,
                lambda_funcs,
                lambda_map,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                func_signatures,
            )?;
        }
        AstNode::For { body, .. } => {
            compile_lambda_bodies_in_node(
                body,
                codegen,
                lambda_funcs,
                lambda_map,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                func_signatures,
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn compile_function_body(
    codegen: &mut CodeGen,
    func_id: FuncId,
    func_name: &str,
    params: &[(String, String)],
    return_type: &str,
    body: &AstNode,
    runtime_funcs: &HashMap<String, FuncId>,
    declared_funcs: &HashMap<String, FuncId>,
    string_funcs: &HashMap<String, FuncId>,
    global_data_ids: &HashMap<String, DataId>,
    func_signatures: &HashMap<String, Vec<Type>>,
    lambda_funcs: &HashMap<usize, FuncId>,
) -> Result<(), CompileError> {
    compile_function_body_with_captures(
        codegen,
        func_id,
        func_name,
        params,
        return_type,
        body,
        runtime_funcs,
        declared_funcs,
        string_funcs,
        global_data_ids,
        func_signatures,
        lambda_funcs,
        &[], // no captures
    )
}

fn compile_function_body_with_captures(
    codegen: &mut CodeGen,
    func_id: FuncId,
    func_name: &str,
    params: &[(String, String)],
    return_type: &str,
    body: &AstNode,
    runtime_funcs: &HashMap<String, FuncId>,
    declared_funcs: &HashMap<String, FuncId>,
    string_funcs: &HashMap<String, FuncId>,
    global_data_ids: &HashMap<String, DataId>,
    func_signatures: &HashMap<String, Vec<Type>>,
    lambda_funcs: &HashMap<usize, FuncId>,
    captured_vars: &[String], // variable names read from CLOSURE_ENV slots 0,1,2,...
) -> Result<(), CompileError> {
    let mut ctx = codegen.module.make_context();

    for (_, ty) in params {
        let cl_ty = forge_type_to_cranelift(ty);
        ctx.func.signature.params.push(AbiParam::new(cl_ty));
    }
    let ret_ty = forge_type_to_cranelift(return_type);
    ctx.func.signature.returns.push(AbiParam::new(ret_ty));

    let mut builder_ctx = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
        let mut variables: HashMap<String, LocalVar> = HashMap::new();

        let entry_block = builder.create_block();
        builder.append_block_params_for_function_params(entry_block);
        builder.switch_to_block(entry_block);

        // Collect block params into a Vec to avoid borrow issues
        let block_params: Vec<Value> = builder.block_params(entry_block).to_vec();
        eprintln!("DEBUG: Lambda {} has {} params", func_name, params.len());
        for (i, (param_name, param_ty)) in params.iter().enumerate() {
            eprintln!(
                "DEBUG: Binding param {}: {} of type {}",
                i, param_name, param_ty
            );
            let param_val = block_params[i];
            let ty = forge_type_to_cranelift(param_ty);

            // Create a variable for this parameter
            let var = next_variable();
            builder.declare_var(var, ty);
            builder.def_var(var, param_val);

            let kind = infer_kind_from_type_name(param_ty);
            variables.insert(param_name.clone(), LocalVar { var, ty, kind });

            // Track struct type for parameters
            if crate::get_struct_layout(param_ty).is_some() {
                crate::set_var_struct_type(param_name, param_ty);
            }
        }

        // Set up captured variables from the closure environment (CLOSURE_ENV global array)
        if !captured_vars.is_empty() {
            if let Some(&get_env_id) = runtime_funcs.get("forge_closure_get_env") {
                let get_env_ref = codegen
                    .module
                    .declare_func_in_func(get_env_id, builder.func);
                for (slot, cap_name) in captured_vars.iter().enumerate() {
                    let slot_val = builder.ins().iconst(types::I64, slot as i64);
                    let call = builder.ins().call(get_env_ref, &[slot_val]);
                    let cap_val = builder.func.dfg.first_result(call);

                    let var = next_variable();
                    builder.declare_var(var, types::I64);
                    builder.def_var(var, cap_val);
                    variables.insert(
                        cap_name.clone(),
                        LocalVar {
                            var,
                            ty: types::I64,
                            kind: ValueKind::Int,
                        },
                    );
                }
            }
        }

        // Call __init_globals at the start of main (if it exists)
        if func_name == "main" {
            if let Some(&init_id) = declared_funcs.get("__init_globals") {
                let init_ref = codegen.module.declare_func_in_func(init_id, builder.func);
                builder.ins().call(init_ref, &[]);
            }
        }

        // Check if this is a lambda (name starts with __lambda_) or a regular function
        let is_lambda = func_name.starts_with("__lambda_");

        if is_lambda {
            // Lambda bodies are expressions (they return a value), not statements
            // So we need to compile them with compile_expr and then return the result
            let body_val = compile_expr(
                &mut builder,
                &mut variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                &mut codegen.module,
                body,
                func_signatures,
                lambda_funcs,
                global_data_ids,
            )?;

            // Return the body value
            builder.ins().return_(&[body_val]);
        } else {
            // Regular function body - compile as statement
            let filled = compile_stmt(
                &mut builder,
                &mut variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                &mut codegen.module,
                ret_ty,
                body,
                None,
                None,
                func_signatures,
                lambda_funcs,
                global_data_ids,
            )?;

            // Try to add return if the body compilation didn't fill a block
            if !filled {
                let current = builder.current_block().unwrap();
                let return_block = builder.create_block();
                builder.ins().jump(return_block, &[]);
                builder.switch_to_block(return_block);
                let zero = builder.ins().iconst(ret_ty, 0);
                builder.ins().return_(&[zero]);
            }
        }

        // Seal all blocks to complete SSA construction
        builder.seal_all_blocks();
    }

    let result = codegen.module.define_function(func_id, &mut ctx);

    if let Err(e) = result {
        eprintln!("DEBUG: Error in '{}': {:?}", func_name, e);
        // Try to get more detailed error info
        let err_str = format!("{:?}", e);
        if err_str.contains("verifier") {
            eprintln!("DEBUG: Verifier error detected in function '{}'", func_name);
            // Print the function IR for debugging
            eprintln!("DEBUG: Function IR:\n{}", ctx.func.display());
        }
        return Err(CompileError::ModuleError(e.to_string()));
    }

    Ok(())
}

/// Compile a test function body
fn compile_test_body(
    codegen: &mut CodeGen,
    func_id: FuncId,
    body: &AstNode,
    runtime_funcs: &HashMap<String, FuncId>,
    declared_funcs: &HashMap<String, FuncId>,
    string_funcs: &HashMap<String, FuncId>,
    func_signatures: &HashMap<String, Vec<Type>>,
    lambda_funcs: &HashMap<usize, FuncId>,
    global_data_ids: &HashMap<String, DataId>,
) -> Result<(), CompileError> {
    let mut ctx = codegen.module.make_context();
    ctx.func.signature.returns.push(AbiParam::new(types::I64));

    let mut builder_ctx = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
        let mut variables: HashMap<String, LocalVar> = HashMap::new();

        let entry_block = builder.create_block();
        builder.switch_to_block(entry_block);

        // Compile the test body
        let filled = compile_stmt(
            &mut builder,
            &mut variables,
            runtime_funcs,
            declared_funcs,
            string_funcs,
            &mut codegen.module,
            types::I64,
            body,
            None,
            None,
            func_signatures,
            lambda_funcs,
            global_data_ids,
        )?;

        // If block not filled (e.g., no explicit return), return 0 (success)
        if !filled {
            let zero = builder.ins().iconst(types::I64, 0);
            builder.ins().return_(&[zero]);
        }

        // Seal all blocks to complete SSA construction
        builder.seal_all_blocks();
    }

    codegen
        .module
        .define_function(func_id, &mut ctx)
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;

    Ok(())
}

/// Generate a test runner main() function
fn generate_test_runner(
    codegen: &mut CodeGen,
    runtime_funcs: &HashMap<String, FuncId>,
    declared_funcs: &HashMap<String, FuncId>,
    test_names: &[String],
) -> Result<(), CompileError> {
    use cranelift_module::{Linkage, Module};

    let mut ctx = codegen.module.make_context();
    ctx.func.signature.returns.push(AbiParam::new(types::I64));

    // Declare the main function
    let func_id = codegen
        .module
        .declare_function("main", Linkage::Export, &ctx.func.signature)
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;

    let mut builder_ctx = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);

        let entry_block = builder.create_block();
        builder.switch_to_block(entry_block);

        // Reset test state
        if let Some(&reset_id) = runtime_funcs.get("forge_test_reset") {
            let reset_ref = codegen.module.declare_func_in_func(reset_id, builder.func);
            builder.ins().call(reset_ref, &[]);
        }

        // Call each test function
        for test_name in test_names {
            if let Some(&test_id) = declared_funcs.get(test_name) {
                let test_ref = codegen.module.declare_func_in_func(test_id, builder.func);
                builder.ins().call(test_ref, &[]);
            }
        }

        // Get test result
        let result_val = if let Some(&result_id) = runtime_funcs.get("forge_test_result") {
            let result_ref = codegen.module.declare_func_in_func(result_id, builder.func);
            let call = builder.ins().call(result_ref, &[]);
            builder.func.dfg.first_result(call)
        } else {
            builder.ins().iconst(types::I64, 0)
        };

        // Return test result (0 = success, 1 = failure)
        builder.ins().return_(&[result_val]);
    }

    codegen
        .module
        .define_function(func_id, &mut ctx)
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;

    Ok(())
}

fn compile_stmt(
    builder: &mut FunctionBuilder,
    variables: &mut HashMap<String, LocalVar>,
    runtime_funcs: &HashMap<String, FuncId>,
    declared_funcs: &HashMap<String, FuncId>,
    string_funcs: &HashMap<String, FuncId>,
    module: &mut dyn Module,
    return_type: Type,
    node: &AstNode,
    loop_header: Option<Block>,
    loop_exit: Option<Block>,
    func_signatures: &HashMap<String, Vec<Type>>,
    lambda_funcs: &HashMap<usize, FuncId>,
    global_data_ids: &HashMap<String, DataId>,
) -> Result<bool, CompileError> {
    match node {
        AstNode::Let {
            name,
            value,
            type_annotation,
        } => {
            let val = compile_expr(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                value,
                func_signatures,
                lambda_funcs,
                global_data_ids,
            )?;
            let ty = builder.func.dfg.value_type(val);

            // Create a new variable and declare it
            let var = next_variable();
            builder.declare_var(var, ty);
            builder.def_var(var, val);

            let mut kind = infer_value_kind(value, variables);

            // Refine kind from type annotation when value inference is ambiguous
            if let Some(ann) = type_annotation {
                let ann_kind = infer_kind_from_type_name(ann);
                // Type annotation is more specific than value inference
                if matches!(
                    kind,
                    ValueKind::Unknown | ValueKind::Map | ValueKind::ListUnknown
                ) {
                    if ann_kind != ValueKind::Unknown {
                        kind = ann_kind;
                    }
                }
                // Track struct type from List[StructName] annotations
                if ann.starts_with("List[") && ann.ends_with(']') {
                    let elem_type = &ann[5..ann.len() - 1];
                    if crate::get_struct_layout(elem_type).is_some() {
                        // Store the list-element struct type for for-loop inference
                        crate::set_var_struct_type(&format!("__list_elem_{}", name), elem_type);
                    }
                }
                // Track map value type from Map[K, V] annotations
                if ann.starts_with("Map[") {
                    crate::set_var_struct_type(&format!("__map_val_{}", name), ann);
                }
            }

            // Track struct type for variables assigned from StructInit or struct constructors
            if let AstNode::StructInit { name: sname, .. } = value.as_ref() {
                crate::set_var_struct_type(name, sname);
            } else if let AstNode::Call { func, .. } = value.as_ref() {
                if crate::get_struct_layout(func).is_some() {
                    // Direct struct constructor call: Point(10, 20)
                    crate::set_var_struct_type(name, func);
                } else if let Some(ret_type) = crate::get_func_return_type(func) {
                    // Function returning a struct type: add_points(...) -> Point
                    if crate::get_struct_layout(&ret_type).is_some() {
                        crate::set_var_struct_type(name, &ret_type);
                    }
                }
            } else if let AstNode::Index { expr, .. } = value.as_ref() {
                // Track struct type for variables assigned from list indexing: entry := items[i]
                // where items is List[SomeStruct]
                if let AstNode::Identifier(list_name) = expr.as_ref() {
                    // Check __list_elem_ tracking first (from type annotations)
                    let elem_type =
                        crate::get_var_struct_type(&format!("__list_elem_{}", list_name))
                            .map(|s| s.to_string())
                            .or_else(|| {
                                // Check global var type for List[StructName]
                                crate::get_global_var_type(list_name).and_then(|gtype| {
                                    if gtype.starts_with("List[") && gtype.ends_with(']') {
                                        Some(gtype[5..gtype.len() - 1].to_string())
                                    } else {
                                        None
                                    }
                                })
                            });
                    if let Some(ref elem) = elem_type {
                        if crate::get_struct_layout(elem).is_some() {
                            crate::set_var_struct_type(name, elem);
                            kind = ValueKind::Struct;
                        }
                    }
                }
            }

            variables.insert(name.clone(), LocalVar { var, ty, kind });
            Ok(false)
        }

        AstNode::Block(stmts) => {
            for stmt in stmts {
                let filled = compile_stmt(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    return_type,
                    stmt,
                    loop_header,
                    loop_exit,
                    func_signatures,
                    lambda_funcs,
                    global_data_ids,
                )?;
                if filled {
                    return Ok(true);
                }
            }
            Ok(false)
        }

        AstNode::Return(expr) => {
            if let Some(e) = expr {
                let val = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    e,
                    func_signatures,
                    lambda_funcs,
                    global_data_ids,
                )?;

                // Convert to return type if necessary
                let val_ty = builder.func.dfg.value_type(val);
                let final_val = if val_ty != return_type {
                    if val_ty.bits() > return_type.bits() {
                        // Truncate (e.g., i64 -> i8)
                        builder.ins().ireduce(return_type, val)
                    } else {
                        // Extend (e.g., i8 -> i64)
                        builder.ins().uextend(return_type, val)
                    }
                } else {
                    val
                };

                builder.ins().return_(&[final_val]);
            } else {
                let zero = builder.ins().iconst(return_type, 0);
                builder.ins().return_(&[zero]);
            }
            Ok(true)
        }

        AstNode::Break => {
            if let Some(exit) = loop_exit {
                builder.ins().jump(exit, &[]);
                Ok(true)
            } else {
                Err(CompileError::UnsupportedFeature(
                    "break outside of loop".to_string(),
                ))
            }
        }

        AstNode::Continue => {
            if let Some(header) = loop_header {
                builder.ins().jump(header, &[]);
                Ok(true)
            } else {
                Err(CompileError::UnsupportedFeature(
                    "continue outside of loop".to_string(),
                ))
            }
        }

        AstNode::If {
            cond,
            then_branch,
            else_branch,
        } => {
            let cond_val = compile_expr(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                cond,
                func_signatures,
                lambda_funcs,
                global_data_ids,
            )?;

            let then_block = builder.create_block();
            let else_block = builder.create_block();
            let merge_block = builder.create_block();

            builder
                .ins()
                .brif(cond_val, then_block, &[], else_block, &[]);

            // Then branch
            builder.switch_to_block(then_block);
            let then_filled = compile_stmt(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                return_type,
                then_branch,
                loop_header,
                loop_exit,
                func_signatures,
                lambda_funcs,
                global_data_ids,
            )?;
            if !then_filled {
                builder.ins().jump(merge_block, &[]);
            }
            builder.seal_block(then_block);

            // Else branch
            builder.switch_to_block(else_block);
            let else_filled = if let Some(else_stmt) = else_branch {
                compile_stmt(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    return_type,
                    else_stmt,
                    loop_header,
                    loop_exit,
                    func_signatures,
                    lambda_funcs,
                    global_data_ids,
                )?
            } else {
                false
            };
            if !else_filled {
                builder.ins().jump(merge_block, &[]);
            }
            builder.seal_block(else_block);

            // Continue after if - only if at least one branch falls through
            // If both branches are filled (return/break), the code after if is unreachable
            if then_filled && else_filled {
                // Both branches filled - merge block is unreachable, create new block
                let unreachable_block = builder.create_block();
                builder.switch_to_block(unreachable_block);
                Ok(true) // Current block is filled (unreachable)
            } else {
                // At least one branch falls through - continue at merge
                builder.switch_to_block(merge_block);
                builder.seal_block(merge_block);
                Ok(false)
            }
        }

        AstNode::While { cond, body } => {
            // Create blocks for while loop
            let header_block = builder.create_block();
            let body_block = builder.create_block();
            let exit_block = builder.create_block();

            // Jump to header from current block
            builder.ins().jump(header_block, &[]);

            // Header: check condition
            builder.switch_to_block(header_block);
            let cond_val = compile_expr(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                cond,
                func_signatures,
                lambda_funcs,
                global_data_ids,
            )?;
            builder
                .ins()
                .brif(cond_val, body_block, &[], exit_block, &[]);
            // Don't seal header yet - body will jump back to it

            // Body: compile loop body
            builder.switch_to_block(body_block);
            let body_filled = compile_stmt(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                return_type,
                body,
                Some(header_block),
                Some(exit_block),
                func_signatures,
                lambda_funcs,
                global_data_ids,
            )?;
            if !body_filled {
                builder.ins().jump(header_block, &[]);
            }
            // Seal body block - all its predecessors are known
            builder.seal_block(body_block);

            // Now seal header block - all its predecessors (entry and body) are known
            builder.seal_block(header_block);

            // Continue at exit block
            builder.switch_to_block(exit_block);

            Ok(false)
        }

        AstNode::For {
            var,
            index_var,
            iterable,
            body,
        } => {
            // Simplified for loop using index-based iteration

            // Determine iterable kind before compiling
            let iter_kind_pre = infer_value_kind(iterable, variables);
            let is_string_iter = matches!(iter_kind_pre, ValueKind::String);

            // Compile the iterable and get length
            let list_val = compile_expr(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                iterable,
                func_signatures,
                lambda_funcs,
                global_data_ids,
            )?;

            // Get length — dispatch based on iterable type
            let len_func_name = if is_string_iter { "forge_cstring_len" } else { "forge_list_len" };
            let len_func_id = runtime_funcs
                .get(len_func_name)
                .ok_or_else(|| CompileError::UnknownFunction(len_func_name.to_string()))?;
            let len_func_ref = module.declare_func_in_func(*len_func_id, builder.func);
            let len_call = builder.ins().call(len_func_ref, &[list_val]);
            let len_val = builder.func.dfg.first_result(len_call);

            // Create variables before any branching
            let idx_var = next_variable();
            let count_var = next_variable();
            builder.declare_var(idx_var, types::I64);
            builder.declare_var(count_var, types::I64);

            // Set initial values
            let zero = builder.ins().iconst(types::I64, 0);
            builder.def_var(idx_var, zero);
            builder.def_var(count_var, len_val);

            // Create blocks
            let header = builder.create_block();
            let body_block = builder.create_block();
            let increment_block = builder.create_block();
            let exit = builder.create_block();

            // Jump to header
            builder.ins().jump(header, &[]);

            // Header block
            builder.switch_to_block(header);
            let idx_val = builder.use_var(idx_var);
            let count_val = builder.use_var(count_var);
            let cmp = builder
                .ins()
                .icmp(IntCC::UnsignedLessThan, idx_val, count_val);
            builder.ins().brif(cmp, body_block, &[], exit, &[]);

            // Body block
            builder.switch_to_block(body_block);

            // Get the current index
            let cur_idx = builder.use_var(idx_var);

            // Get the element at current index
            let element_val = if is_string_iter {
                // String iteration: get char at index
                let char_at_id = runtime_funcs
                    .get("forge_cstring_char_at")
                    .ok_or_else(|| CompileError::UnknownFunction("forge_cstring_char_at".to_string()))?;
                let char_at_ref = module.declare_func_in_func(*char_at_id, builder.func);
                let get_call = builder.ins().call(char_at_ref, &[list_val, cur_idx]);
                builder.func.dfg.first_result(get_call)
            } else {
                // List iteration: get element by index
                let get_func_id = runtime_funcs
                    .get("forge_list_get_value")
                    .ok_or_else(|| CompileError::UnknownFunction("forge_list_get_value".to_string()))?;
                let get_func_ref = module.declare_func_in_func(*get_func_id, builder.func);
                let get_call = builder.ins().call(get_func_ref, &[list_val, cur_idx]);
                builder.func.dfg.first_result(get_call)
            };

            // Create loop variable with the element value
            let loop_var = next_variable();
            builder.declare_var(loop_var, types::I64);
            builder.def_var(loop_var, element_val);

            // Infer loop variable kind from iterable type
            let iter_kind = infer_value_kind(iterable, variables);
            let elem_kind = match iter_kind {
                ValueKind::String => ValueKind::String,
                ValueKind::ListString => ValueKind::String,
                ValueKind::ListUnknown => {
                    // Check if the iterable variable has a List[StructName] annotation
                    if let AstNode::Identifier(iter_name) = iterable.as_ref() {
                        // Check for struct element type stored during Let
                        if let Some(elem_struct) =
                            crate::get_var_struct_type(&format!("__list_elem_{}", iter_name))
                        {
                            crate::set_var_struct_type(var, &elem_struct);
                            ValueKind::Struct
                        } else if let Some(gtype) = crate::get_global_var_type(iter_name) {
                            if gtype.starts_with("List[") && gtype.ends_with(']') {
                                let elem_type = &gtype[5..gtype.len() - 1];
                                if crate::get_struct_layout(elem_type).is_some() {
                                    crate::set_var_struct_type(var, elem_type);
                                    ValueKind::Struct
                                } else {
                                    infer_kind_from_type_name(elem_type)
                                }
                            } else {
                                ValueKind::Unknown
                            }
                        } else {
                            ValueKind::Unknown
                        }
                    } else {
                        ValueKind::Unknown
                    }
                }
                _ => ValueKind::Unknown,
            };
            let var_info = LocalVar {
                var: loop_var,
                ty: types::I64,
                kind: elem_kind,
            };
            variables.insert(var.clone(), var_info);

            // If there's an index variable, add it too (with the index value)
            if let Some(idx_var_name) = index_var {
                let idx_loop_var = next_variable();
                builder.declare_var(idx_loop_var, types::I64);
                builder.def_var(idx_loop_var, cur_idx);

                let idx_var_info = LocalVar {
                    var: idx_loop_var,
                    ty: types::I64,
                    kind: ValueKind::Unknown,
                };
                variables.insert(idx_var_name.clone(), idx_var_info);
            }

            // Compile body — continue jumps to increment_block (not header)
            let body_ref: &AstNode = &**body;
            let filled = compile_stmt(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                return_type,
                body_ref,
                Some(increment_block),
                Some(exit),
                func_signatures,
                lambda_funcs,
                global_data_ids,
            )?;

            variables.remove(var);
            if let Some(idx_var_name) = index_var {
                variables.remove(idx_var_name);
            }

            // Jump to increment block at end of body
            if !filled {
                builder.ins().jump(increment_block, &[]);
            }

            builder.seal_block(body_block);

            // Increment block: idx++ then jump to header
            builder.switch_to_block(increment_block);
            let cur_idx_2 = builder.use_var(idx_var);
            let next_idx = builder.ins().iadd_imm(cur_idx_2, 1);
            builder.def_var(idx_var, next_idx);
            builder.ins().jump(header, &[]);

            builder.seal_block(increment_block);
            builder.seal_block(header);

            // Exit
            builder.switch_to_block(exit);

            Ok(false)
        }

        AstNode::Assign { name, value } => {
            let val = compile_expr(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                value,
                func_signatures,
                lambda_funcs,
                global_data_ids,
            )?;

            // Check for field assignment (e.g., "obj.field")
            if let Some(dot_pos) = name.find('.') {
                let obj_name = &name[..dot_pos];
                let field_name = &name[dot_pos + 1..];

                if let Some(var_info) = variables.get(obj_name) {
                    let obj_ptr = builder.use_var(var_info.var);

                    // Look up field offset
                    let offset = if let Some(stype) = crate::get_var_struct_type(obj_name) {
                        crate::get_struct_field_offset(&stype, field_name)
                    } else {
                        find_field_in_any_struct(field_name)
                    };

                    if let Some(offset) = offset {
                        // Ensure value is i64
                        let val_ty = builder.func.dfg.value_type(val);
                        let val_i64 = if val_ty != types::I64 {
                            if val_ty.is_float() {
                                builder.ins().bitcast(types::I64, MemFlags::new(), val)
                            } else {
                                builder.ins().uextend(types::I64, val)
                            }
                        } else {
                            val
                        };
                        builder
                            .ins()
                            .store(MemFlags::new(), val_i64, obj_ptr, offset as i32);
                        return Ok(false);
                    }
                }
                // Fall through to error if field not found
                eprintln!("WARN: Unknown field assignment '{}'", name);
                return Ok(false);
            }

            // Regular variable assignment
            if let Some(var_info) = variables.get(name) {
                let val_ty = builder.func.dfg.value_type(val);
                let final_val = if val_ty != var_info.ty {
                    if val_ty.bits() > var_info.ty.bits() {
                        builder.ins().ireduce(var_info.ty, val)
                    } else {
                        builder.ins().uextend(var_info.ty, val)
                    }
                } else {
                    val
                };
                builder.def_var(var_info.var, final_val);
                Ok(false)
            } else if let Some(data_id) = global_data_ids.get(name) {
                // Global variable store
                let gv = module.declare_data_in_func(*data_id, builder.func);
                let addr = builder
                    .ins()
                    .global_value(module.target_config().pointer_type(), gv);
                let val_ty = builder.func.dfg.value_type(val);
                let val_i64 = if val_ty != types::I64 {
                    if val_ty.is_float() {
                        builder.ins().bitcast(types::I64, MemFlags::new(), val)
                    } else {
                        builder.ins().uextend(types::I64, val)
                    }
                } else {
                    val
                };
                builder.ins().store(MemFlags::new(), val_i64, addr, 0);
                Ok(false)
            } else {
                Err(CompileError::UnknownVariable(name.clone()))
            }
        }

        AstNode::Import { .. } => {
            // Import statements are handled at module level, not in function body
            // For now, just skip them
            Ok(false)
        }

        AstNode::EnumDecl { .. } => {
            // Enum declarations are handled at module level
            // Skip in function body context
            Ok(false)
        }

        AstNode::StructDecl { .. } => {
            // Struct declarations are handled at module level (Pass 0)
            Ok(false)
        }

        AstNode::InterfaceDecl { .. } => {
            // Interface declarations are handled at module level
            // They define method signatures for type checking
            Ok(false)
        }

        AstNode::ImplBlock { methods, .. } => {
            // Impl blocks are handled at module level
            // They provide method implementations for types
            // For now, compile each method as a regular function
            for method in methods {
                compile_stmt(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    return_type,
                    method,
                    loop_header,
                    loop_exit,
                    func_signatures,
                    lambda_funcs,
                    global_data_ids,
                )?;
            }
            Ok(false)
        }

        _ => {
            compile_expr(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                node,
                func_signatures,
                lambda_funcs,
                global_data_ids,
            )?;
            Ok(false)
        }
    }
}

/// Compile pattern matching check for match expressions
/// Returns a boolean value indicating whether the pattern matches the subject
fn compile_pattern_check(
    builder: &mut FunctionBuilder,
    variables: &mut HashMap<String, LocalVar>,
    runtime_funcs: &HashMap<String, FuncId>,
    declared_funcs: &HashMap<String, FuncId>,
    string_funcs: &HashMap<String, FuncId>,
    module: &mut dyn Module,
    pattern: &crate::ast::MatchPattern,
    subject_val: Value,
    func_signatures: &HashMap<String, Vec<Type>>,
    lambda_funcs: &HashMap<usize, FuncId>,
    global_data_ids: &HashMap<String, DataId>,
) -> Result<Value, CompileError> {
    use crate::ast::MatchPattern;

    match pattern {
        MatchPattern::Wildcard => {
            // Wildcard always matches
            Ok(builder.ins().iconst(types::I8, 1))
        }
        MatchPattern::Variable(var_name) => {
            // Variable binding always matches
            // Bind the variable to the subject value
            let var = next_variable();
            builder.declare_var(var, types::I64);
            builder.def_var(var, subject_val);
            variables.insert(
                var_name.clone(),
                LocalVar {
                    var,
                    ty: types::I64,
                    kind: ValueKind::Unknown,
                },
            );
            Ok(builder.ins().iconst(types::I8, 1))
        }
        MatchPattern::Literal(lit) => {
            // Compare subject with literal value
            let lit_val = compile_expr(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                lit,
                func_signatures,
                lambda_funcs,
                global_data_ids,
            )?;

            // Compare values for equality (icmp returns i8 which is what brif expects)
            Ok(builder.ins().icmp(IntCC::Equal, subject_val, lit_val))
        }
        MatchPattern::EnumVariant {
            enum_name,
            variant_name,
            bind_vars,
        } => {
            // For enum variants, we need to check the tag field
            // Layout: { tag: i64, data: ... }
            // For now, assume enums are represented as i64 with the tag value

            // This is simplified - proper enum matching would:
            // 1. Load the tag field from the subject
            // 2. Compare with the expected variant's tag index
            // 3. Bind any associated data variables

            // For now, just return true (always matches)
            // TODO: Implement proper enum variant matching
            let _ = (enum_name, variant_name, bind_vars); // silence unused warnings
            Ok(builder.ins().iconst(types::I8, 1))
        }
    }
}

fn compile_expr(
    builder: &mut FunctionBuilder,
    variables: &mut HashMap<String, LocalVar>,
    runtime_funcs: &HashMap<String, FuncId>,
    declared_funcs: &HashMap<String, FuncId>,
    string_funcs: &HashMap<String, FuncId>,
    module: &mut dyn Module,
    node: &AstNode,
    func_signatures: &HashMap<String, Vec<Type>>,
    lambda_funcs: &HashMap<usize, FuncId>,
    global_data_ids: &HashMap<String, DataId>,
) -> Result<Value, CompileError> {
    match node {
        AstNode::IntLiteral(n) => Ok(builder.ins().iconst(types::I64, *n)),

        AstNode::FloatLiteral(f) => Ok(builder.ins().f64const(*f)),

        AstNode::BoolLiteral(b) => Ok(builder.ins().iconst(types::I64, if *b { 1 } else { 0 })),

        AstNode::StringLiteral(s) => {
            // Call the string data function to get the address
            // For now, just return the pointer directly (using simple strlen for .len())
            let stripped_s = strip_string_quotes(s);
            let ptr_val = if let Some(&str_func_id) = string_funcs.get(stripped_s.as_str()) {
                let str_func_ref = module.declare_func_in_func(str_func_id, builder.func);
                let call = builder.ins().call(str_func_ref, &[]);
                builder.func.dfg.first_result(call)
            } else {
                // Fallback: return null pointer
                // This should not happen if all strings are collected properly
                builder.ins().iconst(types::I64, 0)
            };

            // Note: For full ForgeString struct support, we would:
            // 1. Create a stack slot for the 24-byte struct
            // 2. Call forge_string_from_cstr_ptr to initialize it
            // 3. Return the address of the stack slot
            // For now, we just return the raw pointer and use strlen-based len()

            Ok(ptr_val)
        }

        AstNode::StringInterp { parts } => {
            // String interpolation: concatenate all parts
            // Start with an empty string
            let mut result: Option<Value> = None;

            for part in parts {
                let part_val = match part {
                    crate::ast::StringInterpPart::Literal(s) => {
                        // Get string pointer from string_funcs (keys are quote-stripped)
                        let stripped_s = strip_string_quotes(s);
                        if let Some(&str_func_id) = string_funcs.get(stripped_s.as_str()) {
                            let str_func_ref =
                                module.declare_func_in_func(str_func_id, builder.func);
                            let call = builder.ins().call(str_func_ref, &[]);
                            builder.func.dfg.first_result(call)
                        } else {
                            builder.ins().iconst(types::I64, 0)
                        }
                    }
                    crate::ast::StringInterpPart::Expr(expr) => {
                        // Compile the expression and convert to string
                        let expr_val = compile_expr(
                            builder,
                            variables,
                            runtime_funcs,
                            declared_funcs,
                            string_funcs,
                            module,
                            expr,
                            func_signatures,
                            lambda_funcs,
                            global_data_ids,
                        )?;

                        // Convert to string based on actual type
                        let expr_ty = builder.func.dfg.value_type(expr_val);
                        let expr_kind = infer_value_kind(expr, variables);
                        if matches!(expr_kind, ValueKind::String) {
                            // Already a cstring pointer — pass through directly
                            expr_val
                        } else {
                            let converter_name = if expr_ty == types::F64 {
                                "forge_float_to_cstr"
                            } else if matches!(expr_kind, ValueKind::Bool) {
                                "forge_bool_to_cstr"
                            } else {
                                "forge_int_to_cstr"
                            };
                            if let Some(&to_str_id) = runtime_funcs.get(converter_name) {
                                let to_str_ref =
                                    module.declare_func_in_func(to_str_id, builder.func);
                                // Coerce sub-i64 integer types (e.g. i8 from bool) to i64
                                let coerced = if expr_ty != types::I64 && expr_ty != types::F64 {
                                    builder.ins().uextend(types::I64, expr_val)
                                } else {
                                    expr_val
                                };
                                let call = builder.ins().call(to_str_ref, &[coerced]);
                                builder.func.dfg.first_result(call)
                            } else {
                                builder.ins().iconst(types::I64, 0)
                            }
                        }
                    }
                };

                // Concatenate with result
                if let Some(curr_result) = result {
                    if let Some(&concat_id) = runtime_funcs.get("forge_concat_cstr") {
                        let concat_ref = module.declare_func_in_func(concat_id, builder.func);
                        let call = builder.ins().call(concat_ref, &[curr_result, part_val]);
                        result = Some(builder.func.dfg.first_result(call));
                    }
                } else {
                    result = Some(part_val);
                }
            }

            Ok(result.unwrap_or_else(|| builder.ins().iconst(types::I64, 0)))
        }

        AstNode::StructInit { name, fields } => {
            // Get struct layout
            let layout = crate::get_struct_layout(name);
            let num_fields = layout.as_ref().map(|l| l.len()).unwrap_or(fields.len());

            // Allocate struct: forge_struct_alloc(num_fields) -> ptr
            let alloc_func = runtime_funcs
                .get("forge_struct_alloc")
                .ok_or_else(|| CompileError::UnknownFunction("forge_struct_alloc".to_string()))?;
            let alloc_ref = module.declare_func_in_func(*alloc_func, builder.func);
            let num_fields_val = builder.ins().iconst(types::I64, num_fields as i64);
            let alloc_call = builder.ins().call(alloc_ref, &[num_fields_val]);
            let struct_ptr = builder.func.dfg.first_result(alloc_call);

            // Store each field value at its offset
            for (field_name, field_value) in fields {
                let field_val = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    field_value,
                    func_signatures,
                    lambda_funcs,
                    global_data_ids,
                )?;

                // Determine field offset
                let offset = if let Some(ref layout_fields) = layout {
                    layout_fields
                        .iter()
                        .find(|(n, _)| n == field_name)
                        .map(|(_, off)| *off as i32)
                        .unwrap_or(0)
                } else {
                    // Fallback: use field order in init expression
                    let idx = fields
                        .iter()
                        .position(|(n, _)| n == field_name)
                        .unwrap_or(0);
                    (idx * 8) as i32
                };

                // Ensure value is i64
                let val_ty = builder.func.dfg.value_type(field_val);
                let val_i64 = if val_ty != types::I64 {
                    if val_ty.is_float() {
                        builder
                            .ins()
                            .bitcast(types::I64, MemFlags::new(), field_val)
                    } else {
                        builder.ins().uextend(types::I64, field_val)
                    }
                } else {
                    field_val
                };

                // Store at struct_ptr + offset
                builder
                    .ins()
                    .store(MemFlags::new(), val_i64, struct_ptr, offset);
            }

            Ok(struct_ptr)
        }

        AstNode::ListLiteral {
            elements,
            elem_type: _,
        } => {
            // Create a new list and populate it with elements
            // For simplicity, assume list of Ints (I64) for now
            let elem_size = 8i64; // sizeof(i64)
            let type_tag = 0i32; // Primitive type

            // Call forge_list_new(elem_size, type_tag)
            let list_new_func = runtime_funcs
                .get("forge_list_new")
                .ok_or_else(|| CompileError::UnknownFunction("forge_list_new".to_string()))?;
            let list_new_ref = module.declare_func_in_func(*list_new_func, builder.func);
            let elem_size_val = builder.ins().iconst(types::I64, elem_size);
            let type_tag_val = builder.ins().iconst(types::I32, type_tag as i64);
            let new_call = builder
                .ins()
                .call(list_new_ref, &[elem_size_val, type_tag_val]);
            let list_val = builder.func.dfg.first_result(new_call);

            // Push each element to the list using push_value (simpler ABI)
            let push_func = runtime_funcs.get("forge_list_push_value").ok_or_else(|| {
                CompileError::UnknownFunction("forge_list_push_value".to_string())
            })?;
            let push_ref = module.declare_func_in_func(*push_func, builder.func);

            for elem in elements {
                let elem_val = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    elem,
                    func_signatures,
                    lambda_funcs,
                    global_data_ids,
                )?;
                builder.ins().call(push_ref, &[list_val, elem_val]);
            }

            // Return the list VALUE
            Ok(list_val)
        }

        AstNode::MapLiteral {
            entries,
            key_type: kt,
            val_type: _,
        } => {
            // Determine key type from annotation or first entry
            let is_int_key = kt.as_ref().map(|k| k == "Int").unwrap_or_else(|| {
                entries
                    .first()
                    .map(|(k, _)| matches!(k, AstNode::IntLiteral(_)))
                    .unwrap_or(false)
            });
            let key_type = if is_int_key { 0i32 } else { 1i32 };
            let val_size = 8i64; // sizeof(i64) for all values
            let val_is_heap = 0i8; // false

            // Call forge_map_new(key_type, val_size, val_is_heap)
            let map_new_func = runtime_funcs
                .get("forge_map_new")
                .ok_or_else(|| CompileError::UnknownFunction("forge_map_new".to_string()))?;
            let map_new_ref = module.declare_func_in_func(*map_new_func, builder.func);
            let key_type_val = builder.ins().iconst(types::I32, key_type as i64);
            let val_size_val = builder.ins().iconst(types::I64, val_size);
            let val_is_heap_val = builder.ins().iconst(types::I64, val_is_heap as i64);
            let new_call = builder
                .ins()
                .call(map_new_ref, &[key_type_val, val_size_val, val_is_heap_val]);
            let map_val = builder.func.dfg.first_result(new_call);

            // Insert each entry using handle-based API
            let insert_func_name = if is_int_key {
                "forge_map_insert_ikey"
            } else {
                "forge_map_insert_cstr"
            };
            let insert_func = runtime_funcs
                .get(insert_func_name)
                .ok_or_else(|| CompileError::UnknownFunction(insert_func_name.to_string()))?;
            let insert_ref = module.declare_func_in_func(*insert_func, builder.func);

            for (key, value) in entries {
                let key_val = match key {
                    AstNode::StringLiteral(s) => {
                        let stripped = strip_string_quotes(s);
                        if let Some(&str_func_id) = string_funcs.get(stripped.as_str()) {
                            let str_func_ref =
                                module.declare_func_in_func(str_func_id, builder.func);
                            let call = builder.ins().call(str_func_ref, &[]);
                            builder.func.dfg.first_result(call)
                        } else {
                            builder.ins().iconst(types::I64, 0)
                        }
                    }
                    AstNode::IntLiteral(n) => builder.ins().iconst(types::I64, *n),
                    _ => builder.ins().iconst(types::I64, 0),
                };

                let val_val = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    value,
                    func_signatures,
                    lambda_funcs,
                    global_data_ids,
                )?;

                let val_i64 = if builder.func.dfg.value_type(val_val) != types::I64 {
                    builder.ins().uextend(types::I64, val_val)
                } else {
                    val_val
                };

                // Call insert: forge_map_insert_{cstr|ikey}(map_handle, key, value)
                builder.ins().call(insert_ref, &[map_val, key_val, val_i64]);
            }

            Ok(map_val)
        }

        AstNode::SetLiteral { elements, .. } => {
            // Determine element type from first element
            let is_int_elem = elements
                .first()
                .map(|e| matches!(e, AstNode::IntLiteral(_)))
                .unwrap_or(false);
            let elem_type = if is_int_elem { 0i32 } else { 1i32 };

            // Call forge_set_new_handle(elem_type) -> i64
            let set_new_func = runtime_funcs
                .get("forge_set_new_handle")
                .ok_or_else(|| CompileError::UnknownFunction("forge_set_new_handle".to_string()))?;
            let set_new_ref = module.declare_func_in_func(*set_new_func, builder.func);
            let elem_type_val = builder.ins().iconst(types::I32, elem_type as i64);
            let new_call = builder.ins().call(set_new_ref, &[elem_type_val]);
            let set_val = builder.func.dfg.first_result(new_call);

            // Insert each element
            let add_func_name = if is_int_elem {
                "forge_set_add_ikey"
            } else {
                "forge_set_add_cstr"
            };
            // For now we only have cstr variant; int sets would need forge_set_add_ikey
            if let Some(&add_id) = runtime_funcs.get(add_func_name) {
                let add_ref = module.declare_func_in_func(add_id, builder.func);
                for elem in elements {
                    let elem_val = compile_expr(
                        builder, variables, runtime_funcs, declared_funcs, string_funcs,
                        module, elem, func_signatures, lambda_funcs, global_data_ids,
                    )?;
                    builder.ins().call(add_ref, &[set_val, elem_val]);
                }
            }

            Ok(set_val)
        }

        AstNode::Try { expr } => {
            // Try operator (!): For now, just pass through the value
            // TODO: Implement proper Result type checking and error propagation
            // The function returns the value directly (not wrapped in Result struct yet)
            compile_expr(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                expr,
                func_signatures,
                lambda_funcs,
                global_data_ids,
            )
        }

        AstNode::Fail { error } => {
            // Fail statement: construct error result and return
            // Compile the error expression
            let err_val = compile_expr(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                error,
                func_signatures,
                lambda_funcs,
                global_data_ids,
            )?;

            // Get the enclosing function's return type
            let current_func = builder.func.name.to_string();
            let enclosing_ret_type = crate::get_func_return_type(&current_func);

            if let Some(ref ret_ty) = enclosing_ret_type {
                if crate::is_result_type(ret_ty) {
                    // Allocate result struct on stack
                    let result_slot = builder.create_sized_stack_slot(StackSlotData::new(
                        StackSlotKind::ExplicitSlot,
                        24,
                        8,
                    ));
                    let result_ptr = builder.ins().stack_addr(types::I64, result_slot, 0);

                    // Store is_ok = false
                    let false_val = builder.ins().iconst(types::I8, 0);
                    builder
                        .ins()
                        .store(MemFlags::new(), false_val, result_ptr, 0);

                    // Store error message
                    let err_field_ptr = builder.ins().iadd_imm(result_ptr, 16);
                    builder
                        .ins()
                        .store(MemFlags::new(), err_val, err_field_ptr, 0);

                    // Return the error result
                    builder.ins().return_(&[result_ptr]);

                    // Return dummy value (unreachable)
                    Ok(builder.ins().iconst(types::I64, 0))
                } else {
                    // Enclosing function doesn't return Result - print and exit
                    if let Some(&print_id) = runtime_funcs.get("print_err") {
                        let print_ref = module.declare_func_in_func(print_id, builder.func);
                        builder.ins().call(print_ref, &[err_val]);
                    }
                    if let Some(&exit_id) = runtime_funcs.get("exit") {
                        let exit_ref = module.declare_func_in_func(exit_id, builder.func);
                        let exit_code = builder.ins().iconst(types::I64, 1);
                        builder.ins().call(exit_ref, &[exit_code]);
                    }
                    Ok(builder.ins().iconst(types::I64, 0))
                }
            } else {
                // No return type info - print and exit
                if let Some(&print_id) = runtime_funcs.get("print_err") {
                    let print_ref = module.declare_func_in_func(print_id, builder.func);
                    builder.ins().call(print_ref, &[err_val]);
                }
                if let Some(&exit_id) = runtime_funcs.get("exit") {
                    let exit_ref = module.declare_func_in_func(exit_id, builder.func);
                    let exit_code = builder.ins().iconst(types::I64, 1);
                    builder.ins().call(exit_ref, &[exit_code]);
                }
                Ok(builder.ins().iconst(types::I64, 0))
            }
        }

        AstNode::Index { expr, index } => {
            // Index access: expr[index]
            // Determine the type of expression to dispatch correctly
            let expr_kind = infer_value_kind(expr, variables);

            let expr_val = compile_expr(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                expr,
                func_signatures,
                lambda_funcs,
                global_data_ids,
            )?;
            let index_val = compile_expr(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                index,
                func_signatures,
                lambda_funcs,
                global_data_ids,
            )?;

            match expr_kind {
                ValueKind::String => {
                    // String indexing: forge_cstring_char_at(str, index)
                    if let Some(&func_id) = runtime_funcs.get("forge_cstring_char_at") {
                        let func_ref = module.declare_func_in_func(func_id, builder.func);
                        let call = builder.ins().call(func_ref, &[expr_val, index_val]);
                        Ok(builder.func.dfg.first_result(call))
                    } else {
                        Ok(builder.ins().iconst(types::I64, 0))
                    }
                }
                ValueKind::ListString | ValueKind::ListUnknown => {
                    // List indexing: forge_list_get_value(list, index)
                    if let Some(&func_id) = runtime_funcs.get("forge_list_get_value") {
                        let func_ref = module.declare_func_in_func(func_id, builder.func);
                        let call = builder.ins().call(func_ref, &[expr_val, index_val]);
                        Ok(builder.func.dfg.first_result(call))
                    } else {
                        Ok(builder.ins().iconst(types::I64, 0))
                    }
                }
                ValueKind::Map => {
                    // String-key map indexing: forge_map_get_cstr(map, key)
                    if let Some(&func_id) = runtime_funcs.get("forge_map_get_cstr") {
                        let func_ref = module.declare_func_in_func(func_id, builder.func);
                        let call = builder.ins().call(func_ref, &[expr_val, index_val]);
                        Ok(builder.func.dfg.first_result(call))
                    } else {
                        Ok(builder.ins().iconst(types::I64, 0))
                    }
                }
                ValueKind::MapIntKey => {
                    // Int-key map indexing: forge_map_get_ikey(map, key)
                    if let Some(&func_id) = runtime_funcs.get("forge_map_get_ikey") {
                        let func_ref = module.declare_func_in_func(func_id, builder.func);
                        let call = builder.ins().call(func_ref, &[expr_val, index_val]);
                        Ok(builder.func.dfg.first_result(call))
                    } else {
                        Ok(builder.ins().iconst(types::I64, 0))
                    }
                }
                _ => {
                    // Unknown type, try list get as fallback
                    if let Some(&func_id) = runtime_funcs.get("forge_list_get_value") {
                        let func_ref = module.declare_func_in_func(func_id, builder.func);
                        let call = builder.ins().call(func_ref, &[expr_val, index_val]);
                        Ok(builder.func.dfg.first_result(call))
                    } else {
                        Ok(builder.ins().iconst(types::I64, 0))
                    }
                }
            }
        }

        AstNode::FieldAccess { obj, field } => {
            // Remove leading dot from field name if present
            let field_name = if field.starts_with('.') {
                &field[1..]
            } else {
                field.as_str()
            };

            // Check if this is tuple field access (.0, .1, etc.)
            if let Ok(tuple_idx) = field_name.parse::<usize>() {
                // Tuple field access - compile the object and load from offset
                let obj_val = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    obj,
                    func_signatures,
                    lambda_funcs,
                    global_data_ids,
                )?;

                // Calculate offset: tuple fields are 8 bytes each
                let offset = (tuple_idx * 8) as i32;
                let loaded = builder
                    .ins()
                    .load(types::I64, MemFlags::new(), obj_val, offset);
                return Ok(loaded);
            }

            // Check if obj is IntLiteral(0) — the parser's sentinel for `self`
            let is_self_access = matches!(obj.as_ref(), AstNode::IntLiteral(0));

            // Determine the object's struct type (if any)
            let obj_struct_type = if is_self_access {
                crate::get_var_struct_type("self")
            } else {
                match obj.as_ref() {
                    AstNode::Identifier(name) => crate::get_var_struct_type(name),
                    AstNode::Call { func, .. } => {
                        // If calling a function whose name matches a struct, it's a constructor
                        if crate::get_struct_layout(func).is_some() {
                            Some(func.clone())
                        } else if let Some(ret_type) = crate::get_func_return_type(func) {
                            if crate::get_struct_layout(&ret_type).is_some() {
                                Some(ret_type)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }
                    AstNode::FieldAccess { obj: inner_obj, .. } => {
                        // Nested field access: try to determine type from inner object's struct
                        if let AstNode::Identifier(name) = inner_obj.as_ref() {
                            let _ = crate::get_var_struct_type(name);
                            None
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            };

            // Check if this is a struct field access
            let struct_offset = if let Some(ref stype) = obj_struct_type {
                crate::get_struct_field_offset(stype, field_name)
            } else {
                // Try to find the field in any known struct (heuristic for unknown types)
                find_field_in_any_struct(field_name)
            };

            // Compile the object expression (use `self` variable for sentinel IntLiteral(0))
            let obj_val = if is_self_access {
                if let Some(self_var) = variables.get("self") {
                    builder.use_var(self_var.var)
                } else {
                    compile_expr(
                        builder,
                        variables,
                        runtime_funcs,
                        declared_funcs,
                        string_funcs,
                        module,
                        obj,
                        func_signatures,
                        lambda_funcs,
                        global_data_ids,
                    )?
                }
            } else {
                compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    obj,
                    func_signatures,
                    lambda_funcs,
                    global_data_ids,
                )?
            };

            // Determine the object's value kind
            let obj_kind = if is_self_access {
                variables
                    .get("self")
                    .map(|v| v.kind)
                    .unwrap_or(ValueKind::Unknown)
            } else {
                infer_value_kind(obj, variables)
            };

            if let Some(offset) = struct_offset {
                // Struct field access: load from ptr + offset
                let loaded =
                    builder
                        .ins()
                        .load(types::I64, MemFlags::new(), obj_val, offset as i32);
                Ok(loaded)
            } else {
                // Not a struct field - handle built-in properties
                match field_name {
                    "len" => {
                        // Dispatch based on object type
                        match obj_kind {
                            ValueKind::String => {
                                if let Some(&len_id) = runtime_funcs.get("forge_cstring_len") {
                                    let len_ref = module.declare_func_in_func(len_id, builder.func);
                                    let call = builder.ins().call(len_ref, &[obj_val]);
                                    Ok(builder.func.dfg.first_result(call))
                                } else {
                                    Ok(builder.ins().iconst(types::I64, 0))
                                }
                            }
                            ValueKind::Map | ValueKind::MapIntKey => {
                                if let Some(&len_id) = runtime_funcs.get("forge_map_len_handle") {
                                    let len_ref = module.declare_func_in_func(len_id, builder.func);
                                    let call = builder.ins().call(len_ref, &[obj_val]);
                                    Ok(builder.func.dfg.first_result(call))
                                } else {
                                    Ok(builder.ins().iconst(types::I64, 0))
                                }
                            }
                            ValueKind::Set => {
                                if let Some(&len_id) = runtime_funcs.get("forge_set_len_handle") {
                                    let len_ref = module.declare_func_in_func(len_id, builder.func);
                                    let call = builder.ins().call(len_ref, &[obj_val]);
                                    Ok(builder.func.dfg.first_result(call))
                                } else {
                                    Ok(builder.ins().iconst(types::I64, 0))
                                }
                            }
                            _ => {
                                if let Some(&len_id) = runtime_funcs.get("forge_list_len") {
                                    let len_ref = module.declare_func_in_func(len_id, builder.func);
                                    let call = builder.ins().call(len_ref, &[obj_val]);
                                    Ok(builder.func.dfg.first_result(call))
                                } else {
                                    Ok(builder.ins().iconst(types::I64, 0))
                                }
                            }
                        }
                    }
                    _ => {
                        // Unknown field on non-struct - return 0
                        eprintln!(
                            "WARN: Unknown field access '.{}' on {:?} (kind={:?})",
                            field_name, obj, obj_kind
                        );
                        Ok(builder.ins().iconst(types::I64, 0))
                    }
                }
            }
        }

        AstNode::Identifier(name) => match variables.get(name) {
            Some(var_info) => {
                // Use Cranelift's Variable system to get the current value
                let val = builder.use_var(var_info.var);
                Ok(val)
            }
            None => {
                // Check if this is a declared user function (for passing as a value)
                if let Some(&func_id) = declared_funcs.get(name) {
                    let func_ref = module.declare_func_in_func(func_id, builder.func);
                    let addr = builder.ins().func_addr(types::I64, func_ref);
                    return Ok(addr);
                }
                // Check if this is a global variable
                if let Some(data_id) = global_data_ids.get(name) {
                    let gv = module.declare_data_in_func(*data_id, builder.func);
                    let addr = builder
                        .ins()
                        .global_value(module.target_config().pointer_type(), gv);
                    let loaded = builder.ins().load(types::I64, MemFlags::new(), addr, 0);
                    Ok(loaded)
                } else {
                    eprintln!(
                        "DEBUG: Unknown variable '{}' in function, available vars: {:?}",
                        name,
                        variables.keys().collect::<Vec<_>>()
                    );
                    // Print a backtrace-like message
                    eprintln!("DEBUG: Backtrace for unknown var '{}' lookup", name);
                    Err(CompileError::UnknownVariable(name.clone()))
                }
            }
        },

        AstNode::UnaryOp { op, operand } => {
            let val = compile_expr(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                operand,
                func_signatures,
                lambda_funcs,
                global_data_ids,
            )?;

            Ok(match op {
                UnaryOp::Neg => builder.ins().ineg(val),
                UnaryOp::Not => {
                    let ty = builder.func.dfg.value_type(val);
                    let zero = builder.ins().iconst(ty, 0);
                    let cmp = builder.ins().icmp(IntCC::Equal, val, zero);
                    builder.ins().uextend(types::I64, cmp)
                }
                UnaryOp::BitNot => builder.ins().bnot(val),
            })
        }

        AstNode::BinaryOp { op, left, right } => {
            // Check if this is string concatenation
            let left_kind = infer_value_kind(left, variables);
            let right_kind = infer_value_kind(right, variables);
            let is_string_op = matches!(left_kind, ValueKind::String)
                || matches!(right_kind, ValueKind::String)
                || matches!(left.as_ref(), AstNode::StringLiteral(_))
                || matches!(right.as_ref(), AstNode::StringLiteral(_))
                || matches!(left.as_ref(), AstNode::StringInterp { .. })
                || matches!(right.as_ref(), AstNode::StringInterp { .. });

            if is_string_op && matches!(op, BinaryOp::Add) {
                // String concatenation using forge_concat_cstr
                let left_val = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    left,
                    func_signatures,
                    lambda_funcs,
                    global_data_ids,
                )?;
                let right_val = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    right,
                    func_signatures,
                    lambda_funcs,
                    global_data_ids,
                )?;

                // Coerce values to cstring pointers for concatenation.
                // String values (i64 = pointer) pass through directly.
                // Int/Bool values (i64 = number) need forge_int_to_cstr.
                // Float values need forge_float_to_cstr.
                macro_rules! coerce_to_cstr {
                    ($val:expr, $kind:expr, $ty:expr) => {{
                        let v = $val;
                        let k = $kind;
                        let t = $ty;
                        if t.is_float() {
                            if let Some(&conv_id) = runtime_funcs.get("forge_float_to_cstr") {
                                let conv_ref = module.declare_func_in_func(conv_id, builder.func);
                                let call = builder.ins().call(conv_ref, &[v]);
                                builder.func.dfg.first_result(call)
                            } else {
                                builder.ins().iconst(types::I64, 0)
                            }
                        } else {
                            let v64 = if t != types::I64 {
                                builder.ins().uextend(types::I64, v)
                            } else {
                                v
                            };
                            if matches!(k, ValueKind::Int | ValueKind::Bool) {
                                if let Some(&conv_id) = runtime_funcs.get("forge_int_to_cstr") {
                                    let conv_ref =
                                        module.declare_func_in_func(conv_id, builder.func);
                                    let call = builder.ins().call(conv_ref, &[v64]);
                                    builder.func.dfg.first_result(call)
                                } else {
                                    v64
                                }
                            } else {
                                v64
                            }
                        }
                    }};
                }

                let left_ty = builder.func.dfg.value_type(left_val);
                let left_cstr = coerce_to_cstr!(left_val, left_kind, left_ty);
                let right_ty = builder.func.dfg.value_type(right_val);
                let right_cstr = coerce_to_cstr!(right_val, right_kind, right_ty);

                if let Some(&concat_id) = runtime_funcs.get("forge_concat_cstr") {
                    let concat_ref = module.declare_func_in_func(concat_id, builder.func);
                    let call = builder.ins().call(concat_ref, &[left_cstr, right_cstr]);
                    Ok(builder.func.dfg.first_result(call))
                } else {
                    Ok(left_val)
                }
            } else {
                // Regular integer arithmetic
                let mut left_val = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    left,
                    func_signatures,
                    lambda_funcs,
                    global_data_ids,
                )?;
                let mut right_val = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    right,
                    func_signatures,
                    lambda_funcs,
                    global_data_ids,
                )?;

                let left_ty = builder.func.dfg.value_type(left_val);
                let right_ty = builder.func.dfg.value_type(right_val);
                let is_float = left_ty == types::F64 || right_ty == types::F64;

                if !is_float && left_ty != right_ty {
                    if left_ty.bits() > right_ty.bits() {
                        right_val = builder.ins().uextend(left_ty, right_val);
                    } else {
                        left_val = builder.ins().uextend(right_ty, left_val);
                    }
                }

                Ok(match op {
                    BinaryOp::Add => {
                        if is_float {
                            builder.ins().fadd(left_val, right_val)
                        } else {
                            builder.ins().iadd(left_val, right_val)
                        }
                    }
                    BinaryOp::Sub => {
                        if is_float {
                            builder.ins().fsub(left_val, right_val)
                        } else {
                            builder.ins().isub(left_val, right_val)
                        }
                    }
                    BinaryOp::Mul => {
                        if is_float {
                            builder.ins().fmul(left_val, right_val)
                        } else {
                            builder.ins().imul(left_val, right_val)
                        }
                    }
                    BinaryOp::Div => {
                        if is_float {
                            builder.ins().fdiv(left_val, right_val)
                        } else {
                            builder.ins().sdiv(left_val, right_val)
                        }
                    }
                    BinaryOp::BitAnd => builder.ins().band(left_val, right_val),
                    BinaryOp::BitOr => builder.ins().bor(left_val, right_val),
                    BinaryOp::BitXor => builder.ins().bxor(left_val, right_val),
                    BinaryOp::Shl => builder.ins().ishl(left_val, right_val),
                    BinaryOp::Shr => builder.ins().sshr(left_val, right_val),
                    BinaryOp::Eq => {
                        // Check if comparing strings - if so, use content-based comparison
                        let left_kind = infer_value_kind(left, variables);
                        let right_kind = infer_value_kind(right, variables);
                        let is_string_comparison =
                            matches!(left_kind, ValueKind::String | ValueKind::ListString)
                                || matches!(right_kind, ValueKind::String | ValueKind::ListString);

                        if is_string_comparison {
                            // Use forge_cstring_eq for content-based C-string comparison
                            if let Some(&eq_func_id) = runtime_funcs.get("forge_cstring_eq") {
                                let eq_func_ref =
                                    module.declare_func_in_func(eq_func_id, builder.func);
                                let call = builder.ins().call(eq_func_ref, &[left_val, right_val]);
                                builder.func.dfg.first_result(call)
                            } else {
                                // Fallback to pointer comparison if runtime function not found
                                let cmp = builder.ins().icmp(IntCC::Equal, left_val, right_val);
                                builder.ins().uextend(types::I64, cmp)
                            }
                        } else if is_float {
                            let cmp = builder.ins().fcmp(FloatCC::Equal, left_val, right_val);
                            builder.ins().uextend(types::I64, cmp)
                        } else {
                            let cmp = builder.ins().icmp(IntCC::Equal, left_val, right_val);
                            builder.ins().uextend(types::I64, cmp)
                        }
                    }
                    BinaryOp::Neq => {
                        let left_kind = infer_value_kind(left, variables);
                        let right_kind = infer_value_kind(right, variables);
                        let is_string_comparison =
                            matches!(left_kind, ValueKind::String | ValueKind::ListString)
                                || matches!(right_kind, ValueKind::String | ValueKind::ListString);

                        if is_string_comparison {
                            if let Some(&eq_func_id) = runtime_funcs.get("forge_cstring_eq") {
                                let eq_func_ref =
                                    module.declare_func_in_func(eq_func_id, builder.func);
                                let call = builder.ins().call(eq_func_ref, &[left_val, right_val]);
                                let eq_result = builder.func.dfg.first_result(call);
                                // Negate: eq returns 1 for equal, we want 1 for not-equal
                                let zero = builder.ins().iconst(types::I64, 0);
                                let cmp = builder.ins().icmp(IntCC::Equal, eq_result, zero);
                                builder.ins().uextend(types::I64, cmp)
                            } else {
                                let cmp = builder.ins().icmp(IntCC::NotEqual, left_val, right_val);
                                builder.ins().uextend(types::I64, cmp)
                            }
                        } else if is_float {
                            let cmp = builder.ins().fcmp(FloatCC::NotEqual, left_val, right_val);
                            builder.ins().uextend(types::I64, cmp)
                        } else {
                            let cmp = builder.ins().icmp(IntCC::NotEqual, left_val, right_val);
                            builder.ins().uextend(types::I64, cmp)
                        }
                    }
                    BinaryOp::Mod => {
                        if is_float {
                            // No fmod in cranelift, convert to int
                            builder.ins().srem(left_val, right_val)
                        } else {
                            builder.ins().srem(left_val, right_val)
                        }
                    }
                    BinaryOp::Gt | BinaryOp::Lt | BinaryOp::Gte | BinaryOp::Lte => {
                        let left_kind = infer_value_kind(left, variables);
                        let right_kind = infer_value_kind(right, variables);
                        let is_string_cmp = matches!(left_kind, ValueKind::String)
                            || matches!(right_kind, ValueKind::String)
                            || matches!(left.as_ref(), AstNode::StringLiteral(_))
                            || matches!(right.as_ref(), AstNode::StringLiteral(_));

                        if is_string_cmp {
                            // Use forge_cstring_cmp for lexicographic comparison
                            if let Some(&cmp_func_id) = runtime_funcs.get("forge_cstring_cmp") {
                                let cmp_func_ref =
                                    module.declare_func_in_func(cmp_func_id, builder.func);
                                let call = builder.ins().call(cmp_func_ref, &[left_val, right_val]);
                                let cmp_result = builder.func.dfg.first_result(call);
                                let zero = builder.ins().iconst(types::I64, 0);
                                let int_cc = match op {
                                    BinaryOp::Gt => IntCC::SignedGreaterThan,
                                    BinaryOp::Lt => IntCC::SignedLessThan,
                                    BinaryOp::Gte => IntCC::SignedGreaterThanOrEqual,
                                    BinaryOp::Lte => IntCC::SignedLessThanOrEqual,
                                    _ => unreachable!(),
                                };
                                let cmp = builder.ins().icmp(int_cc, cmp_result, zero);
                                builder.ins().uextend(types::I64, cmp)
                            } else {
                                // Fallback to pointer comparison
                                let cmp = builder.ins().icmp(
                                    IntCC::SignedGreaterThan,
                                    left_val,
                                    right_val,
                                );
                                builder.ins().uextend(types::I64, cmp)
                            }
                        } else if is_float {
                            let float_cc = match op {
                                BinaryOp::Gt => FloatCC::GreaterThan,
                                BinaryOp::Lt => FloatCC::LessThan,
                                BinaryOp::Gte => FloatCC::GreaterThanOrEqual,
                                BinaryOp::Lte => FloatCC::LessThanOrEqual,
                                _ => unreachable!(),
                            };
                            let cmp = builder.ins().fcmp(float_cc, left_val, right_val);
                            builder.ins().uextend(types::I64, cmp)
                        } else {
                            let int_cc = match op {
                                BinaryOp::Gt => IntCC::SignedGreaterThan,
                                BinaryOp::Lt => IntCC::SignedLessThan,
                                BinaryOp::Gte => IntCC::SignedGreaterThanOrEqual,
                                BinaryOp::Lte => IntCC::SignedLessThanOrEqual,
                                _ => unreachable!(),
                            };
                            let cmp = builder.ins().icmp(int_cc, left_val, right_val);
                            builder.ins().uextend(types::I64, cmp)
                        }
                    }
                    BinaryOp::And => {
                        // Logical AND: return 1 if both non-zero, else 0
                        let left_ty = builder.func.dfg.value_type(left_val);
                        let right_ty = builder.func.dfg.value_type(right_val);
                        let left_zero = builder.ins().iconst(left_ty, 0);
                        let right_zero = builder.ins().iconst(right_ty, 0);
                        let left_bool = builder.ins().icmp(IntCC::NotEqual, left_val, left_zero);
                        let right_bool = builder.ins().icmp(IntCC::NotEqual, right_val, right_zero);
                        let result = builder.ins().band(left_bool, right_bool);
                        builder.ins().uextend(types::I64, result)
                    }
                    BinaryOp::Or => {
                        // Logical OR: return 1 if either non-zero, else 0
                        let left_ty = builder.func.dfg.value_type(left_val);
                        let right_ty = builder.func.dfg.value_type(right_val);
                        let left_zero = builder.ins().iconst(left_ty, 0);
                        let right_zero = builder.ins().iconst(right_ty, 0);
                        let left_bool = builder.ins().icmp(IntCC::NotEqual, left_val, left_zero);
                        let right_bool = builder.ins().icmp(IntCC::NotEqual, right_val, right_zero);
                        let result = builder.ins().bor(left_bool, right_bool);
                        builder.ins().uextend(types::I64, result)
                    }
                    _ => builder.ins().iconst(types::I64, 0),
                })
            }
        }

        AstNode::Call { func, args } => {
            // Check if this is a method call on a list literal
            let (is_list_literal, is_string_literal, is_identifier, is_string_like) =
                if !args.is_empty() {
                    let first_arg = &args[0];
                    let is_list_literal = matches!(first_arg, AstNode::ListLiteral { .. });
                    let is_string_literal = matches!(
                        first_arg,
                        AstNode::StringLiteral(_) | AstNode::StringInterp { .. }
                    );

                    let is_identifier = matches!(first_arg, AstNode::Identifier(_));
                    let is_string_like = matches!(
                        first_arg,
                        AstNode::FieldAccess { .. } | AstNode::Index { .. }
                    ) && matches!(
                        infer_value_kind(first_arg, variables),
                        ValueKind::String | ValueKind::Unknown
                    );
                    (
                        is_list_literal,
                        is_string_literal,
                        is_identifier,
                        is_string_like,
                    )
                } else {
                    (false, false, false, false)
                };

            if !args.is_empty() {
                // If calling .len() on a list literal, use list method
                if is_list_literal && func == "len" {
                    // Fall through to list method check below
                } else if is_string_literal
                    || is_string_like
                    || (is_identifier
                        && matches!(infer_value_kind(&args[0], variables), ValueKind::String))
                {
                    // Check for string method calls on string literals, field accesses, or string-typed identifiers
                    let string_methods = [
                        "len",
                        "contains",
                        "substring",
                        "trim",
                        "starts_with",
                        "ends_with",
                        "to_upper",
                        "to_lower",
                        "reverse",
                        "replace",
                        "index_of",
                    ];
                    if string_methods.contains(&func.as_str()) {
                        let string_arg = &args[0];
                        let string_val = compile_expr(
                            builder,
                            variables,
                            runtime_funcs,
                            declared_funcs,
                            string_funcs,
                            module,
                            string_arg,
                            func_signatures,
                            lambda_funcs,
                            global_data_ids,
                        )?;

                        // Special case: use simple strlen for .len() on raw strings
                        if func == "len" {
                            if let Some(&len_func_id) = runtime_funcs.get("forge_cstring_len") {
                                let len_func_ref =
                                    module.declare_func_in_func(len_func_id, builder.func);
                                let call = builder.ins().call(len_func_ref, &[string_val]);
                                return Ok(builder.func.dfg.first_result(call));
                            }
                        }

                        // Prefer forge_cstring_* variants for all string methods.
                        // These use raw C-string pointers and have consistent return types.
                        // forge_string_* variants use ForgeString structs and return i8 for
                        // bool methods (contains, starts_with, ends_with) — avoid those.
                        let cstring_func_name = format!("forge_cstring_{}", func);
                        if let Some(&func_id) = runtime_funcs.get(&cstring_func_name) {
                            let func_ref = module.declare_func_in_func(func_id, builder.func);
                            let mut arg_values = vec![string_val];
                            for arg in &args[1..] {
                                arg_values.push(compile_expr(
                                    builder,
                                    variables,
                                    runtime_funcs,
                                    declared_funcs,
                                    string_funcs,
                                    module,
                                    arg,
                                    func_signatures,
                                    lambda_funcs,
                                    global_data_ids,
                                )?);
                            }
                            let call = builder.ins().call(func_ref, &arg_values);
                            return if !builder.func.dfg.inst_results(call).is_empty() {
                                Ok(builder.func.dfg.first_result(call))
                            } else {
                                Ok(builder.ins().iconst(types::I64, 0))
                            };
                        }

                        let runtime_func_name = format!("forge_string_{}", func);
                        if let Some(&func_id) = runtime_funcs.get(&runtime_func_name) {
                            let func_ref = module.declare_func_in_func(func_id, builder.func);

                            let mut arg_values = vec![string_val];
                            for arg in &args[1..] {
                                arg_values.push(compile_expr(
                                    builder,
                                    variables,
                                    runtime_funcs,
                                    declared_funcs,
                                    string_funcs,
                                    module,
                                    arg,
                                    func_signatures,
                                    lambda_funcs,
                                    global_data_ids,
                                )?);
                            }

                            let call = builder.ins().call(func_ref, &arg_values);
                            return if !builder.func.dfg.inst_results(call).is_empty() {
                                Ok(builder.func.dfg.first_result(call))
                            } else {
                                Ok(builder.ins().iconst(types::I64, 0))
                            };
                        }
                    }
                } else if is_identifier {
                    // For identifiers with unknown types, try string methods as fallback
                    // This maintains compatibility with code that relies on the heuristic
                    let string_methods = [
                        "len",
                        "contains",
                        "substring",
                        "trim",
                        "starts_with",
                        "ends_with",
                        "to_upper",
                        "to_lower",
                        "reverse",
                        "replace",
                        "index_of",
                    ];
                    if string_methods.contains(&func.as_str()) {
                        let ident_name = match &args[0] {
                            AstNode::Identifier(name) => name,
                            _ => "",
                        };
                        let ident_kind =
                            variables
                                .get(ident_name)
                                .map(|v| v.kind)
                                .unwrap_or_else(|| {
                                    // Check global variable type before defaulting to Unknown
                                    if !ident_name.is_empty() {
                                        if let Some(gtype) = crate::get_global_var_type(ident_name)
                                        {
                                            return infer_kind_from_type_name(&gtype);
                                        }
                                    }
                                    ValueKind::Unknown
                                });

                        // Only try string methods if type is String or Unknown
                        if ident_kind == ValueKind::String || ident_kind == ValueKind::Unknown {
                            let string_arg = &args[0];
                            let string_val = compile_expr(
                                builder,
                                variables,
                                runtime_funcs,
                                declared_funcs,
                                string_funcs,
                                module,
                                string_arg,
                                func_signatures,
                                lambda_funcs,
                                global_data_ids,
                            )?;

                            if func == "len" {
                                if let Some(&len_func_id) = runtime_funcs.get("forge_cstring_len") {
                                    let len_func_ref =
                                        module.declare_func_in_func(len_func_id, builder.func);
                                    let call = builder.ins().call(len_func_ref, &[string_val]);
                                    return Ok(builder.func.dfg.first_result(call));
                                }
                            }

                            // Prefer forge_cstring_* variants (consistent return types, no i8 bools)
                            let cstring_func_name = format!("forge_cstring_{}", func);
                            if let Some(&func_id) = runtime_funcs.get(&cstring_func_name) {
                                let func_ref = module.declare_func_in_func(func_id, builder.func);
                                let mut arg_values = vec![string_val];
                                for arg in &args[1..] {
                                    arg_values.push(compile_expr(
                                        builder,
                                        variables,
                                        runtime_funcs,
                                        declared_funcs,
                                        string_funcs,
                                        module,
                                        arg,
                                        func_signatures,
                                        lambda_funcs,
                                        global_data_ids,
                                    )?);
                                }
                                let call = builder.ins().call(func_ref, &arg_values);
                                return if !builder.func.dfg.inst_results(call).is_empty() {
                                    Ok(builder.func.dfg.first_result(call))
                                } else {
                                    Ok(builder.ins().iconst(types::I64, 0))
                                };
                            }

                            let runtime_func_name = format!("forge_string_{}", func);
                            if let Some(&func_id) = runtime_funcs.get(&runtime_func_name) {
                                let func_ref = module.declare_func_in_func(func_id, builder.func);

                                let mut arg_values = vec![string_val];
                                for arg in &args[1..] {
                                    arg_values.push(compile_expr(
                                        builder,
                                        variables,
                                        runtime_funcs,
                                        declared_funcs,
                                        string_funcs,
                                        module,
                                        arg,
                                        func_signatures,
                                        lambda_funcs,
                                        global_data_ids,
                                    )?);
                                }

                                let call = builder.ins().call(func_ref, &arg_values);
                                return if !builder.func.dfg.inst_results(call).is_empty() {
                                    Ok(builder.func.dfg.first_result(call))
                                } else {
                                    Ok(builder.ins().iconst(types::I64, 0))
                                };
                            }
                        }
                    }
                }
            }

            // Special case: len() — dispatch based on arg type
            if func == "len" && args.len() == 1 {
                let arg_val = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    &args[0],
                    func_signatures,
                    lambda_funcs,
                    global_data_ids,
                )?;
                let arg_kind = infer_value_kind(&args[0], variables);
                let len_func_name = match arg_kind {
                    ValueKind::String => "forge_cstring_len",
                    ValueKind::Map | ValueKind::MapIntKey => "forge_map_len_handle",
                    ValueKind::Set => "forge_set_len_handle",
                    _ => "forge_list_len",
                };
                if let Some(&len_id) = runtime_funcs.get(len_func_name) {
                    let len_ref = module.declare_func_in_func(len_id, builder.func);
                    let call = builder.ins().call(len_ref, &[arg_val]);
                    return Ok(builder.func.dfg.first_result(call));
                }
            }

            // Special case: contains() — dispatch based on arg type
            if func == "contains" && args.len() == 2 {
                let arg_kind = infer_value_kind(&args[0], variables);
                match arg_kind {
                    ValueKind::String => {
                        // String contains: forge_cstring_contains(haystack, needle)
                        let haystack = compile_expr(
                            builder,
                            variables,
                            runtime_funcs,
                            declared_funcs,
                            string_funcs,
                            module,
                            &args[0],
                            func_signatures,
                            lambda_funcs,
                            global_data_ids,
                        )?;
                        let needle = compile_expr(
                            builder,
                            variables,
                            runtime_funcs,
                            declared_funcs,
                            string_funcs,
                            module,
                            &args[1],
                            func_signatures,
                            lambda_funcs,
                            global_data_ids,
                        )?;
                        if let Some(&cid) = runtime_funcs.get("forge_cstring_contains") {
                            let cref = module.declare_func_in_func(cid, builder.func);
                            let call = builder.ins().call(cref, &[haystack, needle]);
                            return Ok(builder.func.dfg.first_result(call));
                        }
                    }
                    ValueKind::Set => {
                        // Set contains: forge_set_contains_cstr(set, elem)
                        let set_val = compile_expr(
                            builder, variables, runtime_funcs, declared_funcs, string_funcs,
                            module, &args[0], func_signatures, lambda_funcs, global_data_ids,
                        )?;
                        let elem_val = compile_expr(
                            builder, variables, runtime_funcs, declared_funcs, string_funcs,
                            module, &args[1], func_signatures, lambda_funcs, global_data_ids,
                        )?;
                        if let Some(&cid) = runtime_funcs.get("forge_set_contains_cstr") {
                            let cref = module.declare_func_in_func(cid, builder.func);
                            let call = builder.ins().call(cref, &[set_val, elem_val]);
                            return Ok(builder.func.dfg.first_result(call));
                        }
                    }
                    ValueKind::ListString | ValueKind::ListUnknown => {
                        // List contains int: forge_list_contains_int(list, value)
                        let list_val = compile_expr(
                            builder,
                            variables,
                            runtime_funcs,
                            declared_funcs,
                            string_funcs,
                            module,
                            &args[0],
                            func_signatures,
                            lambda_funcs,
                            global_data_ids,
                        )?;
                        let elem_val = compile_expr(
                            builder,
                            variables,
                            runtime_funcs,
                            declared_funcs,
                            string_funcs,
                            module,
                            &args[1],
                            func_signatures,
                            lambda_funcs,
                            global_data_ids,
                        )?;
                        let elem_val = if builder.func.dfg.value_type(elem_val) != types::I64 {
                            builder.ins().uextend(types::I64, elem_val)
                        } else {
                            elem_val
                        };
                        if let Some(&cid) = runtime_funcs.get("forge_list_contains_int") {
                            let cref = module.declare_func_in_func(cid, builder.func);
                            let call = builder.ins().call(cref, &[list_val, elem_val]);
                            return Ok(builder.func.dfg.first_result(call));
                        }
                    }
                    _ => {}
                }
            }

            // Special case: index_of() — dispatch based on arg type
            if func == "index_of" && args.len() == 2 {
                let arg_kind = infer_value_kind(&args[0], variables);
                match arg_kind {
                    ValueKind::String => {
                        // String index_of: forge_cstring_index_of(haystack, needle)
                        let s = compile_expr(
                            builder,
                            variables,
                            runtime_funcs,
                            declared_funcs,
                            string_funcs,
                            module,
                            &args[0],
                            func_signatures,
                            lambda_funcs,
                            global_data_ids,
                        )?;
                        let needle = compile_expr(
                            builder,
                            variables,
                            runtime_funcs,
                            declared_funcs,
                            string_funcs,
                            module,
                            &args[1],
                            func_signatures,
                            lambda_funcs,
                            global_data_ids,
                        )?;
                        if let Some(&cid) = runtime_funcs.get("forge_cstring_index_of") {
                            let cref = module.declare_func_in_func(cid, builder.func);
                            let call = builder.ins().call(cref, &[s, needle]);
                            return Ok(builder.func.dfg.first_result(call));
                        }
                    }
                    ValueKind::ListString | ValueKind::ListUnknown => {
                        // List index_of: forge_list_index_of_int(list, value)
                        let list_val = compile_expr(
                            builder,
                            variables,
                            runtime_funcs,
                            declared_funcs,
                            string_funcs,
                            module,
                            &args[0],
                            func_signatures,
                            lambda_funcs,
                            global_data_ids,
                        )?;
                        let elem_val = compile_expr(
                            builder,
                            variables,
                            runtime_funcs,
                            declared_funcs,
                            string_funcs,
                            module,
                            &args[1],
                            func_signatures,
                            lambda_funcs,
                            global_data_ids,
                        )?;
                        let elem_val = if builder.func.dfg.value_type(elem_val) != types::I64 {
                            builder.ins().uextend(types::I64, elem_val)
                        } else {
                            elem_val
                        };
                        if let Some(&cid) = runtime_funcs.get("forge_list_index_of_int") {
                            let cref = module.declare_func_in_func(cid, builder.func);
                            let call = builder.ins().call(cref, &[list_val, elem_val]);
                            return Ok(builder.func.dfg.first_result(call));
                        }
                    }
                    _ => {}
                }
            }

            // Special case: sort() on a list — sorts in-place, returns same list
            if func == "sort" && args.len() == 1 {
                let arg_kind = infer_value_kind(&args[0], variables);
                if matches!(arg_kind, ValueKind::ListString | ValueKind::ListUnknown) {
                    let list_val = compile_expr(
                        builder,
                        variables,
                        runtime_funcs,
                        declared_funcs,
                        string_funcs,
                        module,
                        &args[0],
                        func_signatures,
                        lambda_funcs,
                        global_data_ids,
                    )?;
                    // String lists: sort by dereferencing C-string pointers
                    // Unknown/int lists: sort by i64 value
                    let sort_fn = if matches!(arg_kind, ValueKind::ListString) {
                        "forge_list_sort_strings"
                    } else {
                        "forge_list_sort"
                    };
                    if let Some(&sort_id) = runtime_funcs.get(sort_fn) {
                        let sort_ref = module.declare_func_in_func(sort_id, builder.func);
                        builder.ins().call(sort_ref, &[list_val]);
                    }
                    return Ok(list_val);
                }
            }

            // Special case: slice(start, end) on a list
            if func == "slice" && args.len() == 3 {
                let arg_kind = infer_value_kind(&args[0], variables);
                if matches!(arg_kind, ValueKind::ListString | ValueKind::ListUnknown) {
                    let list_val = compile_expr(
                        builder,
                        variables,
                        runtime_funcs,
                        declared_funcs,
                        string_funcs,
                        module,
                        &args[0],
                        func_signatures,
                        lambda_funcs,
                        global_data_ids,
                    )?;
                    let start_val = compile_expr(
                        builder,
                        variables,
                        runtime_funcs,
                        declared_funcs,
                        string_funcs,
                        module,
                        &args[1],
                        func_signatures,
                        lambda_funcs,
                        global_data_ids,
                    )?;
                    let end_val = compile_expr(
                        builder,
                        variables,
                        runtime_funcs,
                        declared_funcs,
                        string_funcs,
                        module,
                        &args[2],
                        func_signatures,
                        lambda_funcs,
                        global_data_ids,
                    )?;
                    let start_val = if builder.func.dfg.value_type(start_val) != types::I64 {
                        builder.ins().uextend(types::I64, start_val)
                    } else {
                        start_val
                    };
                    let end_val = if builder.func.dfg.value_type(end_val) != types::I64 {
                        builder.ins().uextend(types::I64, end_val)
                    } else {
                        end_val
                    };
                    if let Some(&slice_id) = runtime_funcs.get("forge_list_slice") {
                        let slice_ref = module.declare_func_in_func(slice_id, builder.func);
                        let call = builder
                            .ins()
                            .call(slice_ref, &[list_val, start_val, end_val]);
                        return Ok(builder.func.dfg.first_result(call));
                    }
                }
            }

            // Check for list method calls (includes fallback for variables)
            let list_methods = [
                "push", "pop", "get", "set", "join", "remove", "is_empty", "clear", "reverse", "slice", "sort",
            ];
            // Only dispatch to list methods if the first arg is actually a list.
            // String args (e.g. join("/home", "docs") from std.os.path) must NOT
            // be routed here — they should fall through to the runtime_funcs lookup.
            let first_arg_is_list = args
                .first()
                .map(|a| {
                    let k = infer_value_kind(a, variables);
                    matches!(k, ValueKind::ListString | ValueKind::ListUnknown)
                })
                .unwrap_or(false);
            if list_methods.contains(&func.as_str()) && !args.is_empty() && first_arg_is_list {
                // Transform list.method(args) to forge_list_method(list, args)
                let list_arg = &args[0];
                let list_val = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    list_arg,
                    func_signatures,
                    lambda_funcs,
                    global_data_ids,
                )?;

                if func == "push" {
                    let push_id = runtime_funcs.get("forge_list_push_value").ok_or_else(|| {
                        CompileError::UnknownFunction("forge_list_push_value".to_string())
                    })?;
                    let push_ref = module.declare_func_in_func(*push_id, builder.func);

                    let elem_val = if let Some(arg) = args.get(1) {
                        compile_expr(
                            builder,
                            variables,
                            runtime_funcs,
                            declared_funcs,
                            string_funcs,
                            module,
                            arg,
                            func_signatures,
                            lambda_funcs,
                            global_data_ids,
                        )?
                    } else {
                        builder.ins().iconst(types::I64, 0)
                    };

                    let elem_val = if builder.func.dfg.value_type(elem_val) != types::I64 {
                        builder.ins().uextend(types::I64, elem_val)
                    } else {
                        elem_val
                    };

                    builder.ins().call(push_ref, &[list_val, elem_val]);
                    return Ok(builder.ins().iconst(types::I64, 0));
                }

                if func == "remove" {
                    // Call forge_list_remove(list_ptr, index)
                    let remove_id = runtime_funcs.get("forge_list_remove").ok_or_else(|| {
                        CompileError::UnknownFunction("forge_list_remove".to_string())
                    })?;
                    let remove_ref = module.declare_func_in_func(*remove_id, builder.func);
                    let idx_val = if let Some(arg) = args.get(1) {
                        let v = compile_expr(
                            builder,
                            variables,
                            runtime_funcs,
                            declared_funcs,
                            string_funcs,
                            module,
                            arg,
                            func_signatures,
                            lambda_funcs,
                            global_data_ids,
                        )?;
                        if builder.func.dfg.value_type(v) != types::I64 {
                            builder.ins().uextend(types::I64, v)
                        } else {
                            v
                        }
                    } else {
                        builder.ins().iconst(types::I64, 0)
                    };
                    builder.ins().call(remove_ref, &[list_val, idx_val]);
                    return Ok(builder.ins().iconst(types::I64, 0));
                }

                if func == "join" {
                    let join_id = runtime_funcs.get("forge_list_join").ok_or_else(|| {
                        CompileError::UnknownFunction("forge_list_join".to_string())
                    })?;
                    let join_ref = module.declare_func_in_func(*join_id, builder.func);
                    let sep_val = if let Some(arg) = args.get(1) {
                        compile_expr(
                            builder,
                            variables,
                            runtime_funcs,
                            declared_funcs,
                            string_funcs,
                            module,
                            arg,
                            func_signatures,
                            lambda_funcs,
                            global_data_ids,
                        )?
                    } else {
                        builder.ins().iconst(types::I64, 0)
                    };
                    let call = builder.ins().call(join_ref, &[list_val, sep_val]);
                    return Ok(builder.func.dfg.first_result(call));
                }

                if func == "contains" {
                    let contains_id =
                        runtime_funcs
                            .get("forge_list_contains_int")
                            .ok_or_else(|| {
                                CompileError::UnknownFunction("forge_list_contains_int".to_string())
                            })?;
                    let contains_ref = module.declare_func_in_func(*contains_id, builder.func);
                    let elem_val = if let Some(arg) = args.get(1) {
                        let v = compile_expr(
                            builder,
                            variables,
                            runtime_funcs,
                            declared_funcs,
                            string_funcs,
                            module,
                            arg,
                            func_signatures,
                            lambda_funcs,
                            global_data_ids,
                        )?;
                        if builder.func.dfg.value_type(v) != types::I64 {
                            builder.ins().uextend(types::I64, v)
                        } else {
                            v
                        }
                    } else {
                        builder.ins().iconst(types::I64, 0)
                    };
                    let call = builder.ins().call(contains_ref, &[list_val, elem_val]);
                    return Ok(builder.func.dfg.first_result(call));
                }

                if func == "is_empty" {
                    let is_empty_id =
                        runtime_funcs.get("forge_list_is_empty").ok_or_else(|| {
                            CompileError::UnknownFunction("forge_list_is_empty".to_string())
                        })?;
                    let is_empty_ref = module.declare_func_in_func(*is_empty_id, builder.func);
                    let call = builder.ins().call(is_empty_ref, &[list_val]);
                    return Ok(builder.func.dfg.first_result(call));
                }

                if func == "clear" {
                    let clear_id = runtime_funcs.get("forge_list_clear").ok_or_else(|| {
                        CompileError::UnknownFunction("forge_list_clear".to_string())
                    })?;
                    let clear_ref = module.declare_func_in_func(*clear_id, builder.func);
                    builder.ins().call(clear_ref, &[list_val]);
                    return Ok(builder.ins().iconst(types::I64, 0));
                }

                if func == "reverse" {
                    let reverse_id = runtime_funcs.get("forge_list_reverse").ok_or_else(|| {
                        CompileError::UnknownFunction("forge_list_reverse".to_string())
                    })?;
                    let reverse_ref = module.declare_func_in_func(*reverse_id, builder.func);
                    builder.ins().call(reverse_ref, &[list_val]);
                    return Ok(builder.ins().iconst(types::I64, 0));
                }

                if func == "slice" && args.len() >= 3 {
                    let start = compile_expr(
                        builder, variables, runtime_funcs, declared_funcs, string_funcs,
                        module, &args[1], func_signatures, lambda_funcs, global_data_ids,
                    )?;
                    let end = compile_expr(
                        builder, variables, runtime_funcs, declared_funcs, string_funcs,
                        module, &args[2], func_signatures, lambda_funcs, global_data_ids,
                    )?;
                    if let Some(&slice_id) = runtime_funcs.get("forge_list_slice") {
                        let slice_ref = module.declare_func_in_func(slice_id, builder.func);
                        let call = builder.ins().call(slice_ref, &[list_val, start, end]);
                        return Ok(builder.func.dfg.first_result(call));
                    }
                }

                if func == "sort" {
                    // Determine if string list to use string sort
                    let list_kind = infer_value_kind(&args[0], variables);
                    let sort_name = if matches!(list_kind, ValueKind::ListString) {
                        "forge_list_sort_strings"
                    } else {
                        "forge_list_sort"
                    };
                    if let Some(&sort_id) = runtime_funcs.get(sort_name) {
                        let sort_ref = module.declare_func_in_func(sort_id, builder.func);
                        builder.ins().call(sort_ref, &[list_val]);
                    }
                    return Ok(list_val);
                }

                let runtime_func_name = format!("forge_list_{}", func);
                if let Some(&func_id) = runtime_funcs.get(&runtime_func_name) {
                    let func_ref = module.declare_func_in_func(func_id, builder.func);

                    // Compile additional args (if any)
                    let mut arg_values = vec![list_val];
                    for arg in &args[1..] {
                        arg_values.push(compile_expr(
                            builder,
                            variables,
                            runtime_funcs,
                            declared_funcs,
                            string_funcs,
                            module,
                            arg,
                            func_signatures,
                            lambda_funcs,
                            global_data_ids,
                        )?);
                    }

                    let call = builder.ins().call(func_ref, &arg_values);
                    return if !builder.func.dfg.inst_results(call).is_empty() {
                        Ok(builder.func.dfg.first_result(call))
                    } else {
                        Ok(builder.ins().iconst(types::I64, 0))
                    };
                }
            }

            // Check for map method calls
            let map_methods = ["contains_key", "keys", "values", "insert", "remove", "get", "clear", "is_empty"];
            // Only dispatch to map methods if the first arg is actually a map.
            let first_arg_map_kind = args
                .first()
                .map(|a| infer_value_kind(a, variables))
                .unwrap_or(ValueKind::Unknown);
            let first_arg_is_map =
                matches!(first_arg_map_kind, ValueKind::Map | ValueKind::MapIntKey);
            let is_int_key_map = matches!(first_arg_map_kind, ValueKind::MapIntKey);
            if map_methods.contains(&func.as_str()) && !args.is_empty() && first_arg_is_map {
                let map_arg = &args[0];
                let map_val = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    map_arg,
                    func_signatures,
                    lambda_funcs,
                    global_data_ids,
                )?;

                if func == "insert" && args.len() >= 3 {
                    let key_val = compile_expr(
                        builder,
                        variables,
                        runtime_funcs,
                        declared_funcs,
                        string_funcs,
                        module,
                        &args[1],
                        func_signatures,
                        lambda_funcs,
                        global_data_ids,
                    )?;
                    let val_val = compile_expr(
                        builder,
                        variables,
                        runtime_funcs,
                        declared_funcs,
                        string_funcs,
                        module,
                        &args[2],
                        func_signatures,
                        lambda_funcs,
                        global_data_ids,
                    )?;
                    let val_i64 = if builder.func.dfg.value_type(val_val) != types::I64 {
                        builder.ins().uextend(types::I64, val_val)
                    } else {
                        val_val
                    };
                    let insert_name = if is_int_key_map {
                        "forge_map_insert_ikey"
                    } else {
                        "forge_map_insert_cstr"
                    };
                    if let Some(&insert_id) = runtime_funcs.get(insert_name) {
                        let insert_ref = module.declare_func_in_func(insert_id, builder.func);
                        builder.ins().call(insert_ref, &[map_val, key_val, val_i64]);
                    }
                    return Ok(builder.ins().iconst(types::I64, 0));
                }

                if func == "contains_key" && args.len() >= 2 {
                    let key_val = compile_expr(
                        builder,
                        variables,
                        runtime_funcs,
                        declared_funcs,
                        string_funcs,
                        module,
                        &args[1],
                        func_signatures,
                        lambda_funcs,
                        global_data_ids,
                    )?;
                    let contains_name = if is_int_key_map {
                        "forge_map_contains_ikey"
                    } else {
                        "forge_map_contains_cstr"
                    };
                    if let Some(&contains_id) = runtime_funcs.get(contains_name) {
                        let contains_ref = module.declare_func_in_func(contains_id, builder.func);
                        let call = builder.ins().call(contains_ref, &[map_val, key_val]);
                        return Ok(builder.func.dfg.first_result(call));
                    }
                    return Ok(builder.ins().iconst(types::I64, 0));
                }

                if func == "get" && args.len() >= 2 {
                    let key_val = compile_expr(
                        builder,
                        variables,
                        runtime_funcs,
                        declared_funcs,
                        string_funcs,
                        module,
                        &args[1],
                        func_signatures,
                        lambda_funcs,
                        global_data_ids,
                    )?;
                    let get_name = if is_int_key_map {
                        "forge_map_get_ikey"
                    } else {
                        "forge_map_get_cstr"
                    };
                    if let Some(&get_id) = runtime_funcs.get(get_name) {
                        let get_ref = module.declare_func_in_func(get_id, builder.func);
                        let call = builder.ins().call(get_ref, &[map_val, key_val]);
                        return Ok(builder.func.dfg.first_result(call));
                    }
                    return Ok(builder.ins().iconst(types::I64, 0));
                }

                if func == "remove" && args.len() >= 2 {
                    let key_val = compile_expr(
                        builder,
                        variables,
                        runtime_funcs,
                        declared_funcs,
                        string_funcs,
                        module,
                        &args[1],
                        func_signatures,
                        lambda_funcs,
                        global_data_ids,
                    )?;
                    let remove_name = if is_int_key_map {
                        "forge_map_remove_ikey"
                    } else {
                        "forge_map_remove_cstr"
                    };
                    if let Some(&remove_id) = runtime_funcs.get(remove_name) {
                        let remove_ref = module.declare_func_in_func(remove_id, builder.func);
                        builder.ins().call(remove_ref, &[map_val, key_val]);
                    }
                    return Ok(builder.ins().iconst(types::I64, 0));
                }

                if func == "keys" {
                    // map.keys() → forge_map_keys_cstr(map)
                    if let Some(&keys_id) = runtime_funcs.get("forge_map_keys_cstr") {
                        let keys_ref = module.declare_func_in_func(keys_id, builder.func);
                        let call = builder.ins().call(keys_ref, &[map_val]);
                        return Ok(builder.func.dfg.first_result(call));
                    }
                    // Fallback: return empty list
                    let list_new_id = runtime_funcs.get("forge_list_new").ok_or_else(|| {
                        CompileError::UnknownFunction("forge_list_new".to_string())
                    })?;
                    let list_new_ref = module.declare_func_in_func(*list_new_id, builder.func);
                    let elem_size = builder.ins().iconst(types::I64, 8);
                    let type_tag = builder.ins().iconst(types::I32, 0);
                    let call = builder.ins().call(list_new_ref, &[elem_size, type_tag]);
                    return Ok(builder.func.dfg.first_result(call));
                }

                if func == "values" {
                    if let Some(&values_id) = runtime_funcs.get("forge_map_values_handle") {
                        let values_ref = module.declare_func_in_func(values_id, builder.func);
                        let call = builder.ins().call(values_ref, &[map_val]);
                        return Ok(builder.func.dfg.first_result(call));
                    }
                    // Fallback: return empty list
                    let list_new_id = runtime_funcs.get("forge_list_new").ok_or_else(|| {
                        CompileError::UnknownFunction("forge_list_new".to_string())
                    })?;
                    let list_new_ref = module.declare_func_in_func(*list_new_id, builder.func);
                    let elem_size = builder.ins().iconst(types::I64, 8);
                    let type_tag = builder.ins().iconst(types::I32, 0);
                    let call = builder.ins().call(list_new_ref, &[elem_size, type_tag]);
                    return Ok(builder.func.dfg.first_result(call));
                }

                if func == "clear" {
                    if let Some(&clear_id) = runtime_funcs.get("forge_map_clear_handle") {
                        let clear_ref = module.declare_func_in_func(clear_id, builder.func);
                        builder.ins().call(clear_ref, &[map_val]);
                    }
                    return Ok(builder.ins().iconst(types::I64, 0));
                }

                if func == "is_empty" {
                    if let Some(&is_empty_id) = runtime_funcs.get("forge_map_is_empty_handle") {
                        let is_empty_ref = module.declare_func_in_func(is_empty_id, builder.func);
                        let call = builder.ins().call(is_empty_ref, &[map_val]);
                        return Ok(builder.func.dfg.first_result(call));
                    }
                    return Ok(builder.ins().iconst(types::I64, 0));
                }

                let runtime_func_name = format!("forge_map_{}", func);
                if let Some(&func_id) = runtime_funcs.get(&runtime_func_name) {
                    let func_ref = module.declare_func_in_func(func_id, builder.func);
                    let mut arg_values = vec![map_val];
                    for arg in &args[1..] {
                        arg_values.push(compile_expr(
                            builder,
                            variables,
                            runtime_funcs,
                            declared_funcs,
                            string_funcs,
                            module,
                            arg,
                            func_signatures,
                            lambda_funcs,
                            global_data_ids,
                        )?);
                    }
                    let call = builder.ins().call(func_ref, &arg_values);
                    return if !builder.func.dfg.inst_results(call).is_empty() {
                        Ok(builder.func.dfg.first_result(call))
                    } else {
                        Ok(builder.ins().iconst(types::I64, 0))
                    };
                }
            }

            // Check for set method calls
            let set_methods = ["contains", "add", "remove", "len", "clear", "is_empty"];
            let first_arg_set_kind = args
                .first()
                .map(|a| infer_value_kind(a, variables))
                .unwrap_or(ValueKind::Unknown);
            if set_methods.contains(&func.as_str()) && !args.is_empty() && matches!(first_arg_set_kind, ValueKind::Set) {
                let set_val = compile_expr(
                    builder, variables, runtime_funcs, declared_funcs, string_funcs,
                    module, &args[0], func_signatures, lambda_funcs, global_data_ids,
                )?;

                if func == "len" {
                    if let Some(&len_id) = runtime_funcs.get("forge_set_len_handle") {
                        let len_ref = module.declare_func_in_func(len_id, builder.func);
                        let call = builder.ins().call(len_ref, &[set_val]);
                        return Ok(builder.func.dfg.first_result(call));
                    }
                }

                if (func == "contains" || func == "add" || func == "remove") && args.len() >= 2 {
                    let elem_val = compile_expr(
                        builder, variables, runtime_funcs, declared_funcs, string_funcs,
                        module, &args[1], func_signatures, lambda_funcs, global_data_ids,
                    )?;
                    let rt_name = match func.as_str() {
                        "contains" => "forge_set_contains_cstr",
                        "add" => "forge_set_add_cstr",
                        "remove" => "forge_set_remove_cstr",
                        _ => unreachable!(),
                    };
                    if let Some(&func_id) = runtime_funcs.get(rt_name) {
                        let func_ref = module.declare_func_in_func(func_id, builder.func);
                        let call = builder.ins().call(func_ref, &[set_val, elem_val]);
                        return if !builder.func.dfg.inst_results(call).is_empty() {
                            Ok(builder.func.dfg.first_result(call))
                        } else {
                            Ok(builder.ins().iconst(types::I64, 0))
                        };
                    }
                }

                if func == "clear" {
                    if let Some(&clear_id) = runtime_funcs.get("forge_set_clear_handle") {
                        let clear_ref = module.declare_func_in_func(clear_id, builder.func);
                        builder.ins().call(clear_ref, &[set_val]);
                    }
                    return Ok(builder.ins().iconst(types::I64, 0));
                }

                if func == "is_empty" {
                    if let Some(&ie_id) = runtime_funcs.get("forge_set_is_empty_handle") {
                        let ie_ref = module.declare_func_in_func(ie_id, builder.func);
                        let call = builder.ins().call(ie_ref, &[set_val]);
                        return Ok(builder.func.dfg.first_result(call));
                    }
                }

                return Ok(builder.ins().iconst(types::I64, 0));
            }

            // Handle to_string type-aware dispatch
            if func == "to_string" && !args.is_empty() {
                let arg_kind = infer_value_kind(&args[0], variables);
                // If the argument is already a string, just pass it through
                if matches!(arg_kind, ValueKind::String) {
                    return compile_expr(
                        builder,
                        variables,
                        runtime_funcs,
                        declared_funcs,
                        string_funcs,
                        module,
                        &args[0],
                        func_signatures,
                        lambda_funcs,
                        global_data_ids,
                    );
                }
                let arg_val = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    &args[0],
                    func_signatures,
                    lambda_funcs,
                    global_data_ids,
                )?;
                let arg_ty = builder.func.dfg.value_type(arg_val);
                let converter = if arg_ty == types::F64 {
                    runtime_funcs.get("forge_float_to_cstr")
                } else if matches!(arg_kind, ValueKind::Bool) {
                    runtime_funcs.get("forge_bool_to_cstr")
                } else if matches!(arg_kind, ValueKind::Unknown) {
                    // Unknown type — use smart to_string that handles both int and string pointers
                    runtime_funcs.get("forge_smart_to_string")
                        .or_else(|| runtime_funcs.get("forge_int_to_cstr"))
                } else {
                    runtime_funcs.get("forge_int_to_cstr")
                };
                if let Some(&cid) = converter {
                    let cref = module.declare_func_in_func(cid, builder.func);
                    let call = builder.ins().call(cref, &[arg_val]);
                    return Ok(builder.func.dfg.first_result(call));
                }
                return Ok(arg_val);
            }

            // Special case: args() → use forge_args_to_list for proper List return
            if func == "args" && args.is_empty() {
                if let Some(&args_list_id) = runtime_funcs.get("forge_args_to_list") {
                    let args_ref = module.declare_func_in_func(args_list_id, builder.func);
                    let call = builder.ins().call(args_ref, &[]);
                    return Ok(builder.func.dfg.first_result(call));
                }
            }

            // Handle print function selection based on argument type
            let (func_name, arg_values) = if func == "print" && !args.is_empty() {
                let arg_kind = infer_value_kind(&args[0], variables);
                // A BinaryOp is numeric unless it's a string concat (Add with a string operand)
                let is_string_binop = matches!(&args[0], AstNode::BinaryOp { op, left, right }
                    if matches!(op, BinaryOp::Add) && (
                        matches!(left.as_ref(), AstNode::StringLiteral(_) | AstNode::StringInterp { .. })
                        || matches!(right.as_ref(), AstNode::StringLiteral(_) | AstNode::StringInterp { .. })
                    )
                );
                let looks_numeric = !is_string_binop
                    && !matches!(arg_kind, ValueKind::String)
                    && (matches!(
                        &args[0],
                        AstNode::IntLiteral(_)
                            | AstNode::BoolLiteral(_)
                            | AstNode::FloatLiteral(_)
                            | AstNode::BinaryOp { .. }
                            | AstNode::UnaryOp { .. }
                            | AstNode::Index { .. }
                    ) || matches!(&args[0], AstNode::Call { func, .. } if func == "len")
                        || matches!(arg_kind, ValueKind::Int | ValueKind::Bool));

                // Compile the first argument to determine its type
                let first_arg = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    &args[0],
                    func_signatures,
                    lambda_funcs,
                    global_data_ids,
                )?;
                let first_arg_ty = builder.func.dfg.value_type(first_arg);

                // Compile remaining args if any
                let mut all_args = vec![first_arg];
                for arg in &args[1..] {
                    all_args.push(compile_expr(
                        builder,
                        variables,
                        runtime_funcs,
                        declared_funcs,
                        string_funcs,
                        module,
                        arg,
                        func_signatures,
                        lambda_funcs,
                        global_data_ids,
                    )?);
                }

                // Choose print function based on argument type
                if first_arg_ty == types::F64 {
                    // Float: convert to string first, then print as cstr
                    if let Some(&conv_id) = runtime_funcs.get("forge_float_to_cstr") {
                        let conv_ref = module.declare_func_in_func(conv_id, builder.func);
                        let call = builder.ins().call(conv_ref, &[all_args[0]]);
                        let s = builder.func.dfg.first_result(call);
                        ("forge_print_cstr", vec![s])
                    } else {
                        ("forge_print_int", all_args)
                    }
                } else if matches!(arg_kind, ValueKind::Bool)
                    || matches!(&args[0], AstNode::BoolLiteral(_))
                {
                    // Bool: convert to "true"/"false" string first, then print
                    if let Some(&conv_id) = runtime_funcs.get("forge_bool_to_cstr") {
                        let conv_ref = module.declare_func_in_func(conv_id, builder.func);
                        let call = builder.ins().call(conv_ref, &[all_args[0]]);
                        let s = builder.func.dfg.first_result(call);
                        ("forge_print_cstr", vec![s])
                    } else {
                        ("forge_print_int", all_args)
                    }
                } else if looks_numeric {
                    ("forge_print_int", all_args)
                } else {
                    ("forge_print_cstr", all_args)
                }
            } else {
                // Original logic for other functions
                // Dispatch TOML vs JSON based on arg count for shared names
                let fname = match (func.as_str(), args.len()) {
                    ("print", _) => "forge_print_cstr",
                    ("print_int", _) => "forge_print_int",
                    // TOML 2-arg variants (handle, key) — only TOML uses these with 2 args
                    ("get_string", 2) => "toml_get_string",
                    ("get_int", 2) => "toml_get_int",
                    ("get_float", 2) => "toml_get_float",
                    ("get_bool", 2) => "toml_get_bool",
                    ("keys", 1) => "toml_keys",
                    _ => func,
                };

                // Compile all arguments
                let mut avals = Vec::new();
                for arg in args {
                    avals.push(compile_expr(
                        builder,
                        variables,
                        runtime_funcs,
                        declared_funcs,
                        string_funcs,
                        module,
                        arg,
                        func_signatures,
                        lambda_funcs,
                        global_data_ids,
                    )?);
                }
                (fname, avals)
            };

            // User-defined functions take priority over runtime aliases
            // Also check for monomorphized generic functions (e.g., show -> show_Point)
            let Some(func_id) = declared_funcs
                .get(func)
                .copied()
                .or_else(|| {
                    // Try generic function resolution: infer type from first arg
                    if !arg_values.is_empty() {
                        let type_name = crate::monomorphize::infer_arg_type_from_node(
                            args.first().unwrap(), variables,
                        );
                        if let Some(tn) = type_name {
                            let mangled = format!("{}_{}", func, tn);
                            declared_funcs.get(&mangled).copied()
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
                .or_else(|| runtime_funcs.get(func_name).copied())
                .or_else(|| runtime_funcs.get(func).copied())
            else {
                if func
                    .chars()
                    .next()
                    .map(|c| c.is_ascii_uppercase())
                    .unwrap_or(false)
                {
                    // Struct constructor: TypeName(field1, field2, ...)
                    if let Some(layout) = crate::get_struct_layout(func) {
                        // Allocate struct
                        let alloc_func_id =
                            runtime_funcs.get("forge_struct_alloc").ok_or_else(|| {
                                CompileError::UnknownFunction("forge_struct_alloc".to_string())
                            })?;
                        let alloc_ref = module.declare_func_in_func(*alloc_func_id, builder.func);
                        let num_fields_val = builder.ins().iconst(types::I64, layout.len() as i64);
                        let alloc_call = builder.ins().call(alloc_ref, &[num_fields_val]);
                        let struct_ptr = builder.func.dfg.first_result(alloc_call);

                        // Store each arg at the corresponding field offset
                        for (i, arg) in args.iter().enumerate() {
                            let arg_val = compile_expr(
                                builder,
                                variables,
                                runtime_funcs,
                                declared_funcs,
                                string_funcs,
                                module,
                                arg,
                                func_signatures,
                                lambda_funcs,
                                global_data_ids,
                            )?;
                            let offset = if i < layout.len() {
                                layout[i].1 as i32
                            } else {
                                (i * 8) as i32
                            };
                            let val_ty = builder.func.dfg.value_type(arg_val);
                            let val_i64 = if val_ty != types::I64 {
                                if val_ty.is_float() {
                                    builder.ins().bitcast(types::I64, MemFlags::new(), arg_val)
                                } else {
                                    builder.ins().uextend(types::I64, arg_val)
                                }
                            } else {
                                arg_val
                            };
                            builder
                                .ins()
                                .store(MemFlags::new(), val_i64, struct_ptr, offset);
                        }

                        return Ok(struct_ptr);
                    }
                    // Unknown uppercase function (not a known struct) - return 0
                    return Ok(builder.ins().iconst(types::I64, 0));
                }

                // Check if this is a variable holding a function pointer (Fn param or lambda)
                if let Some(_var_info) = variables.get(func) {
                    // Compile arguments
                    let mut indirect_args: Vec<Value> = Vec::new();
                    for arg in args {
                        let v = compile_expr(
                            builder,
                            variables,
                            runtime_funcs,
                            declared_funcs,
                            string_funcs,
                            module,
                            arg,
                            func_signatures,
                            lambda_funcs,
                            global_data_ids,
                        )?;
                        // Ensure all args are I64 for indirect call
                        let v64 = if builder.func.dfg.value_type(v) != types::I64 {
                            builder.ins().uextend(types::I64, v)
                        } else {
                            v
                        };
                        indirect_args.push(v64);
                    }
                    // Get the function pointer from the variable
                    let fn_ptr = {
                        let vi = variables.get(func).unwrap();
                        builder.use_var(vi.var)
                    };
                    // Build signature: N x I64 args -> I64
                    let mut sig = module.make_signature();
                    for _ in &indirect_args {
                        sig.params.push(AbiParam::new(types::I64));
                    }
                    sig.returns.push(AbiParam::new(types::I64));
                    let sig_ref = builder.import_signature(sig);
                    // call_indirect: sig_ref, callee ptr, args
                    let mut call_args = indirect_args;
                    let call = builder.ins().call_indirect(sig_ref, fn_ptr, &call_args);
                    return Ok(builder.func.dfg.first_result(call));
                }

                return Err(CompileError::UnknownFunction(func.clone()));
            };

            let func_ref = module.declare_func_in_func(func_id, builder.func);

            let coerced_args = if let Some(param_types) = func_signatures
                .get(func_name)
                .or_else(|| func_signatures.get(func))
            {
                let mut coerced = Vec::with_capacity(param_types.len());
                for (i, &target_ty) in param_types.iter().enumerate() {
                    let val = if let Some(&existing) = arg_values.get(i) {
                        let val_ty = builder.func.dfg.value_type(existing);
                        if val_ty == target_ty {
                            // Types already match
                            existing
                        } else if val_ty.is_float() && target_ty.is_int() {
                            // Float -> Int: bitcast (reinterpret bits)
                            builder.ins().bitcast(target_ty, MemFlags::new(), existing)
                        } else if val_ty.is_int() && target_ty.is_float() {
                            // Int -> Float: bitcast (reinterpret bits)
                            builder.ins().bitcast(target_ty, MemFlags::new(), existing)
                        } else if val_ty.is_int() && target_ty.is_int() {
                            if val_ty.bits() > target_ty.bits() {
                                builder.ins().ireduce(target_ty, existing)
                            } else {
                                builder.ins().uextend(target_ty, existing)
                            }
                        } else {
                            // Float to float or other: use bitcast as fallback
                            builder.ins().bitcast(target_ty, MemFlags::new(), existing)
                        }
                    } else if target_ty.is_float() {
                        builder.ins().f64const(0.0)
                    } else {
                        builder.ins().iconst(target_ty, 0)
                    };
                    coerced.push(val);
                }
                coerced
            } else {
                arg_values
            };

            let call = builder.ins().call(func_ref, &coerced_args);

            if !builder.func.dfg.inst_results(call).is_empty() {
                Ok(builder.func.dfg.first_result(call))
            } else {
                Ok(builder.ins().iconst(types::I64, 0))
            }
        }

        AstNode::Match { expr, arms } => {
            // Compile the subject expression (the value being matched)
            let subject_val = compile_expr(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                expr,
                func_signatures,
                lambda_funcs,
                global_data_ids,
            )?;

            // Create the merge block that will receive the final result
            let merge_block = builder.create_block();
            builder.func.dfg.append_block_param(merge_block, types::I64);

            // Create a default block for when no pattern matches
            let default_block = builder.create_block();

            // Collect all the check blocks and arm blocks we need
            let mut check_blocks: Vec<Block> = Vec::new();
            let mut arm_blocks: Vec<Block> = Vec::new();

            for _ in 0..arms.len() {
                check_blocks.push(builder.create_block());
                arm_blocks.push(builder.create_block());
            }

            // Jump from the current block to the first check block
            builder.ins().jump(check_blocks[0], &[]);

            // Now process each arm
            for (i, arm) in arms.iter().enumerate() {
                let check_block = check_blocks[i];
                let arm_block = arm_blocks[i];
                let next_check = if i + 1 < arms.len() {
                    check_blocks[i + 1]
                } else {
                    // No more arms, go to default block
                    default_block
                };

                // Switch to the check block and compile the pattern check
                builder.switch_to_block(check_block);
                let pattern_matches = compile_pattern_check(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    &arm.pattern,
                    subject_val,
                    func_signatures,
                    lambda_funcs,
                    global_data_ids,
                )?;

                // Branch to arm block if pattern matches, otherwise to next check
                builder
                    .ins()
                    .brif(pattern_matches, arm_block, &[], next_check, &[]);
                builder.seal_block(check_block);

                // Compile the arm body
                builder.switch_to_block(arm_block);
                let arm_result = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    &arm.expr,
                    func_signatures,
                    lambda_funcs,
                    global_data_ids,
                )?;
                builder.ins().jump(merge_block, &[arm_result]);
                builder.seal_block(arm_block);
            }

            // Compile the default block (no pattern matched)
            builder.switch_to_block(default_block);
            let default_val = builder.ins().iconst(types::I64, 0); // Default to 0
            builder.ins().jump(merge_block, &[default_val]);
            builder.seal_block(default_block);

            // Finalize: switch to merge block and get the result
            builder.switch_to_block(merge_block);
            let result = builder.block_params(merge_block)[0];
            builder.seal_block(merge_block);
            Ok(result)
        }

        AstNode::EnumDecl { .. } => {
            // Enum declarations don't generate code at the expression level
            // They're handled at module level
            Ok(builder.ins().iconst(types::I64, 0))
        }

        AstNode::EnumVariantConstruct { .. } => {
            // For now, return 0 as placeholder
            // Full implementation requires runtime support for tagged unions
            Ok(builder.ins().iconst(types::I64, 0))
        }

        AstNode::InterfaceDecl { .. } => {
            // Interface declarations don't generate code at expression level
            Ok(builder.ins().iconst(types::I64, 0))
        }

        AstNode::ImplBlock { .. } => {
            // Impl blocks don't generate code at expression level
            Ok(builder.ins().iconst(types::I64, 0))
        }

        AstNode::Lambda { .. } => {
            // Lambda compiled as a separate function.
            // 1. Emit forge_closure_set_env(slot, value) for each captured variable.
            // 2. Return the lambda function's address as i64.
            let ptr = node as *const AstNode as usize;
            let captures = get_lambda_captures(ptr);
            eprintln!(
                "DEBUG: Lambda node ptr={:#x}, in lambda_funcs={}, captures={:?}",
                ptr,
                lambda_funcs.contains_key(&ptr),
                captures
            );
            if let Some(&lambda_func_id) = lambda_funcs.get(&ptr) {
                // Emit closure env setup for captured vars
                if !captures.is_empty() {
                    if let Some(&set_env_id) = runtime_funcs.get("forge_closure_set_env") {
                        let set_env_ref = module.declare_func_in_func(set_env_id, builder.func);
                        for (slot, cap_name) in captures.iter().enumerate() {
                            let slot_val = builder.ins().iconst(types::I64, slot as i64);
                            // Look up the captured variable's current value
                            if let Some(var_info) = variables.get(cap_name) {
                                let cap_val = builder.use_var(var_info.var);
                                let cap_val64 =
                                    if builder.func.dfg.value_type(cap_val) != types::I64 {
                                        builder.ins().uextend(types::I64, cap_val)
                                    } else {
                                        cap_val
                                    };
                                builder.ins().call(set_env_ref, &[slot_val, cap_val64]);
                            }
                        }
                    }
                }
                let func_ref = module.declare_func_in_func(lambda_func_id, builder.func);
                let addr = builder.ins().func_addr(types::I64, func_ref);
                Ok(addr)
            } else {
                // Fallback: return 0 (lambda not compiled)
                Ok(builder.ins().iconst(types::I64, 0))
            }
        }

        AstNode::Spawn { expr } => {
            // spawn func(args) → forge_spawn(fn_ptr, arg)
            // The inner expression should be a Call node
            if let AstNode::Call { func, args } = expr.as_ref() {
                // Get the function pointer
                let fn_ptr = if let Some(&func_id) = declared_funcs.get(func.as_str()) {
                    let func_ref = module.declare_func_in_func(func_id, builder.func);
                    builder.ins().func_addr(types::I64, func_ref)
                } else {
                    // Unknown function — return 0
                    return Ok(builder.ins().iconst(types::I64, 0));
                };

                // Compile the first argument (spawn only supports single-arg functions for now)
                let arg_val = if !args.is_empty() {
                    compile_expr(
                        builder, variables, runtime_funcs, declared_funcs, string_funcs,
                        module, &args[0], func_signatures, lambda_funcs, global_data_ids,
                    )?
                } else {
                    builder.ins().iconst(types::I64, 0)
                };

                // Call forge_spawn(fn_ptr, arg)
                if let Some(&spawn_id) = runtime_funcs.get("forge_spawn") {
                    let spawn_ref = module.declare_func_in_func(spawn_id, builder.func);
                    let call = builder.ins().call(spawn_ref, &[fn_ptr, arg_val]);
                    Ok(builder.func.dfg.first_result(call))
                } else {
                    Ok(builder.ins().iconst(types::I64, 0))
                }
            } else {
                // Nested spawn/await or other expression
                Ok(builder.ins().iconst(types::I64, 0))
            }
        }

        AstNode::Await { expr } => {
            // await task → forge_await(task_handle)
            // The inner expression could be an identifier (task variable) or a spawn expression
            let task_val = compile_expr(
                builder, variables, runtime_funcs, declared_funcs, string_funcs,
                module, expr, func_signatures, lambda_funcs, global_data_ids,
            )?;

            if let Some(&await_id) = runtime_funcs.get("forge_await") {
                let await_ref = module.declare_func_in_func(await_id, builder.func);
                let call = builder.ins().call(await_ref, &[task_val]);
                Ok(builder.func.dfg.first_result(call))
            } else {
                Ok(task_val) // Fallback: just return the value
            }
        }

        _ => Err(CompileError::UnsupportedFeature(format!("{:?}", node))),
    }
}
