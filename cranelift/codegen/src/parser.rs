//! Parse Forge AST from text format
//!
//! Converts the output from `forge parse` into our AST structure.
//! The format is:
//!   module
//!     fn name
//!       body
//!         ...

use crate::ast::{AstNode, BinaryOp, StringInterpPart, UnaryOp};
use crate::CompileError;
use std::collections::HashMap;

/// A line in the AST text output
#[derive(Debug, Clone)]
struct AstLine {
    indent: usize,
    kind: String,
    value: String,
}

/// Parser state
pub struct TextAstParser {
    lines: Vec<AstLine>,
    pos: usize,
}

impl TextAstParser {
    /// Parse AST from text output
    pub fn parse(input: &str) -> Result<Vec<AstNode>, CompileError> {
        let mut parser = Self::new(input)?;
        parser.parse_module()
    }

    /// Create a new parser from input text
    fn new(input: &str) -> Result<Self, CompileError> {
        let mut lines = Vec::new();

        for line in input.lines() {
            if line.trim().is_empty() {
                continue;
            }

            // Count indentation (2 spaces per level)
            let indent = line.len() - line.trim_start().len();
            let level = indent / 2;

            // Parse the line content
            let content = line.trim();
            let parts: Vec<&str> = content.splitn(2, ' ').collect();

            let kind = parts[0].to_string();
            let value = if parts.len() > 1 {
                parts[1].to_string()
            } else {
                String::new()
            };

            lines.push(AstLine {
                indent: level,
                kind,
                value,
            });
        }

        Ok(TextAstParser { lines, pos: 0 })
    }

    /// Get current line
    fn current(&self) -> Option<&AstLine> {
        self.lines.get(self.pos)
    }

    /// Peek at next line
    fn peek(&self, offset: usize) -> Option<&AstLine> {
        self.lines.get(self.pos + offset)
    }

    /// Advance to next line
    fn advance(&mut self) {
        self.pos += 1;
    }

    /// Parse the module (root)
    fn parse_module(&mut self) -> Result<Vec<AstNode>, CompileError> {
        let mut declarations = Vec::new();

        // Expect "module" at root
        if let Some(line) = self.current() {
            if line.kind != "module" {
                return Err(CompileError::UnsupportedFeature(format!(
                    "Expected 'module', got '{}'",
                    line.kind
                )));
            }
            self.advance();
        }

        // Parse top-level declarations
        while self.current().is_some() {
            let decl = self.parse_top_level()?;
            declarations.push(decl);
        }

        Ok(declarations)
    }

    /// Parse a top-level declaration
    fn parse_top_level(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().ok_or_else(|| {
            CompileError::UnsupportedFeature("Unexpected end of input".to_string())
        })?;

        match line.kind.as_str() {
            "fn" => self.parse_function(),
            "struct" => self.parse_struct(),
            "from" => self.parse_import(),
            "test" => self.parse_test(),
            "pub" => {
                self.advance();
                self.parse_top_level()
            }
            _ => Err(CompileError::UnsupportedFeature(format!(
                "Unknown top-level kind: {}",
                line.kind
            ))),
        }
    }

