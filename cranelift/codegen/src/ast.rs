//! AST to Cranelift IR translation
//!
//! This module parses Forge AST and generates Cranelift intermediate representation.
//! It traverses the AST recursively, generating instructions for each node type.

use crate::{forge_type_to_cranelift, CodeGen, CompileError};
use cranelift::prelude::*;
use cranelift_module::{FuncId, Module};
use std::collections::HashMap;

/// AST node types (simplified)
#[derive(Debug, Clone)]
pub enum AstNode {
    /// Integer literal: 42
    IntLiteral(i64),
    /// Float literal: 3.14
    FloatLiteral(f64),
    /// Boolean literal: true/false
    BoolLiteral(bool),
    /// String literal: "hello"
    StringLiteral(String),
    /// Variable reference: x
    Identifier(String),
    /// Binary operation: a + b, x * y
    BinaryOp {
        op: BinaryOp,
        left: Box<AstNode>,
        right: Box<AstNode>,
    },
    /// Unary operation: -x, !cond
    UnaryOp { op: UnaryOp, operand: Box<AstNode> },
    /// Function call: foo(a, b)
    Call { func: String, args: Vec<AstNode> },
    /// Variable declaration: let x = value
    Let {
        name: String,
        type_annotation: Option<String>,
        value: Box<AstNode>,
    },
    /// Assignment: x = value
    Assign { name: String, value: Box<AstNode> },
    /// Block of statements
    Block(Vec<AstNode>),
    /// If expression: if cond { then_branch } else { else_branch }
    If {
        cond: Box<AstNode>,
        then_branch: Box<AstNode>,
        else_branch: Option<Box<AstNode>>,
    },
    /// While loop: while cond { body }
    While {
        cond: Box<AstNode>,
        body: Box<AstNode>,
    },
    /// For-in loop: for var[, index] in iterable { body }
    For {
        var: String,
        index_var: Option<String>, // Optional index variable for enumeration
        iterable: Box<AstNode>,
        body: Box<AstNode>,
    },
    /// Return statement: return value
    Return(Option<Box<AstNode>>),
    /// Break statement: break (exit loop)
    Break,
    /// Continue statement: continue (skip to next iteration)
    Continue,
    /// Import statement: from module import names
    Import { module: String, names: Vec<String> },
    /// Test declaration: test "name": body
    Test { name: String, body: Box<AstNode> },
    /// String interpolation: "hello, {name}!"
    StringInterp { parts: Vec<StringInterpPart> },
    /// List literal: [1, 2, 3]
    ListLiteral {
        elements: Vec<AstNode>,
        elem_type: Option<String>,
    },
    /// Map literal: {key1: val1, key2: val2}
    MapLiteral {
        entries: Vec<(AstNode, AstNode)>, // (key, value) pairs
        key_type: Option<String>,
        val_type: Option<String>,
    },
    /// Set literal: {"a", "b", "c"}
    SetLiteral {
        elements: Vec<AstNode>,
        elem_type: Option<String>,
    },
    /// Type alias: type Name = Target
    TypeAlias {
        name: String,
        target: String,
    },
    /// Spawn expression: spawn func(args) -> Task[T]
    Spawn { expr: Box<AstNode> },
    /// Await expression: await task -> T
    Await { expr: Box<AstNode> },
    /// Try expression: expr? (error propagation)
    Try { expr: Box<AstNode> },
    /// Fail statement: fail error
    Fail { error: Box<AstNode> },
    /// Index access: list[i] or map[key]
    Index {
        expr: Box<AstNode>,
        index: Box<AstNode>,
    },
    /// Struct declaration: struct Name { fields... }
    StructDecl {
        name: String,
        fields: Vec<(String, String)>, // (field_name, field_type)
        is_pub: bool,
    },
    /// Enum declaration: enum Name { variants... }
    EnumDecl {
        name: String,
        variants: Vec<EnumVariant>,
        is_pub: bool,
    },
    /// Enum variant constructor: EnumName.VariantName or EnumName.VariantName(args)
    EnumVariantConstruct {
        enum_name: String,
        variant_name: String,
        args: Vec<AstNode>,
    },
    /// Match expression: match expr { arms... }
    Match {
        expr: Box<AstNode>,
        arms: Vec<MatchArm>,
    },
    /// Struct initialization: TypeName { field: value, ... }
    StructInit {
        name: String,
        fields: Vec<(String, AstNode)>, // (field_name, value)
    },
    /// Field access: obj.field
    FieldAccess { obj: Box<AstNode>, field: String },
    /// Lambda/closure expression: fn(params) => body
    Lambda {
        params: Vec<(String, String)>, // (name, type)
        return_type: Option<String>,
        body: Box<AstNode>,
        capture_vars: Vec<String>, // Variables captured from outer scope
    },
    /// Interface declaration: interface Name { methods... }
    InterfaceDecl {
        name: String,
        methods: Vec<InterfaceMethod>,
        is_pub: bool,
    },
    /// Impl block: impl Interface for Type { methods... }
    ImplBlock {
        interface: String,
        target_type: String,
        methods: Vec<AstNode>, // Function definitions
    },
    /// Function declaration
    Function {
        name: String,
        params: Vec<(String, String)>, // (name, type)
        return_type: String,
        body: Box<AstNode>,
    },
}

