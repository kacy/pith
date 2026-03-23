//! Generic monomorphization for Cranelift backend
//!
//! This module handles compile-time instantiation of generic functions.
//! When a generic function like `identity[T](x: T) -> T` is called with
//! a concrete type like `identity[Int](42)`, we generate a concrete
//! function `identity_Int` specialized for Int.

use crate::ast::{AstNode, MatchArm, MatchPattern};
use std::collections::HashMap;

/// Information about a generic function call site
#[derive(Debug, Clone)]
pub struct GenericCallSite {
    /// The generic function name (e.g., "identity")
    pub base_name: String,
    /// The concrete type arguments (e.g., ["Int"])
    pub type_args: Vec<String>,
    /// The mangled instantiation name (e.g., "identity_Int")
    pub instantiation_name: String,
    /// The AST node index of the original call
    pub call_node: AstNode,
}

/// Generic function declaration info
#[derive(Debug, Clone)]
pub struct GenericFunctionDecl {
    /// Function name (e.g., "identity[T]")
    pub name: String,
    /// Generic parameter names (e.g., ["T"])
    pub type_params: Vec<String>,
    /// Function parameters
    pub params: Vec<(String, String)>,
    /// Return type
    pub return_type: String,
    /// Function body AST
    pub body: AstNode,
}

/// Monomorphization state
#[derive(Debug, Default)]
pub struct Monomorphizer {
    /// Generic function declarations: base_name -> GenericFunctionDecl
    pub generic_decls: HashMap<String, GenericFunctionDecl>,
    /// Instantiated functions: instantiation_name -> (base_name, type_args)
    pub instantiations: HashMap<String, (String, Vec<String>)>,
    /// Track which instantiations have been compiled
    pub compiled: HashMap<String, bool>,
}

