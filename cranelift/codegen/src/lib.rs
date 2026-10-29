//! Cranelift code generation for Pith
//!
//! Pipeline: Pith IR text → ir_consumer → Cranelift → native object code

use cranelift::prelude::*;
use cranelift_module::Module;
use cranelift_object::{ObjectBuilder, ObjectModule};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;

pub mod ir_consumer;
pub mod linker;
mod loop_hoist;
pub mod runtime_imports;

pub use runtime_imports::declare_runtime_functions;

// --- Struct layout registry (used by ir_consumer for field access) ---

static STRUCT_LAYOUTS: OnceLock<Mutex<HashMap<String, Vec<(String, usize)>>>> = OnceLock::new();

pub fn register_struct_layout(name: &str, fields: &[(String, String)]) {
    let layouts = STRUCT_LAYOUTS.get_or_init(|| Mutex::new(HashMap::new()));
    let field_info: Vec<_> = fields
        .iter()
        .enumerate()
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
    STRUCT_LAYOUTS
        .get()
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
    // cranelift defaults to opt_level "none"; without this the generated
    // code keeps every redundant load and dead value the lowering emits.
    let mut flag_builder = settings::builder();
    flag_builder
        .set("opt_level", "speed")
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;
    // start every function on a cache line. unaligned, a function's hot loop
    // lands wherever the code before it happens to end, so growing an
    // unrelated function moved tight_loop's loop across a line boundary and
    // made it 50% slower with identical machine code. the runtime is aligned
    // the same way (.cargo/config.toml).
    flag_builder
        .set("log2_min_function_alignment", "6")
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;
    // macos links everything position-independent and its ld rejects text
    // relocations outright, so absolute addressing in the text section — what
    // cranelift emits by default — fails to link. the gnu toolchain accepts
    // both, so we only pay the pic addressing cost where it is required.
    if cfg!(target_os = "macos") {
        flag_builder
            .set("is_pic", "true")
            .map_err(|e| CompileError::ModuleError(e.to_string()))?;
    }
    let isa = isa_builder
        .finish(settings::Flags::new(flag_builder))
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;
    let builder = ObjectBuilder::new(
        isa,
        "pith_module",
        cranelift_module::default_libcall_names(),
    )
    .map_err(|e| CompileError::ModuleError(e.to_string()))?;
    Ok(CodeGen {
        module: ObjectModule::new(builder),
    })
}
/// Declare a string in the data section and return its address
pub fn declare_string_data(
    module: &mut ObjectModule,
    name: &str,
    content: &str,
) -> Result<cranelift_module::DataId, CompileError> {
    use cranelift_module::DataDescription;

    // a sixteen-byte header precedes the characters, laid out like a heap
    // cstring's: [magic: u32][0: u32][length: u64]. its magic is
    // `runtime_core::CSTRING_STATIC_MAGIC`, never the heap one, so retain and
    // release still leave a literal alone, but every reader of a string's
    // length takes it from the header instead of scanning for the NUL. the
    // data is 8-aligned so the address after the header passes the same
    // alignment check a heap cstring does. the returned address skips the
    // header.
    let mut data = Vec::with_capacity(16 + content.len() + 1);
    data.extend_from_slice(&pith_runtime::CSTRING_STATIC_MAGIC.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(&(content.len() as u64).to_le_bytes());
    data.extend_from_slice(content.as_bytes());
    data.push(0); // Null terminator

    let mut data_desc = DataDescription::new();
    data_desc.define(data.into_boxed_slice());
    data_desc.set_align(8);

    let data_id = module
        .declare_data(name, cranelift_module::Linkage::Local, false, false)
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;

    module
        .define_data(data_id, &data_desc)
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;

    Ok(data_id)
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