/// Enum variant definition
#[derive(Debug, Clone)]
pub struct EnumVariant {
    pub name: String,
    pub data_types: Vec<String>, // Types of associated data (empty for simple variants)
}

/// Interface method signature
#[derive(Debug, Clone)]
pub struct InterfaceMethod {
    pub name: String,
    pub params: Vec<(String, String)>, // (param_name, param_type)
    pub return_type: String,
}

/// Match arm: pattern => expr
#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: MatchPattern,
    pub expr: Box<AstNode>,
}

/// Match pattern
#[derive(Debug, Clone)]
pub enum MatchPattern {
    /// Enum variant pattern: EnumName.VariantName or EnumName.VariantName(args)
    EnumVariant {
        enum_name: String,
        variant_name: String,
        bind_vars: Vec<String>, // Variable names to bind for associated data
    },
    /// Literal value
    Literal(AstNode),
    /// Wildcard: _
    Wildcard,
    /// Variable binding
    Variable(String),
}

/// Binary operators
#[derive(Debug, Clone, Copy)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Neq,
    Lt,
    Gt,
    Lte,
    Gte,
    And,
    Or,
    BitAnd, // &
    BitOr,  // |
    BitXor, // ^
    Shl,    // <<
    Shr,    // >>
}

/// Unary operators
#[derive(Debug, Clone, Copy)]
pub enum UnaryOp {
    Neg,
    Not,
    BitNot, // ~
}

/// String interpolation part - either literal text or an expression
#[derive(Debug, Clone)]
pub enum StringInterpPart {
    /// Literal string part
    Literal(String),
    /// Expression to evaluate and convert to string
    Expr(Box<AstNode>),
}

/// Local variable slot
#[derive(Debug)]
pub struct Variable {
    /// SSA value
    value: Value,
    /// Type
    ty: Type,
}

