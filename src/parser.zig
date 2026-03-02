// parser — recursive descent parser for forge
//
// consumes tokens from the lexer and builds an AST.
// hand-written for full control over error messages
// and error recovery.
//
// design: pre-tokenize the full source via lexer.tokenize(),
// then walk the token array with arbitrary lookahead.
// all AST nodes are arena-allocated and freed in one shot.

const std = @import("std");
const ast = @import("ast.zig");
const lexer_mod = @import("lexer.zig");
const errors = @import("errors.zig");

const Token = lexer_mod.Token;
const TokenKind = lexer_mod.TokenKind;
const Lexer = lexer_mod.Lexer;
const Location = errors.Location;

/// errors that can occur during parsing.
/// just allocation failures — parse errors are collected in diagnostics.
pub const ParseError = std.mem.Allocator.Error;

pub const Parser = struct {
    tokens: []const Token,
    pos: u32,
    allocator: std.mem.Allocator,
    diagnostics: errors.DiagnosticList,
    source: []const u8,

    pub fn init(tokens: []const Token, source: []const u8, allocator: std.mem.Allocator) Parser {
        return .{
            .tokens = tokens,
            .pos = 0,
            .allocator = allocator,
            .diagnostics = errors.DiagnosticList.init(allocator, source),
            .source = source,
        };
    }

    pub fn deinit(self: *Parser) void {
        self.diagnostics.deinit();
    }

    // ---------------------------------------------------------------
    // token navigation
    // ---------------------------------------------------------------
    // all of these skip comment tokens automatically.

    /// look at the current token without consuming it.
    fn peek(self: *const Parser) Token {
        var i = self.pos;
        while (i < self.tokens.len) {
            if (self.tokens[i].kind != .comment) return self.tokens[i];
            i += 1;
        }
        // past the end — return the last token (should be eof)
        return self.tokens[self.tokens.len - 1];
    }

    /// look ahead by `offset` non-comment tokens.
    fn peekAhead(self: *const Parser, offset: u32) Token {
        var i = self.pos;
        var skipped: u32 = 0;
        while (i < self.tokens.len) {
            if (self.tokens[i].kind != .comment) {
                if (skipped == offset) return self.tokens[i];
                skipped += 1;
            }
            i += 1;
        }
        return self.tokens[self.tokens.len - 1];
    }

    /// consume the current token and return it.
    fn advance(self: *Parser) Token {
        while (self.pos < self.tokens.len) {
            const tok = self.tokens[self.pos];
            self.pos += 1;
            if (tok.kind != .comment) return tok;
        }
        return self.tokens[self.tokens.len - 1];
    }

    /// check if the current token matches the expected kind.
    fn check(self: *const Parser, kind: TokenKind) bool {
        return self.peek().kind == kind;
    }

    /// if the current token matches, consume it and return true.
    fn match(self: *Parser, kind: TokenKind) bool {
        if (self.check(kind)) {
            _ = self.advance();
            return true;
        }
        return false;
    }

    /// consume a token of the expected kind, or emit an error.
    fn expect(self: *Parser, kind: TokenKind) ParseError!Token {
        const tok = self.peek();
        if (tok.kind == kind) {
            return self.advance();
        }
        try self.diagnostics.addError(
            tok.location,
            try std.fmt.allocPrint(self.allocator, "expected {s}, got {s}", .{
                @tagName(kind),
                @tagName(tok.kind),
            }),
        );
        return tok;
    }

    /// skip over newline tokens. useful inside arg lists, param lists, etc.
    fn skipNewlines(self: *Parser) void {
        while (self.peek().kind == .newline) {
            _ = self.advance();
        }
    }

    /// allocate a value on the arena and return a pointer to it.
    fn create(self: *Parser, comptime T: type, value: T) ParseError!*const T {
        const ptr = try self.allocator.create(T);
        @as(*T, @constCast(ptr)).* = value;
        return ptr;
    }

    /// skip tokens until we reach a synchronization point.
    /// used for error recovery — gets us back to a known state.
    fn synchronize(self: *Parser) void {
        while (self.peek().kind != .eof) {
            const kind = self.peek().kind;
            if (kind == .newline or kind == .dedent) {
                _ = self.advance();
                return;
            }
            _ = self.advance();
        }
    }

    // ---------------------------------------------------------------
    // type expressions
    // ---------------------------------------------------------------

    /// type_expr = base_type ["?"] | base_type "!" [type_expr]
    fn parseTypeExpr(self: *Parser) ParseError!*const ast.TypeExpr {
        const base = try self.parseBaseType();

        // optional: T?
        if (self.check(.question)) {
            const q_tok = self.advance();
            return self.create(ast.TypeExpr, .{
                .kind = .{ .optional = base },
                .location = Location.span(base.location, q_tok.location),
            });
        }

        // result: T! or T!E
        if (self.check(.bang)) {
            const bang_tok = self.advance();

            // check if there's an error type following
            // it's T!E only if the next token can start a type (identifier or fn or lparen)
            const next = self.peek().kind;
            if (next == .identifier or next == .kw_fn or next == .lparen) {
                const err_type = try self.parseTypeExpr();
                return self.create(ast.TypeExpr, .{
                    .kind = .{ .result = .{
                        .ok_type = base,
                        .err_type = err_type,
                    } },
                    .location = Location.span(base.location, err_type.location),
                });
            }

            return self.create(ast.TypeExpr, .{
                .kind = .{ .result = .{
                    .ok_type = base,
                    .err_type = null,
                } },
                .location = Location.span(base.location, bang_tok.location),
            });
        }

        return base;
    }

    /// base_type = IDENT ["[" type_list "]"]
    ///           | "(" type_list ")"
    ///           | fn_type
    fn parseBaseType(self: *Parser) ParseError!*const ast.TypeExpr {
        const tok = self.peek();

        // fn type: fn(Int, String) -> Bool
        if (tok.kind == .kw_fn) {
            return self.parseFnType();
        }

        // tuple type: (Int, String)
        if (tok.kind == .lparen) {
            return self.parseTupleType();
        }

        // named or generic type
        if (tok.kind == .identifier) {
            const name_tok = self.advance();

            // check for generic args: Type[T, U]
            if (self.check(.lbracket)) {
                _ = self.advance(); // skip [
                var args: std.ArrayList(*const ast.TypeExpr) = .empty;
                try args.append(self.allocator, try self.parseTypeExpr());
                while (self.match(.comma)) {
                    try args.append(self.allocator, try self.parseTypeExpr());
                }
                const end_tok = try self.expect(.rbracket);

                return self.create(ast.TypeExpr, .{
                    .kind = .{ .generic = .{
                        .name = name_tok.lexeme,
                        .args = try args.toOwnedSlice(self.allocator),
                    } },
                    .location = Location.span(name_tok.location, end_tok.location),
                });
            }

            return self.create(ast.TypeExpr, .{
                .kind = .{ .named = name_tok.lexeme },
                .location = name_tok.location,
            });
        }

        // unexpected token
        try self.diagnostics.addError(tok.location, "expected type");
        self.synchronize();
        return self.create(ast.TypeExpr, .{
            .kind = .{ .named = "" },
            .location = tok.location,
        });
    }

    /// fn_type = "fn" "(" [type_list] ")" ["->" type_expr]
    fn parseFnType(self: *Parser) ParseError!*const ast.TypeExpr {
        const fn_tok = self.advance(); // skip fn
        _ = try self.expect(.lparen);

        var params: std.ArrayList(*const ast.TypeExpr) = .empty;
        if (!self.check(.rparen)) {
            try params.append(self.allocator, try self.parseTypeExpr());
            while (self.match(.comma)) {
                try params.append(self.allocator, try self.parseTypeExpr());
            }
        }
        var end_loc = (try self.expect(.rparen)).location;

        var return_type: ?*const ast.TypeExpr = null;
        if (self.match(.arrow)) {
            const ret = try self.parseTypeExpr();
            return_type = ret;
            end_loc = ret.location;
        }

        return self.create(ast.TypeExpr, .{
            .kind = .{ .fn_type = .{
                .params = try params.toOwnedSlice(self.allocator),
                .return_type = return_type,
            } },
            .location = Location.span(fn_tok.location, end_loc),
        });
    }

    /// tuple type: "(" type "," { type "," } ")"
    fn parseTupleType(self: *Parser) ParseError!*const ast.TypeExpr {
        const lparen_tok = self.advance(); // skip (

        var types: std.ArrayList(*const ast.TypeExpr) = .empty;
        if (!self.check(.rparen)) {
            try types.append(self.allocator, try self.parseTypeExpr());
            while (self.match(.comma)) {
                if (self.check(.rparen)) break;
                try types.append(self.allocator, try self.parseTypeExpr());
            }
        }
        const rparen_tok = try self.expect(.rparen);

        return self.create(ast.TypeExpr, .{
            .kind = .{ .tuple = try types.toOwnedSlice(self.allocator) },
            .location = Location.span(lparen_tok.location, rparen_tok.location),
        });
    }

    // ---------------------------------------------------------------
    // expressions
    // ---------------------------------------------------------------

    /// entry point for expression parsing.
    pub fn parseExpression(self: *Parser) ParseError!*const ast.Expr {
        return self.parseOrExpr();
    }

    /// or_expr = and_expr { "or" and_expr }
    fn parseOrExpr(self: *Parser) ParseError!*const ast.Expr {
        var left = try self.parseAndExpr();
        while (self.check(.kw_or)) {
            _ = self.advance();
            const right = try self.parseAndExpr();
            left = try self.create(ast.Expr, .{
                .kind = .{ .binary = .{ .left = left, .op = .@"or", .right = right } },
                .location = Location.span(left.location, right.location),
            });
        }
        return left;
    }

    /// and_expr = not_expr { "and" not_expr }
    fn parseAndExpr(self: *Parser) ParseError!*const ast.Expr {
        var left = try self.parseNotExpr();
        while (self.check(.kw_and)) {
            _ = self.advance();
            const right = try self.parseNotExpr();
            left = try self.create(ast.Expr, .{
                .kind = .{ .binary = .{ .left = left, .op = .@"and", .right = right } },
                .location = Location.span(left.location, right.location),
            });
        }
        return left;
    }

    /// not_expr = "not" not_expr | comparison
    fn parseNotExpr(self: *Parser) ParseError!*const ast.Expr {
        if (self.check(.kw_not)) {
            const tok = self.advance();
            const operand = try self.parseNotExpr();
            return self.create(ast.Expr, .{
                .kind = .{ .unary = .{ .op = .not, .operand = operand } },
                .location = Location.span(tok.location, operand.location),
            });
        }
        return self.parseComparison();
    }

    /// comparison = pipe_expr { comp_op pipe_expr }
    fn parseComparison(self: *Parser) ParseError!*const ast.Expr {
        var left = try self.parsePipeExpr();
        while (true) {
            const op: ast.BinaryOp = switch (self.peek().kind) {
                .eq_eq => .eq,
                .bang_eq => .neq,
                .less => .lt,
                .greater => .gt,
                .less_eq => .lte,
                .greater_eq => .gte,
                else => break,
            };
            _ = self.advance();
            const right = try self.parsePipeExpr();
            left = try self.create(ast.Expr, .{
                .kind = .{ .binary = .{ .left = left, .op = op, .right = right } },
                .location = Location.span(left.location, right.location),
            });
        }
        return left;
    }

    /// pipe_expr = add_expr { "|" add_expr }
    fn parsePipeExpr(self: *Parser) ParseError!*const ast.Expr {
        var left = try self.parseAddExpr();
        while (self.check(.pipe)) {
            _ = self.advance();
            const right = try self.parseAddExpr();
            left = try self.create(ast.Expr, .{
                .kind = .{ .binary = .{ .left = left, .op = .pipe, .right = right } },
                .location = Location.span(left.location, right.location),
            });
        }
        return left;
    }

    /// add_expr = mul_expr { ("+" | "-") mul_expr }
    fn parseAddExpr(self: *Parser) ParseError!*const ast.Expr {
        var left = try self.parseMulExpr();
        while (true) {
            const op: ast.BinaryOp = switch (self.peek().kind) {
                .plus => .add,
                .minus => .sub,
                else => break,
            };
            _ = self.advance();
            const right = try self.parseMulExpr();
            left = try self.create(ast.Expr, .{
                .kind = .{ .binary = .{ .left = left, .op = op, .right = right } },
                .location = Location.span(left.location, right.location),
            });
        }
        return left;
    }

    /// mul_expr = unary_expr { ("*" | "/" | "%") unary_expr }
    fn parseMulExpr(self: *Parser) ParseError!*const ast.Expr {
        var left = try self.parseUnaryExpr();
        while (true) {
            const op: ast.BinaryOp = switch (self.peek().kind) {
                .star => .mul,
                .slash => .div,
                .percent => .mod,
                else => break,
            };
            _ = self.advance();
            const right = try self.parseUnaryExpr();
            left = try self.create(ast.Expr, .{
                .kind = .{ .binary = .{ .left = left, .op = op, .right = right } },
                .location = Location.span(left.location, right.location),
            });
        }
        return left;
    }

    /// unary_expr = "-" unary_expr | postfix_expr
    fn parseUnaryExpr(self: *Parser) ParseError!*const ast.Expr {
        if (self.check(.minus)) {
            const tok = self.advance();
            const operand = try self.parseUnaryExpr();
            return self.create(ast.Expr, .{
                .kind = .{ .unary = .{ .op = .negate, .operand = operand } },
                .location = Location.span(tok.location, operand.location),
            });
        }
        return self.parsePostfixExpr();
    }

    /// postfix_expr = primary { "?" | "!" | call | index | field_access | method_call }
    fn parsePostfixExpr(self: *Parser) ParseError!*const ast.Expr {
        var expr = try self.parsePrimary();

        while (true) {
            switch (self.peek().kind) {
                // unwrap: expr?
                .question => {
                    const tok = self.advance();
                    expr = try self.create(ast.Expr, .{
                        .kind = .{ .unwrap = expr },
                        .location = Location.span(expr.location, tok.location),
                    });
                },
                // try: expr!
                .bang => {
                    const tok = self.advance();
                    expr = try self.create(ast.Expr, .{
                        .kind = .{ .try_expr = expr },
                        .location = Location.span(expr.location, tok.location),
                    });
                },
                // call: expr(args)
                .lparen => {
                    expr = try self.parseCallExpr(expr);
                },
                // index: expr[index]
                .lbracket => {
                    _ = self.advance(); // skip [
                    const index = try self.parseExpression();
                    const end_tok = try self.expect(.rbracket);
                    expr = try self.create(ast.Expr, .{
                        .kind = .{ .index = .{ .object = expr, .index = index } },
                        .location = Location.span(expr.location, end_tok.location),
                    });
                },
                // field access or method call: expr.name or expr.name(args)
                .dot => {
                    _ = self.advance(); // skip .
                    const name_tok = try self.expect(.identifier);

                    // method call: expr.name(args)
                    if (self.check(.lparen)) {
                        _ = self.advance(); // skip (
                        const args = try self.parseArgList();
                        const end_tok = try self.expect(.rparen);
                        expr = try self.create(ast.Expr, .{
                            .kind = .{ .method_call = .{
                                .receiver = expr,
                                .method = name_tok.lexeme,
                                .args = args,
                            } },
                            .location = Location.span(expr.location, end_tok.location),
                        });
                    } else {
                        // field access: expr.name
                        expr = try self.create(ast.Expr, .{
                            .kind = .{ .field_access = .{
                                .object = expr,
                                .field = name_tok.lexeme,
                            } },
                            .location = Location.span(expr.location, name_tok.location),
                        });
                    }
                },
                else => break,
            }
        }
        return expr;
    }

    /// parse a function call's argument list (already past the opening paren).
    fn parseCallExpr(self: *Parser, callee: *const ast.Expr) ParseError!*const ast.Expr {
        _ = self.advance(); // skip (
        const args = try self.parseArgList();
        const end_tok = try self.expect(.rparen);
        return self.create(ast.Expr, .{
            .kind = .{ .call = .{ .callee = callee, .args = args } },
            .location = Location.span(callee.location, end_tok.location),
        });
    }

    /// parse comma-separated argument list: [name "="] expr { "," [name "="] expr }
    fn parseArgList(self: *Parser) ParseError![]const ast.Arg {
        var args: std.ArrayList(ast.Arg) = .empty;
        self.skipNewlines();
        if (self.check(.rparen)) return args.toOwnedSlice(self.allocator);

        try args.append(self.allocator, try self.parseArg());
        while (self.match(.comma)) {
            self.skipNewlines();
            if (self.check(.rparen)) break;
            try args.append(self.allocator, try self.parseArg());
        }
        self.skipNewlines();
        return args.toOwnedSlice(self.allocator);
    }

    /// parse a single argument: [name "="] expr
    fn parseArg(self: *Parser) ParseError!ast.Arg {
        const loc = self.peek().location;

        // check for named argument: name = expr
        if (self.peek().kind == .identifier and self.peekAhead(1).kind == .eq) {
            const name = self.advance().lexeme;
            _ = self.advance(); // skip =
            const value = try self.parseExpression();
            return .{ .name = name, .value = value, .location = loc };
        }

        const value = try self.parseExpression();
        return .{ .name = null, .value = value, .location = loc };
    }

    /// primary = literal | ident | self | grouped/tuple | list | map/set | if_expr | match_expr | lambda
    fn parsePrimary(self: *Parser) ParseError!*const ast.Expr {
        const tok = self.peek();

        switch (tok.kind) {
            // integer literal
            .int_lit => {
                _ = self.advance();
                return self.create(ast.Expr, .{
                    .kind = .{ .int_lit = tok.lexeme },
                    .location = tok.location,
                });
            },
            // float literal
            .float_lit => {
                _ = self.advance();
                return self.create(ast.Expr, .{
                    .kind = .{ .float_lit = tok.lexeme },
                    .location = tok.location,
                });
            },
            // string literal (no interpolation)
            .string_lit => {
                _ = self.advance();
                return self.create(ast.Expr, .{
                    .kind = .{ .string_lit = tok.lexeme },
                    .location = tok.location,
                });
            },
            // interpolated string
            .string_start => {
                return self.parseStringInterpolation();
            },
            // boolean literals
            .kw_true => {
                _ = self.advance();
                return self.create(ast.Expr, .{
                    .kind = .{ .bool_lit = true },
                    .location = tok.location,
                });
            },
            .kw_false => {
                _ = self.advance();
                return self.create(ast.Expr, .{
                    .kind = .{ .bool_lit = false },
                    .location = tok.location,
                });
            },
            // none
            .kw_none => {
                _ = self.advance();
                return self.create(ast.Expr, .{
                    .kind = .none_lit,
                    .location = tok.location,
                });
            },
            // self
            .kw_self => {
                _ = self.advance();
                return self.create(ast.Expr, .{
                    .kind = .self_expr,
                    .location = tok.location,
                });
            },
            // identifier
            .identifier => {
                _ = self.advance();
                return self.create(ast.Expr, .{
                    .kind = .{ .ident = tok.lexeme },
                    .location = tok.location,
                });
            },
            // grouped expression or tuple: (expr) or (expr, expr, ...)
            .lparen => {
                return self.parseGroupedOrTuple();
            },
            // list literal: [expr, expr, ...]
            .lbracket => {
                return self.parseListLiteral();
            },
            // map or set literal: {k: v, ...} or {x, y, ...}
            .lbrace => {
                return self.parseMapOrSetLiteral();
            },
            // if expression
            .kw_if => {
                return self.parseIfExpr();
            },
            // match expression
            .kw_match => {
                return self.parseMatchExpr();
            },
            // lambda: fn(params) => expr  or  fn(params): block
            .kw_fn => {
                return self.parseLambda();
            },
            else => {
                try self.diagnostics.addError(tok.location, "expected expression");
                self.synchronize();
                return self.create(ast.Expr, .{
                    .kind = .err,
                    .location = tok.location,
                });
            },
        }
    }

    /// parse grouped expression or tuple: (expr) or (expr,) or (expr, expr)
    fn parseGroupedOrTuple(self: *Parser) ParseError!*const ast.Expr {
        const lparen = self.advance(); // skip (
        self.skipNewlines();

        // empty tuple: ()
        if (self.check(.rparen)) {
            const rparen = self.advance();
            return self.create(ast.Expr, .{
                .kind = .{ .tuple = &.{} },
                .location = Location.span(lparen.location, rparen.location),
            });
        }

        const first = try self.parseExpression();
        self.skipNewlines();

        // tuple with trailing comma or multiple elements
        if (self.check(.comma)) {
            var elements: std.ArrayList(*const ast.Expr) = .empty;
            try elements.append(self.allocator, first);
            while (self.match(.comma)) {
                self.skipNewlines();
                if (self.check(.rparen)) break;
                try elements.append(self.allocator, try self.parseExpression());
                self.skipNewlines();
            }
            const rparen = try self.expect(.rparen);
            return self.create(ast.Expr, .{
                .kind = .{ .tuple = try elements.toOwnedSlice(self.allocator) },
                .location = Location.span(lparen.location, rparen.location),
            });
        }

        // grouped: (expr)
        const rparen = try self.expect(.rparen);
        return self.create(ast.Expr, .{
            .kind = .{ .grouped = first },
            .location = Location.span(lparen.location, rparen.location),
        });
    }

    /// list literal: [expr, expr, ...]
    fn parseListLiteral(self: *Parser) ParseError!*const ast.Expr {
        const lbracket = self.advance(); // skip [
        var elements: std.ArrayList(*const ast.Expr) = .empty;
        self.skipNewlines();

        if (!self.check(.rbracket)) {
            try elements.append(self.allocator, try self.parseExpression());
            while (self.match(.comma)) {
                self.skipNewlines();
                if (self.check(.rbracket)) break;
                try elements.append(self.allocator, try self.parseExpression());
            }
        }
        self.skipNewlines();
        const rbracket = try self.expect(.rbracket);
        return self.create(ast.Expr, .{
            .kind = .{ .list = try elements.toOwnedSlice(self.allocator) },
            .location = Location.span(lbracket.location, rbracket.location),
        });
    }

    /// map or set literal. {} = empty map, {k: v} = map, {x} = set
    fn parseMapOrSetLiteral(self: *Parser) ParseError!*const ast.Expr {
        const lbrace = self.advance(); // skip {
        self.skipNewlines();

        // empty map: {}
        if (self.check(.rbrace)) {
            const rbrace = self.advance();
            return self.create(ast.Expr, .{
                .kind = .{ .map = &.{} },
                .location = Location.span(lbrace.location, rbrace.location),
            });
        }

        // parse first expression to determine map vs set
        const first = try self.parseExpression();
        self.skipNewlines();

        // map: first expression followed by ":"
        if (self.check(.colon)) {
            _ = self.advance(); // skip :
            var entries: std.ArrayList(ast.MapEntry) = .empty;
            const first_value = try self.parseExpression();
            try entries.append(self.allocator, .{
                .key = first,
                .value = first_value,
                .location = first.location,
            });

            while (self.match(.comma)) {
                self.skipNewlines();
                if (self.check(.rbrace)) break;
                const key = try self.parseExpression();
                _ = try self.expect(.colon);
                const value = try self.parseExpression();
                try entries.append(self.allocator, .{
                    .key = key,
                    .value = value,
                    .location = key.location,
                });
            }
            self.skipNewlines();
            const rbrace = try self.expect(.rbrace);
            return self.create(ast.Expr, .{
                .kind = .{ .map = try entries.toOwnedSlice(self.allocator) },
                .location = Location.span(lbrace.location, rbrace.location),
            });
        }

        // set: {x} or {x, y, ...}
        var elements: std.ArrayList(*const ast.Expr) = .empty;
        try elements.append(self.allocator, first);
        while (self.match(.comma)) {
            self.skipNewlines();
            if (self.check(.rbrace)) break;
            try elements.append(self.allocator, try self.parseExpression());
        }
        self.skipNewlines();
        const rbrace = try self.expect(.rbrace);
        return self.create(ast.Expr, .{
            .kind = .{ .set = try elements.toOwnedSlice(self.allocator) },
            .location = Location.span(lbrace.location, rbrace.location),
        });
    }

    /// if expression: if cond: expr {elif cond: expr} else: expr
    fn parseIfExpr(self: *Parser) ParseError!*const ast.Expr {
        const if_tok = self.advance(); // skip if
        const condition = try self.parseExpression();
        _ = try self.expect(.colon);
        const then_expr = try self.parseExpression();

        var elifs: std.ArrayList(ast.ElifExprBranch) = .empty;
        while (self.check(.kw_elif)) {
            const elif_tok = self.advance();
            const elif_cond = try self.parseExpression();
            _ = try self.expect(.colon);
            const elif_expr = try self.parseExpression();
            try elifs.append(self.allocator, .{
                .condition = elif_cond,
                .expr = elif_expr,
                .location = elif_tok.location,
            });
        }

        _ = try self.expect(.kw_else);
        _ = try self.expect(.colon);
        const else_expr = try self.parseExpression();

        return self.create(ast.Expr, .{
            .kind = .{ .if_expr = .{
                .condition = condition,
                .then_expr = then_expr,
                .elif_branches = try elifs.toOwnedSlice(self.allocator),
                .else_expr = else_expr,
            } },
            .location = Location.span(if_tok.location, else_expr.location),
        });
    }

    /// match expression: match subject: NEWLINE INDENT {arm NEWLINE} DEDENT
    fn parseMatchExpr(self: *Parser) ParseError!*const ast.Expr {
        const match_tok = self.advance(); // skip match
        const subject = try self.parseExpression();
        _ = try self.expect(.colon);
        _ = try self.expect(.newline);
        _ = try self.expect(.indent);

        var arms: std.ArrayList(ast.MatchArm) = .empty;
        while (!self.check(.dedent) and !self.check(.eof)) {
            const arm = try self.parseMatchArm();
            try arms.append(self.allocator, arm);
            if (self.check(.newline)) _ = self.advance();
        }
        const end_tok = try self.expect(.dedent);

        return self.create(ast.Expr, .{
            .kind = .{ .match_expr = .{
                .subject = subject,
                .arms = try arms.toOwnedSlice(self.allocator),
            } },
            .location = Location.span(match_tok.location, end_tok.location),
        });
    }

    /// match arm: pattern ["if" expr] "=>" (expr | block)
    fn parseMatchArm(self: *Parser) ParseError!ast.MatchArm {
        const loc = self.peek().location;
        const pattern = try self.parsePattern();

        var guard: ?*const ast.Expr = null;
        if (self.check(.kw_if)) {
            _ = self.advance();
            guard = try self.parseExpression();
        }

        _ = try self.expect(.fat_arrow);

        // the body is either an inline expression or a block
        const body: ast.MatchBody = if (self.check(.newline))
            .{ .block = try self.parseBlock() }
        else
            .{ .expr = try self.parseExpression() };

        return .{
            .pattern = pattern,
            .guard = guard,
            .body = body,
            .location = loc,
        };
    }

    /// lambda: fn(params) => expr | fn(params): block
    fn parseLambda(self: *Parser) ParseError!*const ast.Expr {
        const fn_tok = self.advance(); // skip fn

        // if next token is an identifier, this is a fn declaration, not a lambda.
        // but in expression context we treat it as a lambda.
        // lambdas always have ( immediately after fn.
        _ = try self.expect(.lparen);
        const params = try self.parseLambdaParams();
        _ = try self.expect(.rparen);

        // short form: fn(x) => expr
        if (self.check(.fat_arrow)) {
            _ = self.advance();
            const body_expr = try self.parseExpression();
            return self.create(ast.Expr, .{
                .kind = .{ .lambda = .{
                    .params = params,
                    .body = .{ .expr = body_expr },
                } },
                .location = Location.span(fn_tok.location, body_expr.location),
            });
        }

        // block form: fn(x): block
        if (self.check(.colon)) {
            const body = try self.parseBlock();
            return self.create(ast.Expr, .{
                .kind = .{ .lambda = .{
                    .params = params,
                    .body = .{ .block = body },
                } },
                .location = Location.span(fn_tok.location, body.location),
            });
        }

        try self.diagnostics.addError(self.peek().location, "expected '=>' or ':' after lambda parameters");
        return self.create(ast.Expr, .{
            .kind = .err,
            .location = fn_tok.location,
        });
    }

    /// parse lambda parameter list (simplified — no defaults).
    fn parseLambdaParams(self: *Parser) ParseError![]const ast.Param {
        var params: std.ArrayList(ast.Param) = .empty;
        if (self.check(.rparen)) return params.toOwnedSlice(self.allocator);

        try params.append(self.allocator, try self.parseLambdaParam());
        while (self.match(.comma)) {
            try params.append(self.allocator, try self.parseLambdaParam());
        }
        return params.toOwnedSlice(self.allocator);
    }

    /// parse a single lambda param: [mut] [ref] name [: type]
    fn parseLambdaParam(self: *Parser) ParseError!ast.Param {
        const loc = self.peek().location;
        const is_mut = self.match(.kw_mut);
        const is_ref = self.match(.kw_ref);
        const name_tok = try self.expect(.identifier);

        var type_expr: ?*const ast.TypeExpr = null;
        if (self.match(.colon)) {
            type_expr = try self.parseTypeExpr();
        }

        return .{
            .name = name_tok.lexeme,
            .type_expr = type_expr,
            .default = null,
            .is_mut = is_mut,
            .is_ref = is_ref,
            .location = loc,
        };
    }

    /// string interpolation: string_start {interp_expr (string_mid | string_end)}
    fn parseStringInterpolation(self: *Parser) ParseError!*const ast.Expr {
        const start_tok = self.advance(); // consume string_start
        var parts: std.ArrayList(ast.StringPart) = .empty;

        // add the leading string text
        if (start_tok.lexeme.len > 0) {
            try parts.append(self.allocator, .{ .literal = start_tok.lexeme });
        }

        var end_loc = start_tok.location;

        while (true) {
            // expect an interpolation expression
            if (self.check(.interpolation_expr)) {
                const interp_tok = self.advance();
                // sub-lex and sub-parse the interpolation expression
                const expr = try self.parseInterpolationExpr(interp_tok);
                try parts.append(self.allocator, .{ .expr = expr });
                end_loc = interp_tok.location;
            }

            // string_mid means more interpolations follow
            if (self.check(.string_mid)) {
                const mid_tok = self.advance();
                if (mid_tok.lexeme.len > 0) {
                    try parts.append(self.allocator, .{ .literal = mid_tok.lexeme });
                }
                end_loc = mid_tok.location;
                continue;
            }

            // string_end means we're done
            if (self.check(.string_end)) {
                const end_tok = self.advance();
                if (end_tok.lexeme.len > 0) {
                    try parts.append(self.allocator, .{ .literal = end_tok.lexeme });
                }
                end_loc = end_tok.location;
                break;
            }

            // unexpected token — emit diagnostic and stop
            try self.diagnostics.addError(self.peek().location, "unexpected token in string interpolation");
            break;
        }

        return self.create(ast.Expr, .{
            .kind = .{ .string_interp = .{
                .parts = try parts.toOwnedSlice(self.allocator),
            } },
            .location = Location.span(start_tok.location, end_loc),
        });
    }

    /// sub-lex and sub-parse an interpolation expression token.
    /// the lexer gives us the raw text between { and }, we need to
    /// lex it into tokens and parse it as an expression.
    fn parseInterpolationExpr(self: *Parser, interp_tok: Token) ParseError!*const ast.Expr {
        var lex = Lexer.init(interp_tok.lexeme, self.allocator) catch {
            return self.create(ast.Expr, .{
                .kind = .err,
                .location = interp_tok.location,
            });
        };
        defer lex.deinit();

        const sub_tokens = lex.tokenize() catch {
            return self.create(ast.Expr, .{
                .kind = .err,
                .location = interp_tok.location,
            });
        };

        // create a sub-parser for the interpolation expression
        var sub_parser = Parser.init(sub_tokens, interp_tok.lexeme, self.allocator);
        // don't deinit sub_parser.diagnostics — we share the allocator

        const expr = sub_parser.parseExpression() catch {
            return self.create(ast.Expr, .{
                .kind = .err,
                .location = interp_tok.location,
            });
        };
        return expr;
    }

    // ---------------------------------------------------------------
    // patterns (forward declaration — used by match arms)
    // ---------------------------------------------------------------

    /// parse a pattern. full implementation in the next commit,
    /// but match expressions need a basic version.
    fn parsePattern(self: *Parser) ParseError!ast.Pattern {
        const tok = self.peek();

        switch (tok.kind) {
            .underscore => {
                _ = self.advance();
                return .{ .kind = .wildcard, .location = tok.location };
            },
            .int_lit => {
                _ = self.advance();
                return .{ .kind = .{ .int_lit = tok.lexeme }, .location = tok.location };
            },
            .float_lit => {
                _ = self.advance();
                return .{ .kind = .{ .float_lit = tok.lexeme }, .location = tok.location };
            },
            .string_lit => {
                _ = self.advance();
                return .{ .kind = .{ .string_lit = tok.lexeme }, .location = tok.location };
            },
            .kw_true => {
                _ = self.advance();
                return .{ .kind = .{ .bool_lit = true }, .location = tok.location };
            },
            .kw_false => {
                _ = self.advance();
                return .{ .kind = .{ .bool_lit = false }, .location = tok.location };
            },
            .kw_none => {
                _ = self.advance();
                return .{ .kind = .none_lit, .location = tok.location };
            },
            .identifier => {
                _ = self.advance();
                // check for qualified variant: Type.Variant or Type.Variant(fields)
                if (self.check(.dot)) {
                    _ = self.advance();
                    const variant_tok = try self.expect(.identifier);

                    if (self.check(.lparen)) {
                        _ = self.advance();
                        var fields: std.ArrayList(ast.Pattern) = .empty;
                        if (!self.check(.rparen)) {
                            try fields.append(self.allocator, try self.parsePattern());
                            while (self.match(.comma)) {
                                try fields.append(self.allocator, try self.parsePattern());
                            }
                        }
                        const end_tok = try self.expect(.rparen);
                        return .{
                            .kind = .{ .variant = .{
                                .type_name = tok.lexeme,
                                .variant = variant_tok.lexeme,
                                .fields = try fields.toOwnedSlice(self.allocator),
                            } },
                            .location = Location.span(tok.location, end_tok.location),
                        };
                    }

                    return .{
                        .kind = .{ .variant = .{
                            .type_name = tok.lexeme,
                            .variant = variant_tok.lexeme,
                            .fields = &.{},
                        } },
                        .location = Location.span(tok.location, variant_tok.location),
                    };
                }
                return .{ .kind = .{ .binding = tok.lexeme }, .location = tok.location };
            },
            .lparen => {
                _ = self.advance();
                var patterns: std.ArrayList(ast.Pattern) = .empty;
                if (!self.check(.rparen)) {
                    try patterns.append(self.allocator, try self.parsePattern());
                    while (self.match(.comma)) {
                        try patterns.append(self.allocator, try self.parsePattern());
                    }
                }
                const end_tok = try self.expect(.rparen);
                return .{
                    .kind = .{ .tuple = try patterns.toOwnedSlice(self.allocator) },
                    .location = Location.span(tok.location, end_tok.location),
                };
            },
            else => {
                try self.diagnostics.addError(tok.location, "expected pattern");
                self.synchronize();
                return .{ .kind = .wildcard, .location = tok.location };
            },
        }
    }

    // ---------------------------------------------------------------
    // blocks and statements
    // ---------------------------------------------------------------

    /// block = NEWLINE INDENT { statement NEWLINE } DEDENT
    fn parseBlock(self: *Parser) ParseError!ast.Block {
        const loc = self.peek().location;
        _ = try self.expect(.newline);
        _ = try self.expect(.indent);

        var stmts: std.ArrayList(ast.Stmt) = .empty;
        while (!self.check(.dedent) and !self.check(.eof)) {
            const stmt = try self.parseStatement();
            try stmts.append(self.allocator, stmt);
            // consume trailing newlines between statements
            while (self.check(.newline)) _ = self.advance();
        }
        const end_tok = try self.expect(.dedent);

        return .{
            .stmts = try stmts.toOwnedSlice(self.allocator),
            .location = Location.span(loc, end_tok.location),
        };
    }

    /// dispatch to the appropriate statement parser based on the leading token.
    fn parseStatement(self: *Parser) ParseError!ast.Stmt {
        const tok = self.peek();

        // mut binding: mut name [:type] := expr
        if (tok.kind == .kw_mut) {
            return self.parseBinding();
        }

        // binding: name [:type] := expr
        // need to distinguish from assignment and expr-stmt.
        // if we see ident followed by := or ident : type :=, it's a binding.
        if (tok.kind == .identifier) {
            if (self.peekAhead(1).kind == .colon_eq) {
                return self.parseBinding();
            }
            if (self.peekAhead(1).kind == .colon and self.peekAhead(2).kind == .identifier) {
                // could be binding with type annotation: name: Type := expr
                // we need to look further to find :=
                // but it could also be an expr statement like foo: (which would be weird)
                // let's check — scan ahead past potential type to find :=
                if (self.looksLikeBinding()) {
                    return self.parseBinding();
                }
            }
        }

        // control flow
        if (tok.kind == .kw_if) return self.parseIfStmt();
        if (tok.kind == .kw_for) return self.parseForStmt();
        if (tok.kind == .kw_while) return self.parseWhileStmt();
        if (tok.kind == .kw_match) return self.parseMatchStmt();
        if (tok.kind == .kw_return) return self.parseReturnStmt();
        if (tok.kind == .kw_fail) return self.parseFailStmt();

        if (tok.kind == .kw_break) {
            _ = self.advance();
            return .{ .kind = .break_stmt, .location = tok.location };
        }
        if (tok.kind == .kw_continue) {
            _ = self.advance();
            return .{ .kind = .continue_stmt, .location = tok.location };
        }

        // expression statement or assignment
        return self.parseExprStmtOrAssignment();
    }

    /// heuristic: does the current position look like a typed binding?
    /// checks for: ident ":" type ":="
    /// scans past tokens that can appear in type expressions until we
    /// find := (binding) or something that can't be part of a type.
    fn looksLikeBinding(self: *const Parser) bool {
        // start from offset 2 (past ident and colon)
        var i: u32 = 2;
        while (true) {
            const kind = self.peekAhead(i).kind;
            if (kind == .colon_eq) return true;
            if (kind == .eof or kind == .newline or kind == .dedent) return false;
            // tokens that can appear in type expressions
            if (kind == .identifier or kind == .lbracket or kind == .rbracket or
                kind == .lparen or kind == .rparen or kind == .comma or
                kind == .question or kind == .bang or kind == .arrow or
                kind == .kw_fn or kind == .plus)
            {
                i += 1;
                continue;
            }
            return false;
        }
    }

    /// parse a binding: [mut] name [: type] := expr
    fn parseBinding(self: *Parser) ParseError!ast.Stmt {
        const loc = self.peek().location;
        const is_mut = self.match(.kw_mut);
        const name_tok = try self.expect(.identifier);

        var type_expr: ?*const ast.TypeExpr = null;
        if (self.match(.colon)) {
            type_expr = try self.parseTypeExpr();
        }

        _ = try self.expect(.colon_eq);
        const value = try self.parseExpression();

        return .{
            .kind = .{ .binding = .{
                .name = name_tok.lexeme,
                .type_expr = type_expr,
                .value = value,
                .is_mut = is_mut,
            } },
            .location = Location.span(loc, value.location),
        };
    }

    /// parse an expression statement or assignment.
    /// first parse as expression, then check for assignment operator.
    fn parseExprStmtOrAssignment(self: *Parser) ParseError!ast.Stmt {
        const loc = self.peek().location;
        const expr = try self.parseExpression();

        // check for assignment operators
        const op: ?ast.AssignOp = switch (self.peek().kind) {
            .eq => .assign,
            .plus_eq => .add,
            .minus_eq => .sub,
            .star_eq => .mul,
            .slash_eq => .div,
            else => null,
        };

        if (op) |assign_op| {
            _ = self.advance();
            const value = try self.parseExpression();
            return .{
                .kind = .{ .assignment = .{
                    .target = expr,
                    .op = assign_op,
                    .value = value,
                } },
                .location = Location.span(loc, value.location),
            };
        }

        return .{
            .kind = .{ .expr_stmt = expr },
            .location = expr.location,
        };
    }

    /// if statement: if expr: block {elif expr: block} [else: block]
    fn parseIfStmt(self: *Parser) ParseError!ast.Stmt {
        const if_tok = self.advance(); // skip if
        const condition = try self.parseExpression();
        _ = try self.expect(.colon);
        const then_block = try self.parseBlock();

        var elifs: std.ArrayList(ast.ElifBranch) = .empty;
        while (self.check(.kw_elif)) {
            const elif_tok = self.advance();
            const elif_cond = try self.parseExpression();
            _ = try self.expect(.colon);
            const elif_block = try self.parseBlock();
            try elifs.append(self.allocator, .{
                .condition = elif_cond,
                .block = elif_block,
                .location = elif_tok.location,
            });
        }

        var else_block: ?ast.Block = null;
        if (self.check(.kw_else)) {
            _ = self.advance();
            _ = try self.expect(.colon);
            else_block = try self.parseBlock();
        }

        const end_loc = if (else_block) |eb|
            eb.location
        else if (elifs.items.len > 0)
            elifs.items[elifs.items.len - 1].block.location
        else
            then_block.location;

        return .{
            .kind = .{ .if_stmt = .{
                .condition = condition,
                .then_block = then_block,
                .elif_branches = try elifs.toOwnedSlice(self.allocator),
                .else_block = else_block,
            } },
            .location = Location.span(if_tok.location, end_loc),
        };
    }

    /// for statement: for name [, index] in expr: block
    fn parseForStmt(self: *Parser) ParseError!ast.Stmt {
        const for_tok = self.advance(); // skip for
        const binding_tok = try self.expect(.identifier);

        var index_name: ?[]const u8 = null;
        if (self.match(.comma)) {
            const index_tok = try self.expect(.identifier);
            index_name = index_tok.lexeme;
        }

        _ = try self.expect(.kw_in);
        const iterable = try self.parseExpression();
        _ = try self.expect(.colon);
        const body = try self.parseBlock();

        return .{
            .kind = .{ .for_stmt = .{
                .binding = binding_tok.lexeme,
                .index = index_name,
                .iterable = iterable,
                .body = body,
            } },
            .location = Location.span(for_tok.location, body.location),
        };
    }

    /// while statement: while expr: block
    fn parseWhileStmt(self: *Parser) ParseError!ast.Stmt {
        const while_tok = self.advance(); // skip while
        const condition = try self.parseExpression();
        _ = try self.expect(.colon);
        const body = try self.parseBlock();

        return .{
            .kind = .{ .while_stmt = .{
                .condition = condition,
                .body = body,
            } },
            .location = Location.span(while_tok.location, body.location),
        };
    }

    /// match statement (same as match expr, used in statement context)
    fn parseMatchStmt(self: *Parser) ParseError!ast.Stmt {
        const loc = self.peek().location;
        const result = try self.parseMatchExpr();
        return .{
            .kind = switch (result.kind) {
                .match_expr => |m| .{ .match_stmt = m },
                else => .{ .expr_stmt = result },
            },
            .location = loc,
        };
    }

    /// return statement: return [expr]
    fn parseReturnStmt(self: *Parser) ParseError!ast.Stmt {
        const tok = self.advance(); // skip return

        // return has a value if the next token isn't a newline/dedent/eof
        var value: ?*const ast.Expr = null;
        const next = self.peek().kind;
        if (next != .newline and next != .dedent and next != .eof) {
            value = try self.parseExpression();
        }

        const end_loc = if (value) |v| v.location else tok.location;
        return .{
            .kind = .{ .return_stmt = .{ .value = value } },
            .location = Location.span(tok.location, end_loc),
        };
    }

    /// fail statement: fail expr
    fn parseFailStmt(self: *Parser) ParseError!ast.Stmt {
        const tok = self.advance(); // skip fail
        const value = try self.parseExpression();
        return .{
            .kind = .{ .fail_stmt = .{ .value = value } },
            .location = Location.span(tok.location, value.location),
        };
    }

    // ---------------------------------------------------------------
    // declarations
    // ---------------------------------------------------------------

    /// module = { import_decl NEWLINE } { top_level_decl } EOF
    pub fn parseModule(self: *Parser) ParseError!ast.Module {
        var imports: std.ArrayList(ast.ImportDecl) = .empty;
        var decls: std.ArrayList(ast.Decl) = .empty;

        self.skipNewlines();

        // parse imports (must come first)
        while (self.check(.kw_import) or self.check(.kw_from)) {
            const imp = try self.parseImportDecl();
            try imports.append(self.allocator, imp);
            self.skipNewlines();
        }

        // parse top-level declarations
        while (!self.check(.eof)) {
            self.skipNewlines();
            if (self.check(.eof)) break;
            const decl = try self.parseTopLevelDecl();
            try decls.append(self.allocator, decl);
            self.skipNewlines();
        }

        return .{
            .imports = try imports.toOwnedSlice(self.allocator),
            .decls = try decls.toOwnedSlice(self.allocator),
        };
    }

    /// import_decl = "import" path ["as" name]
    ///             | "from" path "import" import_list
    fn parseImportDecl(self: *Parser) ParseError!ast.ImportDecl {
        const loc = self.peek().location;

        if (self.check(.kw_from)) {
            // from path import names
            _ = self.advance();
            const path = try self.parseDottedPath();
            _ = try self.expect(.kw_import);

            var names: std.ArrayList(ast.ImportName) = .empty;
            const name = try self.parseImportName();
            try names.append(self.allocator, name);
            while (self.match(.comma)) {
                try names.append(self.allocator, try self.parseImportName());
            }

            return .{
                .kind = .{ .from = .{
                    .path = path,
                    .names = try names.toOwnedSlice(self.allocator),
                } },
                .location = loc,
            };
        }

        // import path [as alias]
        _ = self.advance(); // skip import
        const path = try self.parseDottedPath();

        var alias: ?[]const u8 = null;
        if (self.match(.kw_as)) {
            alias = (try self.expect(.identifier)).lexeme;
        }

        return .{
            .kind = .{ .simple = .{
                .path = path,
                .alias = alias,
            } },
            .location = loc,
        };
    }

    /// parse a dotted path: ident { "." ident }
    fn parseDottedPath(self: *Parser) ParseError![]const []const u8 {
        var parts: std.ArrayList([]const u8) = .empty;
        const first = try self.expect(.identifier);
        try parts.append(self.allocator, first.lexeme);
        while (self.match(.dot)) {
            const next = try self.expect(.identifier);
            try parts.append(self.allocator, next.lexeme);
        }
        return parts.toOwnedSlice(self.allocator);
    }

    /// parse a single import name: ident ["as" ident]
    fn parseImportName(self: *Parser) ParseError!ast.ImportName {
        const name_tok = try self.expect(.identifier);
        var alias: ?[]const u8 = null;
        if (self.match(.kw_as)) {
            alias = (try self.expect(.identifier)).lexeme;
        }
        return .{
            .name = name_tok.lexeme,
            .alias = alias,
            .location = name_tok.location,
        };
    }

    /// top_level_decl = ["pub"] (fn_decl | struct_decl | enum_decl | interface_decl | impl_decl | type_alias | binding)
    fn parseTopLevelDecl(self: *Parser) ParseError!ast.Decl {
        const loc = self.peek().location;
        const is_pub = self.match(.kw_pub);

        const kind: ast.DeclKind = switch (self.peek().kind) {
            .kw_fn => .{ .fn_decl = try self.parseFnDecl() },
            .kw_struct => .{ .struct_decl = try self.parseStructDecl() },
            .kw_enum => .{ .enum_decl = try self.parseEnumDecl() },
            .kw_interface => .{ .interface_decl = try self.parseInterfaceDecl() },
            .kw_impl => .{ .impl_decl = try self.parseImplDecl() },
            .kw_type => .{ .type_alias = try self.parseTypeAliasDecl() },
            // binding at top level
            .kw_mut, .identifier => blk: {
                const stmt = try self.parseBinding();
                break :blk .{ .binding = stmt.kind.binding };
            },
            else => {
                try self.diagnostics.addError(loc, "expected declaration");
                self.synchronize();
                return .{
                    .kind = .{ .binding = .{
                        .name = "",
                        .type_expr = null,
                        .value = try self.create(ast.Expr, .{ .kind = .err, .location = loc }),
                        .is_mut = false,
                    } },
                    .is_pub = is_pub,
                    .location = loc,
                };
            },
        };

        return .{ .kind = kind, .is_pub = is_pub, .location = loc };
    }

    /// fn_decl = "fn" name [generic_params] "(" [param_list] ")" ["->" type] ":" block
    fn parseFnDecl(self: *Parser) ParseError!ast.FnDecl {
        _ = self.advance(); // skip fn
        const name_tok = try self.expect(.identifier);

        // optional generic params
        const generics = if (self.check(.lbracket))
            try self.parseGenericParams()
        else
            &.{};

        _ = try self.expect(.lparen);
        const params = try self.parseParamList();
        _ = try self.expect(.rparen);

        var return_type: ?*const ast.TypeExpr = null;
        if (self.match(.arrow)) {
            return_type = try self.parseTypeExpr();
        }

        _ = try self.expect(.colon);
        const body = try self.parseBlock();

        return .{
            .name = name_tok.lexeme,
            .generic_params = generics,
            .params = params,
            .return_type = return_type,
            .body = body,
        };
    }

    /// param_list = param { "," param }
    fn parseParamList(self: *Parser) ParseError![]const ast.Param {
        var params: std.ArrayList(ast.Param) = .empty;
        self.skipNewlines();
        if (self.check(.rparen)) return params.toOwnedSlice(self.allocator);

        try params.append(self.allocator, try self.parseParam());
        while (self.match(.comma)) {
            self.skipNewlines();
            try params.append(self.allocator, try self.parseParam());
        }
        self.skipNewlines();
        return params.toOwnedSlice(self.allocator);
    }

    /// param = [mut] [ref] name ":" type ["=" expr]
    fn parseParam(self: *Parser) ParseError!ast.Param {
        const loc = self.peek().location;
        const is_mut = self.match(.kw_mut);
        const is_ref = self.match(.kw_ref);
        const name_tok = try self.expect(.identifier);

        _ = try self.expect(.colon);
        const type_expr = try self.parseTypeExpr();

        var default: ?*const ast.Expr = null;
        if (self.match(.eq)) {
            default = try self.parseExpression();
        }

        return .{
            .name = name_tok.lexeme,
            .type_expr = type_expr,
            .default = default,
            .is_mut = is_mut,
            .is_ref = is_ref,
            .location = loc,
        };
    }

    /// generic_params = "[" generic_param { "," generic_param } "]"
    fn parseGenericParams(self: *Parser) ParseError![]const ast.GenericParam {
        _ = self.advance(); // skip [
        var params: std.ArrayList(ast.GenericParam) = .empty;

        try params.append(self.allocator, try self.parseGenericParam());
        while (self.match(.comma)) {
            try params.append(self.allocator, try self.parseGenericParam());
        }
        _ = try self.expect(.rbracket);
        return params.toOwnedSlice(self.allocator);
    }

    /// generic_param = name [":" type_bound]
    /// type_bound = type { "+" type }
    fn parseGenericParam(self: *Parser) ParseError!ast.GenericParam {
        const name_tok = try self.expect(.identifier);
        var bounds: std.ArrayList(*const ast.TypeExpr) = .empty;

        if (self.match(.colon)) {
            try bounds.append(self.allocator, try self.parseTypeExpr());
            while (self.match(.plus)) {
                try bounds.append(self.allocator, try self.parseTypeExpr());
            }
        }

        return .{
            .name = name_tok.lexeme,
            .bounds = try bounds.toOwnedSlice(self.allocator),
            .location = name_tok.location,
        };
    }

    /// struct_decl = "struct" name [generic_params] ":" NEWLINE INDENT { field NEWLINE } DEDENT
    fn parseStructDecl(self: *Parser) ParseError!ast.StructDecl {
        _ = self.advance(); // skip struct
        const name_tok = try self.expect(.identifier);

        const generics = if (self.check(.lbracket))
            try self.parseGenericParams()
        else
            &.{};

        _ = try self.expect(.colon);
        _ = try self.expect(.newline);
        _ = try self.expect(.indent);

        var fields: std.ArrayList(ast.StructField) = .empty;
        while (!self.check(.dedent) and !self.check(.eof)) {
            try fields.append(self.allocator, try self.parseStructField());
            while (self.check(.newline)) _ = self.advance();
        }
        _ = try self.expect(.dedent);

        return .{
            .name = name_tok.lexeme,
            .generic_params = generics,
            .fields = try fields.toOwnedSlice(self.allocator),
        };
    }

    /// struct_field = [pub] [mut] [weak] name ":" type ["=" expr]
    fn parseStructField(self: *Parser) ParseError!ast.StructField {
        const loc = self.peek().location;
        const is_pub = self.match(.kw_pub);
        const is_mut = self.match(.kw_mut);
        const is_weak = self.match(.kw_weak);
        const name_tok = try self.expect(.identifier);
        _ = try self.expect(.colon);
        const type_expr = try self.parseTypeExpr();

        var default: ?*const ast.Expr = null;
        if (self.match(.eq)) {
            default = try self.parseExpression();
        }

        return .{
            .name = name_tok.lexeme,
            .type_expr = type_expr,
            .default = default,
            .is_pub = is_pub,
            .is_mut = is_mut,
            .is_weak = is_weak,
            .location = loc,
        };
    }

    /// enum_decl = "enum" name [generic_params] ":" NEWLINE INDENT { variant NEWLINE } DEDENT
    fn parseEnumDecl(self: *Parser) ParseError!ast.EnumDecl {
        _ = self.advance(); // skip enum
        const name_tok = try self.expect(.identifier);

        const generics = if (self.check(.lbracket))
            try self.parseGenericParams()
        else
            &.{};

        _ = try self.expect(.colon);
        _ = try self.expect(.newline);
        _ = try self.expect(.indent);

        var variants: std.ArrayList(ast.EnumVariant) = .empty;
        while (!self.check(.dedent) and !self.check(.eof)) {
            try variants.append(self.allocator, try self.parseEnumVariant());
            while (self.check(.newline)) _ = self.advance();
        }
        _ = try self.expect(.dedent);

        return .{
            .name = name_tok.lexeme,
            .generic_params = generics,
            .variants = try variants.toOwnedSlice(self.allocator),
        };
    }

    /// enum_variant = name ["(" type_list ")"]
    fn parseEnumVariant(self: *Parser) ParseError!ast.EnumVariant {
        const name_tok = try self.expect(.identifier);

        var fields: std.ArrayList(*const ast.TypeExpr) = .empty;
        if (self.match(.lparen)) {
            try fields.append(self.allocator, try self.parseTypeExpr());
            while (self.match(.comma)) {
                try fields.append(self.allocator, try self.parseTypeExpr());
            }
            _ = try self.expect(.rparen);
        }

        return .{
            .name = name_tok.lexeme,
            .fields = try fields.toOwnedSlice(self.allocator),
            .location = name_tok.location,
        };
    }

    /// interface_decl = "interface" name [generic_params] ":" NEWLINE INDENT { fn_sig NEWLINE } DEDENT
    fn parseInterfaceDecl(self: *Parser) ParseError!ast.InterfaceDecl {
        _ = self.advance(); // skip interface
        const name_tok = try self.expect(.identifier);

        const generics = if (self.check(.lbracket))
            try self.parseGenericParams()
        else
            &.{};

        _ = try self.expect(.colon);
        _ = try self.expect(.newline);
        _ = try self.expect(.indent);

        var methods: std.ArrayList(ast.FnSig) = .empty;
        while (!self.check(.dedent) and !self.check(.eof)) {
            try methods.append(self.allocator, try self.parseFnSig());
            while (self.check(.newline)) _ = self.advance();
        }
        _ = try self.expect(.dedent);

        return .{
            .name = name_tok.lexeme,
            .generic_params = generics,
            .methods = try methods.toOwnedSlice(self.allocator),
        };
    }

    /// fn_sig = "fn" name [generic_params] "(" [param_list] ")" ["->" type]
    fn parseFnSig(self: *Parser) ParseError!ast.FnSig {
        const loc = self.peek().location;
        _ = try self.expect(.kw_fn);
        const name_tok = try self.expect(.identifier);

        const generics = if (self.check(.lbracket))
            try self.parseGenericParams()
        else
            &.{};

        _ = try self.expect(.lparen);
        const params = try self.parseParamList();
        _ = try self.expect(.rparen);

        var return_type: ?*const ast.TypeExpr = null;
        if (self.match(.arrow)) {
            return_type = try self.parseTypeExpr();
        }

        return .{
            .name = name_tok.lexeme,
            .generic_params = generics,
            .params = params,
            .return_type = return_type,
            .location = loc,
        };
    }

    /// impl_decl = "impl" type ["for" type] ":" NEWLINE INDENT { [pub] fn_decl NEWLINE } DEDENT
    fn parseImplDecl(self: *Parser) ParseError!ast.ImplDecl {
        _ = self.advance(); // skip impl
        const target = try self.parseTypeExpr();

        var interface: ?*const ast.TypeExpr = null;
        if (self.match(.kw_for)) {
            interface = try self.parseTypeExpr();
        }

        _ = try self.expect(.colon);
        _ = try self.expect(.newline);
        _ = try self.expect(.indent);

        var methods: std.ArrayList(ast.ImplMethod) = .empty;
        while (!self.check(.dedent) and !self.check(.eof)) {
            const method_loc = self.peek().location;
            const method_pub = self.match(.kw_pub);
            const fn_decl = try self.parseFnDecl();
            try methods.append(self.allocator, .{
                .is_pub = method_pub,
                .decl = fn_decl,
                .location = method_loc,
            });
            while (self.check(.newline)) _ = self.advance();
        }
        _ = try self.expect(.dedent);

        return .{
            .target = target,
            .interface = interface,
            .methods = try methods.toOwnedSlice(self.allocator),
        };
    }

    /// type_alias = "type" name [generic_params] "=" type_expr
    fn parseTypeAliasDecl(self: *Parser) ParseError!ast.TypeAlias {
        _ = self.advance(); // skip type
        const name_tok = try self.expect(.identifier);

        const generics = if (self.check(.lbracket))
            try self.parseGenericParams()
        else
            &.{};

        _ = try self.expect(.eq);
        const type_expr = try self.parseTypeExpr();

        return .{
            .name = name_tok.lexeme,
            .generic_params = generics,
            .type_expr = type_expr,
        };
    }
};

