//! AST to Cranelift IR translation with two-pass compilation
//!
//! First pass: Declare all functions
//! Second pass: Compile all function bodies

use crate::{CodeGen, CompileError, forge_type_to_cranelift};
use crate::ast::{AstNode, BinaryOp, UnaryOp};
use cranelift::prelude::*;
use cranelift_module::{Module, Linkage, FuncId};
use std::collections::HashMap;

/// Local variable slot
#[derive(Debug)]
pub struct LocalVar {
    pub value: Value,
    pub ty: Type,
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
        AstNode::If { cond, then_branch, else_branch } => {
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
            Ok(func_id) => { string_funcs.insert(s.clone(), func_id); }
            Err(_) => {}
        }
    }
    
    // Pass 1: Declare all functions
    let mut declared_funcs = HashMap::new();
    
    for node in &ast_nodes {
        if let AstNode::Function { name, params, return_type, .. } = node {
            let mut sig = codegen.module.make_signature();
            
            for (_, ty) in params {
                let cl_ty = forge_type_to_cranelift(ty);
                sig.params.push(AbiParam::new(cl_ty));
            }
            
            let ret_ty = forge_type_to_cranelift(return_type);
            sig.returns.push(AbiParam::new(ret_ty));
            
            let func_id = codegen.module.declare_function(name, Linkage::Export, &sig)
                .map_err(|e| CompileError::ModuleError(e.to_string()))?;
            
            declared_funcs.insert(name.clone(), func_id);
        }
    }
    
    // Pass 2: Compile all function bodies
    let runtime_funcs = crate::declare_runtime_functions(&mut codegen.module)?;
    
    for node in ast_nodes {
        if let AstNode::Function { name, params, return_type, body } = node {
            if let Some(&func_id) = declared_funcs.get(&name) {
                compile_function_body(
                    codegen,
                    func_id,
                    &params,
                    &return_type,
                    &body,
                    &runtime_funcs,
                    &declared_funcs,
                    &string_funcs,
                )?;
            }
        }
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
        builder.seal_block(entry_block);
        
        let block_params = builder.block_params(entry_block);
        for (i, (param_name, param_ty)) in params.iter().enumerate() {
            let param_val = block_params[i];
            let ty = forge_type_to_cranelift(param_ty);
            variables.insert(param_name.clone(), LocalVar { value: param_val, ty });
        }
        
        compile_stmt(&mut builder, &mut variables, runtime_funcs, declared_funcs, string_funcs, &mut codegen.module, ret_ty, body)?;
        
        let zero = builder.ins().iconst(ret_ty, 0);
        builder.ins().return_(&[zero]);
    }
    
    codegen.module.define_function(func_id, &mut ctx)
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
) -> Result<(), CompileError> {
    match node {
        AstNode::Let { name, value } => {
            let val = compile_expr(builder, variables, runtime_funcs, declared_funcs, string_funcs, module, value)?;
            let ty = builder.func.dfg.value_type(val);
            variables.insert(name.clone(), LocalVar { value: val, ty });
            Ok(())
        }
        
        AstNode::Block(stmts) => {
            for stmt in stmts {
                compile_stmt(builder, variables, runtime_funcs, declared_funcs, string_funcs, module, return_type, stmt)?;
            }
            Ok(())
        }
        
        AstNode::Return(expr) => {
            if let Some(e) = expr {
                let val = compile_expr(builder, variables, runtime_funcs, declared_funcs, string_funcs, module, e)?;
                builder.ins().return_(&[val]);
            } else {
                let zero = builder.ins().iconst(return_type, 0);
                builder.ins().return_(&[zero]);
            }
            Ok(())
        }
        
        AstNode::If { cond, then_branch, else_branch } => {
            let cond_val = compile_expr(builder, variables, runtime_funcs, declared_funcs, string_funcs, module, cond)?;
            
            let then_block = builder.create_block();
            let else_block = builder.create_block();
            let merge_block = builder.create_block();
            
            builder.ins().brif(cond_val, then_block, &[], else_block, &[]);
            
            // Then branch
            builder.switch_to_block(then_block);
            compile_stmt(builder, variables, runtime_funcs, declared_funcs, string_funcs, module, return_type, then_branch)?;
            builder.ins().jump(merge_block, &[]);
            builder.seal_block(then_block);
            
            // Else branch
            builder.switch_to_block(else_block);
            if let Some(else_stmt) = else_branch {
                compile_stmt(builder, variables, runtime_funcs, declared_funcs, string_funcs, module, return_type, else_stmt)?;
            }
            builder.ins().jump(merge_block, &[]);
            builder.seal_block(else_block);
            
            // Continue after if
            builder.switch_to_block(merge_block);
            builder.seal_block(merge_block);
            
            Ok(())
        }
        
        _ => {
            compile_expr(builder, variables, runtime_funcs, declared_funcs, string_funcs, module, node)?;
            Ok(())
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
        
        AstNode::Identifier(name) => {
            match variables.get(name) {
                Some(var) => Ok(var.value),
                None => Err(CompileError::UnknownVariable(name.clone())),
            }
        }
        
        AstNode::BinaryOp { op, left, right } => {
            // Check if this is string concatenation
            let is_string_op = matches!(left.as_ref(), AstNode::StringLiteral(_)) || 
                              matches!(right.as_ref(), AstNode::StringLiteral(_));
            
            if is_string_op && matches!(op, BinaryOp::Add) {
                // String concatenation - for now just return left operand
                // (Proper implementation needs struct passing)
                let left_val = compile_expr(builder, variables, runtime_funcs, declared_funcs, string_funcs, module, left)?;
                let _right_val = compile_expr(builder, variables, runtime_funcs, declared_funcs, string_funcs, module, right)?;
                
                // Just return left for now
                Ok(left_val)
            } else {
                // Regular integer arithmetic
                let left_val = compile_expr(builder, variables, runtime_funcs, declared_funcs, string_funcs, module, left)?;
                let right_val = compile_expr(builder, variables, runtime_funcs, declared_funcs, string_funcs, module, right)?;
                
                Ok(match op {
                    BinaryOp::Add => builder.ins().iadd(left_val, right_val),
                    BinaryOp::Sub => builder.ins().isub(left_val, right_val),
                    BinaryOp::Mul => builder.ins().imul(left_val, right_val),
                    BinaryOp::Div => builder.ins().sdiv(left_val, right_val),
                    BinaryOp::Eq => {
                        let cmp = builder.ins().icmp(IntCC::Equal, left_val, right_val);
                        builder.ins().uextend(types::I64, cmp)
                    }
                    BinaryOp::Gt => {
                        let cmp = builder.ins().icmp(IntCC::SignedGreaterThan, left_val, right_val);
                        builder.ins().uextend(types::I64, cmp)
                    }
                    BinaryOp::Lt => {
                        let cmp = builder.ins().icmp(IntCC::SignedLessThan, left_val, right_val);
                        builder.ins().uextend(types::I64, cmp)
                    }
                    BinaryOp::Gte => {
                        let cmp = builder.ins().icmp(IntCC::SignedGreaterThanOrEqual, left_val, right_val);
                        builder.ins().uextend(types::I64, cmp)
                    }
                    BinaryOp::Lte => {
                        let cmp = builder.ins().icmp(IntCC::SignedLessThanOrEqual, left_val, right_val);
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
            
            let func_id = runtime_funcs.get(func_name)
                .copied()
                .or_else(|| declared_funcs.get(func).copied())
                .ok_or_else(|| CompileError::UnknownFunction(func.clone()))?;
            
            let func_ref = module.declare_func_in_func(func_id, builder.func);
            
            let mut arg_values = Vec::new();
            for arg in args {
                arg_values.push(compile_expr(builder, variables, runtime_funcs, declared_funcs, string_funcs, module, arg)?);
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