impl Monomorphizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a generic function declaration
    pub fn register_generic_decl(&mut self, name: &str, decl: GenericFunctionDecl) {
        // Extract base name (e.g., "identity" from "identity[T]")
        let base_name = if let Some(idx) = name.find('[') {
            name[..idx].to_string()
        } else {
            name.to_string()
        };
        self.generic_decls.insert(base_name, decl);
    }

    /// Check if a function name is a generic instantiation (contains underscore)
    /// e.g., "identity_Int" is an instantiation of generic "identity[T]"
    pub fn is_generic_name(&self, name: &str) -> bool {
        // Check if this looks like a mangled generic name (contains underscore)
        // and the base name is registered as a generic function
        if let Some(underscore_idx) = name.find('_') {
            let base_name = &name[..underscore_idx];
            return self.generic_decls.contains_key(base_name);
        }
        false
    }

    /// Extract base name and type arguments from a mangled generic function name
    /// e.g., "identity_Int" -> ("identity", ["Int"])
    pub fn parse_generic_call(&self, name: &str) -> Option<(String, Vec<String>)> {
        let underscore_idx = name.find('_')?;
        let base_name = name[..underscore_idx].to_string();

        // Check if this is a registered generic function
        if !self.generic_decls.contains_key(&base_name) {
            return None;
        }

        // Everything after the first underscore is the type argument
        let type_arg = name[underscore_idx + 1..].to_string();
        let type_args = vec![type_arg];

        Some((base_name, type_args))
    }

    /// Parse comma-separated type arguments
    fn parse_type_args(&self, args_str: &str) -> Vec<String> {
        let mut args = Vec::new();
        let mut current = String::new();
        let mut depth = 0;

        for c in args_str.chars() {
            match c {
                '[' => {
                    depth += 1;
                    current.push(c);
                }
                ']' => {
                    depth -= 1;
                    current.push(c);
                }
                ',' if depth == 0 => {
                    args.push(current.trim().to_string());
                    current.clear();
                }
                _ => current.push(c),
            }
        }

        if !current.is_empty() {
            args.push(current.trim().to_string());
        }

        args
    }

    /// Generate mangled instantiation name
    /// e.g., ("identity", ["Int"]) -> "identity_Int"
    pub fn mangle_instantiation(&self, base_name: &str, type_args: &[String]) -> String {
        if type_args.is_empty() {
            base_name.to_string()
        } else {
            format!("{}_{}", base_name, type_args.join("_"))
        }
    }

    /// Register a generic instantiation
    pub fn register_instantiation(&mut self, base_name: &str, type_args: Vec<String>) -> String {
        let inst_name = self.mangle_instantiation(base_name, &type_args);

        if !self.instantiations.contains_key(&inst_name) {
            self.instantiations
                .insert(inst_name.clone(), (base_name.to_string(), type_args));
        }

        inst_name
    }

    /// Create a monomorphized version of a generic function
    /// Returns the new function AST with type parameters substituted
    pub fn monomorphize_function(
        &self,
        decl: &GenericFunctionDecl,
        type_args: &[String],
    ) -> Option<AstNode> {
        if type_args.len() != decl.type_params.len() {
            return None;
        }

        // Build substitution map: type_param -> concrete_type
        let subst_map: HashMap<String, String> = decl
            .type_params
            .iter()
            .cloned()
            .zip(type_args.iter().cloned())
            .collect();

        // Create the mangled name
        let inst_name = self.mangle_instantiation(&decl.name, type_args);

        // Substitute types in parameters
        let new_params: Vec<(String, String)> = decl
            .params
            .iter()
            .map(|(name, ty)| (name.clone(), self.substitute_type(ty, &subst_map)))
            .collect();

        // Substitute type in return type
        let new_return_type = self.substitute_type(&decl.return_type, &subst_map);

        // Substitute types in body
        let new_body = self.substitute_types_in_node(&decl.body, &subst_map);

        Some(AstNode::Function {
            name: inst_name,
            params: new_params,
            return_type: new_return_type,
            body: Box::new(new_body),
        })
    }

    /// Substitute type parameters with concrete types in a type string
    fn substitute_type(&self, ty: &str, subst_map: &HashMap<String, String>) -> String {
        // Handle generic types like "List[T]"
        if let Some(start) = ty.find('[') {
            let end = ty.rfind(']').unwrap_or(ty.len());
            let base = &ty[..start];
            let inner = &ty[start + 1..end];

            // Recursively substitute in inner types
            let inner_substituted = self.substitute_type(inner, subst_map);

            format!("{}[{}]", base, inner_substituted)
        } else {
            // Simple type substitution
            subst_map.get(ty).cloned().unwrap_or_else(|| ty.to_string())
        }
    }

    /// Recursively substitute types in an AST node
    fn substitute_types_in_node(
        &self,
        node: &AstNode,
        subst_map: &HashMap<String, String>,
    ) -> AstNode {
        match node {
            AstNode::Let {
                name,
                type_annotation,
                value,
            } => AstNode::Let {
                name: name.clone(),
                type_annotation: type_annotation
                    .as_ref()
                    .map(|ty| self.substitute_type(ty, subst_map)),
                value: Box::new(self.substitute_types_in_node(value, subst_map)),
            },
            AstNode::Call { func, args } => {
                // Check if this is a call to a generic function
                let new_func = if self.is_generic_name(func) {
                    if let Some((base_name, type_args)) = self.parse_generic_call(func) {
                        self.mangle_instantiation(&base_name, &type_args)
                    } else {
                        func.clone()
                    }
                } else {
                    func.clone()
                };

                AstNode::Call {
                    func: new_func,
                    args: args
                        .iter()
                        .map(|arg| self.substitute_types_in_node(arg, subst_map))
                        .collect(),
                }
            }
            AstNode::Block(stmts) => AstNode::Block(
                stmts
                    .iter()
                    .map(|stmt| self.substitute_types_in_node(stmt, subst_map))
                    .collect(),
            ),
            AstNode::If {
                cond,
                then_branch,
                else_branch,
            } => AstNode::If {
                cond: Box::new(self.substitute_types_in_node(cond, subst_map)),
                then_branch: Box::new(self.substitute_types_in_node(then_branch, subst_map)),
                else_branch: else_branch
                    .as_ref()
                    .map(|b| Box::new(self.substitute_types_in_node(b, subst_map))),
            },
            AstNode::While { cond, body } => AstNode::While {
                cond: Box::new(self.substitute_types_in_node(cond, subst_map)),
                body: Box::new(self.substitute_types_in_node(body, subst_map)),
            },
            AstNode::For {
                var,
                index_var,
                iterable,
                body,
            } => AstNode::For {
                var: var.clone(),
                index_var: index_var.clone(),
                iterable: Box::new(self.substitute_types_in_node(iterable, subst_map)),
                body: Box::new(self.substitute_types_in_node(body, subst_map)),
            },
            AstNode::Return(expr) => AstNode::Return(
                expr.as_ref()
                    .map(|e| Box::new(self.substitute_types_in_node(e, subst_map))),
            ),
            AstNode::BinaryOp { op, left, right } => AstNode::BinaryOp {
                op: *op,
                left: Box::new(self.substitute_types_in_node(left, subst_map)),
                right: Box::new(self.substitute_types_in_node(right, subst_map)),
            },
            AstNode::UnaryOp { op, operand } => AstNode::UnaryOp {
                op: *op,
                operand: Box::new(self.substitute_types_in_node(operand, subst_map)),
            },
            AstNode::Match { expr, arms } => AstNode::Match {
                expr: Box::new(self.substitute_types_in_node(expr, subst_map)),
                arms: arms
                    .iter()
                    .map(|arm| MatchArm {
                        pattern: arm.pattern.clone(),
                        expr: Box::new(self.substitute_types_in_node(&arm.expr, subst_map)),
                    })
                    .collect(),
            },
            AstNode::ListLiteral {
                elements,
                elem_type,
            } => AstNode::ListLiteral {
                elements: elements
                    .iter()
                    .map(|e| self.substitute_types_in_node(e, subst_map))
                    .collect(),
                elem_type: elem_type
                    .as_ref()
                    .map(|ty| self.substitute_type(ty, subst_map)),
            },
            AstNode::MapLiteral {
                entries,
                key_type,
                val_type,
            } => AstNode::MapLiteral {
                entries: entries
                    .iter()
                    .map(|(k, v)| {
                        (
                            self.substitute_types_in_node(k, subst_map),
                            self.substitute_types_in_node(v, subst_map),
                        )
                    })
                    .collect(),
                key_type: key_type
                    .as_ref()
                    .map(|ty| self.substitute_type(ty, subst_map)),
                val_type: val_type
                    .as_ref()
                    .map(|ty| self.substitute_type(ty, subst_map)),
            },
            // For nodes that don't contain type information, just clone
            _ => node.clone(),
        }
    }
}