/// Compile an expression and return its SSA value
fn compile_expr(
    builder: &mut FunctionBuilder,
    variables: &mut HashMap<String, Variable>,
    runtime_funcs: &HashMap<String, FuncId>,
    module: &mut dyn Module,
    node: &AstNode,
) -> Result<Value, CompileError> {
    match node {
        AstNode::IntLiteral(n) => {
            let val = builder.ins().iconst(types::I64, *n);
            Ok(val)
        }

        AstNode::FloatLiteral(f) => {
            let val = builder.ins().f64const(*f);
            Ok(val)
        }

        AstNode::BoolLiteral(b) => {
            let val = builder.ins().iconst(types::I8, if *b { 1 } else { 0 });
            Ok(val)
        }

        AstNode::StringLiteral(s) => {
            // Allocate a string literal
            // For now, return a placeholder
            // In full implementation, we'd allocate in data section
            let ptr = builder.ins().iconst(types::I64, s.as_ptr() as i64);
            let len = builder.ins().iconst(types::I64, s.len() as i64);
            // Return as a tuple-like struct (ptr, len, is_heap=false)
            // For simplicity, just return ptr for now
            Ok(ptr)
        }

        AstNode::Identifier(name) => match variables.get(name) {
            Some(var) => Ok(var.value),
            None => Err(CompileError::UnknownVariable(name.clone())),
        },

        AstNode::BinaryOp { op, left, right } => {
            let left_val = compile_expr(builder, variables, runtime_funcs, module, left)?;
            let right_val = compile_expr(builder, variables, runtime_funcs, module, right)?;

            let result = match op {
                BinaryOp::Add => builder.ins().iadd(left_val, right_val),
                BinaryOp::Sub => builder.ins().isub(left_val, right_val),
                BinaryOp::Mul => builder.ins().imul(left_val, right_val),
                BinaryOp::Div => builder.ins().sdiv(left_val, right_val),
                BinaryOp::Mod => builder.ins().srem(left_val, right_val),
                BinaryOp::Eq => {
                    let cmp = builder.ins().icmp(IntCC::Equal, left_val, right_val);
                    builder.ins().uextend(types::I64, cmp)
                }
                BinaryOp::Neq => {
                    let cmp = builder.ins().icmp(IntCC::NotEqual, left_val, right_val);
                    builder.ins().uextend(types::I64, cmp)
                }
                BinaryOp::Lt => {
                    let cmp = builder
                        .ins()
                        .icmp(IntCC::SignedLessThan, left_val, right_val);
                    builder.ins().uextend(types::I64, cmp)
                }
                BinaryOp::Gt => {
                    let cmp = builder
                        .ins()
                        .icmp(IntCC::SignedGreaterThan, left_val, right_val);
                    builder.ins().uextend(types::I64, cmp)
                }
                BinaryOp::Lte => {
                    let cmp = builder
                        .ins()
                        .icmp(IntCC::SignedLessThanOrEqual, left_val, right_val);
                    builder.ins().uextend(types::I64, cmp)
                }
                BinaryOp::Gte => {
                    let cmp =
                        builder
                            .ins()
                            .icmp(IntCC::SignedGreaterThanOrEqual, left_val, right_val);
                    builder.ins().uextend(types::I64, cmp)
                }
                BinaryOp::And => builder.ins().band(left_val, right_val),
                BinaryOp::Or => builder.ins().bor(left_val, right_val),
                BinaryOp::BitAnd => builder.ins().band(left_val, right_val),
                BinaryOp::BitOr => builder.ins().bor(left_val, right_val),
                BinaryOp::BitXor => builder.ins().bxor(left_val, right_val),
                BinaryOp::Shl => builder.ins().ishl(left_val, right_val),
                BinaryOp::Shr => builder.ins().sshr(left_val, right_val),
            };

            Ok(result)
        }

        AstNode::UnaryOp { op, operand } => {
            let val = compile_expr(builder, variables, runtime_funcs, module, operand)?;

            let result = match op {
                UnaryOp::Neg => builder.ins().ineg(val),
                UnaryOp::Not => {
                    // XOR with 1 for boolean not
                    let one = builder.ins().iconst(types::I8, 1);
                    builder.ins().bxor(val, one)
                }
                UnaryOp::BitNot => builder.ins().bnot(val),
            };

            Ok(result)
        }

        AstNode::Call { func, args } => {
            compile_call(builder, variables, runtime_funcs, module, func, args)
        }

        AstNode::Block(stmts) => {
            let mut last_val = builder.ins().iconst(types::I64, 0);

            for stmt in stmts {
                last_val = compile_expr(builder, variables, runtime_funcs, module, stmt)?;
            }

            Ok(last_val)
        }

        _ => Err(CompileError::UnsupportedFeature(format!("{:?}", node))),
    }
}

