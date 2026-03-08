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

    // Pass 1: Declare all functions and tests
    let mut declared_funcs = HashMap::new();

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
                    params,
                    return_type,
                    body,
                    &runtime_funcs,
                    &declared_funcs,
                    &string_funcs,
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
    params: &[(String, String)],
    return_type: &str,
    body: &AstNode,
    runtime_funcs: &HashMap<String, FuncId>,
    declared_funcs: &HashMap<String, FuncId>,
    string_funcs: &HashMap<String, FuncId>,
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
        for (i, (param_name, param_ty)) in params.iter().enumerate() {
            let param_val = block_params[i];
            let ty = forge_type_to_cranelift(param_ty);

            // Create a variable for this parameter
            let var = next_variable();
            builder.declare_var(var, ty);
            builder.def_var(var, param_val);

            variables.insert(param_name.clone(), LocalVar { var, ty });
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

    codegen
        .module
        .define_function(func_id, &mut ctx)
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;

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
) -> Result<bool, CompileError> {
    match node {
        AstNode::Let { name, value } => {
            let val = compile_expr(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                value,
            )?;
            let ty = builder.func.dfg.value_type(val);

            // Create a new variable and declare it
            let var = next_variable();
            builder.declare_var(var, ty);
            builder.def_var(var, val);

            variables.insert(name.clone(), LocalVar { var, ty });
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
                )?;
                builder.ins().return_(&[val]);
            } else {
                let zero = builder.ins().iconst(return_type, 0);
                builder.ins().return_(&[zero]);
            }
            Ok(true)
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
                )?
            } else {
                false
            };
            if !else_filled {
                builder.ins().jump(merge_block, &[]);
            }
            builder.seal_block(else_block);

            // Continue after if
            builder.switch_to_block(merge_block);
            // Merge block will be sealed later when all predecessors are known

            Ok(false)
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

        AstNode::Assign { name, value } => {
            let val = compile_expr(
                builder,
                variables,
                runtime_funcs,
                declared_funcs,
                string_funcs,
                module,
                value,
            )?;
            // Update existing variable using def_var (Cranelift handles SSA)
            if let Some(var_info) = variables.get(name) {
                builder.def_var(var_info.var, val);
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
) -> Result<Value, CompileError> {
    match node {
        AstNode::IntLiteral(n) => Ok(builder.ins().iconst(types::I64, *n)),

        AstNode::FloatLiteral(f) => Ok(builder.ins().f64const(*f)),

        AstNode::StringLiteral(s) => {
            // Call the string data function to get the address
            if let Some(&str_func_id) = string_funcs.get(s) {
                let str_func_ref = module.declare_func_in_func(str_func_id, builder.func);
                let call = builder.ins().call(str_func_ref, &[]);
                Ok(builder.func.dfg.first_result(call))
            } else {
                // Fallback: return pointer directly (will segfault but compiles)
                let ptr = s.as_ptr() as i64;
                Ok(builder.ins().iconst(types::I64, ptr))
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

        AstNode::BinaryOp { op, left, right } => {
            // Check if this is string concatenation
            let is_string_op = matches!(left.as_ref(), AstNode::StringLiteral(_))
                || matches!(right.as_ref(), AstNode::StringLiteral(_));

            if is_string_op && matches!(op, BinaryOp::Add) {
                // String concatenation - for now just return left operand
                // (Proper implementation needs struct passing)
                let left_val = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    left,
                )?;
                let _right_val = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    right,
                )?;

                // Just return left for now
                Ok(left_val)
            } else {
                // Regular integer arithmetic
                let left_val = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    left,
                )?;
                let right_val = compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    right,
                )?;

                Ok(match op {
                    BinaryOp::Add => builder.ins().iadd(left_val, right_val),
                    BinaryOp::Sub => builder.ins().isub(left_val, right_val),
                    BinaryOp::Mul => builder.ins().imul(left_val, right_val),
                    BinaryOp::Div => builder.ins().sdiv(left_val, right_val),
                    BinaryOp::BitAnd => builder.ins().band(left_val, right_val),
                    BinaryOp::BitOr => builder.ins().bor(left_val, right_val),
                    BinaryOp::BitXor => builder.ins().bxor(left_val, right_val),
                    BinaryOp::Shl => builder.ins().ishl(left_val, right_val),
                    BinaryOp::Shr => builder.ins().sshr(left_val, right_val),
                    BinaryOp::Eq => {
                        let cmp = builder.ins().icmp(IntCC::Equal, left_val, right_val);
                        builder.ins().uextend(types::I64, cmp)
                    }
                    BinaryOp::Gt => {
                        let cmp = builder
                            .ins()
                            .icmp(IntCC::SignedGreaterThan, left_val, right_val);
                        builder.ins().uextend(types::I64, cmp)
                    }
                    BinaryOp::Lt => {
                        let cmp = builder
                            .ins()
                            .icmp(IntCC::SignedLessThan, left_val, right_val);
                        builder.ins().uextend(types::I64, cmp)
                    }
                    BinaryOp::Gte => {
                        let cmp = builder.ins().icmp(
                            IntCC::SignedGreaterThanOrEqual,
                            left_val,
                            right_val,
                        );
                        builder.ins().uextend(types::I64, cmp)
                    }
                    BinaryOp::Lte => {
                        let cmp =
                            builder
                                .ins()
                                .icmp(IntCC::SignedLessThanOrEqual, left_val, right_val);
                        builder.ins().uextend(types::I64, cmp)
                    }
                    _ => builder.ins().iconst(types::I64, 0),
                })
            }
        }

        AstNode::Call { func, args } => {
            // Use forge_print_cstr for all print calls (expects just a pointer)
            let func_name = if func == "print" {
                "forge_print_cstr"
            } else {
                match func.as_str() {
                    "print_int" => "forge_print_int",
                    _ => func,
                }
            };

            let func_id = runtime_funcs
                .get(func_name)
                .copied()
                .or_else(|| declared_funcs.get(func).copied())
                .ok_or_else(|| CompileError::UnknownFunction(func.clone()))?;

            let func_ref = module.declare_func_in_func(func_id, builder.func);

            let mut arg_values = Vec::new();
            for arg in args {
                arg_values.push(compile_expr(
                    builder,
                    variables,
                    runtime_funcs,
                    declared_funcs,
                    string_funcs,
                    module,
                    arg,
                )?);
            }

            let call = builder.ins().call(func_ref, &arg_values);

            if !builder.func.dfg.inst_results(call).is_empty() {
                Ok(builder.func.dfg.first_result(call))
            } else {
                Ok(builder.ins().iconst(types::I64, 0))
            }
        }

        _ => Err(CompileError::UnsupportedFeature(format!("{:?}", node))),
    }
}
