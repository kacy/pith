//! Cranelift code generation for Forge
//!
//! This module compiles Forge AST into native machine code using Cranelift.
//! It generates object files that can be linked with the runtime library.

use cranelift::prelude::*;
use cranelift_module::{FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::collections::HashMap;

pub mod ast;
pub mod compiler;
pub mod linker;
pub mod parser;

/// Struct field information
#[derive(Debug, Clone)]
pub struct StructField {
    pub name: String,
    pub ty: String,
    pub offset: usize,
    pub size: usize,
}

/// Struct type information
#[derive(Debug, Clone)]
pub struct StructType {
    pub name: String,
    pub fields: Vec<StructField>,
    pub total_size: usize,
    pub alignment: usize,
}

/// Generic type instantiation (e.g., "List[Int]" -> concrete type)
#[derive(Debug, Clone)]
pub struct GenericInstantiation {
    pub base_type: String,      // "List", "Map", etc.
    pub type_args: Vec<String>, // ["Int"], ["String", "Int"], etc.
    pub concrete_name: String,  // "List_Int", "Map_String_Int", etc.
}

/// Generic type registry for monomorphization
#[derive(Debug, Default)]
pub struct GenericRegistry {
    /// Map from full type name (e.g., "List[Int]") to concrete info
    pub instantiations: HashMap<String, GenericInstantiation>,
    /// Counter for generating unique IDs
    pub next_id: u64,
}

impl GenericRegistry {
    /// Register a generic type usage and return the concrete name
    pub fn register(&mut self, full_type_name: &str) -> String {
        if let Some(existing) = self.instantiations.get(full_type_name) {
            return existing.concrete_name.clone();
        }

        // Parse the type name: e.g., "List[Int]" or "Map[String, Int]"
        let (base_type, type_args) = parse_generic_type(full_type_name);

        // Generate concrete name: e.g., "List_Int", "Map_String_Int"
        let concrete_name = if type_args.is_empty() {
            base_type.clone()
        } else {
            format!("{}_{}", base_type, type_args.join("_"))
        };

        let instantiation = GenericInstantiation {
            base_type: base_type.clone(),
            type_args: type_args.clone(),
            concrete_name: concrete_name.clone(),
        };

        self.instantiations
            .insert(full_type_name.to_string(), instantiation);
        concrete_name
    }

    /// Get all registered instantiations for a base type
    pub fn get_for_base(&self, base: &str) -> Vec<&GenericInstantiation> {
        self.instantiations
            .values()
            .filter(|inst| inst.base_type == base)
            .collect()
    }
}

/// Parse a generic type name into base and type arguments
/// e.g., "List[Int]" -> ("List", vec!["Int"])
/// e.g., "Map[String, Int]" -> ("Map", vec!["String", "Int"])
pub fn parse_generic_type(type_name: &str) -> (String, Vec<String>) {
    // Check if it's a generic type with [...]
    if let Some(bracket_pos) = type_name.find('[') {
        let base = type_name[..bracket_pos].to_string();
        let args_part = &type_name[bracket_pos + 1..type_name.len() - 1]; // Remove trailing ']'

        // Parse comma-separated type arguments
        let args: Vec<String> = args_part
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        (base, args)
    } else {
        // Not a generic type
        (type_name.to_string(), vec![])
    }
}

/// Code generator state
pub struct CodeGen {
    /// The Cranelift module being built
    pub module: ObjectModule,
    /// Function builder context
    pub builder_ctx: FunctionBuilderContext,
    /// Current function being compiled
    pub current_func: Option<FuncId>,
    /// Variable map (name -> SSA value)
    pub variables: HashMap<String, Value>,
    /// Current instruction builder
    pub builder: Option<FunctionBuilder<'static>>, // Will fix lifetime issues
    /// Struct type registry (name -> struct info)
    pub struct_types: HashMap<String, StructType>,
    /// Generic type registry for monomorphization
    pub generic_registry: GenericRegistry,
}

/// Result of compilation
#[derive(Debug)]
pub struct CompileResult {
    /// The generated object file bytes
    pub object_bytes: Vec<u8>,
    /// Entry point function name
    pub entry_point: String,
}

/// Error during compilation
#[derive(Debug)]
pub enum CompileError {
    ModuleError(String),
    TypeError(String),
    UnknownFunction(String),
    UnknownVariable(String),
    UnsupportedFeature(String),
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::ModuleError(e) => write!(f, "Module error: {}", e),
            CompileError::TypeError(e) => write!(f, "Type error: {}", e),
            CompileError::UnknownFunction(name) => write!(f, "Unknown function: {}", name),
            CompileError::UnknownVariable(name) => write!(f, "Unknown variable: {}", name),
            CompileError::UnsupportedFeature(feat) => write!(f, "Unsupported feature: {}", feat),
        }
    }
}

