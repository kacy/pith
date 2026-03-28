//! Cranelift code generation for Forge
//!
//! Pipeline: Forge IR text → ir_consumer → Cranelift → native object code

use cranelift::prelude::*;
use cranelift_module::{FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;

pub mod ir_consumer;
pub mod linker;

// --- Struct layout registry (used by ir_consumer for field access) ---

static STRUCT_LAYOUTS: OnceLock<Mutex<HashMap<String, Vec<(String, usize)>>>> = OnceLock::new();

pub fn register_struct_layout(name: &str, fields: &[(String, String)]) {
    let layouts = STRUCT_LAYOUTS.get_or_init(|| Mutex::new(HashMap::new()));
    let field_info: Vec<_> = fields.iter().enumerate()
        .map(|(i, (fname, _))| {
            let clean = fname.strip_suffix(" pub").unwrap_or(fname).to_string();
            (clean, i * 8)
        })
        .collect();
    if let Ok(mut map) = layouts.lock() {
        map.insert(name.to_string(), field_info);
    }
}

pub fn get_struct_layout(name: &str) -> Option<Vec<(String, usize)>> {
    STRUCT_LAYOUTS.get()
        .and_then(|m| m.lock().ok())
        .and_then(|map| map.get(name).cloned())
}

pub fn register_struct_alias(alias: &str, target: &str) {
    let layouts = STRUCT_LAYOUTS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(map) = layouts.lock() {
        if let Some(layout) = map.get(target).cloned() {
            drop(map);
            if let Ok(mut map) = layouts.lock() {
                map.insert(alias.to_string(), layout);
            }
        }
    }
}

// --- CodeGen + errors ---

pub struct CodeGen {
    pub module: ObjectModule,
}

#[derive(Debug)]
pub enum CompileError {
    ModuleError(String),
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::ModuleError(e) => write!(f, "Module error: {}", e),
        }
    }
}

impl std::error::Error for CompileError {}

pub fn create_codegen() -> Result<CodeGen, CompileError> {
    let isa_builder = cranelift_native::builder()
        .map_err(|e| CompileError::ModuleError(format!("Unsupported target: {:?}", e)))?;
    let isa = isa_builder
        .finish(settings::Flags::new(settings::builder()))
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;
    let builder = ObjectBuilder::new(isa, "forge_module", cranelift_module::default_libcall_names())
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;
    Ok(CodeGen { module: ObjectModule::new(builder) })
}

// --- Runtime function declarations ---
fn declare_runtime_function(
    module: &mut ObjectModule,
    name: &str,
    params: &[Type],
    returns: &[Type],
) -> Result<FuncId, CompileError> {
    let mut sig = module.make_signature();

    for param in params {
        sig.params.push(AbiParam::new(*param));
    }

    for ret in returns {
        sig.returns.push(AbiParam::new(*ret));
    }

    let func_id = module
        .declare_function(name, Linkage::Import, &sig)
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;

    Ok(func_id)
}