// ---------------------------------------------------------------
// tests
// ---------------------------------------------------------------

const testing = std.testing;

/// helper: lex source, create parser with arena allocator.
/// the arena owns all AST nodes — freed in one shot on deinit.
/// the arena is heap-allocated so the allocator pointer stays stable
/// when this struct is returned by value.
const TestParser = struct {
    parser: Parser,
    tokens: []Token,
    arena: *std.heap.ArenaAllocator,

    fn deinit(self: *TestParser) void {
        self.parser.deinit();
        testing.allocator.free(self.tokens);
        self.arena.deinit();
        testing.allocator.destroy(self.arena);
    }
};

fn testParser(source: []const u8) !TestParser {
    var lex = try Lexer.init(source, testing.allocator);
    defer lex.deinit();

    const tokens = try lex.tokenize();
    const arena = try testing.allocator.create(std.heap.ArenaAllocator);
    arena.* = std.heap.ArenaAllocator.init(testing.allocator);

    return .{
        .parser = Parser.init(tokens, source, arena.allocator()),
        .tokens = tokens,
        .arena = arena,
    };
}

test "parse simple named type" {
    var result = try testParser("Int");
    defer result.deinit();

    const ty = try result.parser.parseTypeExpr();
    try testing.expect(ty.kind == .named);
    try testing.expectEqualStrings("Int", ty.kind.named);
}