    /// Parse a function declaration
    fn parse_function(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
        let name = line
            .value
            .split_whitespace()
            .next()
            .map(|s| s.to_string())
            .unwrap_or_else(|| line.value.clone());
        let indent = line.indent;
        self.advance();

        // Parse function contents
        let mut params = Vec::new();
        let mut body = None;
        let mut return_type = "Void".to_string();

        while let Some(line) = self.current() {
            if line.indent <= indent {
                break; // End of function
            }

            match line.kind.as_str() {
                "param" => {
                    let param_name = line.value.clone();
                    self.advance();
                    // Parse type if present
                    let param_type = if let Some(type_line) = self.current() {
                        if type_line.kind == "type" {
                            let ty = type_line.value.clone();
                            self.advance();
                            ty
                        } else {
                            "Int".to_string() // Default
                        }
                    } else {
                        "Int".to_string() // Default
                    };
                    params.push((param_name, param_type));
                }
                "returns" => {
                    self.advance();
                    // Parse return type - can be regular type, result type, or optional
                    if let Some(type_line) = self.current() {
                        if type_line.kind == "type" {
                            return_type = type_line.value.clone();
                            self.advance();
                        } else if type_line.kind == "result" {
                            // Result type: result <inner_type>
                            self.advance();
                            if let Some(inner_line) = self.current() {
                                if inner_line.kind == "type" {
                                    return_type = format!("{}!", inner_line.value);
                                    self.advance();
                                }
                            }
                        } else if type_line.kind == "optional" {
                            // Optional type: optional <inner_type>
                            self.advance();
                            if let Some(inner_line) = self.current() {
                                if inner_line.kind == "type" {
                                    return_type = format!("{}?", inner_line.value);
                                    self.advance();
                                }
                            }
                        }
                    }
                }
                "body" => {
                    self.advance();
                    body = Some(self.parse_body()?);
                }
                _ => {
                    // Skip unknown nodes for now
                    self.advance();
                }
            }
        }

        let body_node = body.unwrap_or_else(|| AstNode::Block(vec![]));

        Ok(AstNode::Function {
            name,
            params,
            return_type,
            body: Box::new(body_node),
        })
    }

    /// Parse a struct declaration
    fn parse_struct(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
        let name = line.value.clone();
        let _indent = line.indent;
        let is_pub = false; // TODO: track pub status
        self.advance();

        let mut fields = Vec::new();

        // Parse fields until we hit a lower indentation
        while let Some(field_line) = self.current() {
            if field_line.kind != "field" {
                break;
            }

            let field_name = field_line.value.clone();
            self.advance();

            // Parse field type
            let field_type = if let Some(type_line) = self.current() {
                if type_line.kind == "type" {
                    let ty = type_line.value.clone();
                    self.advance();
                    ty
                } else {
                    "Int".to_string()
                }
            } else {
                "Int".to_string()
            };

            fields.push((field_name, field_type));
        }

        Ok(AstNode::StructDecl {
            name,
            fields,
            is_pub,
        })
    }

    /// Parse an import declaration: from module import name1, name2, ...
    fn parse_import(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
        let content = line.value.clone();
        self.advance();

        // Parse format: "module import name1, name2, ..."
        let parts: Vec<&str> = content.split(" import ").collect();
        if parts.len() != 2 {
            return Err(CompileError::UnsupportedFeature(format!(
                "Invalid import syntax: {}",
                content
            )));
        }

        let module = parts[0].trim().to_string();
        let names_str = parts[1].trim();

        // Parse comma-separated names
        let names: Vec<String> = names_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        Ok(AstNode::Import { module, names })
    }

    /// Parse a test declaration: test "name": body
    fn parse_test(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
        let test_name = line.value.clone();
        let indent = line.indent;
        self.advance();

        // Parse test body statements (indented)
        let mut stmts = Vec::new();
        while let Some(line) = self.current() {
            if line.indent <= indent {
                break;
            }
            stmts.push(self.parse_statement()?);
        }

        Ok(AstNode::Test {
            name: test_name,
            body: Box::new(AstNode::Block(stmts)),
        })
    }

    /// Parse function body
    fn parse_body(&mut self) -> Result<AstNode, CompileError> {
        let start_line = self.current();
        let start_indent = start_line.map(|l| l.indent).unwrap_or(0);

        let mut stmts = Vec::new();

        while let Some(line) = self.current() {
            // Stop if we hit a lower indentation
            if line.indent < start_indent {
                break;
            }

            // Stop if we hit another top-level declaration at same level
            // (e.g., another function after a short function like `do_nothing`)
            if line.indent == start_indent
                && (line.kind == "fn" || line.kind == "test" || line.kind == "import")
            {
                break;
            }

            let stmt = self.parse_statement()?;
            stmts.push(stmt);
        }

        Ok(AstNode::Block(stmts))
    }

