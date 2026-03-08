//! Cranelift code generation for Forge
//!
//! This module compiles Forge AST into native machine code using Cranelift.
//! It generates object files that can be linked with the runtime library.

use cranelift::prelude::*;
use cranelift_module::{Module, Linkage, FuncId};
use cranelift_object::{ObjectModule, ObjectBuilder};
use std::collections::HashMap;

pub mod ast;
pub mod parser;

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
    ).map_err(|e| CompileError::ModuleError(e.to_string()))?;
    
    let module = ObjectModule::new(builder);
    
    Ok(CodeGen {
        module,
        builder_ctx: FunctionBuilderContext::new(),
        current_func: None,
        variables: HashMap::new(),
        builder: None,
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
        _ => types::I64, // Default to pointer size
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

/// Declare all runtime functions needed by the compiler
pub fn declare_runtime_functions(module: &mut ObjectModule) -> Result<HashMap<String, FuncId>, CompileError> {
    let mut funcs = HashMap::new();
    
    // String functions
    let string_new = declare_runtime_function(
        module,
        "forge_string_new",
        &[types::I64, types::I64], // data_ptr, len
        &[types::I64], // returns ForgeString struct (simplified as I64 for now)
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
    
    // List functions
    let list_new = declare_runtime_function(
        module,
        "forge_list_new",
        &[types::I64, types::I32], // elem_size, type_tag
        &[types::I64], // returns ForgeList
    )?;
    funcs.insert("forge_list_new".to_string(), list_new);
    
    let list_push = declare_runtime_function(
        module,
        "forge_list_push",
        &[types::I64, types::I64, types::I64], // list_ptr, elem_ptr, elem_size
        &[],
    )?;
    funcs.insert("forge_list_push".to_string(), list_push);
    
    Ok(funcs)
}

/// Generate a simple test function that adds two integers
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
    let bytes = object.emit().map_err(|e| CompileError::ModuleError(e.to_string()))?;
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
    ).map_err(|e| CompileError::ModuleError(e.to_string()))?;
    
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
    let bytes = object.emit().map_err(|e| CompileError::ModuleError(e.to_string()))?;
    
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