/// Compile a function call
fn compile_call(
    builder: &mut FunctionBuilder,
    variables: &mut HashMap<String, Variable>,
    runtime_funcs: &HashMap<String, FuncId>,
    module: &mut dyn Module,
    func_name: &str,
    args: &[AstNode],
) -> Result<Value, CompileError> {
    // Handle method calls like .to_string
    if func_name.starts_with('.') {
        let method = &func_name[1..]; // Remove the leading dot

        // Convert method call to function call with receiver as first argument
        // obj.method() -> method(obj)
        if args.len() != 1 {
            return Err(CompileError::UnsupportedFeature(format!(
                "Method call {} requires exactly one receiver, got {}",
                func_name,
                args.len()
            )));
        }

        let receiver = &args[0];

        match method {
            "to_string" => {
                // Compile the receiver
                let val = compile_expr(builder, variables, runtime_funcs, module, receiver)?;

                // Call int_to_string
                let func_id = runtime_funcs
                    .get("forge_int_to_string")
                    .copied()
                    .ok_or_else(|| {
                        CompileError::UnknownFunction("forge_int_to_string".to_string())
                    })?;

                let func_ref = module.declare_func_in_func(func_id, builder.func);
                let call = builder.ins().call(func_ref, &[val]);

                if !builder.func.dfg.inst_results(call).is_empty() {
                    Ok(builder.func.dfg.first_result(call))
                } else {
                    Ok(builder.ins().iconst(types::I64, 0))
                }
            }
            _ => Err(CompileError::UnsupportedFeature(format!(
                "Method {}",
                method
            ))),
        }
    } else {
        // Regular function call
        // Map high-level function names to runtime function names
        let runtime_name = match func_name {
            "print" => "forge_print",
            "print_int" => "forge_print_int",
            _ => func_name,
        };

        // Look up the function in runtime functions
        let func_id = runtime_funcs.get(runtime_name).copied().ok_or_else(|| {
            CompileError::UnknownFunction(format!("{} (mapped from {})", runtime_name, func_name))
        })?;

        // Get func ref in current function
        let func_ref = module.declare_func_in_func(func_id, builder.func);

        // Compile arguments
        let mut arg_values = Vec::new();
        for arg in args {
            arg_values.push(compile_expr(
                builder,
                variables,
                runtime_funcs,
                module,
                arg,
            )?);
        }

        // Make the call
        let call = builder.ins().call(func_ref, &arg_values);

        // Get return value if any
        if !builder.func.dfg.inst_results(call).is_empty() {
            Ok(builder.func.dfg.first_result(call))
        } else {
            Ok(builder.ins().iconst(types::I64, 0))
        }
    }
}