test "parse generic type" {
    var result = try testParser("List[Int]");
    defer result.deinit();

    const ty = try result.parser.parseTypeExpr();
    try testing.expect(ty.kind == .generic);
    try testing.expectEqualStrings("List", ty.kind.generic.name);
    try testing.expectEqual(@as(usize, 1), ty.kind.generic.args.len);
}

test "parse multi-arg generic type" {
    var result = try testParser("Map[String, Int]");
    defer result.deinit();

    const ty = try result.parser.parseTypeExpr();
    try testing.expect(ty.kind == .generic);
    try testing.expectEqualStrings("Map", ty.kind.generic.name);
    try testing.expectEqual(@as(usize, 2), ty.kind.generic.args.len);
}

test "parse optional type" {
    var result = try testParser("Int?");
    defer result.deinit();

    const ty = try result.parser.parseTypeExpr();
    try testing.expect(ty.kind == .optional);
    try testing.expect(ty.kind.optional.kind == .named);
    try testing.expectEqualStrings("Int", ty.kind.optional.kind.named);
}

test "parse result type" {
    var result = try testParser("Int!");
    defer result.deinit();

    const ty = try result.parser.parseTypeExpr();
    try testing.expect(ty.kind == .result);
    try testing.expect(ty.kind.result.err_type == null);
}