/// Declare all runtime functions
pub fn declare_runtime_functions(
    module: &mut ObjectModule,
) -> Result<HashMap<String, FuncId>, CompileError> {
    let mut funcs = HashMap::new();

    // String functions
    let string_new = declare_runtime_function(
        module,
        "forge_string_new",
        &[types::I64, types::I64], // data_ptr, len
        &[types::I64],             // returns ForgeString struct (simplified as I64 for now)
    )?;
    funcs.insert("forge_string_new".to_string(), string_new);

    let string_concat = declare_runtime_function(
        module,
        "forge_string_concat",
        &[types::I64, types::I64], // a, b (both ForgeString)
        &[types::I64],
    )?;
    funcs.insert("forge_string_concat".to_string(), string_concat);

    let string_release = declare_runtime_function(
        module,
        "forge_string_release",
        &[types::I64], // ForgeString
        &[],
    )?;
    funcs.insert("forge_string_release".to_string(), string_release);

    let string_from_cstr = declare_runtime_function(
        module,
        "forge_string_from_cstr_ptr",
        &[types::I64, types::I64], // cstr pointer, out pointer
        &[],
    )?;
    funcs.insert("forge_string_from_cstr".to_string(), string_from_cstr);

    // Print function
    let print = declare_runtime_function(
        module,
        "forge_print",
        &[types::I64], // String
        &[],
    )?;
    funcs.insert("forge_print".to_string(), print);

    // Int to string conversion
    let int_to_string = declare_runtime_function(
        module,
        "forge_int_to_string",
        &[types::I64], // Int
        &[types::I64], // Returns ForgeString
    )?;
    funcs.insert("forge_int_to_string".to_string(), int_to_string);

    // Int to C string conversion (for method calls)
    let int_to_cstr = declare_runtime_function(
        module,
        "forge_int_to_cstr",
        &[types::I64], // Int
        &[types::I64], // Returns *mut i8
    )?;
    funcs.insert("forge_int_to_cstr".to_string(), int_to_cstr);
    funcs.insert("to_string".to_string(), int_to_cstr); // default to_string maps to int conversion

    // Float to C string
    let float_to_cstr = declare_runtime_function(
        module,
        "forge_float_to_cstr",
        &[types::F64], // f64
        &[types::I64], // returns *mut i8
    )?;
    funcs.insert("forge_float_to_cstr".to_string(), float_to_cstr);

    // Bool to C string
    let bool_to_cstr = declare_runtime_function(
        module,
        "forge_bool_to_cstr",
        &[types::I64], // i64 (bool as int)
        &[types::I64], // returns *mut i8
    )?;
    funcs.insert("forge_bool_to_cstr".to_string(), bool_to_cstr);

    // ord/chr functions (C string versions)
    let ord = declare_runtime_function(
        module,
        "forge_ord_cstr",
        &[types::I64], // *const i8
        &[types::I64], // Returns i64
    )?;
    funcs.insert("ord".to_string(), ord);

    let chr = declare_runtime_function(
        module,
        "forge_chr_cstr",
        &[types::I64], // i64
        &[types::I64], // Returns *mut i8
    )?;
    funcs.insert("chr".to_string(), chr);

    // Print int function (for debugging)
    let print_int = declare_runtime_function(
        module,
        "forge_print_int",
        &[types::I64], // Int
        &[],
    )?;
    funcs.insert("forge_print_int".to_string(), print_int);

    // Print C string function (for string literals)
    let print_cstr = declare_runtime_function(
        module,
        "forge_print_cstr",
        &[types::I64], // *const i8
        &[],
    )?;
    funcs.insert("forge_print_cstr".to_string(), print_cstr);

    // Closure environment functions for capturing lambdas
    let closure_set_env = declare_runtime_function(
        module,
        "forge_closure_set_env",
        &[types::I64, types::I64], // slot, value
        &[],
    )?;
    funcs.insert("forge_closure_set_env".to_string(), closure_set_env);

    let closure_get_env = declare_runtime_function(
        module,
        "forge_closure_get_env",
        &[types::I64], // slot
        &[types::I64], // value
    )?;
    funcs.insert("forge_closure_get_env".to_string(), closure_get_env);

    // Print error function (to stderr)
    let print_err = declare_runtime_function(
        module,
        "forge_print_err",
        &[types::I64], // *const i8
        &[],
    )?;
    funcs.insert("print_err".to_string(), print_err);

    // String concatenation (pointer-based)
    let concat_cstr = declare_runtime_function(
        module,
        "forge_concat_cstr",
        &[types::I64, types::I64], // *const i8, *const i8
        &[types::I64],             // returns *mut i8
    )?;
    funcs.insert("forge_concat_cstr".to_string(), concat_cstr);

    // Bitwise operations
    let bit_and = declare_runtime_function(
        module,
        "forge_bit_and",
        &[types::I64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("bit_and".to_string(), bit_and);

    let bit_or = declare_runtime_function(
        module,
        "forge_bit_or",
        &[types::I64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("bit_or".to_string(), bit_or);

    let bit_xor = declare_runtime_function(
        module,
        "forge_bit_xor",
        &[types::I64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("bit_xor".to_string(), bit_xor);

    let bit_shl = declare_runtime_function(
        module,
        "forge_bit_shl",
        &[types::I64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("bit_shl".to_string(), bit_shl);

    let bit_shr = declare_runtime_function(
        module,
        "forge_bit_shr",
        &[types::I64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("bit_shr".to_string(), bit_shr);

    let bit_not = declare_runtime_function(module, "forge_bit_not", &[types::I64], &[types::I64])?;
    funcs.insert("bit_not".to_string(), bit_not);

    // Math functions
    let abs = declare_runtime_function(module, "forge_abs", &[types::I64], &[types::I64])?;
    funcs.insert("abs".to_string(), abs);

    let min = declare_runtime_function(
        module,
        "forge_min",
        &[types::I64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("min".to_string(), min);

    let max = declare_runtime_function(
        module,
        "forge_max",
        &[types::I64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("max".to_string(), max);

    let clamp = declare_runtime_function(
        module,
        "forge_clamp",
        &[types::I64, types::I64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("clamp".to_string(), clamp);

    // Float math functions
    let pow = declare_runtime_function(
        module,
        "forge_pow",
        &[types::F64, types::F64],
        &[types::F64],
    )?;
    funcs.insert("pow".to_string(), pow);

    let sqrt = declare_runtime_function(module, "forge_sqrt", &[types::F64], &[types::F64])?;
    funcs.insert("sqrt".to_string(), sqrt);

    let floor = declare_runtime_function(module, "forge_floor", &[types::F64], &[types::F64])?;
    funcs.insert("floor".to_string(), floor);

    let ceil = declare_runtime_function(module, "forge_ceil", &[types::F64], &[types::F64])?;
    funcs.insert("ceil".to_string(), ceil);

    let round = declare_runtime_function(module, "forge_round", &[types::F64], &[types::F64])?;
    funcs.insert("round".to_string(), round);

    // Test assertion functions
    let assert_fn = declare_runtime_function(module, "forge_assert", &[types::I64], &[])?;
    funcs.insert("assert".to_string(), assert_fn);

    let assert_eq =
        declare_runtime_function(module, "forge_assert_eq", &[types::I64, types::I64], &[])?;
    funcs.insert("assert_eq".to_string(), assert_eq);

    let assert_ne =
        declare_runtime_function(module, "forge_assert_ne", &[types::I64, types::I64], &[])?;
    funcs.insert("assert_ne".to_string(), assert_ne);

    // List functions
    let list_new = declare_runtime_function(
        module,
        "forge_list_new",
        &[types::I64, types::I32], // elem_size, type_tag
        &[types::I64],             // returns ForgeList
    )?;
    funcs.insert("forge_list_new".to_string(), list_new);

    let list_new_default = declare_runtime_function(
        module,
        "forge_list_new_default",
        &[],
        &[types::I64],
    )?;
    funcs.insert("forge_list_new_default".to_string(), list_new_default);

    let map_new_default = declare_runtime_function(module, "forge_map_new_default", &[], &[types::I64])?;
    funcs.insert("forge_map_new_default".to_string(), map_new_default);

    let map_new_int = declare_runtime_function(module, "forge_map_new_int", &[], &[types::I64])?;
    funcs.insert("forge_map_new_int".to_string(), map_new_int);

    let set_new_default = declare_runtime_function(module, "forge_set_new_default", &[], &[types::I64])?;
    funcs.insert("forge_set_new_default".to_string(), set_new_default);

    let list_push = declare_runtime_function(
        module,
        "forge_list_push",
        &[types::I64, types::I64, types::I64], // *mut ForgeList (list addr), *mut elem, elem_size
        &[],
    )?;
    funcs.insert("forge_list_push".to_string(), list_push);

    let list_push_value = declare_runtime_function(
        module,
        "forge_list_push_value",
        &[types::I64, types::I64], // list handle, value/pointer-sized element
        &[],
    )?;
    funcs.insert("forge_list_push_value".to_string(), list_push_value);

    let list_set_value = declare_runtime_function(
        module,
        "forge_list_set_value",
        &[types::I64, types::I64, types::I64], // list, index, value
        &[],
    )?;
    funcs.insert("forge_list_set_value".to_string(), list_set_value);

    let list_join = declare_runtime_function(
        module,
        "forge_list_join",
        &[types::I64, types::I64], // list handle, separator cstr
        &[types::I64],
    )?;
    funcs.insert("forge_list_join".to_string(), list_join);

    let list_get_value = declare_runtime_function(
        module,
        "forge_list_get_value",
        &[types::I64, types::I64], // list handle, index
        &[types::I64],
    )?;
    funcs.insert("forge_list_get_value".to_string(), list_get_value);

    let list_len = declare_runtime_function(
        module,
        "forge_list_len",
        &[types::I64], // list
        &[types::I64], // returns length
    )?;
    funcs.insert("forge_list_len".to_string(), list_len);

    // Map functions
    let map_new = declare_runtime_function(
        module,
        "forge_map_new",
        &[types::I32, types::I64, types::I64], // key_type, val_size, val_is_heap
        &[types::I64],                         // returns ForgeMap
    )?;
    funcs.insert("forge_map_new".to_string(), map_new);

    let map_len = declare_runtime_function(
        module,
        "forge_map_len",
        &[types::I64], // map
        &[types::I64], // returns length
    )?;
    funcs.insert("forge_map_len".to_string(), map_len);

    let map_insert_int = declare_runtime_function(
        module,
        "forge_map_insert_int",
        &[types::I64, types::I64, types::I64, types::I64], // *mut map, key, *val, val_size
        &[],
    )?;
    funcs.insert("forge_map_insert_int".to_string(), map_insert_int);

    // String methods (using pointer-based ABI wrappers)
    let string_len = declare_runtime_function(
        module,
        "forge_string_len_ptr",
        &[types::I64], // *const ForgeString
        &[types::I64], // returns length
    )?;
    funcs.insert("forge_string_len".to_string(), string_len);

    let string_contains = declare_runtime_function(
        module,
        "forge_string_contains_ptr",
        &[types::I64, types::I64], // *const haystack, *const needle
        &[types::I64],             // returns bool (i64 for uniform ABI)
    )?;
    funcs.insert("forge_string_contains".to_string(), string_contains);

    let string_substring = declare_runtime_function(
        module,
        "forge_string_substring_ptr",
        &[types::I64, types::I64, types::I64, types::I64], // *const s, start, end, *mut out
        &[],
    )?;
    funcs.insert("forge_string_substring".to_string(), string_substring);

    let string_trim = declare_runtime_function(
        module,
        "forge_string_trim_ptr",
        &[types::I64, types::I64], // *const s, *mut out
        &[],
    )?;
    funcs.insert("forge_string_trim".to_string(), string_trim);

    let string_starts_with = declare_runtime_function(
        module,
        "forge_string_starts_with_ptr",
        &[types::I64, types::I64], // *const s, *const prefix
        &[types::I64],             // returns bool (i64 for uniform ABI)
    )?;
    funcs.insert("forge_string_starts_with".to_string(), string_starts_with);

    let string_ends_with = declare_runtime_function(
        module,
        "forge_string_ends_with_ptr",
        &[types::I64, types::I64], // *const s, *const suffix
        &[types::I64],             // returns bool (i64 for uniform ABI)
    )?;
    funcs.insert("forge_string_ends_with".to_string(), string_ends_with);

    let string_concat = declare_runtime_function(
        module,
        "forge_string_concat_ptr",
        &[types::I64, types::I64, types::I64], // *const a, *const b, *mut out
        &[],
    )?;
    funcs.insert("forge_string_concat".to_string(), string_concat);

    // C-string equality comparison (content-based, for null-terminated C strings)
    let cstring_eq = declare_runtime_function(
        module,
        "forge_cstring_eq",
        &[types::I64, types::I64], // *const a, *const b (null-terminated C strings)
        &[types::I64],             // returns bool (i64 for uniform ABI)
    )?;
    funcs.insert("forge_cstring_eq".to_string(), cstring_eq);

    // C-string lexicographic comparison (like strcmp)
    let cstring_cmp = declare_runtime_function(
        module,
        "forge_cstring_cmp",
        &[types::I64, types::I64], // *const a, *const b
        &[types::I64],             // returns <0, 0, or >0
    )?;
    funcs.insert("forge_cstring_cmp".to_string(), cstring_cmp);

    // Simple strlen-based length for null-terminated strings (debugging/workaround)
    let cstring_len = declare_runtime_function(
        module,
        "forge_cstring_len",
        &[types::I64], // *const cstr
        &[types::I64], // returns length
    )?;
    funcs.insert("forge_cstring_len".to_string(), cstring_len);

    let cstring_is_empty = declare_runtime_function(module, "forge_cstring_is_empty", &[types::I64], &[types::I64])?;
    funcs.insert("forge_cstring_is_empty".to_string(), cstring_is_empty);

    // Filesystem functions
    let file_exists = declare_runtime_function(
        module,
        "forge_file_exists",
        &[types::I64], // *const path
        &[types::I64], // returns bool (0/1)
    )?;
    funcs.insert("file_exists".to_string(), file_exists);

    let dir_exists = declare_runtime_function(
        module,
        "forge_dir_exists",
        &[types::I64], // *const path
        &[types::I64], // returns bool (0/1)
    )?;
    funcs.insert("dir_exists".to_string(), dir_exists);

    let mkdir = declare_runtime_function(
        module,
        "forge_mkdir",
        &[types::I64], // *const path
        &[types::I64], // returns bool (0/1)
    )?;
    funcs.insert("mkdir".to_string(), mkdir);

    let remove_file = declare_runtime_function(
        module,
        "forge_remove_file",
        &[types::I64], // *const path
        &[types::I64], // returns bool (0/1)
    )?;
    funcs.insert("remove_file".to_string(), remove_file);

    let rename_file = declare_runtime_function(
        module,
        "forge_rename_file",
        &[types::I64, types::I64], // *const from, *const to
        &[types::I64],             // returns bool (i64 for uniform ABI) (0/1)
    )?;
    funcs.insert("rename_file".to_string(), rename_file);

    let list_dir = declare_runtime_function(
        module,
        "forge_list_dir",
        &[types::I64], // *const path
        &[types::I64], // returns opaque pointer for now
    )?;
    funcs.insert("list_dir".to_string(), list_dir);

    // File I/O functions
    let read_file = declare_runtime_function(
        module,
        "forge_read_file",
        &[types::I64], // *const path
        &[types::I64], // returns *mut cstr (null on error)
    )?;
    funcs.insert("read_file".to_string(), read_file);

    let write_file = declare_runtime_function(
        module,
        "forge_write_file",
        &[types::I64, types::I64], // *const path, *const content
        &[types::I64],             // returns bool (i64 for uniform ABI) (0/1)
    )?;
    funcs.insert("write_file".to_string(), write_file);

    let append_file = declare_runtime_function(
        module,
        "forge_append_file",
        &[types::I64, types::I64], // *const path, *const content
        &[types::I64],             // returns bool (i64 for uniform ABI) (0/1)
    )?;
    funcs.insert("append_file".to_string(), append_file);

    // Process/environment functions
    let exit = declare_runtime_function(
        module,
        "forge_exit",
        &[types::I64], // exit code
        &[],           // no return
    )?;
    funcs.insert("exit".to_string(), exit);

    let sleep = declare_runtime_function(
        module,
        "forge_sleep",
        &[types::I64], // milliseconds
        &[],           // no return
    )?;
    funcs.insert("sleep".to_string(), sleep);

    let time = declare_runtime_function(
        module,
        "forge_time",
        &[],           // no args
        &[types::I64], // returns timestamp
    )?;
    funcs.insert("time".to_string(), time);

    let env = declare_runtime_function(
        module,
        "forge_env",
        &[types::I64], // *const name
        &[types::I64], // returns *const cstr (null if not found)
    )?;
    funcs.insert("env".to_string(), env);

    let input = declare_runtime_function(
        module,
        "forge_input",
        &[],           // no args
        &[types::I64], // returns *mut cstr
    )?;
    funcs.insert("input".to_string(), input);

    // Command execution
    let exec = declare_runtime_function(
        module,
        "forge_exec",
        &[types::I64], // *const command
        &[types::I64], // returns exit code
    )?;
    funcs.insert("exec".to_string(), exec);

    // Random functions
    let random_float = declare_runtime_function(
        module,
        "forge_random_float",
        &[],           // no args
        &[types::F64], // returns float
    )?;
    funcs.insert("random_float".to_string(), random_float);

    let random_seed = declare_runtime_function(
        module,
        "forge_random_seed",
        &[types::I64], // seed
        &[],           // no return
    )?;
    funcs.insert("random_seed".to_string(), random_seed);

    let random_int = declare_runtime_function(
        module,
        "forge_random_int",
        &[types::I64, types::I64], // min, max
        &[types::I64],             // returns int
    )?;
    funcs.insert("random_int".to_string(), random_int);

    // Math functions (aliases to the ones already defined)
    funcs.insert("math_sqrt".to_string(), sqrt);
    funcs.insert("math_floor".to_string(), floor);
    funcs.insert("math_ceil".to_string(), ceil);
    funcs.insert("math_round".to_string(), round);
    funcs.insert("math_pow".to_string(), pow);

    // String/utility functions
    let fmt_float = declare_runtime_function(
        module,
        "forge_fmt_float",
        &[types::F64, types::I64], // n, precision
        &[types::I64],             // returns *mut cstr
    )?;
    funcs.insert("fmt_float".to_string(), fmt_float);

    let random_string = declare_runtime_function(
        module,
        "forge_random_string",
        &[types::I64], // length
        &[types::I64], // returns *mut cstr
    )?;
    funcs.insert("random_string".to_string(), random_string);

    // Command line arguments
    let args = declare_runtime_function(
        module,
        "forge_args",
        &[],           // no args
        &[types::I64], // returns *mut StringNode (linked list head)
    )?;
    funcs.insert("args".to_string(), args);

    // String utility functions (standalone versions for free function calls)
    // Note: These are simplified versions that work with C strings for now
    let substring = declare_runtime_function(
        module,
        "forge_cstring_substring", // We'll need to implement this
        &[types::I64, types::I64, types::I64], // str, start, end
        &[types::I64],             // returns *mut cstr
    )?;
    funcs.insert("substring".to_string(), substring);
    funcs.insert("forge_cstring_substring".to_string(), substring);

    let contains = declare_runtime_function(
        module,
        "forge_cstring_contains",
        &[types::I64, types::I64], // haystack, needle
        &[types::I64],             // returns 0 or 1
    )?;
    funcs.insert("contains".to_string(), contains);
    funcs.insert("forge_cstring_contains".to_string(), contains);

    let split = declare_runtime_function(
        module,
        "forge_string_split_to_list",
        &[types::I64, types::I64], // str, delimiter
        &[types::I64],             // returns ForgeList (pointer)
    )?;
    funcs.insert("split".to_string(), split);

    let trim = declare_runtime_function(
        module,
        "forge_cstring_trim", // We'll need to implement this
        &[types::I64],        // str
        &[types::I64],        // returns *mut cstr
    )?;
    funcs.insert("trim".to_string(), trim);
    funcs.insert("forge_cstring_trim".to_string(), trim);

    let trim_left = declare_runtime_function(
        module,
        "forge_cstring_trim_left", // We'll need to implement this
        &[types::I64],             // str
        &[types::I64],             // returns *mut cstr
    )?;
    funcs.insert("trim_left".to_string(), trim_left);

    let cstring_char_at = declare_runtime_function(
        module,
        "forge_cstring_char_at",
        &[types::I64, types::I64], // str, index
        &[types::I64],             // returns *mut cstr (single char)
    )?;
    funcs.insert("forge_cstring_char_at".to_string(), cstring_char_at);

    // Concurrency primitive functions
    // Mutex
    let mutex_new = declare_runtime_function(
        module,
        "forge_mutex_new",
        &[],           // no args
        &[types::I64], // returns mutex handle
    )?;
    funcs.insert("Mutex".to_string(), mutex_new);
    funcs.insert("forge_mutex_new".to_string(), mutex_new);

    let mutex_lock = declare_runtime_function(
        module,
        "forge_mutex_lock",
        &[types::I64], // mutex handle
        &[],           // no return
    )?;
    funcs.insert("lock".to_string(), mutex_lock);
    funcs.insert("forge_mutex_lock".to_string(), mutex_lock);

    let mutex_unlock = declare_runtime_function(
        module,
        "forge_mutex_unlock",
        &[types::I64], // mutex handle
        &[],           // no return
    )?;
    funcs.insert("unlock".to_string(), mutex_unlock);
    funcs.insert("forge_mutex_unlock".to_string(), mutex_unlock);

    // WaitGroup
    let wg_new = declare_runtime_function(
        module,
        "forge_waitgroup_new",
        &[],           // no args
        &[types::I64], // returns waitgroup handle
    )?;
    funcs.insert("WaitGroup".to_string(), wg_new);
    funcs.insert("forge_waitgroup_new".to_string(), wg_new);

    let wg_add = declare_runtime_function(
        module,
        "forge_waitgroup_add",
        &[types::I64, types::I64], // handle, count
        &[],                       // no return
    )?;
    funcs.insert("add".to_string(), wg_add);
    funcs.insert("forge_waitgroup_add".to_string(), wg_add);

    let wg_done = declare_runtime_function(
        module,
        "forge_waitgroup_done",
        &[types::I64], // handle
        &[],           // no return
    )?;
    funcs.insert("done".to_string(), wg_done);
    funcs.insert("forge_waitgroup_done".to_string(), wg_done);

    let wg_wait = declare_runtime_function(
        module,
        "forge_waitgroup_wait",
        &[types::I64], // handle
        &[],           // no return
    )?;
    funcs.insert("wait".to_string(), wg_wait);
    funcs.insert("forge_waitgroup_wait".to_string(), wg_wait);

    // Semaphore
    let sem_new = declare_runtime_function(
        module,
        "forge_semaphore_new",
        &[types::I64], // initial count
        &[types::I64], // returns semaphore handle
    )?;
    funcs.insert("Semaphore".to_string(), sem_new);
    funcs.insert("forge_semaphore_new".to_string(), sem_new);

    let sem_acquire = declare_runtime_function(
        module,
        "forge_semaphore_acquire",
        &[types::I64], // handle
        &[],           // no return
    )?;
    funcs.insert("acquire".to_string(), sem_acquire);
    funcs.insert("forge_semaphore_acquire".to_string(), sem_acquire);

    let sem_release = declare_runtime_function(
        module,
        "forge_semaphore_release",
        &[types::I64], // handle
        &[],           // no return
    )?;
    funcs.insert("release".to_string(), sem_release);
    funcs.insert("forge_semaphore_release".to_string(), sem_release);

    // Additional string utility functions
    let to_upper = declare_runtime_function(
        module,
        "forge_cstring_to_upper",
        &[types::I64], // *const cstr
        &[types::I64], // returns *mut cstr
    )?;
    funcs.insert("to_upper".to_string(), to_upper);
    funcs.insert("forge_cstring_to_upper".to_string(), to_upper);

    let to_lower = declare_runtime_function(
        module,
        "forge_cstring_to_lower",
        &[types::I64], // *const cstr
        &[types::I64], // returns *mut cstr
    )?;
    funcs.insert("to_lower".to_string(), to_lower);
    funcs.insert("forge_cstring_to_lower".to_string(), to_lower);

    let reverse = declare_runtime_function(
        module,
        "forge_cstring_reverse",
        &[types::I64], // *const cstr
        &[types::I64], // returns *mut cstr
    )?;
    funcs.insert("reverse".to_string(), reverse);
    funcs.insert("forge_cstring_reverse".to_string(), reverse);

    // String: replace
    let replace = declare_runtime_function(
        module,
        "forge_cstring_replace",
        &[types::I64, types::I64, types::I64], // str, from, to
        &[types::I64],
    )?;
    funcs.insert("replace".to_string(), replace);
    funcs.insert("forge_cstring_replace".to_string(), replace);

    // String: is_empty
    let is_empty_str = declare_runtime_function(
        module,
        "forge_cstring_is_empty",
        &[types::I64], // str
        &[types::I64], // returns 0 or 1
    )?;
    funcs.insert("is_empty".to_string(), is_empty_str);
    funcs.insert("forge_cstring_is_empty".to_string(), is_empty_str);

    // List: is_empty
    let list_is_empty = declare_runtime_function(
        module,
        "forge_list_is_empty",
        &[types::I64], // list ptr
        &[types::I64], // returns 0 or 1
    )?;
    funcs.insert("forge_list_is_empty".to_string(), list_is_empty);

    // List: clear (by-value variant, works with internal pointer)
    let list_clear = declare_runtime_function(
        module,
        "forge_list_clear_value",
        &[types::I64], // ForgeList (passed as i64 pointer value)
        &[],
    )?;
    funcs.insert("clear".to_string(), list_clear);
    funcs.insert("forge_list_clear".to_string(), list_clear);

    // List: remove (by-value variant, works with internal pointer)
    let list_remove = declare_runtime_function(
        module,
        "forge_list_remove_value",
        &[types::I64, types::I64], // ForgeList, index
        &[types::I64],
    )?;
    funcs.insert("remove".to_string(), list_remove);
    funcs.insert("forge_list_remove".to_string(), list_remove);

    // List: reverse (by-value variant, works with internal pointer)
    let list_reverse = declare_runtime_function(
        module,
        "forge_list_reverse_value",
        &[types::I64], // ForgeList
        &[],
    )?;
    funcs.insert("forge_list_reverse".to_string(), list_reverse);

    // List: contains (integer)
    let list_contains_int = declare_runtime_function(
        module,
        "forge_list_contains_int",
        &[types::I64, types::I64], // list ptr, value
        &[types::I64],             // returns 0 or 1
    )?;
    funcs.insert("forge_list_contains_int".to_string(), list_contains_int);

    // List: index_of (integer)
    let list_index_of_int = declare_runtime_function(
        module,
        "forge_list_index_of_int",
        &[types::I64, types::I64], // list ptr, value
        &[types::I64],             // returns index or -1
    )?;
    funcs.insert("forge_list_index_of_int".to_string(), list_index_of_int);

    // List: sort
    let list_sort = declare_runtime_function(
        module,
        "forge_list_sort",
        &[types::I64], // list.ptr as i64
        &[],
    )?;
    funcs.insert("forge_list_sort".to_string(), list_sort);

    // List: sort_strings (sorts C-string-pointer lists lexicographically)
    let list_sort_strings = declare_runtime_function(
        module,
        "forge_list_sort_strings",
        &[types::I64], // list.ptr as i64
        &[],
    )?;
    funcs.insert("forge_list_sort_strings".to_string(), list_sort_strings);

    // List: slice
    let list_slice = declare_runtime_function(
        module,
        "forge_list_slice",
        &[types::I64, types::I64, types::I64], // list.ptr, start, end
        &[types::I64],                         // returns new list.ptr
    )?;
    funcs.insert("forge_list_slice".to_string(), list_slice);

    // Type conversions
    let int_to_float = declare_runtime_function(
        module,
        "forge_int_to_float",
        &[types::I64], // int
        &[types::F64], // float
    )?;
    funcs.insert("to_float".to_string(), int_to_float);
    funcs.insert("forge_int_to_float".to_string(), int_to_float);

    let float_to_int = declare_runtime_function(
        module,
        "forge_float_to_int",
        &[types::F64], // float
        &[types::I64], // int
    )?;
    funcs.insert("to_int".to_string(), float_to_int);
    funcs.insert("forge_float_to_int".to_string(), float_to_int);

    let parse_int = declare_runtime_function(
        module,
        "forge_parse_int",
        &[types::I64], // str
        &[types::I64], // int
    )?;
    funcs.insert("parse_int".to_string(), parse_int);
    funcs.insert("forge_parse_int".to_string(), parse_int);

    let parse_float = declare_runtime_function(
        module,
        "forge_parse_float",
        &[types::I64], // str
        &[types::F64], // float
    )?;
    funcs.insert("parse_float".to_string(), parse_float);
    funcs.insert("forge_parse_float".to_string(), parse_float);

    // Encoding: b64_encode, hex
    let b64_encode = declare_runtime_function(
        module,
        "forge_b64_encode",
        &[types::I64], // str
        &[types::I64], // encoded str
    )?;
    funcs.insert("b64_encode".to_string(), b64_encode);
    funcs.insert("encode".to_string(), b64_encode);
    funcs.insert("forge_b64_encode".to_string(), b64_encode);

    let hex_encode = declare_runtime_function(
        module,
        "forge_hex_encode",
        &[types::I64], // str (byte string)
        &[types::I64], // hex str
    )?;
    funcs.insert("forge_hex_encode".to_string(), hex_encode);

    // hex(int) → hex string (e.g., 255 → "ff")
    let int_to_hex = declare_runtime_function(
        module,
        "forge_int_to_hex",
        &[types::I64], // integer
        &[types::I64], // hex str
    )?;
    funcs.insert("hex".to_string(), int_to_hex);
    funcs.insert("forge_int_to_hex".to_string(), int_to_hex);

    // oct(int) → octal string
    let int_to_oct2 =
        declare_runtime_function(module, "forge_int_to_oct", &[types::I64], &[types::I64])?;
    funcs.insert("oct".to_string(), int_to_oct2);
    funcs.insert("forge_int_to_oct2".to_string(), int_to_oct2);

    // bin(int) → binary string
    let int_to_bin2 =
        declare_runtime_function(module, "forge_int_to_bin", &[types::I64], &[types::I64])?;
    funcs.insert("bin".to_string(), int_to_bin2);
    funcs.insert("forge_int_to_bin2".to_string(), int_to_bin2);

    // Hashing: sha256
    let sha256 = declare_runtime_function(
        module,
        "forge_sha256",
        &[types::I64], // str
        &[types::I64], // hex hash str
    )?;
    funcs.insert("sha256".to_string(), sha256);
    funcs.insert("forge_sha256".to_string(), sha256);

    // Time: format_time (accepts timestamp + format string)
    let format_time = declare_runtime_function(
        module,
        "forge_format_time_fmt",
        &[types::I64, types::I64], // timestamp (i64), format str
        &[types::I64],             // formatted str
    )?;
    funcs.insert("format_time".to_string(), format_time);
    funcs.insert("forge_format_time".to_string(), format_time);
    funcs.insert("forge_format_time_fmt".to_string(), format_time);

    // FS: write
    let fs_write = declare_runtime_function(
        module,
        "forge_fs_write",
        &[types::I64, types::I64], // path, content
        &[types::I64],             // success flag
    )?;
    funcs.insert("write".to_string(), fs_write);
    funcs.insert("forge_fs_write".to_string(), fs_write);

    // Logging
    let log_info = declare_runtime_function(
        module,
        "forge_log_info",
        &[types::I64], // msg
        &[],
    )?;
    funcs.insert("info".to_string(), log_info);
    funcs.insert("forge_log_info".to_string(), log_info);

    let log_warn = declare_runtime_function(
        module,
        "forge_log_warn",
        &[types::I64], // msg
        &[],
    )?;
    funcs.insert("warn".to_string(), log_warn);
    funcs.insert("forge_log_warn".to_string(), log_warn);

    let log_error = declare_runtime_function(
        module,
        "forge_log_error",
        &[types::I64], // msg
        &[],
    )?;
    funcs.insert("forge_log_error".to_string(), log_error);

    // Path operations
    let path_dir = declare_runtime_function(
        module,
        "forge_path_dir",
        &[types::I64], // path
        &[types::I64], // dir str
    )?;
    funcs.insert("dir".to_string(), path_dir);
    funcs.insert("forge_path_dir".to_string(), path_dir);

    let path_basename = declare_runtime_function(
        module,
        "forge_path_basename",
        &[types::I64], // path
        &[types::I64], // basename str
    )?;
    funcs.insert("basename".to_string(), path_basename);
    funcs.insert("forge_path_basename".to_string(), path_basename);

    // JSON/TOML/URL stubs
    let json_parse = declare_runtime_function(
        module,
        "forge_json_parse",
        &[types::I64], // str
        &[types::I64], // parsed (ptr)
    )?;
    funcs.insert("parse".to_string(), json_parse);
    funcs.insert("json_parse".to_string(), json_parse);
    funcs.insert("forge_json_parse".to_string(), json_parse);

    // JSON accessor functions
    let json_type_of = declare_runtime_function(module, "forge_json_type_of", &[types::I64], &[types::I64])?;
    funcs.insert("type_of".to_string(), json_type_of);

    let json_get_string = declare_runtime_function(module, "forge_json_get_string", &[types::I64], &[types::I64])?;
    funcs.insert("get_string".to_string(), json_get_string);

    let json_get_int = declare_runtime_function(module, "forge_json_get_int", &[types::I64], &[types::I64])?;
    funcs.insert("get_int".to_string(), json_get_int);

    let json_get_float = declare_runtime_function(module, "forge_json_get_float", &[types::I64], &[types::F64])?;
    funcs.insert("get_float".to_string(), json_get_float);

    let json_get_bool = declare_runtime_function(module, "forge_json_get_bool", &[types::I64], &[types::I64])?;
    funcs.insert("get_bool".to_string(), json_get_bool);

    let json_array_len = declare_runtime_function(module, "forge_json_array_len", &[types::I64], &[types::I64])?;
    funcs.insert("array_len".to_string(), json_array_len);

    let json_array_get = declare_runtime_function(module, "forge_json_array_get", &[types::I64, types::I64], &[types::I64])?;
    funcs.insert("array_get".to_string(), json_array_get);

    let json_object_get = declare_runtime_function(module, "forge_json_object_get", &[types::I64, types::I64], &[types::I64])?;
    funcs.insert("object_get".to_string(), json_object_get);

    let json_object_has = declare_runtime_function(module, "forge_json_object_has", &[types::I64, types::I64], &[types::I64])?;
    funcs.insert("object_has".to_string(), json_object_has);

    let json_object_keys = declare_runtime_function(module, "forge_json_object_keys", &[types::I64], &[types::I64])?;
    funcs.insert("object_keys".to_string(), json_object_keys);

    let json_make_object = declare_runtime_function(module, "forge_json_make_object", &[], &[types::I64])?;
    funcs.insert("make_object".to_string(), json_make_object);

    let json_make_array = declare_runtime_function(module, "forge_json_make_array", &[], &[types::I64])?;
    funcs.insert("make_array".to_string(), json_make_array);

    let json_make_int = declare_runtime_function(module, "forge_json_make_int", &[types::I64], &[types::I64])?;
    funcs.insert("make_int".to_string(), json_make_int);

    let json_make_string = declare_runtime_function(module, "forge_json_make_string", &[types::I64], &[types::I64])?;
    funcs.insert("make_string".to_string(), json_make_string);

    let json_array_push = declare_runtime_function(module, "forge_json_array_push", &[types::I64, types::I64], &[])?;
    funcs.insert("array_push".to_string(), json_array_push);

    let json_object_set = declare_runtime_function(module, "forge_json_object_set", &[types::I64, types::I64, types::I64], &[])?;
    funcs.insert("object_set".to_string(), json_object_set);

    let json_encode = declare_runtime_function(module, "forge_json_encode", &[types::I64], &[types::I64])?;
    funcs.insert("forge_json_encode".to_string(), json_encode);

    // TOML functions (2-arg variants: handle + key)
    let toml_parse = declare_runtime_function(module, "forge_toml_parse", &[types::I64], &[types::I64])?;
    funcs.insert("toml_parse".to_string(), toml_parse);

    let toml_type_of = declare_runtime_function(module, "forge_toml_type_of", &[types::I64], &[types::I64])?;
    funcs.insert("toml_type_of".to_string(), toml_type_of);

    let toml_get_string = declare_runtime_function(module, "forge_toml_get_string", &[types::I64, types::I64], &[types::I64])?;
    funcs.insert("toml_get_string".to_string(), toml_get_string);

    let toml_get_int = declare_runtime_function(module, "forge_toml_get_int", &[types::I64, types::I64], &[types::I64])?;
    funcs.insert("toml_get_int".to_string(), toml_get_int);

    let toml_get_float = declare_runtime_function(module, "forge_toml_get_float", &[types::I64, types::I64], &[types::F64])?;
    funcs.insert("toml_get_float".to_string(), toml_get_float);

    let toml_get_bool = declare_runtime_function(module, "forge_toml_get_bool", &[types::I64, types::I64], &[types::I64])?;
    funcs.insert("toml_get_bool".to_string(), toml_get_bool);

    let toml_has = declare_runtime_function(module, "forge_toml_has", &[types::I64, types::I64], &[types::I64])?;
    funcs.insert("has".to_string(), toml_has);
    funcs.insert("toml_has".to_string(), toml_has);

    let toml_get_array = declare_runtime_function(module, "forge_toml_get_array", &[types::I64, types::I64], &[types::I64])?;
    funcs.insert("get_array".to_string(), toml_get_array);
    funcs.insert("toml_get_array".to_string(), toml_get_array);

    let toml_array_len = declare_runtime_function(module, "forge_toml_array_len", &[types::I64], &[types::I64])?;
    funcs.insert("toml_array_len".to_string(), toml_array_len);

    let toml_array_get = declare_runtime_function(module, "forge_toml_array_get", &[types::I64, types::I64], &[types::I64])?;
    funcs.insert("toml_array_get".to_string(), toml_array_get);

    let toml_get_table = declare_runtime_function(module, "forge_toml_get_table", &[types::I64, types::I64], &[types::I64])?;
    funcs.insert("get_table".to_string(), toml_get_table);
    funcs.insert("toml_get_table".to_string(), toml_get_table);

    let toml_keys = declare_runtime_function(module, "forge_toml_keys", &[types::I64], &[types::I64])?;
    funcs.insert("toml_keys".to_string(), toml_keys);

    // Spawn/await for concurrency
    let spawn_func = declare_runtime_function(
        module,
        "forge_spawn",
        &[types::I64, types::I64], // fn_ptr, arg
        &[types::I64],             // returns task handle
    )?;
    funcs.insert("forge_spawn".to_string(), spawn_func);

    let await_func = declare_runtime_function(
        module,
        "forge_await",
        &[types::I64], // task handle
        &[types::I64], // returns result
    )?;
    funcs.insert("forge_await".to_string(), await_func);

    // Smart to_string for Unknown-typed values
    let smart_to_string = declare_runtime_function(module, "forge_smart_to_string", &[types::I64], &[types::I64])?;
    funcs.insert("forge_smart_to_string".to_string(), smart_to_string);

    // Identity function
    let identity = declare_runtime_function(
        module,
        "forge_identity",
        &[types::I64], // x
        &[types::I64], // x
    )?;
    funcs.insert("identity".to_string(), identity);
    funcs.insert("forge_identity".to_string(), identity);

    // Process operations
    let process_spawn = declare_runtime_function(
        module,
        "forge_process_spawn",
        &[types::I64], // cmd
        &[types::I64], // pid
    )?;
    funcs.insert("process_spawn".to_string(), process_spawn);
    funcs.insert("forge_process_spawn".to_string(), process_spawn);

    let exec_output = declare_runtime_function(
        module,
        "forge_exec_output",
        &[types::I64], // cmd
        &[types::I64], // output str
    )?;
    funcs.insert("exec_output".to_string(), exec_output);
    funcs.insert("forge_exec_output".to_string(), exec_output);

    // b64_decode
    let b64_decode =
        declare_runtime_function(module, "forge_b64_decode", &[types::I64], &[types::I64])?;
    funcs.insert("b64_decode".to_string(), b64_decode);
    funcs.insert("decode".to_string(), b64_decode);

    // fnv1a hash
    let fnv1a = declare_runtime_function(module, "forge_fnv1a", &[types::I64], &[types::I64])?;
    funcs.insert("fnv1a".to_string(), fnv1a);
    funcs.insert("forge_fnv1a".to_string(), fnv1a);

    // (oct/bin are declared earlier with forge_int_to_oct / forge_int_to_bin)

    // String: index_of
    let index_of = declare_runtime_function(
        module,
        "forge_cstring_index_of",
        &[types::I64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("index_of".to_string(), index_of);
    funcs.insert("forge_cstring_index_of".to_string(), index_of);

    let starts_with = declare_runtime_function(
        module,
        "forge_cstring_starts_with",
        &[types::I64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("starts_with".to_string(), starts_with);
    funcs.insert("forge_cstring_starts_with".to_string(), starts_with);

    let ends_with = declare_runtime_function(
        module,
        "forge_cstring_ends_with",
        &[types::I64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("ends_with".to_string(), ends_with);
    funcs.insert("forge_cstring_ends_with".to_string(), ends_with);

    // String: pad_left, pad_right, repeat
    let pad_left = declare_runtime_function(
        module,
        "forge_cstring_pad_left",
        &[types::I64, types::I64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("pad_left".to_string(), pad_left);
    funcs.insert("forge_cstring_pad_left".to_string(), pad_left);

    let pad_right = declare_runtime_function(
        module,
        "forge_cstring_pad_right",
        &[types::I64, types::I64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("pad_right".to_string(), pad_right);
    funcs.insert("forge_cstring_pad_right".to_string(), pad_right);

    let repeat = declare_runtime_function(
        module,
        "forge_cstring_repeat",
        &[types::I64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("repeat".to_string(), repeat);
    funcs.insert("forge_cstring_repeat".to_string(), repeat);

    // String: chars
    let chars =
        declare_runtime_function(module, "forge_cstring_chars", &[types::I64], &[types::I64])?;
    funcs.insert("chars".to_string(), chars);
    funcs.insert("forge_cstring_chars".to_string(), chars);

    // List: sort/slice aliases (reuse earlier declarations)
    funcs.insert("sort".to_string(), list_sort);
    funcs.insert("slice".to_string(), list_slice);

    // Path: ext, stem, join
    let path_ext =
        declare_runtime_function(module, "forge_path_ext", &[types::I64], &[types::I64])?;
    funcs.insert("ext".to_string(), path_ext);
    funcs.insert("extension".to_string(), path_ext);
    funcs.insert("forge_path_ext".to_string(), path_ext);

    let path_stem =
        declare_runtime_function(module, "forge_path_stem", &[types::I64], &[types::I64])?;
    funcs.insert("stem".to_string(), path_stem);
    funcs.insert("forge_path_stem".to_string(), path_stem);

    let path_join = declare_runtime_function(
        module,
        "forge_path_join",
        &[types::I64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("join".to_string(), path_join);
    funcs.insert("forge_path_join".to_string(), path_join);

    // Path: base (alias for basename)
    funcs.insert("base".to_string(), path_basename);
    funcs.insert("base_name".to_string(), path_basename);

    // type_of (generic stub - JSON type_of takes priority from earlier declaration)

    // second (generics)
    let second = declare_runtime_function(
        module,
        "forge_second",
        &[types::I64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("second".to_string(), second);

    // exists
    let path_exists =
        declare_runtime_function(module, "forge_path_exists", &[types::I64], &[types::I64])?;
    funcs.insert("exists".to_string(), path_exists);
    funcs.insert("forge_path_exists".to_string(), path_exists);

    // process_read
    let process_read =
        declare_runtime_function(module, "forge_process_read", &[types::I64], &[types::I64])?;
    funcs.insert("process_read".to_string(), process_read);

    // TCP stubs
    let tcp_listen = declare_runtime_function(
        module,
        "forge_tcp_listen",
        &[types::I64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("tcp_listen".to_string(), tcp_listen);

    let tcp_connect = declare_runtime_function(
        module,
        "forge_tcp_connect",
        &[types::I64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("tcp_connect".to_string(), tcp_connect);

    let tcp_accept =
        declare_runtime_function(module, "forge_tcp_accept", &[types::I64], &[types::I64])?;
    funcs.insert("tcp_accept".to_string(), tcp_accept);

    let tcp_read =
        declare_runtime_function(module, "forge_tcp_read", &[types::I64], &[types::I64])?;
    funcs.insert("tcp_read".to_string(), tcp_read);

    let tcp_write = declare_runtime_function(
        module,
        "forge_tcp_write",
        &[types::I64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("tcp_write".to_string(), tcp_write);

    let tcp_close = declare_runtime_function(module, "forge_tcp_close", &[types::I64], &[])?;
    funcs.insert("tcp_close".to_string(), tcp_close);

    // Channel stubs
    let channel_new =
        declare_runtime_function(module, "forge_channel_new", &[types::I64], &[types::I64])?;
    funcs.insert("Channel".to_string(), channel_new);
    funcs.insert("forge_channel_new".to_string(), channel_new);

    let channel_send =
        declare_runtime_function(module, "forge_channel_send", &[types::I64, types::I64], &[])?;
    funcs.insert("send".to_string(), channel_send);
    funcs.insert("forge_channel_send".to_string(), channel_send);

    let channel_recv =
        declare_runtime_function(module, "forge_channel_recv", &[types::I64], &[types::I64])?;
    funcs.insert("recv".to_string(), channel_recv);
    funcs.insert("forge_channel_recv".to_string(), channel_recv);

    // Logging: error/debug aliases (reuse log_error)
    funcs.insert("error".to_string(), log_error);
    funcs.insert("debug".to_string(), log_error); // stub debug as well

    // to_hex (alias for hex_encode)
    funcs.insert("to_hex".to_string(), hex_encode);

    // from_hex (hex decode)
    let from_hex =
        declare_runtime_function(module, "forge_from_hex", &[types::I64], &[types::I64])?;
    funcs.insert("from_hex".to_string(), from_hex);
    funcs.insert("forge_from_hex".to_string(), from_hex);

    // float_fixed (format float with fixed decimal places)
    let float_fixed = declare_runtime_function(
        module,
        "forge_float_fixed",
        &[types::F64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("float_fixed".to_string(), float_fixed);

    // is_dir
    let is_dir = declare_runtime_function(module, "forge_is_dir", &[types::I64], &[types::I64])?;
    funcs.insert("is_dir".to_string(), is_dir);

    // show (generic display — stub, returns arg as-is; reuse identity)
    funcs.insert("show".to_string(), identity);
    funcs.insert("show_and_hash".to_string(), identity);

    // last_index_of
    let last_index_of = declare_runtime_function(
        module,
        "forge_cstring_last_index_of",
        &[types::I64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("last_index_of".to_string(), last_index_of);

    // process_wait
    let process_wait =
        declare_runtime_function(module, "forge_process_wait", &[types::I64], &[types::I64])?;
    funcs.insert("process_wait".to_string(), process_wait);
    funcs.insert("process_kill".to_string(), process_wait);

    // URL components
    let url_scheme =
        declare_runtime_function(module, "forge_url_scheme", &[types::I64], &[types::I64])?;
    funcs.insert("scheme".to_string(), url_scheme);

    let url_host =
        declare_runtime_function(module, "forge_url_host", &[types::I64], &[types::I64])?;
    funcs.insert("host".to_string(), url_host);

    let url_port =
        declare_runtime_function(module, "forge_url_port", &[types::I64], &[types::I64])?;
    funcs.insert("port".to_string(), url_port);

    let url_path =
        declare_runtime_function(module, "forge_url_path", &[types::I64], &[types::I64])?;
    funcs.insert("path".to_string(), url_path);

    let url_query =
        declare_runtime_function(module, "forge_url_query", &[types::I64], &[types::I64])?;
    funcs.insert("query".to_string(), url_query);

    let url_fragment =
        declare_runtime_function(module, "forge_url_fragment", &[types::I64], &[types::I64])?;
    funcs.insert("fragment".to_string(), url_fragment);

    let url_encode =
        declare_runtime_function(module, "forge_url_encode", &[types::I64], &[types::I64])?;
    // Smart encode overrides the generic "encode" — delegates to JSON or URL based on arg
    let smart_encode =
        declare_runtime_function(module, "forge_smart_encode", &[types::I64], &[types::I64])?;
    funcs.insert("encode".to_string(), smart_encode);

    let url_decode =
        declare_runtime_function(module, "forge_url_decode", &[types::I64], &[types::I64])?;
    funcs.insert("decode".to_string(), url_decode);

    // URL parse
    let url_parse =
        declare_runtime_function(module, "forge_url_parse", &[types::I64], &[types::I64])?;
    funcs.insert("url_parse".to_string(), url_parse);

    // URL to_string
    let url_to_string =
        declare_runtime_function(module, "forge_url_to_string", &[types::I64], &[types::I64])?;
    funcs.insert("url_to_string".to_string(), url_to_string);

    // channel close (reuse tcp_close)
    funcs.insert("close".to_string(), tcp_close);

    // tcp_read with buffer size (2 args)
    let tcp_read2 = declare_runtime_function(
        module,
        "forge_tcp_read2",
        &[types::I64, types::I64],
        &[types::I64],
    )?;
    funcs.insert("tcp_read".to_string(), tcp_read2);

    // Note: list_dir returns a linked list - needs special handling
    // For now, declare but don't use directly

    // Struct allocation
    let struct_alloc = declare_runtime_function(
        module,
        "forge_struct_alloc",
        &[types::I64], // num_fields
        &[types::I64], // returns pointer
    )?;
    funcs.insert("forge_struct_alloc".to_string(), struct_alloc);

    // Map operations with C-string keys
    let map_insert_cstr = declare_runtime_function(
        module,
        "forge_map_insert_cstr",
        &[types::I64, types::I64, types::I64], // map_handle, key_cstr, value_i64
        &[],
    )?;
    funcs.insert("forge_map_insert_cstr".to_string(), map_insert_cstr);

    let map_get_cstr = declare_runtime_function(
        module,
        "forge_map_get_cstr",
        &[types::I64, types::I64], // map_handle, key_cstr
        &[types::I64],             // returns value_i64
    )?;
    funcs.insert("forge_map_get_cstr".to_string(), map_get_cstr);

    let map_contains_cstr = declare_runtime_function(
        module,
        "forge_map_contains_cstr",
        &[types::I64, types::I64], // map_handle, key_cstr
        &[types::I64],             // returns 0 or 1
    )?;
    funcs.insert("forge_map_contains_cstr".to_string(), map_contains_cstr);

    let map_remove_cstr = declare_runtime_function(
        module,
        "forge_map_remove_cstr",
        &[types::I64, types::I64], // map_handle, key_cstr
        &[],
    )?;
    funcs.insert("forge_map_remove_cstr".to_string(), map_remove_cstr);

    let map_keys_cstr = declare_runtime_function(
        module,
        "forge_map_keys_cstr",
        &[types::I64], // map_handle
        &[types::I64], // returns ForgeList as i64
    )?;
    funcs.insert("forge_map_keys_cstr".to_string(), map_keys_cstr);

    // Map operations with integer keys (handle-based)
    let map_insert_ikey = declare_runtime_function(
        module,
        "forge_map_insert_ikey",
        &[types::I64, types::I64, types::I64], // map_handle, key_int, value_i64
        &[],
    )?;
    funcs.insert("forge_map_insert_ikey".to_string(), map_insert_ikey);

    let map_get_ikey = declare_runtime_function(
        module,
        "forge_map_get_ikey",
        &[types::I64, types::I64], // map_handle, key_int
        &[types::I64],             // returns value_i64
    )?;
    funcs.insert("forge_map_get_ikey".to_string(), map_get_ikey);

    let map_contains_ikey = declare_runtime_function(
        module,
        "forge_map_contains_ikey",
        &[types::I64, types::I64], // map_handle, key_int
        &[types::I64],             // returns 0 or 1
    )?;
    funcs.insert("forge_map_contains_ikey".to_string(), map_contains_ikey);

    let map_remove_ikey = declare_runtime_function(
        module,
        "forge_map_remove_ikey",
        &[types::I64, types::I64], // map_handle, key_int
        &[],
    )?;
    funcs.insert("forge_map_remove_ikey".to_string(), map_remove_ikey);

    // Map length via handle (for codegen that only has the raw pointer)
    let map_len_handle = declare_runtime_function(
        module,
        "forge_map_len_handle",
        &[types::I64], // map_handle
        &[types::I64], // returns length
    )?;
    funcs.insert("forge_map_len_handle".to_string(), map_len_handle);

    // Map clear via handle
    let map_clear_handle = declare_runtime_function(
        module,
        "forge_map_clear_handle",
        &[types::I64], // map_handle
        &[],           // void
    )?;
    funcs.insert("forge_map_clear_handle".to_string(), map_clear_handle);

    // Map is_empty via handle
    let map_is_empty_handle = declare_runtime_function(
        module,
        "forge_map_is_empty_handle",
        &[types::I64], // map_handle
        &[types::I64], // returns 1 if empty, 0 otherwise
    )?;
    funcs.insert("forge_map_is_empty_handle".to_string(), map_is_empty_handle);

    // Map values via handle
    let map_values_handle = declare_runtime_function(
        module,
        "forge_map_values_handle",
        &[types::I64], // map_handle
        &[types::I64], // returns ForgeList as i64
    )?;
    funcs.insert("forge_map_values_handle".to_string(), map_values_handle);

    // Set functions (handle-based)
    let set_new_handle = declare_runtime_function(
        module,
        "forge_set_new_handle",
        &[types::I32], // elem_type
        &[types::I64], // returns SetImpl ptr as i64
    )?;
    funcs.insert("forge_set_new_handle".to_string(), set_new_handle);

    let set_len_handle = declare_runtime_function(
        module,
        "forge_set_len_handle",
        &[types::I64], // set_handle
        &[types::I64], // returns length
    )?;
    funcs.insert("forge_set_len_handle".to_string(), set_len_handle);

    let set_add_cstr = declare_runtime_function(
        module,
        "forge_set_add_cstr",
        &[types::I64, types::I64], // set_handle, elem (cstr ptr)
        &[types::I64],             // returns 1 if new, 0 if existed
    )?;
    funcs.insert("forge_set_add_cstr".to_string(), set_add_cstr);

    let set_contains_cstr = declare_runtime_function(
        module,
        "forge_set_contains_cstr",
        &[types::I64, types::I64], // set_handle, elem (cstr ptr)
        &[types::I64],             // returns 1 if present, 0 otherwise
    )?;
    funcs.insert("forge_set_contains_cstr".to_string(), set_contains_cstr);

    let set_remove_cstr = declare_runtime_function(
        module,
        "forge_set_remove_cstr",
        &[types::I64, types::I64], // set_handle, elem (cstr ptr)
        &[],                       // void
    )?;
    funcs.insert("forge_set_remove_cstr".to_string(), set_remove_cstr);

    let set_clear_handle = declare_runtime_function(
        module,
        "forge_set_clear_handle",
        &[types::I64], // set_handle
        &[],           // void
    )?;
    funcs.insert("forge_set_clear_handle".to_string(), set_clear_handle);

    let set_is_empty_handle = declare_runtime_function(
        module,
        "forge_set_is_empty_handle",
        &[types::I64], // set_handle
        &[types::I64], // returns 1 if empty, 0 otherwise
    )?;
    funcs.insert("forge_set_is_empty_handle".to_string(), set_is_empty_handle);

    // args() returning a List (ForgeList)
    let args_to_list = declare_runtime_function(
        module,
        "forge_args_to_list",
        &[],           // no args
        &[types::I64], // returns ForgeList as i64
    )?;
    funcs.insert("forge_args_to_list".to_string(), args_to_list);

    Ok(funcs)
}

/// Declare a string in the data section and return its address
pub fn declare_string_data(
    module: &mut ObjectModule,
    name: &str,
    content: &str,
) -> Result<FuncId, CompileError> {
    use cranelift_module::DataDescription;

    // Create null-terminated string data
    let mut data = content.as_bytes().to_vec();
    data.push(0); // Null terminator

    let mut data_desc = DataDescription::new();
    data_desc.define(data.into_boxed_slice());

    let data_id = module
        .declare_data(name, cranelift_module::Linkage::Local, false, false)
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;

    module
        .define_data(data_id, &data_desc)
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;

    // Create a function that returns the address of the string data
    let mut ctx = module.make_context();
    ctx.func.signature.returns.push(AbiParam::new(types::I64));

    let func_name = format!("__str_{}", name);
    let func_id = module
        .declare_function(&func_name, Linkage::Local, &ctx.func.signature)
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;

    let mut builder_ctx = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
        let entry_block = builder.create_block();
        builder.switch_to_block(entry_block);
        builder.seal_block(entry_block);

        // Get address of data
        let data_ref = module.declare_data_in_func(data_id, builder.func);
        let addr = builder.ins().global_value(types::I64, data_ref);

        builder.ins().return_(&[addr]);

        builder.finalize();
    }

    module
        .define_function(func_id, &mut ctx)
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;

    Ok(func_id)
}
/// Finalize the module and return object bytes
pub fn finalize_module(module: ObjectModule) -> Result<Vec<u8>, CompileError> {
    let object = module.finish();
    let bytes = object
        .emit()
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;
    Ok(bytes)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_codegen() {
        let codegen = create_codegen();
        assert!(codegen.is_ok());
    }

    #[test]
    fn test_type_mapping() {
        assert_eq!(forge_type_to_cranelift("Int"), types::I64);
        assert_eq!(forge_type_to_cranelift("Float"), types::F64);
        assert_eq!(forge_type_to_cranelift("Bool"), types::I64);
    }
}
