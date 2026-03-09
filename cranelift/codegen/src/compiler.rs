//! AST to Cranelift IR translation with two-pass compilation
//!
//! First pass: Declare all functions
//! Second pass: Compile all function bodies

use crate::ast::{AstNode, BinaryOp, UnaryOp};
use crate::{forge_type_to_cranelift, CodeGen, CompileError};
use cranelift::prelude::*;
use cranelift_module::{FuncId, Linkage, Module};
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

fn infer_kind_from_type_name(ty: &str) -> ValueKind {
    match ty {
        "String" => ValueKind::String,
        "Bool" => ValueKind::Bool,
        "Int" | "Float" => ValueKind::Int,
        _ if ty.starts_with("List[String]") => ValueKind::ListString,
        _ if ty.starts_with("List[") => ValueKind::ListUnknown,
        _ => ValueKind::Unknown,
    }
}

fn infer_value_kind(node: &AstNode, variables: &HashMap<String, LocalVar>) -> ValueKind {
    match node {
        AstNode::StringLiteral(_) | AstNode::StringInterp { .. } => ValueKind::String,
        AstNode::BoolLiteral(_) => ValueKind::Bool,
        AstNode::IntLiteral(_) | AstNode::FloatLiteral(_) | AstNode::BinaryOp { .. } => {
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
        AstNode::Identifier(name) => variables
            .get(name)
            .map(|v| v.kind)
            .unwrap_or(ValueKind::Unknown),
        AstNode::FieldAccess { field, .. } => match field.as_str() {
            ".children" | ".param_types" => ValueKind::ListUnknown,
            ".value" | ".kind" | ".name" | ".doc" | ".sig" | ".path" => ValueKind::String,
            _ => ValueKind::Unknown,
        },
        AstNode::Index { expr, .. } => match infer_value_kind(expr, variables) {
            ValueKind::String | ValueKind::ListString => ValueKind::String,
            _ => ValueKind::Unknown,
        },
        AstNode::Call { func, .. } => match func.as_str() {
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
            | "convert_path_to_module" => ValueKind::String,
            "split" | "args" | "keys" | "values" | "list_dir" => ValueKind::ListString,
            "len" | "time" | "random_int" | "ord" => ValueKind::Int,
            "contains" | "contains_key" | "starts_with" | "ends_with" | "string_starts_with"
            | "dir_exists" | "file_exists" => ValueKind::Bool,
            _ => ValueKind::Unknown,
        },
        _ => ValueKind::Unknown,
    }
}

/// Collect all string literals from AST
fn collect_strings(node: &AstNode, strings: &mut Vec<String>) {
    match node {
        AstNode::StringLiteral(s) => strings.push(s.clone()),
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

/// Compile all functions from AST with two-pass approach
pub fn compile_module(
    codegen: &mut CodeGen,
    ast_nodes: Vec<AstNode>,
) -> Result<HashMap<String, FuncId>, CompileError> {
    // Collect all string literals first
    let mut all_strings = Vec::new();
    for node in &ast_nodes {
        if let AstNode::Function { body, .. } = node {
            collect_strings(body, &mut all_strings);
        }
    }

    // Declare string data
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

    // Pass 1: Declare all functions and tests
    let mut declared_funcs = HashMap::new();
    let mut func_signatures: HashMap<String, Vec<Type>> = HashMap::new();

    for node in &ast_nodes {
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
            _ => {}
        }
    }

    // Collect test names before consuming ast_nodes
    let test_names: Vec<String> = ast_nodes
        .iter()
        .filter_map(|node| {
            if let AstNode::Test { name, .. } = node {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect();

    // Collect global variable names (top-level bind/Let nodes)
    let global_vars: Vec<String> = ast_nodes
        .iter()
        .filter_map(|node| {
            if let AstNode::Let { name, .. } = node {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect();

    // Pass 2: Compile all function and test bodies
    let runtime_funcs = crate::declare_runtime_functions(&mut codegen.module)?;

    for node in &ast_nodes {
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
                    &global_vars,
                    &func_signatures,
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
                )?;
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
    global_vars: &[String],
    func_signatures: &HashMap<String, Vec<Type>>,
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

        // Add global variables as placeholders (initialized to 0)
        for global_name in global_vars {
            let var = next_variable();
            builder.declare_var(var, types::I64);
            let zero = builder.ins().iconst(types::I64, 0);
            builder.def_var(var, zero);
            variables.insert(
                global_name.clone(),
                LocalVar {
                    var,
                    ty: types::I64,
                    kind: ValueKind::Unknown,
                },
            );
        }

        // Collect block params into a Vec to avoid borrow issues
        let block_params: Vec<Value> = builder.block_params(entry_block).to_vec();
        for (i, (param_name, param_ty)) in params.iter().enumerate() {
            let param_val = block_params[i];
            let ty = forge_type_to_cranelift(param_ty);

            // Create a variable for this parameter
            let var = next_variable();
            builder.declare_var(var, ty);
            builder.def_var(var, param_val);

            let kind = infer_kind_from_type_name(param_ty);
            variables.insert(param_name.clone(), LocalVar { var, ty, kind });
        }

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
        )?;

        // Try to add return if the body compilation didn't fill a block
        // Note: If the entry block was filled (e.g., by a while loop jump),
        // we need to create a new block for the return
        if !filled {
            let current = builder.current_block().unwrap();

            // Create a new block for the return to avoid filled block issues
            let return_block = builder.create_block();
            builder.ins().jump(return_block, &[]);
            builder.switch_to_block(return_block);

            let zero = builder.ins().iconst(ret_ty, 0);
            builder.ins().return_(&[zero]);
        }

        // Seal all blocks to complete SSA construction
        builder.seal_all_blocks();
    }

    eprintln!("DEBUG: Defining '{}'", func_name);
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
) -> Result<bool, CompileError> {
    match node {
        AstNode::Let { name, value, .. } => {
            let val = compile_expr(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                value,
                func_signatures,
            )?;
            let ty = builder.func.dfg.value_type(val);

            // Create a new variable and declare it
            let var = next_variable();
            builder.declare_var(var, ty);
            builder.def_var(var, val);

            let kind = infer_value_kind(value, variables);
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
            iterable,
            body,
        } => {
            // Simplified for loop using index-based iteration

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
            )?;

            // Get list length
            let len_func_id = runtime_funcs
                .get("forge_list_len")
                .ok_or_else(|| CompileError::UnknownFunction("forge_list_len".to_string()))?;
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

            // Create loop variable with current index as placeholder
            let loop_var = next_variable();
            builder.declare_var(loop_var, types::I64);
            let cur_idx = builder.use_var(idx_var);
            builder.def_var(loop_var, cur_idx);

            // Add to scope
            let var_info = LocalVar {
                var: loop_var,
                ty: types::I64,
                kind: ValueKind::Unknown,
            };
            variables.insert(var.clone(), var_info);

            // Compile body
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
                Some(header),
                Some(exit),
                func_signatures,
            )?;

            variables.remove(var);

            // Loop back with incremented index
            if !filled {
                let cur_idx_2 = builder.use_var(idx_var);
                let next_idx = builder.ins().iadd_imm(cur_idx_2, 1);
                builder.def_var(idx_var, next_idx);
                builder.ins().jump(header, &[]);
            }

            builder.seal_block(body_block);
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
            )?;
            // Update existing variable using def_var (Cranelift handles SSA)
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
            } else {
                Err(CompileError::UnknownVariable(name.clone()))
            }
        }

        AstNode::Import { .. } => {
            // Import statements are handled at module level, not in function body
            // For now, just skip them
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
            )?;
            Ok(false)
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
) -> Result<Value, CompileError> {
    match node {
        AstNode::IntLiteral(n) => Ok(builder.ins().iconst(types::I64, *n)),

        AstNode::FloatLiteral(f) => Ok(builder.ins().f64const(*f)),

        AstNode::BoolLiteral(b) => Ok(builder.ins().iconst(types::I64, if *b { 1 } else { 0 })),

        AstNode::StringLiteral(s) => {
            // Call the string data function to get the address
            // For now, just return the pointer directly (using simple strlen for .len())
            let ptr_val = if let Some(&str_func_id) = string_funcs.get(s.as_str()) {
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

        AstNode::StringInterp { parts: _ } => {
            // For now, return empty string (placeholder)
            // Full string interpolation needs proper string struct handling
            let empty_str = builder.ins().iconst(types::I64, 0);
            Ok(empty_str)
        }

        AstNode::StructInit { name: _, fields: _ } => {
            // For now, return 0 (placeholder)
            // Full struct initialization requires:
            // 1. Type layout information
            // 2. Memory allocation for the struct
            // 3. Field value assignment
            let struct_val = builder.ins().iconst(types::I64, 0);
            Ok(struct_val)
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
                )?;
                builder.ins().call(push_ref, &[list_val, elem_val]);
            }

            // Return the list VALUE
            Ok(list_val)
        }

        AstNode::MapLiteral {
            entries,
            key_type: _,
            val_type: _,
        } => {
            // Create a new map
            // For simplicity, assume string keys and int values for now
            let key_type = 1i32; // String key type
            let val_size = 8i64; // sizeof(i64) for Int values
            let val_is_heap = 0i8; // false

            // Call forge_map_new(key_type, val_size, val_is_heap)
            let map_new_func = runtime_funcs
                .get("forge_map_new")
                .ok_or_else(|| CompileError::UnknownFunction("forge_map_new".to_string()))?;
            let map_new_ref = module.declare_func_in_func(*map_new_func, builder.func);
            let key_type_val = builder.ins().iconst(types::I32, key_type as i64);
            let val_size_val = builder.ins().iconst(types::I64, val_size);
            let val_is_heap_val = builder.ins().iconst(types::I8, val_is_heap as i64);
            let new_call = builder
                .ins()
                .call(map_new_ref, &[key_type_val, val_size_val, val_is_heap_val]);
            let map_val = builder.func.dfg.first_result(new_call);

            // Create a stack slot for the map so we can pass its address to insert
            let map_slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8, // ForgeMap is 8 bytes (one pointer)
                3, // align_shift = 8 bytes
            ));
            let map_slot_addr = builder.ins().stack_addr(types::I64, map_slot, 0);
            builder
                .ins()
                .store(MemFlags::new(), map_val, map_slot_addr, 0);
            let map_ptr = map_slot_addr;

            // Insert each entry
            let insert_func = runtime_funcs
                .get("forge_map_insert_int") // For now use int key version
                .ok_or_else(|| CompileError::UnknownFunction("forge_map_insert_int".to_string()))?;
            let insert_ref = module.declare_func_in_func(*insert_func, builder.func);

            for (key, value) in entries {
                // Compile key (for now only support string keys)
                let key_val = match key {
                    AstNode::StringLiteral(s) => {
                        // Get string pointer
                        if let Some(&str_func_id) = string_funcs.get(s.as_str()) {
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

                // Compile value
                let val_val = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    value,
                    func_signatures,
                )?;

                // Store value to stack
                let val_slot = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    8,
                    3,
                ));
                let val_ptr = builder.ins().stack_addr(types::I64, val_slot, 0);
                builder.ins().store(MemFlags::new(), val_val, val_ptr, 0);

                // Call insert: forge_map_insert_int(map_ptr, key, val_ptr, val_size)
                builder
                    .ins()
                    .call(insert_ref, &[map_ptr, key_val, val_ptr, val_size_val]);
            }

            Ok(map_val)
        }

        AstNode::Try { expr } => {
            // For now, just compile the inner expression
            // Full error propagation requires complex control flow
            compile_expr(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                expr,
                func_signatures,
            )
        }

        AstNode::Fail { error } => {
            // For now, just compile the error expression
            // Full fail implementation requires return type checking
            compile_expr(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                error,
                func_signatures,
            )
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
            // Compile the object expression
            let obj_val = compile_expr(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                obj,
                func_signatures,
            )?;

            // For now, just return the object value (placeholder)
            // Full field access requires:
            // 1. Type information to know field offsets
            // 2. Proper struct layout
            // 3. Load instruction with offset

            // Remove leading dot from field name if present
            let field_name = if field.starts_with('.') {
                &field[1..]
            } else {
                field.as_str()
            };

            // For now, return the object value (will need proper offset calculation)
            // In a full implementation, we would:
            // - Look up the type of obj
            // - Find the field offset in the type layout
            // - Use builder.ins().load() with the offset

            // Placeholder: return 0 for most fields
            match field_name {
                "len" => {
                    // Special case: try to call .len() method
                    if let Some(&len_func_id) = runtime_funcs.get("forge_list_len") {
                        let len_func_ref = module.declare_func_in_func(len_func_id, builder.func);
                        let call = builder.ins().call(len_func_ref, &[obj_val]);
                        Ok(builder.func.dfg.first_result(call))
                    } else {
                        Ok(builder.ins().iconst(types::I64, 0))
                    }
                }
                _ => Ok(builder.ins().iconst(types::I64, 0)),
            }
        }

        AstNode::Identifier(name) => match variables.get(name) {
            Some(var_info) => {
                // Use Cranelift's Variable system to get the current value
                let val = builder.use_var(var_info.var);
                Ok(val)
            }
            None => Err(CompileError::UnknownVariable(name.clone())),
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
            let is_string_op = matches!(left.as_ref(), AstNode::StringLiteral(_))
                || matches!(right.as_ref(), AstNode::StringLiteral(_));

            if is_string_op && matches!(op, BinaryOp::Add) {
                // String concatenation - for now just return left operand
                // (Proper implementation needs struct passing)
                let mut left_val = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    left,
                    func_signatures,
                )?;
                let _right_val = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    right,
                    func_signatures,
                )?;

                // Just return left for now
                Ok(left_val)
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
                        if is_float {
                            let cmp = builder.ins().fcmp(FloatCC::Equal, left_val, right_val);
                            builder.ins().uextend(types::I64, cmp)
                        } else {
                            let cmp = builder.ins().icmp(IntCC::Equal, left_val, right_val);
                            builder.ins().uextend(types::I64, cmp)
                        }
                    }
                    BinaryOp::Gt => {
                        if is_float {
                            let cmp = builder
                                .ins()
                                .fcmp(FloatCC::GreaterThan, left_val, right_val);
                            builder.ins().uextend(types::I64, cmp)
                        } else {
                            let cmp =
                                builder
                                    .ins()
                                    .icmp(IntCC::SignedGreaterThan, left_val, right_val);
                            builder.ins().uextend(types::I64, cmp)
                        }
                    }
                    BinaryOp::Lt => {
                        if is_float {
                            let cmp = builder.ins().fcmp(FloatCC::LessThan, left_val, right_val);
                            builder.ins().uextend(types::I64, cmp)
                        } else {
                            let cmp =
                                builder
                                    .ins()
                                    .icmp(IntCC::SignedLessThan, left_val, right_val);
                            builder.ins().uextend(types::I64, cmp)
                        }
                    }
                    BinaryOp::Gte => {
                        if is_float {
                            let cmp = builder.ins().fcmp(
                                FloatCC::GreaterThanOrEqual,
                                left_val,
                                right_val,
                            );
                            builder.ins().uextend(types::I64, cmp)
                        } else {
                            let cmp = builder.ins().icmp(
                                IntCC::SignedGreaterThanOrEqual,
                                left_val,
                                right_val,
                            );
                            builder.ins().uextend(types::I64, cmp)
                        }
                    }
                    BinaryOp::Lte => {
                        if is_float {
                            let cmp =
                                builder
                                    .ins()
                                    .fcmp(FloatCC::LessThanOrEqual, left_val, right_val);
                            builder.ins().uextend(types::I64, cmp)
                        } else {
                            let cmp = builder.ins().icmp(
                                IntCC::SignedLessThanOrEqual,
                                left_val,
                                right_val,
                            );
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
                } else if is_string_literal || is_string_like {
                    // Check for string method calls on string literals or field accesses
                    let string_methods = [
                        "len",
                        "contains",
                        "substring",
                        "trim",
                        "starts_with",
                        "ends_with",
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

                        // String methods that return strings need special handling
                        // (they require output pointer). For now, return placeholder.
                        let string_return_methods = ["substring", "trim"];
                        if string_return_methods.contains(&func.as_str()) {
                            // These methods need proper struct return handling
                            // For now, return the original string as placeholder
                            return Ok(string_val);
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
                    ];
                    if string_methods.contains(&func.as_str()) {
                        let ident_name = match &args[0] {
                            AstNode::Identifier(name) => name,
                            _ => "",
                        };
                        let ident_kind = variables
                            .get(ident_name)
                            .map(|v| v.kind)
                            .unwrap_or(ValueKind::Unknown);

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
                            )?;

                            if func == "len" {
                                if let Some(&len_func_id) = runtime_funcs.get("forge_cstring_len") {
                                    let len_func_ref =
                                        module.declare_func_in_func(len_func_id, builder.func);
                                    let call = builder.ins().call(len_func_ref, &[string_val]);
                                    return Ok(builder.func.dfg.first_result(call));
                                }
                            }

                            let string_return_methods = ["substring", "trim"];
                            if string_return_methods.contains(&func.as_str()) {
                                return Ok(string_val);
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

            // Check for list method calls (includes fallback for variables)
            let list_methods = ["len", "push", "pop", "get", "set", "join", "remove"];
            if list_methods.contains(&func.as_str()) && !args.is_empty() {
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
                    for arg in &args[1..] {
                        let _ = compile_expr(
                            builder,
                            variables,
                            runtime_funcs,
                            declared_funcs,
                            string_funcs,
                            module,
                            arg,
                            func_signatures,
                        )?;
                    }
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
                        )?
                    } else {
                        builder.ins().iconst(types::I64, 0)
                    };
                    let call = builder.ins().call(join_ref, &[list_val, sep_val]);
                    return Ok(builder.func.dfg.first_result(call));
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
            let map_methods = [
                "len",
                "contains",
                "contains_key",
                "keys",
                "values",
                "insert",
                "remove",
            ];
            if map_methods.contains(&func.as_str()) && !args.is_empty() {
                // Transform map.method(args) to forge_map_method(map, args)
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
                )?;

                if func == "insert" || func == "contains_key" || func == "remove" {
                    for arg in &args[1..] {
                        let _ = compile_expr(
                            builder,
                            variables,
                            runtime_funcs,
                            declared_funcs,
                            string_funcs,
                            module,
                            arg,
                            func_signatures,
                        )?;
                    }
                    let _ = map_val;
                    return Ok(builder.ins().iconst(types::I64, 0));
                }

                if func == "keys" || func == "values" {
                    let list_new_id = runtime_funcs.get("forge_list_new").ok_or_else(|| {
                        CompileError::UnknownFunction("forge_list_new".to_string())
                    })?;
                    let list_new_ref = module.declare_func_in_func(*list_new_id, builder.func);
                    let elem_size = builder.ins().iconst(types::I64, 8);
                    let type_tag = builder.ins().iconst(types::I32, 0);
                    let call = builder.ins().call(list_new_ref, &[elem_size, type_tag]);
                    return Ok(builder.func.dfg.first_result(call));
                }

                let runtime_func_name = format!("forge_map_{}", func);
                if let Some(&func_id) = runtime_funcs.get(&runtime_func_name) {
                    let func_ref = module.declare_func_in_func(func_id, builder.func);

                    // Compile additional args (if any)
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

            // Handle print function selection based on argument type
            let (func_name, arg_values) = if func == "print" && !args.is_empty() {
                let arg_kind = infer_value_kind(&args[0], variables);
                let looks_numeric = matches!(
                    &args[0],
                    AstNode::IntLiteral(_)
                        | AstNode::BoolLiteral(_)
                        | AstNode::FloatLiteral(_)
                        | AstNode::BinaryOp { .. }
                        | AstNode::UnaryOp { .. }
                        | AstNode::Index { .. }
                ) || matches!(&args[0], AstNode::Call { func, .. } if func == "len")
                    || matches!(arg_kind, ValueKind::Int | ValueKind::Bool);

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
                    )?);
                }

                // Choose print function based on argument type
                if looks_numeric || first_arg_ty == types::F64 {
                    ("forge_print_int", all_args)
                } else {
                    ("forge_print_cstr", all_args)
                }
            } else {
                // Original logic for other functions
                let fname = match func.as_str() {
                    "print" => "forge_print_cstr",
                    "print_int" => "forge_print_int",
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
                    )?);
                }
                (fname, avals)
            };

            let Some(func_id) = runtime_funcs
                .get(func_name)
                .copied()
                .or_else(|| declared_funcs.get(func).copied())
            else {
                if func
                    .chars()
                    .next()
                    .map(|c| c.is_ascii_uppercase())
                    .unwrap_or(false)
                {
                    // Placeholder for struct constructors like Diagnostic(...)
                    return Ok(builder.ins().iconst(types::I64, 0));
                }
                return Err(CompileError::UnknownFunction(func.clone()));
            };

            let func_ref = module.declare_func_in_func(func_id, builder.func);

            let coerced_args = if let Some(param_types) = func_signatures.get(func) {
                let mut coerced = Vec::with_capacity(param_types.len());
                for (i, &target_ty) in param_types.iter().enumerate() {
                    let val = if let Some(&existing) = arg_values.get(i) {
                        let val_ty = builder.func.dfg.value_type(existing);
                        if val_ty == target_ty {
                            existing
                        } else if val_ty.bits() > target_ty.bits() {
                            builder.ins().ireduce(target_ty, existing)
                        } else {
                            builder.ins().uextend(target_ty, existing)
                        }
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

        _ => Err(CompileError::UnsupportedFeature(format!("{:?}", node))),
    }
}