test "parse result type with error type" {
    var result = try testParser("Int!ParseError");
    defer result.deinit();

    const ty = try result.parser.parseTypeExpr();
    try testing.expect(ty.kind == .result);
    try testing.expect(ty.kind.result.err_type != null);
    try testing.expectEqualStrings("ParseError", ty.kind.result.err_type.?.kind.named);
}

test "parse fn type" {
    var result = try testParser("fn(Int, String) -> Bool");
    defer result.deinit();

    const ty = try result.parser.parseTypeExpr();
    try testing.expect(ty.kind == .fn_type);
    try testing.expectEqual(@as(usize, 2), ty.kind.fn_type.params.len);
    try testing.expect(ty.kind.fn_type.return_type != null);
}

test "parse fn type no return" {
    var result = try testParser("fn(Int)");
    defer result.deinit();

    const ty = try result.parser.parseTypeExpr();
    try testing.expect(ty.kind == .fn_type);
    try testing.expect(ty.kind.fn_type.return_type == null);
}

test "parse tuple type" {
    var result = try testParser("(Int, String, Bool)");
    defer result.deinit();

    const ty = try result.parser.parseTypeExpr();
    try testing.expect(ty.kind == .tuple);
    try testing.expectEqual(@as(usize, 3), ty.kind.tuple.len);
}