    /// Parse a statement
    fn parse_statement(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().ok_or_else(|| {
            CompileError::UnsupportedFeature("Unexpected end of input".to_string())
        })?;

        match line.kind.as_str() {
            "ident" => {
                if line.value == "let" {
                    self.advance();
                    self.parse_let()
                } else {
                    // Expression statement
                    self.parse_expression()
                }
            }
            "return" => self.parse_return(),
            "if" => self.parse_if(),
            "while" => self.parse_while(),
            "fail" => self.parse_fail(),
            "bind" => {
                // bind name <type>? <value> - this is a let statement
                // Handle case where name includes "mut" marker: "bind i mut"
                let name = line
                    .value
                    .split_whitespace()
                    .next()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| line.value.clone());
                let name_indent = line.indent;
                self.advance();

                // Check for optional type annotation
                let type_annotation = if let Some(type_line) = self.current() {
                    if type_line.kind == "type" && type_line.indent > name_indent {
                        let ty = type_line.value.clone();
                        self.advance();
                        Some(ty)
                    } else {
                        None
                    }
                } else {
                    None
                };

                let value = self.parse_expression()?;
                Ok(AstNode::Let {
                    name,
                    type_annotation,
                    value: Box::new(value),
                })
            }
            "call" => self.parse_call(),
            "assign" => self.parse_assign(),
            _ => self.parse_expression(),
        }
    }

    /// Parse assignment: assign = <name> <value>
    fn parse_assign(&mut self) -> Result<AstNode, CompileError> {
        // Current line is "assign ="
        self.advance();

        // Parse variable name
        let name_line = self.current().ok_or_else(|| {
            CompileError::UnsupportedFeature("Expected variable name in assign".to_string())
        })?;

        if name_line.kind != "ident" {
            return Err(CompileError::UnsupportedFeature(format!(
                "Expected 'ident' in assign, got '{}'",
                name_line.kind
            )));
        }

        let name = name_line.value.clone();
        self.advance();

        // Parse value
        let value = self.parse_expression()?;

        Ok(AstNode::Assign {
            name,
            value: Box::new(value),
        })
    }

    /// Parse let statement: let name = value
    fn parse_let(&mut self) -> Result<AstNode, CompileError> {
        // Expect bind node with name
        let line = self.current().ok_or_else(|| {
            CompileError::UnsupportedFeature("Expected bind after let".to_string())
        })?;

        if line.kind != "bind" {
            return Err(CompileError::UnsupportedFeature(format!(
                "Expected 'bind', got '{}'",
                line.kind
            )));
        }

        let name = line
            .value
            .split_whitespace()
            .next()
            .map(|s| s.to_string())
            .unwrap_or_else(|| line.value.clone());
        let name_indent = line.indent;
        self.advance();

        // Check for optional type annotation
        let type_annotation = if let Some(type_line) = self.current() {
            if type_line.kind == "type" && type_line.indent > name_indent {
                let ty = type_line.value.clone();
                self.advance();
                Some(ty)
            } else {
                None
            }
        } else {
            None
        };

        // Parse value
        let value = self.parse_expression()?;

        Ok(AstNode::Let {
            name,
            type_annotation,
            value: Box::new(value),
        })
    }

    /// Parse return statement
    fn parse_return(&mut self) -> Result<AstNode, CompileError> {
        let return_line = self.current().unwrap();
        let return_indent = return_line.indent;
        // Advance past the return keyword first
        self.advance();

        // Only parse a return value if the next line is indented more
        let value = if let Some(line) = self.current() {
            if line.indent > return_indent {
                Some(Box::new(self.parse_expression()?))
            } else {
                None
            }
        } else {
            None
        };

        Ok(AstNode::Return(value))
    }

    /// Parse fail statement: fail <error>
    fn parse_fail(&mut self) -> Result<AstNode, CompileError> {
        // Current line is "fail"
        let fail_line = self.current().unwrap();
        let fail_indent = fail_line.indent;
        self.advance();

        // Parse the error expression (if indented more)
        let error = if let Some(line) = self.current() {
            if line.indent > fail_indent {
                self.parse_expression()?
            } else {
                AstNode::StringLiteral("error".to_string())
            }
        } else {
            AstNode::StringLiteral("error".to_string())
        };

        Ok(AstNode::Fail {
            error: Box::new(error),
        })
    }

    /// Parse a function call
    fn parse_call(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
        let start_indent = line.indent;
        self.advance();

        // Expect 'ident' with function name
        let func_line = self.current().ok_or_else(|| {
            CompileError::UnsupportedFeature("Expected function name after call".to_string())
        })?;

        if func_line.kind != "ident" {
            return Err(CompileError::UnsupportedFeature(format!(
                "Expected function name, got '{}'",
                func_line.kind
            )));
        }

        let func_name = func_line.value.clone();
        self.advance();

        // Parse arguments
        let mut args = Vec::new();
        while let Some(line) = self.current() {
            if line.indent <= start_indent {
                break;
            }

            if line.kind == "arg" {
                self.advance();
                args.push(self.parse_expression()?);
            } else {
                break;
            }
        }

        Ok(AstNode::Call {
            func: func_name,
            args,
        })
    }

    /// Parse an expression
    fn parse_expression(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().ok_or_else(|| {
            CompileError::UnsupportedFeature("Unexpected end of input".to_string())
        })?;

        match line.kind.as_str() {
            "int" => {
                let value_str = &line.value;
                let value = if value_str.starts_with("0x") || value_str.starts_with("0X") {
                    // Parse hexadecimal
                    i64::from_str_radix(&value_str[2..], 16).map_err(|_| {
                        CompileError::UnsupportedFeature(format!(
                            "Invalid hex integer: {}",
                            value_str
                        ))
                    })?
                } else {
                    value_str.parse::<i64>().map_err(|_| {
                        CompileError::UnsupportedFeature(format!("Invalid integer: {}", value_str))
                    })?
                };
                self.advance();
                Ok(AstNode::IntLiteral(value))
            }
            "float" => {
                let value = line.value.parse::<f64>().map_err(|_| {
                    CompileError::UnsupportedFeature(format!("Invalid float: {}", line.value))
                })?;
                self.advance();
                Ok(AstNode::FloatLiteral(value))
            }
            "bool" => {
                let value = line.value == "true";
                self.advance();
                Ok(AstNode::BoolLiteral(value))
            }
            "string" => {
                let value = line.value.clone();
                self.advance();
                Ok(AstNode::StringLiteral(value))
            }
            "string_interp" => self.parse_string_interp(),
            "list" => self.parse_list_literal(),
            "map" => self.parse_map_literal(),
            "struct_init" => self.parse_struct_init(),
            "field" => self.parse_field_access(),
            "try" => self.parse_try(),
            "call" => self.parse_call(),
            "ident" => {
                let name = line
                    .value
                    .split_whitespace()
                    .next()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| line.value.clone());
                self.advance();
                Ok(AstNode::Identifier(name))
            }
            "binary" => {
                // Check if it's a pipe operation
                if let Some(line) = self.current() {
                    if line.value == "pipe" {
                        self.advance();
                        // Parse left operand (value)
                        let value = self.parse_expression()?;
                        // Parse right operand (function name)
                        let func_line = self.current().ok_or_else(|| {
                            CompileError::UnsupportedFeature(
                                "Expected function name after pipe".to_string(),
                            )
                        })?;
                        if func_line.kind != "ident" {
                            return Err(CompileError::UnsupportedFeature(format!(
                                "Expected function identifier in pipe, got '{}'",
                                func_line.kind
                            )));
                        }
                        let func_name = func_line.value.clone();
                        self.advance();
                        // Transform pipe into function call
                        return Ok(AstNode::Call {
                            func: func_name,
                            args: vec![value],
                        });
                    }
                }
                self.parse_binary()
            }
            "method_call" => self.parse_method_call(),
            _ => {
                // Skip unknown nodes
                self.advance();
                Ok(AstNode::IntLiteral(0)) // Placeholder
            }
        }
    }

    /// Parse binary operation
    fn parse_binary(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
        let op_str = line.value.clone();
        let start_indent = line.indent;
        self.advance();

        let op = match op_str.as_str() {
            "add" => BinaryOp::Add,
            "sub" => BinaryOp::Sub,
            "mul" => BinaryOp::Mul,
            "div" => BinaryOp::Div,
            "mod" => BinaryOp::Mod,
            "eq" => BinaryOp::Eq,
            "neq" => BinaryOp::Neq,
            "lt" => BinaryOp::Lt,
            "gt" => BinaryOp::Gt,
            "lte" => BinaryOp::Lte,
            "gte" => BinaryOp::Gte,
            "and" => BinaryOp::And,
            "or" => BinaryOp::Or,
            _ => {
                return Err(CompileError::UnsupportedFeature(format!(
                    "Unknown binary operator: {}",
                    op_str
                )))
            }
        };

        // Parse left operand
        let left = Box::new(self.parse_expression()?);

        // Parse right operand
        let right = Box::new(self.parse_expression()?);

        Ok(AstNode::BinaryOp { op, left, right })
    }

    /// Parse string interpolation: string_interp (N parts) followed by parts
    fn parse_string_interp(&mut self) -> Result<AstNode, CompileError> {
        // Current line is "string_interp (N parts)"
        let interp_line = self.current().unwrap();
        let start_indent = interp_line.indent;
        self.advance();

        let mut parts = Vec::new();

        // Parse each part (lit or ident) until we hit a lower indentation
        while let Some(line) = self.current() {
            if line.indent <= start_indent {
                break;
            }

            match line.kind.as_str() {
                "lit" => {
                    let lit_value = line.value.clone();
                    parts.push(StringInterpPart::Literal(lit_value));
                    self.advance();
                }
                "ident" => {
                    let ident_name = line.value.clone();
                    parts.push(StringInterpPart::Expr(Box::new(AstNode::Identifier(
                        ident_name,
                    ))));
                    self.advance();
                }
                _ => {
                    // Unknown part type, skip it
                    self.advance();
                }
            }
        }

        Ok(AstNode::StringInterp { parts })
    }

    /// Parse list literal: list (N items) followed by elements
    fn parse_list_literal(&mut self) -> Result<AstNode, CompileError> {
        // Current line is "list (N items)"
        let list_line = self.current().unwrap();
        let start_indent = list_line.indent;
        self.advance();

        let mut elements = Vec::new();

        // Parse each element until we hit a lower indentation
        while let Some(line) = self.current() {
            if line.indent <= start_indent {
                break;
            }

            // Parse the element expression
            let elem = self.parse_expression()?;
            elements.push(elem);
        }

        Ok(AstNode::ListLiteral {
            elements,
            elem_type: None,
        })
    }

    /// Parse map literal: map (N entries) followed by key/value pairs
    fn parse_map_literal(&mut self) -> Result<AstNode, CompileError> {
        // Current line is "map (N entries)"
        let map_line = self.current().unwrap();
        let start_indent = map_line.indent;
        self.advance();

        let mut entries = Vec::new();

        // Parse each entry until we hit a lower indentation
        // Format: key <value> (value is indented more)
        while let Some(line) = self.current() {
            if line.indent <= start_indent {
                break;
            }

            // Extract all data from line first to avoid borrow issues
            let line_indent = line.indent;
            let key = if line.kind == "string" {
                let val = line.value.clone();
                self.advance();
                AstNode::StringLiteral(val)
            } else if line.kind == "int" || line.kind == "integer" {
                let val = line.value.parse().unwrap_or(0);
                self.advance();
                AstNode::IntLiteral(val)
            } else if line.kind == "ident" {
                let val = line.value.clone();
                self.advance();
                AstNode::Identifier(val)
            } else {
                // Parse as expression
                self.parse_expression()?
            };

            // Check for value (indented more)
            let value = if let Some(val_line) = self.current() {
                if val_line.indent > line_indent {
                    self.parse_expression()?
                } else {
                    AstNode::IntLiteral(0) // Default/no value
                }
            } else {
                AstNode::IntLiteral(0)
            };

            entries.push((key, value));
        }

        Ok(AstNode::MapLiteral {
            entries,
            key_type: None,
            val_type: None,
        })
    }

    /// Parse struct initialization: struct_init Name followed by fields
    fn parse_struct_init(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
        let name = line.value.clone();
        let start_indent = line.indent;
        self.advance();

        let mut fields = Vec::new();

        // Parse field assignments until lower indentation
        while let Some(field_line) = self.current() {
            if field_line.indent <= start_indent {
                break;
            }

            // Extract data first to avoid borrow issues
            let field_indent = field_line.indent;

            if field_line.kind == "field" || field_line.kind == "ident" {
                let field_name = field_line.value.clone();
                self.advance();

                // Parse value (indented more)
                let value = if let Some(val_line) = self.current() {
                    if val_line.indent > field_indent {
                        self.parse_expression()?
                    } else {
                        AstNode::IntLiteral(0)
                    }
                } else {
                    AstNode::IntLiteral(0)
                };

                fields.push((field_name, value));
            } else {
                // Skip unknown nodes
                self.advance();
            }
        }

        Ok(AstNode::StructInit { name, fields })
    }

    /// Parse field access: field .name followed by object
    fn parse_field_access(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
        let field = line.value.clone();
        let start_indent = line.indent;
        self.advance();

        // Parse the object being accessed (should be next expression)
        let obj = self.parse_expression()?;

        Ok(AstNode::FieldAccess {
            obj: Box::new(obj),
            field,
        })
    }

    /// Parse try expression: try <expr> (the ? operator for error propagation)
    fn parse_try(&mut self) -> Result<AstNode, CompileError> {
        // Current line is "try"
        self.advance();

        // Parse the expression that might fail
        let expr = self.parse_expression()?;

        Ok(AstNode::Try {
            expr: Box::new(expr),
        })
    }

    /// Parse method call: obj.method(args)
    fn parse_method_call(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
        let method = line.value.clone();
        self.advance();

        // Parse object (should be next expression)
        let obj = self.parse_expression()?;

        // Strip leading dot and map to runtime function
        let func_name = if method.starts_with('.') {
            match method.as_str() {
                ".to_string" => "forge_int_to_cstr".to_string(),
                _ => method[1..].to_string(), // Remove leading dot
            }
        } else {
            method
        };

        // Convert to call: func_name(obj)
        Ok(AstNode::Call {
            func: func_name,
            args: vec![obj],
        })
    }

    /// Parse while loop
    fn parse_while(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
        let indent = line.indent;
        self.advance();

        // Parse condition
        let cond = self.parse_expression()?;

        // Parse body statements (everything indented more than 'while')
        let mut body_stmts = Vec::new();
        while let Some(line) = self.current() {
            if line.indent <= indent {
                break;
            }
            body_stmts.push(self.parse_statement()?);
        }

        let body = if body_stmts.len() == 1 {
            body_stmts.into_iter().next().unwrap()
        } else {
            AstNode::Block(body_stmts)
        };

        Ok(AstNode::While {
            cond: Box::new(cond),
            body: Box::new(body),
        })
    }

    /// Parse if statement
    fn parse_if(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
        let indent = line.indent;
        self.advance();

        // Parse condition
        let cond = self.parse_expression()?;

        // Parse then branch
        let mut then_branch = None;
        let mut else_branch = None;

        while let Some(line) = self.current() {
            if line.indent <= indent {
                break;
            }

            match line.kind.as_str() {
                "then" => {
                    self.advance();
                    then_branch = Some(self.parse_statement()?);
                }
                "else" => {
                    self.advance();
                    else_branch = Some(self.parse_statement()?);
                }
                _ => break,
            }
        }

        Ok(AstNode::If {
            cond: Box::new(cond),
            then_branch: Box::new(then_branch.unwrap_or_else(|| AstNode::Block(vec![]))),
            else_branch: else_branch.map(Box::new),
        })
    }
}

/// Parse a .fg file and return AST
pub fn parse_file(path: &str) -> Result<Vec<AstNode>, CompileError> {
    // Run the self-hosted parser
    let output = std::process::Command::new("./self-host/forge_main")
        .args(&["parse", path])
        .output()
        .map_err(|e| CompileError::ModuleError(format!("Failed to run parser: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CompileError::ModuleError(format!(
            "Parser error: {}",
            stderr
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    TextAstParser::parse(&stdout)
}