impl std::error::Error for CompileError {}

/// Create a new code generator
pub fn create_codegen() -> Result<CodeGen, CompileError> {
    // Set up the target ISA for the native host
    let isa_builder = cranelift_native::builder()
        .map_err(|e| CompileError::ModuleError(format!("Unsupported target: {:?}", e)))?;

    let isa = isa_builder
        .finish(settings::Flags::new(settings::builder()))
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;

    // Create an object module
    let builder = ObjectBuilder::new(
        isa,
        "forge_module",
        cranelift_module::default_libcall_names(),
    )
    .map_err(|e| CompileError::ModuleError(e.to_string()))?;

    let module = ObjectModule::new(builder);

    Ok(CodeGen {
        module,
        builder_ctx: FunctionBuilderContext::new(),
        current_func: None,
        variables: HashMap::new(),
        builder: None,
        struct_types: HashMap::new(),
        generic_registry: GenericRegistry::default(),
    })
}

/// Compile a Forge module (placeholder for now)
pub fn compile_module(_ast: &str) -> Result<CompileResult, CompileError> {
    // TODO: Parse AST and compile
    // For now, return empty result
    Ok(CompileResult {
        object_bytes: vec![],
        entry_point: "main".to_string(),
    })
}

/// Get the target triple for the current host
pub fn get_target_triple() -> String {
    target_lexicon::Triple::host().to_string()
}

/// Type mapping from Forge types to Cranelift types
pub fn forge_type_to_cranelift(ty: &str) -> Type {
    match ty {
        "Int" | "Int8" | "Int16" | "Int32" | "Int64" => types::I64,
        "UInt" | "UInt8" | "UInt16" | "UInt32" | "UInt64" => types::I64,
        "Float" => types::F64,
        "Bool" => types::I8,
        "String" | "List" | "Map" | "Set" => types::I64, // Pointer types
        _ => types::I64,                                 // Default to pointer size
    }
}

/// Declare an external runtime function
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

/// Calculate the size of a type in bytes
pub fn type_size(ty: &str) -> usize {
    match ty {
        "Int" | "Int64" | "UInt64" => 8,
        "Int32" | "UInt32" => 4,
        "Int16" | "UInt16" => 2,
        "Int8" | "UInt8" | "Bool" => 1,
        "Float" | "Float64" => 8,
        "Float32" => 4,
        "String" => 24, // ForgeString struct size (ptr + len + capacity)
        "List" => 8,    // Pointer to list implementation
        "Map" => 8,     // Pointer to map implementation
        _ => {
            // Check if it's a struct type (we'll need to look it up)
            // For now, assume 8 bytes for unknown types
            8
        }
    }
}

/// Calculate the alignment of a type in bytes
pub fn type_alignment(ty: &str) -> usize {
    match ty {
        "Int" | "Int64" | "UInt64" | "Float" | "Float64" => 8,
        "Int32" | "UInt32" | "Float32" => 4,
        "Int16" | "UInt16" => 2,
        "Int8" | "UInt8" | "Bool" => 1,
        "String" => 8,
        "List" | "Map" => 8,
        _ => 8,
    }
}