test "parse nested generic type" {
    var result = try testParser("List[Option[Int]]");
    defer result.deinit();

    const ty = try result.parser.parseTypeExpr();
    try testing.expect(ty.kind == .generic);
    try testing.expectEqualStrings("List", ty.kind.generic.name);

    const inner = ty.kind.generic.args[0];
    try testing.expect(inner.kind == .generic);
    try testing.expectEqualStrings("Option", inner.kind.generic.name);
}

// -- expression tests --

test "parse integer literal" {
    var result = try testParser("42");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .int_lit);
    try testing.expectEqualStrings("42", expr.kind.int_lit);
}

test "parse string literal" {
    var result = try testParser("\"hello\"");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .string_lit);
}

test "parse boolean literals" {
    var t = try testParser("true");
    defer t.deinit();
    const e1 = try t.parser.parseExpression();
    try testing.expect(e1.kind == .bool_lit);
    try testing.expectEqual(true, e1.kind.bool_lit);

    var f = try testParser("false");
    defer f.deinit();
    const e2 = try f.parser.parseExpression();
    try testing.expectEqual(false, e2.kind.bool_lit);
}

test "parse none literal" {
    var result = try testParser("none");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .none_lit);
}

test "parse identifier" {
    var result = try testParser("foo");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .ident);
    try testing.expectEqualStrings("foo", expr.kind.ident);
}