/// Pre-scan AST to collect all generic function instantiations
/// This should be called after registering generic declarations
pub fn collect_generic_instantiations(
    ast_nodes: &[AstNode],
    monomorphizer: &Monomorphizer,
) -> Vec<(String, String, Vec<String>)> {
    let mut instantiations = Vec::new();

    for node in ast_nodes {
        collect_instantiations_in_node(node, monomorphizer, &mut instantiations);
    }

    instantiations
}

fn collect_instantiations_in_node(
    node: &AstNode,
    mono: &Monomorphizer,
    instantiations: &mut Vec<(String, String, Vec<String>)>,
) {
    match node {
        AstNode::Call { func, args } => {
            // Check if this is a call to a generic function
            // The func might be an index expression like "identity[Int]"
            if mono.is_generic_name(func) {
                if let Some((base_name, type_args)) = mono.parse_generic_call(func) {
                    let inst_name = mono.mangle_instantiation(&base_name, &type_args);
                    instantiations.push((inst_name, base_name, type_args));
                }
            }
            // Recurse into arguments
            for arg in args {
                collect_instantiations_in_node(arg, mono, instantiations);
            }
        }
        // Handle generic instantiation via index operator (e.g., identity[Int])
        AstNode::Index { expr, index } => {
            if let AstNode::Identifier(func_name) = &**expr {
                if mono.is_generic_name(func_name) {
                    // This is a generic function instantiation: identity[Int]
                    if let AstNode::Identifier(type_name) = &**index {
                        let type_args = vec![type_name.clone()];
                        let inst_name = mono.mangle_instantiation(func_name, &type_args);
                        instantiations.push((inst_name, func_name.clone(), type_args));
                    }
                }
            }
            // Recurse into the expression
            collect_instantiations_in_node(expr, mono, instantiations);
            collect_instantiations_in_node(index, mono, instantiations);
        }
        AstNode::Function { body, .. } => {
            collect_instantiations_in_node(body, mono, instantiations);
        }
        AstNode::Block(stmts) => {
            for stmt in stmts {
                collect_instantiations_in_node(stmt, mono, instantiations);
            }
        }
        AstNode::Let { value, .. } => {
            collect_instantiations_in_node(value, mono, instantiations);
        }
        AstNode::If {
            cond,
            then_branch,
            else_branch,
        } => {
            collect_instantiations_in_node(cond, mono, instantiations);
            collect_instantiations_in_node(then_branch, mono, instantiations);
            if let Some(else_b) = else_branch {
                collect_instantiations_in_node(else_b, mono, instantiations);
            }
        }
        AstNode::While { cond, body } => {
            collect_instantiations_in_node(cond, mono, instantiations);
            collect_instantiations_in_node(body, mono, instantiations);
        }
        AstNode::For { iterable, body, .. } => {
            collect_instantiations_in_node(iterable, mono, instantiations);
            collect_instantiations_in_node(body, mono, instantiations);
        }
        AstNode::Return(expr) => {
            if let Some(e) = expr {
                collect_instantiations_in_node(e, mono, instantiations);
            }
        }
        AstNode::BinaryOp { left, right, .. } => {
            collect_instantiations_in_node(left, mono, instantiations);
            collect_instantiations_in_node(right, mono, instantiations);
        }
        AstNode::UnaryOp { operand, .. } => {
            collect_instantiations_in_node(operand, mono, instantiations);
        }
        AstNode::Match { expr, arms } => {
            collect_instantiations_in_node(expr, mono, instantiations);
            for arm in arms {
                collect_instantiations_in_node(&arm.expr, mono, instantiations);
            }
        }
        AstNode::ListLiteral { elements, .. } => {
            for elem in elements {
                collect_instantiations_in_node(elem, mono, instantiations);
            }
        }
        AstNode::MapLiteral { entries, .. } => {
            for (k, v) in entries {
                collect_instantiations_in_node(k, mono, instantiations);
                collect_instantiations_in_node(v, mono, instantiations);
            }
        }
        _ => {}
    }
}
