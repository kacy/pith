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

/// Compile all functions from AST with two-pass approach
pub fn compile_module(
    codegen: &mut CodeGen,
    ast_nodes: Vec<AstNode>,
) -> Result<HashMap<String, FuncId>, CompileError> {
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
        
        compile_stmt(&mut builder, &mut variables, runtime_funcs, declared_funcs, &mut codegen.module, ret_ty, body)?;
        
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
    module: &mut dyn Module,
    return_type: Type,
    node: &AstNode,
) -> Result<(), CompileError> {
    match node {
        AstNode::Let { name, value } => {
            let val = compile_expr(builder, variables, runtime_funcs, declared_funcs, module, value)?;
            let ty = builder.func.dfg.value_type(val);
            variables.insert(name.clone(), LocalVar { value: val, ty });
            Ok(())
        }
        
        AstNode::Block(stmts) => {
            for stmt in stmts {
                compile_stmt(builder, variables, runtime_funcs, declared_funcs, module, return_type, stmt)?;
            }
            Ok(())
        }
        
        AstNode::Return(expr) => {
            if let Some(e) = expr {
                let val = compile_expr(builder, variables, runtime_funcs, declared_funcs, module, e)?;
                builder.ins().return_(&[val]);
            } else {
                let zero = builder.ins().iconst(return_type, 0);
                builder.ins().return_(&[zero]);
            }
            Ok(())
        }
        
        _ => {
            compile_expr(builder, variables, runtime_funcs, declared_funcs, module, node)?;
            Ok(())
        }
    }
}

fn compile_expr(
    builder: &mut FunctionBuilder,
    variables: &mut HashMap<String, LocalVar>,
    runtime_funcs: &HashMap<String, FuncId>,
    declared_funcs: &HashMap<String, FuncId>,
    module: &mut dyn Module,
    node: &AstNode,
) -> Result<Value, CompileError> {
    match node {
        AstNode::IntLiteral(n) => Ok(builder.ins().iconst(types::I64, *n)),
        
        AstNode::Identifier(name) => {
            match variables.get(name) {
                Some(var) => Ok(var.value),
                None => Err(CompileError::UnknownVariable(name.clone())),
            }
        }
        
        AstNode::BinaryOp { op, left, right } => {
            let left_val = compile_expr(builder, variables, runtime_funcs, declared_funcs, module, left)?;
            let right_val = compile_expr(builder, variables, runtime_funcs, declared_funcs, module, right)?;
            
            Ok(match op {
                BinaryOp::Add => builder.ins().iadd(left_val, right_val),
                BinaryOp::Sub => builder.ins().isub(left_val, right_val),
                BinaryOp::Mul => builder.ins().imul(left_val, right_val),
                BinaryOp::Div => builder.ins().sdiv(left_val, right_val),
                BinaryOp::Eq => {
                    let cmp = builder.ins().icmp(IntCC::Equal, left_val, right_val);
                    builder.ins().uextend(types::I64, cmp)
                }
                _ => builder.ins().iconst(types::I64, 0),
            })
        }
        
        AstNode::Call { func, args } => {
            let func_id = if func.starts_with('.') {
                // Method call - not supported yet
                return Err(CompileError::UnsupportedFeature(format!("method {}", func)));
            } else {
                let runtime_name = match func.as_str() {
                    "print" => "forge_print",
                    "print_int" => "forge_print_int",
                    _ => func,
                };
                
                runtime_funcs.get(runtime_name)
                    .copied()
                    .or_else(|| declared_funcs.get(func).copied())
                    .ok_or_else(|| CompileError::UnknownFunction(func.clone()))?
            };
            
            let func_ref = module.declare_func_in_func(func_id, builder.func);
            
            let mut arg_values = Vec::new();
            for arg in args {
                arg_values.push(compile_expr(builder, variables, runtime_funcs, declared_funcs, module, arg)?);
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