/// Calculate struct layout given field names and types
pub fn calculate_struct_layout(name: String, fields: Vec<(String, String)>) -> StructType {
    let mut struct_fields = Vec::new();
    let mut current_offset = 0;
    let mut max_alignment = 1;

    for (field_name, field_type) in fields {
        let field_size = type_size(&field_type);
        let field_align = type_alignment(&field_type);

        // Align current_offset to field alignment
        let padding = (field_align - (current_offset % field_align)) % field_align;
        current_offset += padding;

        struct_fields.push(StructField {
            name: field_name,
            ty: field_type.clone(),
            offset: current_offset,
            size: field_size,
        });

        current_offset += field_size;
        max_alignment = max_alignment.max(field_align);
    }

    // Pad struct size to its alignment
    let total_size = if current_offset % max_alignment != 0 {
        current_offset + (max_alignment - (current_offset % max_alignment))
    } else {
        current_offset
    };

    StructType {
        name,
        fields: struct_fields,
        total_size,
        alignment: max_alignment,
    }
}

/// Declare all runtime functions needed by the compiler
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

    let list_push = declare_runtime_function(
        module,
        "forge_list_push",
        &[types::I64, types::I64, types::I64], // *mut ForgeList (list addr), *mut elem, elem_size
        &[],
    )?;
    funcs.insert("forge_list_push".to_string(), list_push);

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
        &[types::I32, types::I64, types::I8], // key_type, val_size, val_is_heap
        &[types::I64],                        // returns ForgeMap
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
        &[types::I8],              // returns bool
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
        &[types::I8],              // returns bool
    )?;
    funcs.insert("forge_string_starts_with".to_string(), string_starts_with);

    let string_ends_with = declare_runtime_function(
        module,
        "forge_string_ends_with_ptr",
        &[types::I64, types::I64], // *const s, *const suffix
        &[types::I8],              // returns bool
    )?;
    funcs.insert("forge_string_ends_with".to_string(), string_ends_with);

    let string_concat = declare_runtime_function(
        module,
        "forge_string_concat_ptr",
        &[types::I64, types::I64, types::I64], // *const a, *const b, *mut out
        &[],
    )?;
    funcs.insert("forge_string_concat".to_string(), string_concat);

    // Simple strlen-based length for null-terminated strings (debugging/workaround)
    let cstring_len = declare_runtime_function(
        module,
        "forge_cstring_len",
        &[types::I64], // *const cstr
        &[types::I64], // returns length
    )?;
    funcs.insert("forge_cstring_len".to_string(), cstring_len);

    // Filesystem functions
    let file_exists = declare_runtime_function(
        module,
        "forge_file_exists",
        &[types::I64], // *const path
        &[types::I8],  // returns bool (0/1)
    )?;
    funcs.insert("file_exists".to_string(), file_exists);

    let dir_exists = declare_runtime_function(
        module,
        "forge_dir_exists",
        &[types::I64], // *const path
        &[types::I8],  // returns bool (0/1)
    )?;
    funcs.insert("dir_exists".to_string(), dir_exists);

    let mkdir = declare_runtime_function(
        module,
        "forge_mkdir",
        &[types::I64], // *const path
        &[types::I8],  // returns bool (0/1)
    )?;
    funcs.insert("mkdir".to_string(), mkdir);

    let remove_file = declare_runtime_function(
        module,
        "forge_remove_file",
        &[types::I64], // *const path
        &[types::I8],  // returns bool (0/1)
    )?;
    funcs.insert("remove_file".to_string(), remove_file);

    let rename_file = declare_runtime_function(
        module,
        "forge_rename_file",
        &[types::I64, types::I64], // *const from, *const to
        &[types::I8],              // returns bool (0/1)
    )?;
    funcs.insert("rename_file".to_string(), rename_file);

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
        &[types::I8],              // returns bool (0/1)
    )?;
    funcs.insert("write_file".to_string(), write_file);

    let append_file = declare_runtime_function(
        module,
        "forge_append_file",
        &[types::I64, types::I64], // *const path, *const content
        &[types::I8],              // returns bool (0/1)
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

    let contains = declare_runtime_function(
        module,
        "forge_cstring_contains",  // We'll need to implement this
        &[types::I64, types::I64], // haystack, needle
        &[types::I8],              // returns bool
    )?;
    funcs.insert("contains".to_string(), contains);

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

    let trim_left = declare_runtime_function(
        module,
        "forge_cstring_trim_left", // We'll need to implement this
        &[types::I64],             // str
        &[types::I64],             // returns *mut cstr
    )?;
    funcs.insert("trim_left".to_string(), trim_left);

    // Note: list_dir returns a linked list - needs special handling
    // For now, declare but don't use directly

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
pub fn generate_test_function(module: &mut ObjectModule) -> Result<FuncId, CompileError> {
    let mut ctx = module.make_context();

    // Define function signature: fn(i64, i64) -> i64
    ctx.func.signature.params.push(AbiParam::new(types::I64));
    ctx.func.signature.params.push(AbiParam::new(types::I64));
    ctx.func.signature.returns.push(AbiParam::new(types::I64));

    let func_id = module
        .declare_function("add", Linkage::Local, &ctx.func.signature)
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;

    // Build the function
    let mut builder_ctx = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);

        let entry_block = builder.create_block();
        builder.append_block_params_for_function_params(entry_block);
        builder.switch_to_block(entry_block);
        builder.seal_block(entry_block);

        // Get parameters
        let params = builder.block_params(entry_block);
        let a = params[0];
        let b = params[1];

        // Add them
        let sum = builder.ins().iadd(a, b);

        // Return the result
        builder.ins().return_(&[sum]);
    }

    // Define and finalize the function
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