/// Compile a statement
fn compile_stmt(
    builder: &mut FunctionBuilder,
    variables: &mut HashMap<String, Variable>,
    runtime_funcs: &HashMap<String, FuncId>,
    module: &mut dyn Module,
    return_type: Type,
    _current_block: Block,
    node: &AstNode,
) -> Result<(), CompileError> {
    match node {
        AstNode::Let { name, value, .. } => {
            let val = compile_expr(builder, variables, runtime_funcs, module, value)?;

            // Infer type from value
            let ty = builder.func.dfg.value_type(val);

            // Store in variable table
            variables.insert(name.clone(), Variable { value: val, ty });

            Ok(())
        }

        AstNode::Assign { name, value } => {
            let val = compile_expr(builder, variables, runtime_funcs, module, value)?;

            // Update variable
            if let Some(var) = variables.get(name) {
                let ty = var.ty;
                variables.insert(name.clone(), Variable { value: val, ty });
                Ok(())
            } else {
                Err(CompileError::UnknownVariable(name.clone()))
            }
        }

        AstNode::Return(expr) => {
            match expr {
                Some(e) => {
                    let val = compile_expr(builder, variables, runtime_funcs, module, e)?;

                    // Convert to return type if needed
                    let converted = if builder.func.dfg.value_type(val) != return_type {
                        if return_type == types::I32 {
                            builder.ins().ireduce(types::I32, val)
                        } else {
                            val
                        }
                    } else {
                        val
                    };

                    builder.ins().return_(&[converted]);
                }
                None => {
                    let zero = builder.ins().iconst(return_type, 0);
                    builder.ins().return_(&[zero]);
                }
            }

            Ok(())
        }

        AstNode::If {
            cond,
            then_branch,
            else_branch,
        } => {
            let cond_val = compile_expr(builder, variables, runtime_funcs, module, cond)?;

            // Create blocks
            let then_block = builder.create_block();
            let else_block = builder.create_block();
            let merge_block = builder.create_block();

            // Branch based on condition
            builder
                .ins()
                .brif(cond_val, then_block, &[], else_block, &[]);

            // Compile then branch
            builder.switch_to_block(then_block);
            compile_expr(builder, variables, runtime_funcs, module, then_branch)?;
            builder.ins().jump(merge_block, &[]);
            builder.seal_block(then_block);

            // Compile else branch if present
            builder.switch_to_block(else_block);
            if let Some(else_node) = else_branch {
                compile_expr(builder, variables, runtime_funcs, module, else_node)?;
            }
            builder.ins().jump(merge_block, &[]);
            builder.seal_block(else_block);

            // Continue at merge block
            builder.switch_to_block(merge_block);
            builder.seal_block(merge_block);

            Ok(())
        }

        AstNode::While { cond, body } => {
            // Create blocks
            let header_block = builder.create_block();
            let body_block = builder.create_block();
            let exit_block = builder.create_block();

            // Jump to header
            builder.ins().jump(header_block, &[]);

            // Compile header (condition check)
            builder.switch_to_block(header_block);
            let cond_val = compile_expr(builder, variables, runtime_funcs, module, cond)?;
            builder
                .ins()
                .brif(cond_val, body_block, &[], exit_block, &[]);
            builder.seal_block(header_block);

            // Compile body
            builder.switch_to_block(body_block);
            compile_expr(builder, variables, runtime_funcs, module, body)?;
            builder.ins().jump(header_block, &[]);
            builder.seal_block(body_block);

            // Continue at exit block
            builder.switch_to_block(exit_block);
            builder.seal_block(exit_block);

            Ok(())
        }

        AstNode::Block(stmts) => {
            for stmt in stmts {
                compile_stmt(
                    builder,
                    variables,
                    runtime_funcs,
                    module,
                    return_type,
                    _current_block,
                    stmt,
                )?;
            }
            Ok(())
        }

        _ => {
            // Expression statement - evaluate and discard
            compile_expr(builder, variables, runtime_funcs, module, node)?;
            Ok(())
        }
    }
}

/// Compile a function from AST
pub fn compile_function(
    codegen: &mut CodeGen,
    name: &str,
    params: &[(String, String)],
    return_type: &str,
    body: &AstNode,
) -> Result<FuncId, CompileError> {
    use cranelift_module::Linkage;

    // Build function signature
    let mut ctx = codegen.module.make_context();

    // Add parameters
    for (_, ty) in params {
        let cl_ty = forge_type_to_cranelift(ty);
        ctx.func.signature.params.push(AbiParam::new(cl_ty));
    }

    // Add return type
    let ret_ty = forge_type_to_cranelift(return_type);
    ctx.func.signature.returns.push(AbiParam::new(ret_ty));

    // Declare the function
    let func_id = codegen
        .module
        .declare_function(name, Linkage::Export, &ctx.func.signature)
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;

    // Declare runtime functions first
    let mut runtime_funcs = crate::declare_runtime_functions(&mut codegen.module)?;

    // Build the function body
    let mut builder_ctx = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
        let mut variables = HashMap::new();

        // Create entry block
        let entry_block = builder.create_block();
        builder.append_block_params_for_function_params(entry_block);
        builder.switch_to_block(entry_block);
        builder.seal_block(entry_block);

        // Add parameters to variable table
        let block_params = builder.block_params(entry_block);
        for (i, (param_name, param_ty)) in params.iter().enumerate() {
            let param_val = block_params[i];
            let ty = forge_type_to_cranelift(param_ty);
            variables.insert(
                param_name.clone(),
                Variable {
                    value: param_val,
                    ty,
                },
            );
        }

        // Compile the body
        compile_stmt(
            &mut builder,
            &mut variables,
            &runtime_funcs,
            &mut codegen.module,
            ret_ty,
            entry_block,
            body,
        )?;

        // Ensure we have a return
        let zero = builder.ins().iconst(ret_ty, 0);
        builder.ins().return_(&[zero]);
    }

    // Define the function
    codegen
        .module
        .define_function(func_id, &mut ctx)
        .map_err(|e| CompileError::ModuleError(e.to_string()))?;

    Ok(func_id)
}
