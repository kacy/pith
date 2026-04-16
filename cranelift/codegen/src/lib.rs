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

struct RuntimeDecl {
    key: &'static str,
    symbol: &'static str,
    params: &'static [Type],
    returns: &'static [Type],
}

include!(concat!(env!("OUT_DIR"), "/runtime_table.rs"));

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

/// Declare all runtime functions as a data-driven table
pub fn declare_runtime_functions(
    module: &mut ObjectModule,
) -> Result<HashMap<String, FuncId>, CompileError> {
    let mut funcs = HashMap::new();

    // Declare each function and insert with its key
    let mut declared: HashMap<String, FuncId> = HashMap::new();
    for decl in RUNTIME_FUNCTIONS {
        let fid = if let Some(&existing) = declared.get(decl.symbol) {
            existing
        } else {
            let fid = declare_runtime_function(module, decl.symbol, decl.params, decl.returns)?;
            declared.insert(decl.symbol.to_string(), fid);
            fid
        };
        funcs.insert(decl.key.to_string(), fid);
    }

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
        assert!(create_codegen().is_ok());
    }
}