/// Compile a simple "hello world" test
pub fn compile_hello_world() -> Result<CompileResult, CompileError> {
    let isa_builder = cranelift_native::builder()
        .map_err(|e| CompileError::ModuleError(format!("Unsupported target: {:?}", e)))?;

    let isa = isa_builder
        .finish(settings::Flags::new(settings::builder()))
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;

    let builder = ObjectBuilder::new(
        isa,
        "hello_world",
        cranelift_module::default_libcall_names(),
    )
    .map_err(|e| CompileError::ModuleError(e.to_string()))?;

    let mut module = ObjectModule::new(builder);

    // Declare external printf function
    let mut printf_sig = module.make_signature();
    printf_sig.params.push(AbiParam::new(types::I64)); // format string pointer
    printf_sig.returns.push(AbiParam::new(types::I32)); // return int

    let printf_id = module
        .declare_function("printf", Linkage::Import, &printf_sig)
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;

    // Create main function
    let mut ctx = module.make_context();
    ctx.func.signature.returns.push(AbiParam::new(types::I32));

    let main_id = module
        .declare_function("main", Linkage::Export, &ctx.func.signature)
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;

    // Build main
    let mut builder_ctx = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);

        let entry_block = builder.create_block();
        builder.switch_to_block(entry_block);
        builder.seal_block(entry_block);

        // Get pointer to format string
        let format_str = builder.ins().iconst(types::I64, 0); // Placeholder - would need actual string data

        // Get function reference for printf
        let printf_func_ref = module.declare_func_in_func(printf_id, builder.func);

        // Call printf
        builder.ins().call(printf_func_ref, &[format_str]);

        // Return 0
        let zero = builder.ins().iconst(types::I32, 0);
        builder.ins().return_(&[zero]);
    }

    // Define main
    module
        .define_function(main_id, &mut ctx)
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;

    // Finalize and return
    let object = module.finish();
    let bytes = object
        .emit()
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;

    Ok(CompileResult {
        object_bytes: bytes,
        entry_point: "main".to_string(),
    })
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
        assert_eq!(forge_type_to_cranelift("Bool"), types::I8);
    }
}