test "parse binary arithmetic" {
    var result = try testParser("1 + 2 * 3");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    // should be (1 + (2 * 3)) due to precedence
    try testing.expect(expr.kind == .binary);
    try testing.expect(expr.kind.binary.op == .add);
    try testing.expect(expr.kind.binary.left.kind == .int_lit);
    try testing.expect(expr.kind.binary.right.kind == .binary);
    try testing.expect(expr.kind.binary.right.kind.binary.op == .mul);
}

test "parse comparison" {
    var result = try testParser("x >= 10");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .binary);
    try testing.expect(expr.kind.binary.op == .gte);
}

test "parse logical operators" {
    var result = try testParser("a and b or c");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    // should be ((a and b) or c) — or is lower precedence
    try testing.expect(expr.kind == .binary);
    try testing.expect(expr.kind.binary.op == .@"or");
    try testing.expect(expr.kind.binary.left.kind == .binary);
    try testing.expect(expr.kind.binary.left.kind.binary.op == .@"and");
}

test "parse not" {
    var result = try testParser("not x");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .unary);
    try testing.expect(expr.kind.unary.op == .not);
}

test "parse unary negate" {
    var result = try testParser("-42");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .unary);
    try testing.expect(expr.kind.unary.op == .negate);
    try testing.expect(expr.kind.unary.operand.kind == .int_lit);
}

