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

/// Declare all runtime functions as a data-driven table
pub fn declare_runtime_functions(
    module: &mut ObjectModule,
) -> Result<HashMap<String, FuncId>, CompileError> {
    use types::{I64, I32, F64};
    let mut funcs = HashMap::new();

    // Table: (key, symbol, params, returns)
    // key = name used in ir_consumer lookups
    // symbol = actual C function name in the runtime library
    let rt_table: &[(&str, &str, &[Type], &[Type])] = &[
        ("forge_string_new", "forge_string_new", &[I64, I64], &[I64]),
        ("forge_string_concat", "forge_string_concat_ptr", &[I64, I64, I64], &[]),
        ("forge_string_release", "forge_string_release", &[I64], &[]),
        ("forge_string_from_cstr", "forge_string_from_cstr_ptr", &[I64, I64], &[]),
        ("forge_print", "forge_print", &[I64], &[]),
        ("forge_smart_print", "forge_smart_print", &[I64], &[]),
        ("forge_int_to_string", "forge_int_to_string", &[I64], &[I64]),
        ("forge_int_to_cstr", "forge_int_to_cstr", &[I64], &[I64]),
        ("int_to_string", "forge_int_to_cstr", &[I64], &[I64]),
        ("float_to_string", "forge_float_to_cstr", &[F64], &[I64]),
        ("forge_float_to_cstr", "forge_float_to_cstr", &[F64], &[I64]),
        ("bool_to_string", "forge_bool_to_cstr", &[I64], &[I64]),
        ("forge_bool_to_cstr", "forge_bool_to_cstr", &[I64], &[I64]),
        ("ord", "forge_ord_cstr", &[I64], &[I64]),
        ("chr", "forge_chr_cstr", &[I64], &[I64]),
        ("forge_print_int", "forge_print_int", &[I64], &[]),
        ("forge_print_cstr", "forge_print_cstr", &[I64], &[]),
        ("forge_closure_new", "forge_closure_new", &[I64], &[I64]),
        ("forge_closure_get_fn", "forge_closure_get_fn", &[I64], &[I64]),
        ("forge_closure_set_env", "forge_closure_set_env", &[I64, I64, I64], &[]),
        ("forge_closure_get_env", "forge_closure_get_env", &[I64, I64], &[I64]),
        ("print_err", "forge_print_err", &[I64], &[]),
        ("forge_concat_cstr", "forge_concat_cstr", &[I64, I64], &[I64]),
        ("bit_and", "forge_bit_and", &[I64, I64], &[I64]),
        ("bit_or", "forge_bit_or", &[I64, I64], &[I64]),
        ("bit_xor", "forge_bit_xor", &[I64, I64], &[I64]),
        ("bit_shl", "forge_bit_shl", &[I64, I64], &[I64]),
        ("bit_shr", "forge_bit_shr", &[I64, I64], &[I64]),
        ("bit_not", "forge_bit_not", &[I64], &[I64]),
        ("abs", "forge_abs", &[I64], &[I64]),
        ("min", "forge_min", &[I64, I64], &[I64]),
        ("max", "forge_max", &[I64, I64], &[I64]),
        ("clamp", "forge_clamp", &[I64, I64, I64], &[I64]),
        ("pow", "forge_pow", &[F64, F64], &[F64]),
        ("sqrt", "forge_sqrt", &[F64], &[F64]),
        ("floor", "forge_floor", &[F64], &[F64]),
        ("ceil", "forge_ceil", &[F64], &[F64]),
        ("round", "forge_round", &[F64], &[F64]),
        ("assert", "forge_assert", &[I64], &[]),
        ("assert_eq", "forge_assert_eq", &[I64, I64], &[]),
        ("assert_ne", "forge_assert_ne", &[I64, I64], &[]),
        ("forge_list_new", "forge_list_new", &[I64, I32], &[I64]),
        ("forge_list_new_default", "forge_list_new_default", &[], &[I64]),
        ("forge_map_new_default", "forge_map_new_default", &[], &[I64]),
        ("forge_map_new_int", "forge_map_new_int", &[], &[I64]),
        ("forge_set_new_default", "forge_set_new_default", &[], &[I64]),
        ("forge_list_push", "forge_list_push", &[I64, I64, I64], &[]),
        ("forge_list_push_value", "forge_list_push_value", &[I64, I64], &[]),
        ("forge_list_set_value", "forge_list_set_value", &[I64, I64, I64], &[]),
        ("list_join", "forge_list_join", &[I64, I64], &[I64]),
        ("forge_list_join", "forge_list_join", &[I64, I64], &[I64]),
        ("forge_list_get_value", "forge_list_get_value", &[I64, I64], &[I64]),
        ("forge_list_len", "forge_list_len", &[I64], &[I64]),
        ("forge_list_pop", "forge_list_pop", &[I64], &[I64]),
        ("forge_auto_len", "forge_auto_len", &[I64], &[I64]),
        ("forge_list_map", "forge_list_map", &[I64, I64], &[I64]),
        ("forge_list_filter", "forge_list_filter", &[I64, I64], &[I64]),
        ("forge_list_reduce", "forge_list_reduce", &[I64, I64, I64], &[I64]),
        ("forge_list_each", "forge_list_each", &[I64, I64], &[]),
        ("forge_map_new", "forge_map_new", &[I32, I64, I64], &[I64]),
        ("forge_map_len", "forge_map_len", &[I64], &[I64]),
        ("forge_map_insert_int", "forge_map_insert_int", &[I64, I64, I64, I64], &[]),
        ("forge_string_len", "forge_string_len_ptr", &[I64], &[I64]),
        ("forge_string_contains", "forge_string_contains_ptr", &[I64, I64], &[I64]),
        ("forge_string_substring", "forge_string_substring_ptr", &[I64, I64, I64, I64], &[]),
        ("forge_string_trim", "forge_string_trim_ptr", &[I64, I64], &[]),
        ("forge_string_starts_with", "forge_string_starts_with_ptr", &[I64, I64], &[I64]),
        ("forge_string_ends_with", "forge_string_ends_with_ptr", &[I64, I64], &[I64]),
        ("forge_cstring_eq", "forge_cstring_eq", &[I64, I64], &[I64]),
        ("forge_cstring_cmp", "forge_cstring_cmp", &[I64, I64], &[I64]),
        ("string_len", "forge_cstring_len", &[I64], &[I64]),
        ("forge_cstring_len", "forge_cstring_len", &[I64], &[I64]),
        ("string_is_empty", "forge_cstring_is_empty", &[I64], &[I64]),
        ("forge_cstring_is_empty", "forge_cstring_is_empty", &[I64], &[I64]),
        ("bytes_from_string_utf8", "forge_bytes_from_string_utf8", &[I64], &[I64]),
        ("bytes_to_string_utf8", "forge_bytes_to_string_utf8", &[I64], &[I64]),
        ("bytes_len", "forge_bytes_len", &[I64], &[I64]),
        ("bytes_is_empty", "forge_bytes_is_empty", &[I64], &[I64]),
        ("bytes_get", "forge_bytes_get", &[I64, I64], &[I64]),
        ("bytes_slice", "forge_bytes_slice", &[I64, I64, I64], &[I64]),
        ("bytes_concat", "forge_bytes_concat", &[I64, I64], &[I64]),
        ("bytes_eq", "forge_bytes_eq", &[I64, I64], &[I64]),
        ("byte_buffer_new", "forge_byte_buffer_new", &[], &[I64]),
        ("byte_buffer_with_capacity", "forge_byte_buffer_with_capacity", &[I64], &[I64]),
        ("byte_buffer_write", "forge_byte_buffer_write", &[I64, I64], &[I64]),
        ("byte_buffer_write_byte", "forge_byte_buffer_write_byte", &[I64, I64], &[I64]),
        ("byte_buffer_bytes", "forge_byte_buffer_bytes", &[I64], &[I64]),
        ("byte_buffer_clear", "forge_byte_buffer_clear", &[I64], &[]),
        ("file_exists", "forge_file_exists", &[I64], &[I64]),
        ("dir_exists", "forge_dir_exists", &[I64], &[I64]),
        ("mkdir", "forge_mkdir", &[I64], &[I64]),
        ("remove_file", "forge_remove_file", &[I64], &[I64]),
        ("fs_remove_dir", "forge_remove_dir", &[I64], &[I64]),
        ("fs_remove_tree", "forge_remove_tree", &[I64], &[I64]),
        ("rename_file", "forge_rename_file", &[I64, I64], &[I64]),
        ("fs_file_size", "forge_file_size", &[I64], &[I64]),
        ("list_dir", "forge_list_dir", &[I64], &[I64]),
        ("os_getcwd", "forge_os_getcwd", &[], &[I64]),
        ("os_chdir", "forge_os_chdir", &[I64], &[I64]),
        ("os_temp_dir", "forge_os_temp_dir", &[], &[I64]),
        ("os_home_dir", "forge_os_home_dir", &[], &[I64]),
        ("os_set_env", "forge_os_set_env", &[I64, I64], &[I64]),
        ("os_unset_env", "forge_os_unset_env", &[I64], &[I64]),
        ("read_file", "forge_read_file", &[I64], &[I64]),
        ("read_file_bytes", "forge_read_file_bytes", &[I64], &[I64]),
        ("write_file", "forge_write_file", &[I64, I64], &[I64]),
        ("append_file", "forge_append_file", &[I64, I64], &[I64]),
        ("write_file_bytes", "forge_write_file_bytes", &[I64, I64], &[I64]),
        ("append_file_bytes", "forge_append_file_bytes", &[I64, I64], &[I64]),
        ("file_open_read", "forge_file_open_read", &[I64], &[I64]),
        ("file_open_write", "forge_file_open_write", &[I64], &[I64]),
        ("file_open_append", "forge_file_open_append", &[I64], &[I64]),
        ("file_read", "forge_file_read", &[I64, I64], &[I64]),
        ("file_write", "forge_file_write", &[I64, I64], &[I64]),
        ("file_read_bytes", "forge_file_read_bytes", &[I64, I64], &[I64]),
        ("file_write_bytes", "forge_file_write_bytes", &[I64, I64], &[I64]),
        ("file_close", "forge_file_close", &[I64], &[]),
        ("exit", "forge_exit", &[I64], &[]),
        ("sleep", "forge_sleep", &[I64], &[]),
        ("time", "forge_time", &[], &[I64]),
        ("env", "forge_env", &[I64], &[I64]),
        ("input", "forge_input", &[], &[I64]),
        ("exec", "forge_exec", &[I64], &[I64]),
        ("random_float", "forge_random_float", &[], &[F64]),
        ("random_seed", "forge_random_seed", &[I64], &[]),
        ("random_int", "forge_random_int", &[I64, I64], &[I64]),
        ("math_sqrt", "forge_sqrt", &[F64], &[F64]),
        ("math_floor", "forge_floor", &[F64], &[F64]),
        ("math_ceil", "forge_ceil", &[F64], &[F64]),
        ("math_round", "forge_round", &[F64], &[F64]),
        ("math_pow", "forge_pow", &[F64, F64], &[F64]),
        ("sin", "forge_sin", &[F64], &[F64]),
        ("cos", "forge_cos", &[F64], &[F64]),
        ("tan", "forge_tan", &[F64], &[F64]),
        ("asin", "forge_asin", &[F64], &[F64]),
        ("acos", "forge_acos", &[F64], &[F64]),
        ("atan", "forge_atan", &[F64], &[F64]),
        ("atan2", "forge_atan2", &[F64, F64], &[F64]),
        ("math_log", "forge_log", &[F64], &[F64]),
        ("math_log10", "forge_log10", &[F64], &[F64]),
        ("math_log2", "forge_log2", &[F64], &[F64]),
        ("math_exp", "forge_exp", &[F64], &[F64]),
        ("math_abs_float", "forge_abs_float", &[F64], &[F64]),
        ("forge_cstring_compare", "forge_cstring_compare", &[I64, I64], &[I64]),
        ("forge_cstring_lt", "forge_cstring_lt", &[I64, I64], &[I64]),
        ("forge_cstring_gt", "forge_cstring_gt", &[I64, I64], &[I64]),
        ("forge_cstring_lte", "forge_cstring_lte", &[I64, I64], &[I64]),
        ("forge_cstring_gte", "forge_cstring_gte", &[I64, I64], &[I64]),
        ("fmt_float", "forge_fmt_float", &[F64, I64], &[I64]),
        ("random_string", "forge_random_string", &[I64], &[I64]),
        ("args", "forge_args", &[], &[I64]),
        ("forge_cstring_substring", "forge_cstring_substring", &[I64, I64, I64], &[I64]),
        ("forge_cstring_contains", "forge_cstring_contains", &[I64, I64], &[I64]),
        ("forge_string_split_to_list", "forge_string_split_to_list", &[I64, I64], &[I64]),
        ("forge_cstring_trim", "forge_cstring_trim", &[I64], &[I64]),
        ("trim_left", "forge_cstring_trim_left", &[I64], &[I64]),
        ("char_at", "forge_cstring_char_at", &[I64, I64], &[I64]),
        ("forge_cstring_char_at", "forge_cstring_char_at", &[I64, I64], &[I64]),
        ("Mutex", "forge_mutex_new", &[], &[I64]),
        ("forge_mutex_new", "forge_mutex_new", &[], &[I64]),
        ("lock", "forge_mutex_lock", &[I64], &[]),
        ("forge_mutex_lock", "forge_mutex_lock", &[I64], &[]),
        ("unlock", "forge_mutex_unlock", &[I64], &[]),
        ("forge_mutex_unlock", "forge_mutex_unlock", &[I64], &[]),
        ("WaitGroup", "forge_waitgroup_new", &[], &[I64]),
        ("forge_waitgroup_new", "forge_waitgroup_new", &[], &[I64]),
        ("add", "forge_waitgroup_add", &[I64, I64], &[]),
        ("forge_waitgroup_add", "forge_waitgroup_add", &[I64, I64], &[]),
        ("done", "forge_waitgroup_done", &[I64], &[]),
        ("forge_waitgroup_done", "forge_waitgroup_done", &[I64], &[]),
        ("wait", "forge_waitgroup_wait", &[I64], &[]),
        ("forge_waitgroup_wait", "forge_waitgroup_wait", &[I64], &[]),
        ("Semaphore", "forge_semaphore_new", &[I64], &[I64]),
        ("forge_semaphore_new", "forge_semaphore_new", &[I64], &[I64]),
        ("acquire", "forge_semaphore_acquire", &[I64], &[]),
        ("forge_semaphore_acquire", "forge_semaphore_acquire", &[I64], &[]),
        ("release", "forge_semaphore_release", &[I64], &[]),
        ("forge_semaphore_release", "forge_semaphore_release", &[I64], &[]),
        ("forge_cstring_to_upper", "forge_cstring_to_upper", &[I64], &[I64]),
        ("forge_cstring_to_lower", "forge_cstring_to_lower", &[I64], &[I64]),
        ("forge_cstring_reverse", "forge_cstring_reverse", &[I64], &[I64]),
        ("forge_cstring_replace", "forge_cstring_replace", &[I64, I64, I64], &[I64]),
        ("list_is_empty", "forge_list_is_empty", &[I64], &[I64]),
        ("forge_list_is_empty", "forge_list_is_empty", &[I64], &[I64]),
        ("clear", "forge_list_clear_value", &[I64], &[]),
        ("list_clear", "forge_list_clear_value", &[I64], &[]),
        ("forge_list_clear", "forge_list_clear_value", &[I64], &[]),
        ("remove", "forge_list_remove_value", &[I64, I64], &[I64]),
        ("list_remove", "forge_list_remove_value", &[I64, I64], &[I64]),
        ("forge_list_remove", "forge_list_remove_value", &[I64, I64], &[I64]),
        ("list_reverse", "forge_list_reverse_value", &[I64], &[]),
        ("forge_list_reverse", "forge_list_reverse_value", &[I64], &[]),
        ("list_contains", "forge_list_contains_int", &[I64, I64], &[I64]),
        ("list_contains_string", "forge_list_contains_cstr", &[I64, I64], &[I64]),
        ("forge_list_contains_int", "forge_list_contains_int", &[I64, I64], &[I64]),
        ("forge_list_contains_cstr", "forge_list_contains_cstr", &[I64, I64], &[I64]),
        ("list_index_of", "forge_list_index_of_int", &[I64, I64], &[I64]),
        ("list_index_of_string", "forge_list_index_of_cstr", &[I64, I64], &[I64]),
        ("forge_list_index_of_int", "forge_list_index_of_int", &[I64, I64], &[I64]),
        ("forge_list_index_of_cstr", "forge_list_index_of_cstr", &[I64, I64], &[I64]),
        ("forge_list_sort", "forge_list_sort", &[I64], &[]),
        ("forge_list_sort_strings", "forge_list_sort_strings", &[I64], &[]),
        ("forge_list_slice", "forge_list_slice", &[I64, I64, I64], &[I64]),
        ("to_float", "forge_int_to_float", &[I64], &[F64]),
        ("forge_int_to_float", "forge_int_to_float", &[I64], &[F64]),
        ("to_int", "forge_float_to_int", &[F64], &[I64]),
        ("forge_float_to_int", "forge_float_to_int", &[F64], &[I64]),
        ("parse_int", "forge_parse_int", &[I64], &[I64]),
        ("forge_parse_int", "forge_parse_int", &[I64], &[I64]),
        ("parse_float", "forge_parse_float", &[I64], &[F64]),
        ("forge_parse_float", "forge_parse_float", &[I64], &[F64]),
        ("b64_encode", "forge_b64_encode", &[I64], &[I64]),
        ("encode", "forge_b64_encode", &[I64], &[I64]),
        ("forge_b64_encode", "forge_b64_encode", &[I64], &[I64]),
        ("forge_hex_encode", "forge_hex_encode", &[I64], &[I64]),
        ("hex", "forge_int_to_hex", &[I64], &[I64]),
        ("forge_int_to_hex", "forge_int_to_hex", &[I64], &[I64]),
        ("oct", "forge_int_to_oct", &[I64], &[I64]),
        ("forge_int_to_oct2", "forge_int_to_oct", &[I64], &[I64]),
        ("bin", "forge_int_to_bin", &[I64], &[I64]),
        ("forge_int_to_bin2", "forge_int_to_bin", &[I64], &[I64]),
        ("sha256", "forge_sha256", &[I64], &[I64]),
        ("forge_sha256", "forge_sha256", &[I64], &[I64]),
        ("format_time", "forge_format_time_fmt", &[I64, I64], &[I64]),
        ("forge_format_time", "forge_format_time_fmt", &[I64, I64], &[I64]),
        ("forge_format_time_fmt", "forge_format_time_fmt", &[I64, I64], &[I64]),
        ("write", "forge_fs_write", &[I64, I64], &[I64]),
        ("forge_fs_write", "forge_fs_write", &[I64, I64], &[I64]),
        ("info", "forge_log_info", &[I64], &[]),
        ("forge_log_info", "forge_log_info", &[I64], &[]),
        ("warn", "forge_log_warn", &[I64], &[]),
        ("forge_log_warn", "forge_log_warn", &[I64], &[]),
        ("forge_log_error", "forge_log_error", &[I64], &[]),
        ("dir", "forge_path_dir", &[I64], &[I64]),
        ("forge_path_dir", "forge_path_dir", &[I64], &[I64]),
        ("basename", "forge_path_basename", &[I64], &[I64]),
        ("forge_path_basename", "forge_path_basename", &[I64], &[I64]),
        ("forge_json_parse", "forge_json_parse", &[I64], &[I64]),
        ("forge_json_type_of", "forge_json_type_of", &[I64], &[I64]),
        ("forge_json_get_string", "forge_json_get_string", &[I64], &[I64]),
        ("forge_json_get_int", "forge_json_get_int", &[I64], &[I64]),
        ("forge_json_get_float", "forge_json_get_float", &[I64], &[F64]),
        ("forge_json_get_bool", "forge_json_get_bool", &[I64], &[I64]),
        ("forge_json_array_len", "forge_json_array_len", &[I64], &[I64]),
        ("forge_json_array_get", "forge_json_array_get", &[I64, I64], &[I64]),
        ("forge_json_object_get", "forge_json_object_get", &[I64, I64], &[I64]),
        ("forge_json_object_has", "forge_json_object_has", &[I64, I64], &[I64]),
        ("forge_json_object_keys", "forge_json_object_keys", &[I64], &[I64]),
        ("forge_json_encode", "forge_json_encode", &[I64], &[I64]),
        ("forge_spawn", "forge_spawn", &[I64], &[I64]),
        ("forge_await", "forge_await", &[I64], &[I64]),
        ("smart_to_string", "forge_smart_to_string", &[I64], &[I64]),
        ("forge_smart_to_string", "forge_smart_to_string", &[I64], &[I64]),
        ("identity", "forge_identity", &[I64], &[I64]),
        ("forge_identity", "forge_identity", &[I64], &[I64]),
        ("process_spawn", "forge_process_spawn", &[I64], &[I64]),
        ("forge_process_spawn", "forge_process_spawn", &[I64], &[I64]),
        ("process_spawn_argv", "forge_process_spawn_argv", &[I64, I64, I64, I64, I64], &[I64]),
        ("forge_process_spawn_argv", "forge_process_spawn_argv", &[I64, I64, I64, I64, I64], &[I64]),
        ("process_output_argv", "forge_process_output_argv", &[I64, I64, I64, I64, I64], &[I64]),
        ("forge_process_output_argv", "forge_process_output_argv", &[I64, I64, I64, I64, I64], &[I64]),
        ("exec_output", "forge_exec_output", &[I64], &[I64]),
        ("forge_exec_output", "forge_exec_output", &[I64], &[I64]),
        ("b64_decode", "forge_b64_decode", &[I64], &[I64]),
        ("fnv1a", "forge_fnv1a", &[I64], &[I64]),
        ("forge_fnv1a", "forge_fnv1a", &[I64], &[I64]),
        ("forge_cstring_index_of", "forge_cstring_index_of", &[I64, I64], &[I64]),
        ("forge_cstring_starts_with", "forge_cstring_starts_with", &[I64, I64], &[I64]),
        ("forge_cstring_ends_with", "forge_cstring_ends_with", &[I64, I64], &[I64]),
        ("pad_left", "forge_cstring_pad_left", &[I64, I64, I64], &[I64]),
        ("forge_cstring_pad_left", "forge_cstring_pad_left", &[I64, I64, I64], &[I64]),
        ("pad_right", "forge_cstring_pad_right", &[I64, I64, I64], &[I64]),
        ("forge_cstring_pad_right", "forge_cstring_pad_right", &[I64, I64, I64], &[I64]),
        ("forge_cstring_repeat", "forge_cstring_repeat", &[I64, I64], &[I64]),
        ("forge_cstring_chars", "forge_cstring_chars", &[I64], &[I64]),
        ("sort", "forge_list_sort", &[I64], &[]),
        ("slice", "forge_list_slice", &[I64, I64, I64], &[I64]),
        ("ext", "forge_path_ext", &[I64], &[I64]),
        ("extension", "forge_path_ext", &[I64], &[I64]),
        ("forge_path_ext", "forge_path_ext", &[I64], &[I64]),
        ("stem", "forge_path_stem", &[I64], &[I64]),
        ("forge_path_stem", "forge_path_stem", &[I64], &[I64]),
        ("join", "forge_path_join", &[I64, I64], &[I64]),
        ("forge_path_join", "forge_path_join", &[I64, I64], &[I64]),
        ("base", "forge_path_basename", &[I64], &[I64]),
        ("base_name", "forge_path_basename", &[I64], &[I64]),
        ("second", "forge_second", &[I64, I64], &[I64]),
        ("exists", "forge_path_exists", &[I64], &[I64]),
        ("forge_path_exists", "forge_path_exists", &[I64], &[I64]),
        ("process_read", "forge_process_read", &[I64, I64], &[I64]),
        ("process_read_err", "forge_process_read_err", &[I64, I64], &[I64]),
        ("process_write", "forge_process_write", &[I64, I64], &[I64]),
        ("process_read_bytes", "forge_process_read_bytes", &[I64, I64], &[I64]),
        ("process_read_err_bytes", "forge_process_read_err_bytes", &[I64, I64], &[I64]),
        ("process_write_bytes", "forge_process_write_bytes", &[I64, I64], &[I64]),
        ("tcp_listen", "forge_tcp_listen", &[I64, I64], &[I64]),
        ("tcp_connect", "forge_tcp_connect", &[I64, I64], &[I64]),
        ("tcp_accept", "forge_tcp_accept", &[I64], &[I64]),
        ("tcp_read", "forge_tcp_read", &[I64], &[I64]),
        ("tcp_write", "forge_tcp_write", &[I64, I64], &[I64]),
        ("tcp_read_bytes", "forge_tcp_read_bytes", &[I64, I64], &[I64]),
        ("tcp_write_bytes", "forge_tcp_write_bytes", &[I64, I64], &[I64]),
        ("tcp_wait_readable", "forge_tcp_wait_readable", &[I64, I64], &[I64]),
        ("tcp_wait_writable", "forge_tcp_wait_writable", &[I64, I64], &[I64]),
        ("tcp_close", "forge_tcp_close", &[I64], &[]),
        ("tcp_set_timeout", "forge_tcp_set_timeout", &[I64, I64], &[]),
        ("Channel", "forge_channel_new", &[I64], &[I64]),
        ("forge_channel_new", "forge_channel_new", &[I64], &[I64]),
        ("forge_channel_send", "forge_channel_send", &[I64, I64], &[I64]),
        ("forge_channel_try_send", "forge_channel_try_send", &[I64, I64], &[I64]),
        ("forge_channel_recv", "forge_channel_recv", &[I64], &[I64]),
        ("forge_channel_try_recv", "forge_channel_try_recv", &[I64], &[I64]),
        ("forge_channel_close", "forge_channel_close", &[I64], &[I64]),
        ("forge_channel_is_closed", "forge_channel_is_closed", &[I64], &[I64]),
        ("forge_select_next_index", "forge_select_next_index", &[I64], &[I64]),
        ("forge_channel_len", "forge_channel_len", &[I64], &[I64]),
        ("forge_channel_cap", "forge_channel_cap", &[I64], &[I64]),
        ("forge_task_is_done", "forge_task_is_done", &[I64], &[I64]),
        ("forge_task_detach", "forge_task_detach", &[I64], &[]),
        ("error", "forge_log_error", &[I64], &[]),
        ("debug", "forge_log_error", &[I64], &[]),
        ("to_hex", "forge_hex_encode", &[I64], &[I64]),
        ("from_hex", "forge_from_hex", &[I64], &[I64]),
        ("forge_from_hex", "forge_from_hex", &[I64], &[I64]),
        ("dns_resolve", "forge_dns_resolve", &[I64], &[I64]),
        ("float_fixed", "forge_float_fixed", &[F64, I64], &[I64]),
        ("is_dir", "forge_is_dir", &[I64], &[I64]),
        ("show", "forge_identity", &[I64], &[I64]),
        ("show_and_hash", "forge_identity", &[I64], &[I64]),
        ("forge_cstring_last_index_of", "forge_cstring_last_index_of", &[I64, I64], &[I64]),
        ("process_wait", "forge_process_wait", &[I64], &[I64]),
        ("process_kill", "forge_process_kill", &[I64], &[I64]),
        ("process_close", "forge_process_close", &[I64], &[]),
        ("process_output_status", "forge_process_output_status", &[I64], &[I64]),
        ("process_output_stdout", "forge_process_output_stdout", &[I64], &[I64]),
        ("process_output_stderr", "forge_process_output_stderr", &[I64], &[I64]),
        ("process_output_close", "forge_process_output_close", &[I64], &[]),
        ("url_scheme", "forge_url_scheme", &[I64], &[I64]),
        ("url_host", "forge_url_host", &[I64], &[I64]),
        ("url_port", "forge_url_port", &[I64], &[I64]),
        ("url_path", "forge_url_path", &[I64], &[I64]),
        ("url_query", "forge_url_query", &[I64], &[I64]),
        ("url_fragment", "forge_url_fragment", &[I64], &[I64]),
        ("url_encode", "forge_url_encode", &[I64], &[I64]),
        ("url_parse", "forge_url_parse", &[I64], &[I64]),
        ("url_decode", "forge_url_decode", &[I64], &[I64]),
        ("url_to_string", "forge_url_to_string", &[I64], &[I64]),
        ("tcp_read2", "forge_tcp_read2", &[I64, I64], &[I64]),
        ("forge_struct_alloc", "forge_struct_alloc", &[I64], &[I64]),
        ("insert", "forge_map_insert_cstr", &[I64, I64, I64], &[]),
        ("map_insert", "forge_map_insert_cstr", &[I64, I64, I64], &[]),
        ("forge_map_insert_cstr", "forge_map_insert_cstr", &[I64, I64, I64], &[]),
        ("map_insert_ikey", "forge_map_insert_ikey", &[I64, I64, I64], &[]),
        ("forge_map_insert_ikey", "forge_map_insert_ikey", &[I64, I64, I64], &[]),
        ("map_get", "forge_map_get_cstr", &[I64, I64], &[I64]),
        ("forge_map_get_cstr", "forge_map_get_cstr", &[I64, I64], &[I64]),
        ("map_get_ikey", "forge_map_get_ikey", &[I64, I64], &[I64]),
        ("forge_map_get_ikey", "forge_map_get_ikey", &[I64, I64], &[I64]),
        ("get_default", "forge_map_get_default_cstr", &[I64, I64, I64], &[I64]),
        ("map_get_default", "forge_map_get_default_cstr", &[I64, I64, I64], &[I64]),
        ("forge_map_get_default_cstr", "forge_map_get_default_cstr", &[I64, I64, I64], &[I64]),
        ("map_get_default_ikey", "forge_map_get_default_ikey", &[I64, I64, I64], &[I64]),
        ("forge_map_get_default_ikey", "forge_map_get_default_ikey", &[I64, I64, I64], &[I64]),
        ("contains_key", "forge_map_contains_cstr", &[I64, I64], &[I64]),
        ("map_contains_key", "forge_map_contains_cstr", &[I64, I64], &[I64]),
        ("forge_map_contains_cstr", "forge_map_contains_cstr", &[I64, I64], &[I64]),
        ("map_contains_ikey", "forge_map_contains_ikey", &[I64, I64], &[I64]),
        ("forge_map_contains_ikey", "forge_map_contains_ikey", &[I64, I64], &[I64]),
        ("keys", "forge_map_keys_cstr", &[I64], &[I64]),
        ("map_keys", "forge_map_keys_cstr", &[I64], &[I64]),
        ("forge_map_keys_cstr", "forge_map_keys_cstr", &[I64], &[I64]),
        ("map_remove", "forge_map_remove_cstr", &[I64, I64], &[]),
        ("forge_map_remove_cstr", "forge_map_remove_cstr", &[I64, I64], &[]),
        ("map_remove_ikey", "forge_map_remove_ikey", &[I64, I64], &[]),
        ("forge_map_remove_ikey", "forge_map_remove_ikey", &[I64, I64], &[]),
        ("map_len", "forge_map_len_handle", &[I64], &[I64]),
        ("forge_map_len_handle", "forge_map_len_handle", &[I64], &[I64]),
        ("map_clear", "forge_map_clear_handle", &[I64], &[]),
        ("forge_map_clear_handle", "forge_map_clear_handle", &[I64], &[]),
        ("map_is_empty", "forge_map_is_empty_handle", &[I64], &[I64]),
        ("forge_map_is_empty_handle", "forge_map_is_empty_handle", &[I64], &[I64]),
        ("map_values", "forge_map_values_handle", &[I64], &[I64]),
        ("forge_map_values_handle", "forge_map_values_handle", &[I64], &[I64]),
        ("forge_set_new_handle", "forge_set_new_handle", &[I32], &[I64]),
        ("forge_set_new_int", "forge_set_new_int", &[], &[I64]),
        ("set_len", "forge_set_len_handle", &[I64], &[I64]),
        ("forge_set_len_handle", "forge_set_len_handle", &[I64], &[I64]),
        ("set_add", "forge_set_add_cstr", &[I64, I64], &[I64]),
        ("forge_set_add_cstr", "forge_set_add_cstr", &[I64, I64], &[I64]),
        ("set_add_int", "forge_set_add_int_handle", &[I64, I64], &[I64]),
        (
            "forge_set_add_int_handle",
            "forge_set_add_int_handle",
            &[I64, I64],
            &[I64],
        ),
        ("set_contains", "forge_set_contains_cstr", &[I64, I64], &[I64]),
        ("forge_set_contains_cstr", "forge_set_contains_cstr", &[I64, I64], &[I64]),
        (
            "set_contains_int",
            "forge_set_contains_int_handle",
            &[I64, I64],
            &[I64],
        ),
        (
            "forge_set_contains_int_handle",
            "forge_set_contains_int_handle",
            &[I64, I64],
            &[I64],
        ),
        ("set_remove", "forge_set_remove_cstr", &[I64, I64], &[]),
        ("forge_set_remove_cstr", "forge_set_remove_cstr", &[I64, I64], &[]),
        ("set_remove_int", "forge_set_remove_int_handle", &[I64, I64], &[]),
        (
            "forge_set_remove_int_handle",
            "forge_set_remove_int_handle",
            &[I64, I64],
            &[],
        ),
        ("set_clear", "forge_set_clear_handle", &[I64], &[]),
        ("forge_set_clear_handle", "forge_set_clear_handle", &[I64], &[]),
        ("set_is_empty", "forge_set_is_empty_handle", &[I64], &[I64]),
        ("forge_set_is_empty_handle", "forge_set_is_empty_handle", &[I64], &[I64]),
        ("forge_set_to_list_cstr", "forge_set_to_list_cstr", &[I64], &[I64]),
        (
            "forge_set_to_list_int_handle",
            "forge_set_to_list_int_handle",
            &[I64],
            &[I64],
        ),
        ("forge_args_to_list", "forge_args_to_list", &[], &[I64]),
    ];

    // Declare each function and insert with its key
    let mut declared: HashMap<String, FuncId> = HashMap::new();
    for &(key, symbol, params, returns) in rt_table {
        let fid = if let Some(&existing) = declared.get(symbol) {
            existing
        } else {
            let fid = declare_runtime_function(module, symbol, params, returns)?;
            declared.insert(symbol.to_string(), fid);
            fid
        };
        funcs.insert(key.to_string(), fid);
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
