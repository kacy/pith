//! Parse Forge AST from text format
//!
//! Converts the output from `forge parse` into our AST structure.
//! The format is:
//!   module
//!     fn name
//!       body
//!         ...

use crate::ast::{
    AstNode, BinaryOp, EnumVariant, InterfaceMethod, MatchArm, MatchPattern, StringInterpPart,
    UnaryOp,
};
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

            // Parse the line content (only trim leading whitespace to preserve
            // trailing spaces in string literals like `lit "hello, `)
            let content = line.trim_start();
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

    /// Skip the current line and all its children (lines at greater indent)
    fn skip_subtree(&mut self) {
        if let Some(line) = self.lines.get(self.pos) {
            let parent_indent = line.indent;
            self.pos += 1; // skip current line
                           // skip all lines that are children (deeper indent)
            while let Some(child) = self.lines.get(self.pos) {
                if child.indent > parent_indent {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }
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
            "enum" => self.parse_enum(),
            "interface" => self.parse_interface(),
            "impl" => self.parse_impl(),
            "from" => self.parse_import(),
            "import" => {
                // Module alias: `import std math as math_lib`
                // Just skip it — we don't support aliased imports yet
                let import_indent = line.indent;
                self.advance();
                while let Some(l) = self.current() {
                    if l.indent <= import_indent {
                        break;
                    }
                    self.advance();
                }
                Ok(AstNode::Block(vec![]))
            }
            "type_alias" => self.parse_type_alias(),
            "test" => self.parse_test(),
            "bind" => self.parse_top_level_bind(),
            "pub" => {
                self.advance();
                self.parse_top_level()
            }
            _ => {
                // Unknown top-level: skip the node and all its children
                let skip_indent = line.indent;
                self.advance();
                while let Some(l) = self.current() {
                    if l.indent <= skip_indent {
                        break;
                    }
                    self.advance();
                }
                Ok(AstNode::Block(vec![]))
            }
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
                        } else if type_line.kind == "fn_type" {
                            // Function-type parameter (e.g. f: fn(Int) -> Int)
                            // Skip the entire fn_type subtree and record as "Fn"
                            self.skip_subtree();
                            "Fn".to_string()
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
            let field_indent = field_line.indent;
            self.advance();

            // Parse field type
            let field_type = if let Some(type_line) = self.current() {
                if type_line.kind == "type" && type_line.indent > field_indent {
                    let ty = type_line.value.clone();
                    self.advance();
                    ty
                } else {
                    "Int".to_string()
                }
            } else {
                "Int".to_string()
            };

            // Skip generic type constraint fields (e.g., "T" with type "Renderer[T]")
            // These are type parameters, not real struct fields
            let clean_name = field_name.strip_suffix(" pub").unwrap_or(&field_name);
            let is_type_param = clean_name.len() <= 2
                && clean_name.chars().all(|c| c.is_ascii_uppercase())
                && field_type.contains('[');
            if !is_type_param {
                fields.push((field_name, field_type));
            }
        }

        Ok(AstNode::StructDecl {
            name,
            fields,
            is_pub,
        })
    }

    /// Parse an enum declaration: enum Name { variants... }
    fn parse_enum(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
        let name = line.value.clone();
        let enum_indent = line.indent;
        let is_pub = false; // TODO: track pub status
        self.advance();

        let mut variants = Vec::new();

        // Parse variants until we hit a lower or equal indentation
        while let Some(variant_line) = self.current() {
            if variant_line.indent <= enum_indent {
                break;
            }

            if variant_line.kind == "variant" {
                let variant_name = variant_line.value.clone();
                let variant_indent = variant_line.indent;
                self.advance();

                // Parse associated data types if any (indented under variant)
                let mut data_types = Vec::new();
                while let Some(type_line) = self.current() {
                    if type_line.indent <= variant_indent {
                        break;
                    }
                    if type_line.kind == "type" {
                        data_types.push(type_line.value.clone());
                        self.advance();
                    } else {
                        break;
                    }
                }

                variants.push(crate::ast::EnumVariant {
                    name: variant_name,
                    data_types,
                });
            } else {
                // Skip unknown nodes under enum
                self.advance();
            }
        }

        Ok(AstNode::EnumDecl {
            name,
            variants,
            is_pub,
        })
    }

    /// Parse an interface declaration: interface Name { methods... }
    fn parse_interface(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
        let name = line.value.clone();
        let interface_indent = line.indent;
        let is_pub = false;
        self.advance();

        let mut methods = Vec::new();

        // Parse method signatures — each is a `fn name` with optional params/returns children
        // Interface methods may have no body (just signature)
        loop {
            let (should_parse, is_fn) = if let Some(l) = self.current() {
                if l.indent <= interface_indent {
                    break;
                }
                (true, l.kind == "fn")
            } else {
                break;
            };

            if is_fn {
                let method_name = self
                    .current()
                    .unwrap()
                    .value
                    .split_whitespace()
                    .next()
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                let method_indent = self.current().unwrap().indent;
                self.advance();

                // Skip all children of this fn signature (params, returns, body)
                while let Some(child) = self.current() {
                    if child.indent <= method_indent {
                        break;
                    }
                    self.advance();
                }

                methods.push(crate::ast::InterfaceMethod {
                    name: method_name,
                    params: vec![],
                    return_type: "Void".to_string(),
                });
            } else {
                self.advance();
            }
        }

        Ok(AstNode::InterfaceDecl {
            name,
            methods,
            is_pub,
        })
    }

    /// Parse an impl block.
    /// AST format (from zig-out/bin/forge parse):
    ///   impl
    ///     type Point              ← plain impl: target type
    ///     fn method...
    /// OR:
    ///   impl
    ///     type Display            ← impl for: interface type
    ///     for
    ///       type Point            ← target type
    ///     fn method...
    fn parse_impl(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
        let impl_indent = line.indent;
        self.advance();

        // First child should be `type <Name>` — the interface name (or target for plain impl)
        let first_type = if let Some(l) = self.current() {
            if l.kind == "type" && l.indent > impl_indent {
                let v = l.value.clone();
                self.advance();
                v
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        // Check for `for` keyword (impl Interface for Type)
        let (interface, target_type) = if let Some(l) = self.current() {
            if l.kind == "for" && l.indent > impl_indent {
                self.advance(); // consume `for`
                                // Next should be `type Point`
                let target = if let Some(t) = self.current() {
                    if t.kind == "type" && t.indent > impl_indent {
                        let v = t.value.clone();
                        self.advance();
                        v
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };
                (first_type, target)
            } else {
                // Plain `impl Type:` — no interface
                (String::new(), first_type)
            }
        } else {
            (String::new(), first_type)
        };

        let mut methods = Vec::new();

        // Parse method implementations
        loop {
            let is_fn = if let Some(l) = self.current() {
                if l.indent <= impl_indent {
                    break;
                }
                l.kind == "fn"
            } else {
                break;
            };

            if is_fn {
                methods.push(self.parse_function()?);
            } else {
                self.advance();
            }
        }

        Ok(AstNode::ImplBlock {
            interface,
            target_type,
            methods,
        })
    }

    /// Parse an import declaration.
    /// AST format: `from std math import abs, min, max`
    fn parse_import(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
        let content = line.value.clone();
        let import_indent = line.indent;
        self.advance();

        // Skip any children (wildcard imports may have child nodes)
        while let Some(l) = self.current() {
            if l.indent <= import_indent {
                break;
            }
            self.advance();
        }

        // Parse format: "module import name1, name2, ..."
        let parts: Vec<&str> = content.split(" import ").collect();
        if parts.len() == 2 {
            let module = parts[0].trim().to_string();
            let names: Vec<String> = parts[1]
                .trim()
                .split(',')
                .map(|s| s.trim().trim_matches(|c| c == '(' || c == ')').to_string())
                .filter(|s| !s.is_empty())
                .collect();
            Ok(AstNode::Import { module, names })
        } else {
            // Couldn't parse — treat as empty import
            Ok(AstNode::Import {
                module: content,
                names: vec![],
            })
        }
    }

    /// Parse a type alias: `type_alias Name` with `type Target` child
    fn parse_type_alias(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
        let alias_name = line.value.clone();
        let alias_indent = line.indent;
        self.advance();

        // Extract target type from child "type" node
        let mut target = String::new();
        while let Some(l) = self.current() {
            if l.indent <= alias_indent {
                break;
            }
            if l.kind == "type" {
                target = l.value.clone();
            }
            self.advance();
        }

        Ok(AstNode::TypeAlias {
            name: alias_name,
            target,
        })
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

    /// Parse a top-level bind (global variable declaration)
    /// e.g., mut d_count: Int := 0
    fn parse_top_level_bind(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
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
            "for" => self.parse_for(),
            "break" => {
                self.advance();
                Ok(AstNode::Break)
            }
            "continue" => {
                self.advance();
                Ok(AstNode::Continue)
            }
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
        // Current line is "assign <op>" where op is "=", "+=", "-=", "*=", "/="
        let op = self.current().map(|l| l.value.clone()).unwrap_or_default();
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
        let rhs = self.parse_expression()?;

        // For compound assignments, expand: x += 1 → x = x + 1
        let value = match op.as_str() {
            "+=" => AstNode::BinaryOp {
                op: BinaryOp::Add,
                left: Box::new(AstNode::Identifier(name.clone())),
                right: Box::new(rhs),
            },
            "-=" => AstNode::BinaryOp {
                op: BinaryOp::Sub,
                left: Box::new(AstNode::Identifier(name.clone())),
                right: Box::new(rhs),
            },
            "*=" => AstNode::BinaryOp {
                op: BinaryOp::Mul,
                left: Box::new(AstNode::Identifier(name.clone())),
                right: Box::new(rhs),
            },
            "/=" => AstNode::BinaryOp {
                op: BinaryOp::Div,
                left: Box::new(AstNode::Identifier(name.clone())),
                right: Box::new(rhs),
            },
            _ => rhs, // plain "=" — use rhs directly
        };

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

        // Handle generic type instantiation: call / index / ident Channel / ident Int
        // In this case, we extract the base type name and type arg, then mangle them
        let func_name = if func_line.kind == "index" {
            let index_indent = func_line.indent;
            self.advance(); // consume "index"
                            // Next line should be "ident Channel" (the base type)
            let base_name = if let Some(base) = self.current() {
                if base.kind == "ident" {
                    let n = base.value.clone();
                    self.advance();
                    n
                } else {
                    "Unknown".to_string()
                }
            } else {
                "Unknown".to_string()
            };
            // Get type argument
            let type_arg = if let Some(type_line) = self.current() {
                if type_line.kind == "ident" {
                    let t = type_line.value.clone();
                    self.advance();
                    t
                } else {
                    "Unknown".to_string()
                }
            } else {
                "Unknown".to_string()
            };
            // Skip any remaining children
            while let Some(l) = self.current() {
                if l.indent <= index_indent {
                    break;
                }
                self.advance();
            }
            // Create mangled name for monomorphization: identity_Int
            format!("{}_{}", base_name, type_arg)
        } else if func_line.kind != "ident" {
            return Err(CompileError::UnsupportedFeature(format!(
                "Expected function name, got '{}'",
                func_line.kind
            )));
        } else {
            let n = func_line.value.clone();
            self.advance();
            n
        };

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
                // Strip surrounding quotes from string literals
                let value = if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
                    value[1..value.len() - 1].to_string()
                } else {
                    value
                };
                self.advance();
                Ok(AstNode::StringLiteral(value))
            }
            "string_interp" => self.parse_string_interp(),
            "list" => self.parse_list_literal(),
            "map" => self.parse_map_literal(),
            "set" => self.parse_set_literal(),
            "struct_init" => self.parse_struct_init(),
            "field" => self.parse_field_access(),
            "index" => self.parse_index(),
            "try" => self.parse_try(),
            "spawn" => {
                let line = self.current().unwrap();
                let spawn_indent = line.indent;
                self.advance();
                let expr = self.parse_expression()?;
                Ok(AstNode::Spawn { expr: Box::new(expr) })
            }
            "await" => {
                let line = self.current().unwrap();
                let await_indent = line.indent;
                self.advance();
                let expr = self.parse_expression()?;
                Ok(AstNode::Await { expr: Box::new(expr) })
            }
            "call" => self.parse_call(),
            "lambda" => self.parse_lambda(),
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
                        // For pipe, parse left operand as a simple expression (not full expression
                        // to avoid infinite recursion with chained pipes)
                        let value = self.parse_primary()?;
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
            "unary" => {
                // Parse unary operation: unary <op> followed by operand
                let op_str = line.value.clone();
                self.advance();

                // Parse the operand
                let operand = self.parse_expression()?;

                // Map operator string to UnaryOp
                let op = match op_str.as_str() {
                    "negate" => UnaryOp::Neg,
                    "not" => UnaryOp::Not,
                    "bitnot" => UnaryOp::BitNot,
                    _ => UnaryOp::Neg, // Default to negate for unknown
                };

                Ok(AstNode::UnaryOp {
                    op,
                    operand: Box::new(operand),
                })
            }
            "match" => self.parse_match(),
            "tuple" => self.parse_tuple(),
            _ => {
                // Skip unknown nodes
                self.advance();
                Ok(AstNode::IntLiteral(0)) // Placeholder
            }
        }
    }

    /// Parse a primary expression (simple literals and identifiers, not compound expressions)
    /// Used by pipe operator to avoid infinite recursion with chained pipes
    fn parse_primary(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().ok_or_else(|| {
            CompileError::UnsupportedFeature("Unexpected end of input".to_string())
        })?;

        match line.kind.as_str() {
            "int" => {
                let value_str = &line.value;
                let value = if value_str.starts_with("0x") || value_str.starts_with("0X") {
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
                let value = if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
                    value[1..value.len() - 1].to_string()
                } else {
                    value
                };
                self.advance();
                Ok(AstNode::StringLiteral(value))
            }
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
            "call" => self.parse_call(),
            _ => {
                // For any other type, fall back to full expression parsing
                self.parse_expression()
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
                    // Strip surrounding quotes from string literals
                    let lit_value = if lit_value.len() >= 2
                        && lit_value.starts_with('"')
                        && lit_value.ends_with('"')
                    {
                        lit_value[1..lit_value.len() - 1].to_string()
                    } else {
                        lit_value
                    };
                    parts.push(StringInterpPart::Literal(lit_value));
                    self.advance();
                }
                _ => {
                    // Any expression node (ident, method_call, call, binary, etc.)
                    // Delegate to the full expression parser so method calls like
                    // {x.len()} and {foo()} are correctly parsed rather than dropped.
                    let expr = self.parse_expression()?;
                    parts.push(StringInterpPart::Expr(Box::new(expr)));
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
        // Format: either direct key/value pairs or "entry" wrapper nodes
        while let Some(line) = self.current() {
            if line.indent <= start_indent {
                break;
            }

            // Handle "entry" wrapper nodes from the self-hosted AST printer
            if line.kind == "entry" {
                let entry_indent = line.indent;
                self.advance();

                // Parse key (first child of entry)
                let key = if let Some(key_line) = self.current() {
                    if key_line.indent > entry_indent {
                        if key_line.kind == "string" {
                            let val = key_line.value.clone();
                            let val = if val.len() >= 2 && val.starts_with('"') && val.ends_with('"') {
                                val[1..val.len() - 1].to_string()
                            } else {
                                val
                            };
                            self.advance();
                            AstNode::StringLiteral(val)
                        } else if key_line.kind == "int" || key_line.kind == "integer" {
                            let val = key_line.value.parse().unwrap_or(0);
                            self.advance();
                            AstNode::IntLiteral(val)
                        } else {
                            self.parse_expression()?
                        }
                    } else {
                        continue;
                    }
                } else {
                    break;
                };

                // Parse value (second child of entry)
                let value = if let Some(val_line) = self.current() {
                    if val_line.indent > entry_indent {
                        self.parse_expression()?
                    } else {
                        AstNode::IntLiteral(0)
                    }
                } else {
                    AstNode::IntLiteral(0)
                };

                entries.push((key, value));
                continue;
            }

            // Direct key-value pairs (legacy format)
            let line_indent = line.indent;
            let key = if line.kind == "string" {
                let val = line.value.clone();
                let val = if val.len() >= 2 && val.starts_with('"') && val.ends_with('"') {
                    val[1..val.len() - 1].to_string()
                } else {
                    val
                };
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
                self.parse_expression()?
            };

            // Check for value (indented more)
            let value = if let Some(val_line) = self.current() {
                if val_line.indent > line_indent {
                    self.parse_expression()?
                } else {
                    AstNode::IntLiteral(0)
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

    /// Parse set literal: set (N items) followed by elements
    fn parse_set_literal(&mut self) -> Result<AstNode, CompileError> {
        let set_line = self.current().unwrap();
        let start_indent = set_line.indent;
        self.advance();

        let mut elements = Vec::new();

        while let Some(line) = self.current() {
            if line.indent <= start_indent {
                break;
            }

            if line.kind == "string" {
                let val = line.value.clone();
                let val = if val.len() >= 2 && val.starts_with('"') && val.ends_with('"') {
                    val[1..val.len() - 1].to_string()
                } else {
                    val
                };
                self.advance();
                elements.push(AstNode::StringLiteral(val));
            } else if line.kind == "int" || line.kind == "integer" {
                let val = line.value.parse().unwrap_or(0);
                self.advance();
                elements.push(AstNode::IntLiteral(val));
            } else {
                elements.push(self.parse_expression()?);
            }
        }

        Ok(AstNode::SetLiteral {
            elements,
            elem_type: None,
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
        self.advance();

        // Parse the object being accessed (should be next expression)
        let obj = self.parse_expression()?;

        Ok(AstNode::FieldAccess {
            obj: Box::new(obj),
            field,
        })
    }

    /// Parse index access: index <expr> <index>
    fn parse_index(&mut self) -> Result<AstNode, CompileError> {
        self.advance();
        let expr = self.parse_expression()?;
        let index = self.parse_expression()?;

        Ok(AstNode::Index {
            expr: Box::new(expr),
            index: Box::new(index),
        })
    }

    /// Parse tuple literal: tuple (N items) followed by N expressions
    fn parse_tuple(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
        let tuple_indent = line.indent;
        self.advance();

        // Parse tuple elements (indented expressions)
        let mut elements = Vec::new();
        while let Some(line) = self.current() {
            if line.indent <= tuple_indent {
                break;
            }
            elements.push(self.parse_expression()?);
        }

        // Represent tuple as a struct initialization with numeric field names
        // We'll use a special naming convention: "tuple_2" for 2-element tuple
        let tuple_name = format!("tuple_{}", elements.len());
        let fields: Vec<(String, AstNode)> = elements
            .into_iter()
            .enumerate()
            .map(|(i, elem)| (i.to_string(), elem))
            .collect();

        Ok(AstNode::StructInit {
            name: tuple_name,
            fields,
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
        let base_indent = line.indent;
        self.advance();

        // Parse object (should be next expression)
        let obj = self.parse_expression()?;

        // Parse additional arguments (arg nodes at same or higher indent)
        let mut args = vec![obj];
        while let Some(arg_line) = self.current() {
            if arg_line.kind == "arg" && arg_line.indent >= base_indent {
                self.advance();
                let arg_val = self.parse_expression()?;
                args.push(arg_val);
            } else {
                break;
            }
        }

        // Strip leading dot and map to runtime function
        let func_name = if method.starts_with('.') {
            match method.as_str() {
                ".to_string" => "to_string".to_string(),
                ".to_float" => "to_float".to_string(),
                ".to_int" => "to_int".to_string(),
                ".is_empty" => "is_empty".to_string(),
                ".clear" => "clear".to_string(),
                ".remove" => "remove".to_string(),
                ".reverse" => "reverse".to_string(),
                ".push" => "forge_list_push_value".to_string(),
                ".pop" => "forge_list_pop".to_string(),
                _ => method[1..].to_string(), // Remove leading dot
            }
        } else {
            method
        };

        // Convert to call: func_name(obj, args...)
        Ok(AstNode::Call {
            func: func_name,
            args,
        })
    }

    /// Parse match expression: match expr { arms... }
    fn parse_match(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
        let match_indent = line.indent;
        self.advance();

        // Parse the expression being matched
        let expr = self.parse_expression()?;

        // Parse match arms
        let mut arms = Vec::new();
        while let Some(line) = self.current() {
            if line.indent <= match_indent {
                break;
            }

            if line.kind == "arm" {
                self.advance();
                // Parse pattern and expression for this arm
                let pattern = self.parse_match_pattern()?;

                // Skip the "body" marker if present
                if let Some(body_line) = self.current() {
                    if body_line.kind == "body" {
                        self.advance();
                    }
                }

                let arm_expr = self.parse_expression()?;
                arms.push(crate::ast::MatchArm {
                    pattern,
                    expr: Box::new(arm_expr),
                });
            } else {
                // Skip unknown nodes
                self.advance();
            }
        }

        Ok(AstNode::Match {
            expr: Box::new(expr),
            arms,
        })
    }

    /// Parse a match pattern
    fn parse_match_pattern(&mut self) -> Result<crate::ast::MatchPattern, CompileError> {
        let line = self.current().ok_or_else(|| {
            CompileError::UnsupportedFeature("Expected pattern in match arm".to_string())
        })?;

        match line.kind.as_str() {
            "pattern" => {
                // Pattern lines from self-hosted compiler have format like:
                // "pattern bind <name>" or "pattern _" or "pattern <literal>"
                let value = line.value.clone();
                self.advance();

                // Parse the pattern value
                if value == "_" || value == "wildcard" {
                    Ok(crate::ast::MatchPattern::Wildcard)
                } else if value.starts_with("bind ") {
                    // Binding pattern: "bind <name>"
                    let var_name = value[5..].to_string();
                    Ok(crate::ast::MatchPattern::Variable(var_name))
                } else if value.contains(".") {
                    // Enum variant pattern: EnumName.VariantName
                    let parts: Vec<&str> = value.split('.').collect();
                    if parts.len() == 2 {
                        Ok(crate::ast::MatchPattern::EnumVariant {
                            enum_name: parts[0].to_string(),
                            variant_name: parts[1].to_string(),
                            bind_vars: Vec::new(),
                        })
                    } else {
                        Ok(crate::ast::MatchPattern::Variable(value))
                    }
                } else {
                    // Try to parse as integer literal
                    if let Ok(n) = value.parse::<i64>() {
                        Ok(crate::ast::MatchPattern::Literal(
                            crate::ast::AstNode::IntLiteral(n),
                        ))
                    } else if value == "true" {
                        Ok(crate::ast::MatchPattern::Literal(
                            crate::ast::AstNode::BoolLiteral(true),
                        ))
                    } else if value == "false" {
                        Ok(crate::ast::MatchPattern::Literal(
                            crate::ast::AstNode::BoolLiteral(false),
                        ))
                    } else {
                        // Variable binding
                        Ok(crate::ast::MatchPattern::Variable(value))
                    }
                }
            }
            "variant" => {
                // Enum variant pattern: EnumName.VariantName
                let variant_str = line.value.clone();
                self.advance();

                // Parse EnumName.VariantName format
                let parts: Vec<&str> = variant_str.split('.').collect();
                if parts.len() == 2 {
                    let enum_name = parts[0].to_string();
                    let variant_name = parts[1].to_string();

                    // Check for bind variables (indented under variant)
                    let mut bind_vars = Vec::new();
                    while let Some(bind_line) = self.current() {
                        if bind_line.kind == "bind" {
                            let var_name = bind_line
                                .value
                                .split_whitespace()
                                .next()
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| bind_line.value.clone());
                            bind_vars.push(var_name);
                            self.advance();
                        } else {
                            break;
                        }
                    }

                    Ok(crate::ast::MatchPattern::EnumVariant {
                        enum_name,
                        variant_name,
                        bind_vars,
                    })
                } else {
                    // Simple variant or variable
                    Ok(crate::ast::MatchPattern::Variable(variant_str))
                }
            }
            "wildcard" => {
                self.advance();
                Ok(crate::ast::MatchPattern::Wildcard)
            }
            "ident" => {
                let name = line.value.clone();
                self.advance();
                Ok(crate::ast::MatchPattern::Variable(name))
            }
            _ => {
                // Try to parse as literal
                let literal = self.parse_expression()?;
                Ok(crate::ast::MatchPattern::Literal(literal))
            }
        }
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

    /// Parse for-in loop: for var[, index] in iterable { body }
    fn parse_for(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
        let indent = line.indent;
        let var_spec = line.value.clone(); // May contain "var" or "var, index"
        self.advance();

        // Parse variable names (handle "name, i" syntax for index)
        let (var_name, index_var) = if var_spec.contains(',') {
            let parts: Vec<&str> = var_spec.split(',').map(|s| s.trim()).collect();
            if parts.len() == 2 {
                (parts[0].to_string(), Some(parts[1].to_string()))
            } else {
                (var_spec, None)
            }
        } else {
            (var_spec, None)
        };

        // Parse iterable expression
        let iterable = self.parse_expression()?;

        // Parse body statements (everything indented more than 'for')
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

        Ok(AstNode::For {
            var: var_name,
            index_var,
            iterable: Box::new(iterable),
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

        let mut then_branch = AstNode::Block(vec![]);
        let mut else_branch = None;

        if let Some(line) = self.current() {
            if line.indent > indent && line.kind == "then" {
                let branch_indent = line.indent;
                self.advance();
                then_branch = self.parse_branch_block(branch_indent)?;
            }
        }

        if let Some(line) = self.current() {
            if line.indent > indent && line.kind == "elif" {
                else_branch = Some(Box::new(self.parse_elif_chain()?));
            } else if line.indent > indent && line.kind == "else" {
                let branch_indent = line.indent;
                self.advance();
                else_branch = Some(Box::new(self.parse_branch_block(branch_indent)?));
            }
        }

        Ok(AstNode::If {
            cond: Box::new(cond),
            then_branch: Box::new(then_branch),
            else_branch,
        })
    }

    /// Parse lambda expression: fn(params) => body
    fn parse_lambda(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
        let lambda_indent = line.indent;
        self.advance();

        // Parse parameters (indented under lambda)
        let mut params = Vec::new();
        while let Some(param_line) = self.current() {
            if param_line.indent <= lambda_indent {
                break;
            }
            if param_line.kind == "param" {
                let param_name = param_line.value.clone();
                let param_indent = param_line.indent;
                self.advance();
                // Check for type child node
                let param_type = if let Some(type_line) = self.current() {
                    if type_line.kind == "type" && type_line.indent > param_indent {
                        let ty = type_line.value.clone();
                        self.advance();
                        ty
                    } else {
                        "Int".to_string()
                    }
                } else {
                    "Int".to_string()
                };
                params.push((param_name, param_type));
            } else {
                break;
            }
        }

        // Parse capture variables (optional, marked with "capture" kind)
        let mut capture_vars = Vec::new();
        while let Some(capture_line) = self.current() {
            if capture_line.indent <= lambda_indent {
                break;
            }
            if capture_line.kind == "capture" {
                capture_vars.push(capture_line.value.clone());
                self.advance();
            } else {
                break;
            }
        }

        // Parse body expression (indented under lambda)
        // Skip "body" wrapper node if present
        if let Some(body_line) = self.current() {
            if body_line.kind == "body" && body_line.indent > lambda_indent {
                self.advance();
            }
        }
        let body = if let Some(body_line) = self.current() {
            if body_line.indent > lambda_indent {
                self.parse_expression()?
            } else {
                AstNode::IntLiteral(0) // Placeholder
            }
        } else {
            AstNode::IntLiteral(0) // Placeholder
        };

        Ok(AstNode::Lambda {
            params,
            return_type: None, // Infer from body
            body: Box::new(body),
            capture_vars,
        })
    }

    fn parse_branch_block(&mut self, branch_indent: usize) -> Result<AstNode, CompileError> {
        let mut stmts = Vec::new();

        while let Some(line) = self.current() {
            if line.indent <= branch_indent {
                break;
            }
            stmts.push(self.parse_statement()?);
        }

        Ok(if stmts.len() == 1 {
            stmts.into_iter().next().unwrap()
        } else {
            AstNode::Block(stmts)
        })
    }

    fn parse_elif_chain(&mut self) -> Result<AstNode, CompileError> {
        let line = self.current().unwrap();
        let branch_indent = line.indent;
        self.advance();

        let cond = self.parse_expression()?;
        let then_branch = self.parse_branch_block(branch_indent)?;

        let mut else_branch = None;
        if let Some(line) = self.current() {
            if line.indent == branch_indent && line.kind == "elif" {
                else_branch = Some(Box::new(self.parse_elif_chain()?));
            } else if line.indent == branch_indent && line.kind == "else" {
                self.advance();
                else_branch = Some(Box::new(self.parse_branch_block(branch_indent)?));
            }
        }

        Ok(AstNode::If {
            cond: Box::new(cond),
            then_branch: Box::new(then_branch),
            else_branch,
        })
    }
}

/// Parse a .fg file and return AST
pub fn parse_file(path: &str) -> Result<Vec<AstNode>, CompileError> {
    // Prefer the self-hosted parser when available, otherwise fall back to the
    // bootstrap compiler binary from `zig build`.
    let parser_bin = if std::path::Path::new("./zig-out/bin/forge").exists() {
        "./zig-out/bin/forge"
    } else {
        "./self-host/forge_main"
    };

    let output = std::process::Command::new(parser_bin)
        .args(["parse", path])
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