test "parse function call" {
    var result = try testParser("foo(1, 2)");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .call);
    try testing.expectEqual(@as(usize, 2), expr.kind.call.args.len);
}

test "parse named arguments" {
    var result = try testParser("foo(x = 1, y = 2)");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .call);
    try testing.expectEqualStrings("x", expr.kind.call.args[0].name.?);
    try testing.expectEqualStrings("y", expr.kind.call.args[1].name.?);
}

test "parse method call" {
    var result = try testParser("x.foo(1)");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .method_call);
    try testing.expectEqualStrings("foo", expr.kind.method_call.method);
    try testing.expectEqual(@as(usize, 1), expr.kind.method_call.args.len);
}

test "parse field access" {
    var result = try testParser("x.y");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .field_access);
    try testing.expectEqualStrings("y", expr.kind.field_access.field);
}

test "parse index" {
    var result = try testParser("x[0]");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .index);
    try testing.expect(expr.kind.index.index.kind == .int_lit);
}

test "parse unwrap and try" {
    var result = try testParser("x?");
    defer result.deinit();
    const e1 = try result.parser.parseExpression();
    try testing.expect(e1.kind == .unwrap);

    var result2 = try testParser("x!");
    defer result2.deinit();
    const e2 = try result2.parser.parseExpression();
    try testing.expect(e2.kind == .try_expr);
}

test "parse chained postfix" {
    var result = try testParser("a.b.c(1).d");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    // should be ((((a).b).c(1)).d)
    try testing.expect(expr.kind == .field_access);
    try testing.expectEqualStrings("d", expr.kind.field_access.field);
    try testing.expect(expr.kind.field_access.object.kind == .method_call);
}

test "parse grouped expression" {
    var result = try testParser("(1 + 2)");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .grouped);
    try testing.expect(expr.kind.grouped.kind == .binary);
}

test "parse tuple" {
    var result = try testParser("(1, 2, 3)");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .tuple);
    try testing.expectEqual(@as(usize, 3), expr.kind.tuple.len);
}

test "parse list literal" {
    var result = try testParser("[1, 2, 3]");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .list);
    try testing.expectEqual(@as(usize, 3), expr.kind.list.len);
}

test "parse empty list" {
    var result = try testParser("[]");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .list);
    try testing.expectEqual(@as(usize, 0), expr.kind.list.len);
}

test "parse empty map" {
    var result = try testParser("{}");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .map);
    try testing.expectEqual(@as(usize, 0), expr.kind.map.len);
}

test "parse map literal" {
    var result = try testParser("{\"a\": 1, \"b\": 2}");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .map);
    try testing.expectEqual(@as(usize, 2), expr.kind.map.len);
}

test "parse set literal" {
    var result = try testParser("{1, 2, 3}");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .set);
    try testing.expectEqual(@as(usize, 3), expr.kind.set.len);
}

test "parse if expression" {
    var result = try testParser("if x: 1 else: 2");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .if_expr);
    try testing.expect(expr.kind.if_expr.then_expr.kind == .int_lit);
    try testing.expect(expr.kind.if_expr.else_expr.kind == .int_lit);
}

test "parse lambda short form" {
    var result = try testParser("fn(x) => x * 2");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .lambda);
    try testing.expectEqual(@as(usize, 1), expr.kind.lambda.params.len);
    try testing.expect(expr.kind.lambda.body == .expr);
}

test "parse pipe operator" {
    var result = try testParser("x | y | z");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .binary);
    try testing.expect(expr.kind.binary.op == .pipe);
    // left-associative: ((x | y) | z)
    try testing.expect(expr.kind.binary.left.kind == .binary);
}

test "parse string interpolation" {
    var result = try testParser("\"hello {name}!\"");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .string_interp);
    // parts: "hello " + name + "!"
    try testing.expectEqual(@as(usize, 3), expr.kind.string_interp.parts.len);
    try testing.expect(expr.kind.string_interp.parts[0] == .literal);
    try testing.expect(expr.kind.string_interp.parts[1] == .expr);
    try testing.expect(expr.kind.string_interp.parts[2] == .literal);
}

test "parse self" {
    var result = try testParser("self");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .self_expr);
}

// -- statement tests --

test "parse binding" {
    var result = try testParser("x := 42");
    defer result.deinit();

    const stmt = try result.parser.parseStatement();
    try testing.expect(stmt.kind == .binding);
    try testing.expectEqualStrings("x", stmt.kind.binding.name);
    try testing.expect(!stmt.kind.binding.is_mut);
    try testing.expect(stmt.kind.binding.type_expr == null);
}

test "parse mutable binding" {
    var result = try testParser("mut count := 0");
    defer result.deinit();

    const stmt = try result.parser.parseStatement();
    try testing.expect(stmt.kind == .binding);
    try testing.expect(stmt.kind.binding.is_mut);
}

test "parse typed binding" {
    var result = try testParser("x: Int := 42");
    defer result.deinit();

    const stmt = try result.parser.parseStatement();
    try testing.expect(stmt.kind == .binding);
    try testing.expect(stmt.kind.binding.type_expr != null);
    try testing.expectEqualStrings("Int", stmt.kind.binding.type_expr.?.kind.named);
}

test "parse assignment" {
    var result = try testParser("x = 10");
    defer result.deinit();

    const stmt = try result.parser.parseStatement();
    try testing.expect(stmt.kind == .assignment);
    try testing.expect(stmt.kind.assignment.op == .assign);
}

test "parse compound assignment" {
    var result = try testParser("x += 1");
    defer result.deinit();

    const stmt = try result.parser.parseStatement();
    try testing.expect(stmt.kind == .assignment);
    try testing.expect(stmt.kind.assignment.op == .add);
}

test "parse expression statement" {
    var result = try testParser("foo(42)");
    defer result.deinit();

    const stmt = try result.parser.parseStatement();
    try testing.expect(stmt.kind == .expr_stmt);
    try testing.expect(stmt.kind.expr_stmt.kind == .call);
}

test "parse return with value" {
    var result = try testParser("return 42");
    defer result.deinit();

    const stmt = try result.parser.parseStatement();
    try testing.expect(stmt.kind == .return_stmt);
    try testing.expect(stmt.kind.return_stmt.value != null);
}

test "parse return without value" {
    var result = try testParser("return\n");
    defer result.deinit();

    const stmt = try result.parser.parseStatement();
    try testing.expect(stmt.kind == .return_stmt);
    try testing.expect(stmt.kind.return_stmt.value == null);
}

test "parse fail statement" {
    var result = try testParser("fail error");
    defer result.deinit();

    const stmt = try result.parser.parseStatement();
    try testing.expect(stmt.kind == .fail_stmt);
}

test "parse break and continue" {
    var b = try testParser("break");
    defer b.deinit();
    const s1 = try b.parser.parseStatement();
    try testing.expect(s1.kind == .break_stmt);

    var c = try testParser("continue");
    defer c.deinit();
    const s2 = try c.parser.parseStatement();
    try testing.expect(s2.kind == .continue_stmt);
}

test "parse if statement with block" {
    const source = "if x:\n    y := 1\n";
    var result = try testParser(source);
    defer result.deinit();

    const stmt = try result.parser.parseStatement();
    try testing.expect(stmt.kind == .if_stmt);
    try testing.expectEqual(@as(usize, 1), stmt.kind.if_stmt.then_block.stmts.len);
    try testing.expect(stmt.kind.if_stmt.else_block == null);
}

test "parse if-else statement" {
    const source = "if x:\n    a\nelse:\n    b\n";
    var result = try testParser(source);
    defer result.deinit();

    const stmt = try result.parser.parseStatement();
    try testing.expect(stmt.kind == .if_stmt);
    try testing.expect(stmt.kind.if_stmt.else_block != null);
}

test "parse for statement" {
    const source = "for item in items:\n    print(item)\n";
    var result = try testParser(source);
    defer result.deinit();

    const stmt = try result.parser.parseStatement();
    try testing.expect(stmt.kind == .for_stmt);
    try testing.expectEqualStrings("item", stmt.kind.for_stmt.binding);
    try testing.expect(stmt.kind.for_stmt.index == null);
}

test "parse for with index" {
    const source = "for item, i in items:\n    print(i)\n";
    var result = try testParser(source);
    defer result.deinit();

    const stmt = try result.parser.parseStatement();
    try testing.expect(stmt.kind == .for_stmt);
    try testing.expectEqualStrings("i", stmt.kind.for_stmt.index.?);
}

test "parse while statement" {
    const source = "while x > 0:\n    x -= 1\n";
    var result = try testParser(source);
    defer result.deinit();

    const stmt = try result.parser.parseStatement();
    try testing.expect(stmt.kind == .while_stmt);
}

test "parse block with multiple statements" {
    const source = "if true:\n    a := 1\n    b := 2\n    c := 3\n";
    var result = try testParser(source);
    defer result.deinit();

    const stmt = try result.parser.parseStatement();
    try testing.expect(stmt.kind == .if_stmt);
    try testing.expectEqual(@as(usize, 3), stmt.kind.if_stmt.then_block.stmts.len);
}

// -- declaration tests --

test "parse simple import" {
    var result = try testParser("import std.io\n");
    defer result.deinit();

    const module = try result.parser.parseModule();
    try testing.expectEqual(@as(usize, 1), module.imports.len);
    const imp = module.imports[0];
    try testing.expect(imp.kind == .simple);
    try testing.expectEqual(@as(usize, 2), imp.kind.simple.path.len);
    try testing.expectEqualStrings("std", imp.kind.simple.path[0]);
    try testing.expectEqualStrings("io", imp.kind.simple.path[1]);
}

test "parse import with alias" {
    var result = try testParser("import std.io as io\n");
    defer result.deinit();

    const module = try result.parser.parseModule();
    const imp = module.imports[0];
    try testing.expect(imp.kind == .simple);
    try testing.expectEqualStrings("io", imp.kind.simple.alias.?);
}

test "parse from import" {
    var result = try testParser("from std.io import read_file, write_file\n");
    defer result.deinit();

    const module = try result.parser.parseModule();
    const imp = module.imports[0];
    try testing.expect(imp.kind == .from);
    try testing.expectEqual(@as(usize, 2), imp.kind.from.names.len);
    try testing.expectEqualStrings("read_file", imp.kind.from.names[0].name);
}

