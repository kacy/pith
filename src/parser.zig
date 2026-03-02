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