test "parse function declaration" {
    const source =
        \\fn add(x: Int, y: Int) -> Int:
        \\    return x + y
        \\
    ;
    var result = try testParser(source);
    defer result.deinit();

    const module = try result.parser.parseModule();
    try testing.expectEqual(@as(usize, 1), module.decls.len);
    try testing.expect(module.decls[0].kind == .fn_decl);
    try testing.expectEqualStrings("add", module.decls[0].kind.fn_decl.name);
    try testing.expectEqual(@as(usize, 2), module.decls[0].kind.fn_decl.params.len);
    try testing.expect(module.decls[0].kind.fn_decl.return_type != null);
}

test "parse pub function" {
    const source =
        \\pub fn greet():
        \\    print("hi")
        \\
    ;
    var result = try testParser(source);
    defer result.deinit();

    const module = try result.parser.parseModule();
    try testing.expect(module.decls[0].is_pub);
    try testing.expect(module.decls[0].kind == .fn_decl);
}

test "parse struct declaration" {
    const source =
        \\struct Point:
        \\    x: Float
        \\    y: Float
        \\
    ;
    var result = try testParser(source);
    defer result.deinit();

    const module = try result.parser.parseModule();
    try testing.expect(module.decls[0].kind == .struct_decl);
    try testing.expectEqualStrings("Point", module.decls[0].kind.struct_decl.name);
    try testing.expectEqual(@as(usize, 2), module.decls[0].kind.struct_decl.fields.len);
}

test "parse enum declaration" {
    const source =
        \\enum Color:
        \\    Red
        \\    Green
        \\    Blue
        \\
    ;
    var result = try testParser(source);
    defer result.deinit();

    const module = try result.parser.parseModule();
    try testing.expect(module.decls[0].kind == .enum_decl);
    try testing.expectEqual(@as(usize, 3), module.decls[0].kind.enum_decl.variants.len);
}

test "parse enum with fields" {
    const source =
        \\enum Shape:
        \\    Circle(Float)
        \\    Rect(Float, Float)
        \\
    ;
    var result = try testParser(source);
    defer result.deinit();

    const module = try result.parser.parseModule();
    const variants = module.decls[0].kind.enum_decl.variants;
    try testing.expectEqual(@as(usize, 1), variants[0].fields.len);
    try testing.expectEqual(@as(usize, 2), variants[1].fields.len);
}

test "parse interface declaration" {
    const source =
        \\interface Display:
        \\    fn to_string(self: ref Self) -> String
        \\
    ;
    var result = try testParser(source);
    defer result.deinit();

    const module = try result.parser.parseModule();
    try testing.expect(module.decls[0].kind == .interface_decl);
    try testing.expectEqual(@as(usize, 1), module.decls[0].kind.interface_decl.methods.len);
}

test "parse type alias" {
    var result = try testParser("type StringList = List[String]\n");
    defer result.deinit();

    const module = try result.parser.parseModule();
    try testing.expect(module.decls[0].kind == .type_alias);
    try testing.expectEqualStrings("StringList", module.decls[0].kind.type_alias.name);
}

test "parse generic function" {
    const source =
        \\fn identity[T](x: T) -> T:
        \\    return x
        \\
    ;
    var result = try testParser(source);
    defer result.deinit();

    const module = try result.parser.parseModule();
    const fn_decl = module.decls[0].kind.fn_decl;
    try testing.expectEqual(@as(usize, 1), fn_decl.generic_params.len);
    try testing.expectEqualStrings("T", fn_decl.generic_params[0].name);
}

test "parse complete program" {
    const source =
        \\import std.io
        \\
        \\fn greet(name: String) -> String:
        \\    return "hello, {name}!"
        \\
        \\fn main():
        \\    message := greet("world")
        \\    if message != "":
        \\        print(message)
        \\
    ;
    var result = try testParser(source);
    defer result.deinit();

    const module = try result.parser.parseModule();
    try testing.expectEqual(@as(usize, 1), module.imports.len);
    try testing.expectEqual(@as(usize, 2), module.decls.len);
}

// -- edge case and error tests --

test "parse empty module" {
    var result = try testParser("");
    defer result.deinit();

    const module = try result.parser.parseModule();
    try testing.expectEqual(@as(usize, 0), module.imports.len);
    try testing.expectEqual(@as(usize, 0), module.decls.len);
}

test "parse unexpected token produces error expr" {
    var result = try testParser("@");
    defer result.deinit();

    // the lexer will produce an error token, which parsePrimary handles
    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .err);
}

test "parse trailing comma in list" {
    var result = try testParser("[1, 2, 3,]");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .list);
    try testing.expectEqual(@as(usize, 3), expr.kind.list.len);
}

test "parse trailing comma in map" {
    var result = try testParser("{\"a\": 1, \"b\": 2,}");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .map);
    try testing.expectEqual(@as(usize, 2), expr.kind.map.len);
}

test "parse nested if expression" {
    var result = try testParser("if a: if b: 1 else: 2 else: 3");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .if_expr);
    try testing.expect(expr.kind.if_expr.then_expr.kind == .if_expr);
}

test "parse deeply nested binary" {
    var result = try testParser("1 + 2 + 3 + 4 + 5");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    // left-associative: ((((1+2)+3)+4)+5)
    try testing.expect(expr.kind == .binary);
    try testing.expect(expr.kind.binary.right.kind == .int_lit);
    try testing.expect(expr.kind.binary.left.kind == .binary);
}

test "parse struct with defaults and modifiers" {
    const source =
        \\struct Config:
        \\    pub host: String = "localhost"
        \\    pub mut port: Int = 8080
        \\
    ;
    var result = try testParser(source);
    defer result.deinit();

    const module = try result.parser.parseModule();
    const fields = module.decls[0].kind.struct_decl.fields;
    try testing.expectEqual(@as(usize, 2), fields.len);
    try testing.expect(fields[0].is_pub);
    try testing.expect(!fields[0].is_mut);
    try testing.expect(fields[0].default != null);
    try testing.expect(fields[1].is_pub);
    try testing.expect(fields[1].is_mut);
}

test "parse fn with default parameter" {
    const source =
        \\fn connect(host: String = "localhost", port: Int = 5432):
        \\    return none
        \\
    ;
    var result = try testParser(source);
    defer result.deinit();

    const module = try result.parser.parseModule();
    const params = module.decls[0].kind.fn_decl.params;
    try testing.expectEqual(@as(usize, 2), params.len);
    try testing.expect(params[0].default != null);
    try testing.expect(params[1].default != null);
}

test "parse match with guard" {
    const source =
        \\match x:
        \\    n if n > 0 => "positive"
        \\    _ => "non-positive"
        \\
    ;
    var result = try testParser(source);
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .match_expr);
    try testing.expectEqual(@as(usize, 2), expr.kind.match_expr.arms.len);
    try testing.expect(expr.kind.match_expr.arms[0].guard != null);
    try testing.expect(expr.kind.match_expr.arms[1].guard == null);
}

test "parse pattern variants" {
    var result = try testParser(
        \\match shape:
        \\    Shape.Circle(r) => r
        \\    Shape.Rect(w, h) => w
        \\    _ => 0
        \\
    );
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .match_expr);
    const arms = expr.kind.match_expr.arms;
    try testing.expect(arms[0].pattern.kind == .variant);
    try testing.expectEqualStrings("Circle", arms[0].pattern.kind.variant.variant);
    try testing.expectEqual(@as(usize, 1), arms[0].pattern.kind.variant.fields.len);
    try testing.expect(arms[1].pattern.kind == .variant);
    try testing.expectEqual(@as(usize, 2), arms[1].pattern.kind.variant.fields.len);
    try testing.expect(arms[2].pattern.kind == .wildcard);
}

test "parse impl block" {
    const source =
        \\impl Point:
        \\    pub fn new(x: Float, y: Float) -> Point:
        \\        return Point(x, y)
        \\
    ;
    var result = try testParser(source);
    defer result.deinit();

    const module = try result.parser.parseModule();
    try testing.expect(module.decls[0].kind == .impl_decl);
    const impl_decl = module.decls[0].kind.impl_decl;
    try testing.expectEqual(@as(usize, 1), impl_decl.methods.len);
    try testing.expect(impl_decl.methods[0].is_pub);
}

test "parse impl for interface" {
    const source =
        \\impl Display for Point:
        \\    fn to_string(self: ref Point) -> String:
        \\        return "point"
        \\
    ;
    var result = try testParser(source);
    defer result.deinit();

    const module = try result.parser.parseModule();
    const impl_decl = module.decls[0].kind.impl_decl;
    try testing.expect(impl_decl.interface != null);
}

test "parse lambda with typed params" {
    var result = try testParser("fn(x: Int, y: Int) => x + y");
    defer result.deinit();

    const expr = try result.parser.parseExpression();
    try testing.expect(expr.kind == .lambda);
    try testing.expectEqual(@as(usize, 2), expr.kind.lambda.params.len);
    try testing.expect(expr.kind.lambda.params[0].type_expr != null);
}

test "parse empty function" {
    const source =
        \\fn noop():
        \\    return
        \\
    ;
    var result = try testParser(source);
    defer result.deinit();

    const module = try result.parser.parseModule();
    try testing.expect(module.decls[0].kind == .fn_decl);
    const body = module.decls[0].kind.fn_decl.body;
    try testing.expectEqual(@as(usize, 1), body.stmts.len);
    try testing.expect(body.stmts[0].kind == .return_stmt);
}

test "parse if-elif-else" {
    const source =
        \\if x > 0:
        \\    a
        \\elif x == 0:
        \\    b
        \\else:
        \\    c
        \\
    ;
    var result = try testParser(source);
    defer result.deinit();

    const stmt = try result.parser.parseStatement();
    try testing.expect(stmt.kind == .if_stmt);
    try testing.expectEqual(@as(usize, 1), stmt.kind.if_stmt.elif_branches.len);
    try testing.expect(stmt.kind.if_stmt.else_block != null);
}

test "parse generic struct" {
    const source =
        \\struct Pair[A, B]:
        \\    first: A
        \\    second: B
        \\
    ;
    var result = try testParser(source);
    defer result.deinit();

    const module = try result.parser.parseModule();
    const s = module.decls[0].kind.struct_decl;
    try testing.expectEqualStrings("Pair", s.name);
    try testing.expectEqual(@as(usize, 2), s.generic_params.len);
    try testing.expectEqualStrings("A", s.generic_params[0].name);
    try testing.expectEqualStrings("B", s.generic_params[1].name);
}

test "parse generic with bounds" {
    const source =
        \\fn sort[T: Comparable](items: List[T]):
        \\    return items
        \\
    ;
    var result = try testParser(source);
    defer result.deinit();

    const module = try result.parser.parseModule();
    const gp = module.decls[0].kind.fn_decl.generic_params;
    try testing.expectEqual(@as(usize, 1), gp.len);
    try testing.expectEqual(@as(usize, 1), gp[0].bounds.len);
}
