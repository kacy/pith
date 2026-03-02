// lexer — tokenizer for forge source files
//
// transforms source text into a stream of tokens, handling
// indentation-based blocks (INDENT/DEDENT), string interpolation,
// and all forge operators and keywords.

const std = @import("std");
const intern = @import("intern.zig");
const errors = @import("errors.zig");

// ---------------------------------------------------------------
// token types
// ---------------------------------------------------------------

pub const TokenKind = enum {
    // literals
    int_lit,
    float_lit,
    string_lit,

    // string interpolation sequence
    string_start, // opening " up to first {
    string_mid, // text between } and next {
    string_end, // text after last } up to closing "
    interpolation_expr, // raw expression text between { and }

    // identifier
    identifier,

    // keywords
    kw_fn,
    kw_if,
    kw_elif,
    kw_else,
    kw_for,
    kw_in,
    kw_while,
    kw_match,
    kw_return,
    kw_fail,
    kw_break,
    kw_continue,
    kw_spawn,
    kw_await,
    kw_struct,
    kw_enum,
    kw_interface,
    kw_impl,
    kw_type,
    kw_pub,
    kw_mut,
    kw_ref,
    kw_weak,
    kw_import,
    kw_from,
    kw_as,
    kw_self,
    kw_true,
    kw_false,
    kw_none,
    kw_and,
    kw_or,
    kw_not,

    // operators
    plus, // +
    minus, // -
    star, // *
    slash, // /
    percent, // %
    eq_eq, // ==
    bang_eq, // !=
    less, // <
    greater, // >
    less_eq, // <=
    greater_eq, // >=
    colon_eq, // :=
    eq, // =
    plus_eq, // +=
    minus_eq, // -=
    star_eq, // *=
    slash_eq, // /=
    question, // ?
    bang, // !
    fat_arrow, // =>
    arrow, // ->
    dot, // .

    // delimiters
    colon, // :
    comma, // ,
    lparen, // (
    rparen, // )
    lbracket, // [
    rbracket, // ]
    lbrace, // {
    rbrace, // }
    pipe, // |
    underscore, // _

    // structure
    newline,
    indent,
    dedent,
    comment,
    eof,

    // error recovery
    err,
};

pub const Token = struct {
    kind: TokenKind,
    lexeme: []const u8,
    location: errors.Location,
};

// ---------------------------------------------------------------
// keyword lookup
// ---------------------------------------------------------------

const keyword_map = std.StaticStringMap(TokenKind).initComptime(.{
    .{ "fn", .kw_fn },
    .{ "if", .kw_if },
    .{ "elif", .kw_elif },
    .{ "else", .kw_else },
    .{ "for", .kw_for },
    .{ "in", .kw_in },
    .{ "while", .kw_while },
    .{ "match", .kw_match },
    .{ "return", .kw_return },
    .{ "fail", .kw_fail },
    .{ "break", .kw_break },
    .{ "continue", .kw_continue },
    .{ "spawn", .kw_spawn },
    .{ "await", .kw_await },
    .{ "struct", .kw_struct },
    .{ "enum", .kw_enum },
    .{ "interface", .kw_interface },
    .{ "impl", .kw_impl },
    .{ "type", .kw_type },
    .{ "pub", .kw_pub },
    .{ "mut", .kw_mut },
    .{ "ref", .kw_ref },
    .{ "weak", .kw_weak },
    .{ "import", .kw_import },
    .{ "from", .kw_from },
    .{ "as", .kw_as },
    .{ "self", .kw_self },
    .{ "true", .kw_true },
    .{ "false", .kw_false },
    .{ "none", .kw_none },
    .{ "and", .kw_and },
    .{ "or", .kw_or },
    .{ "not", .kw_not },
});

// ---------------------------------------------------------------
// lexer
// ---------------------------------------------------------------

pub const Lexer = struct {
    source: []const u8,
    pos: u32,
    line: u32,
    column: u32,

    /// stack of indentation levels. starts with 0 (no indent).
    indent_stack: std.ArrayList(u32),
    allocator: std.mem.Allocator,

    /// pending DEDENT tokens to emit (when we drop multiple levels at once).
    pending_dedents: u32,

    /// pending tokens from string interpolation.
    pending_tokens: std.ArrayList(Token),

    /// true if we're at the very start of a line and need to check indentation.
    at_line_start: bool,

    /// true when we've emitted EOF.
    done: bool,

    diagnostics: errors.DiagnosticList,

    pub fn init(source: []const u8, allocator: std.mem.Allocator) !Lexer {
        var indent_stack: std.ArrayList(u32) = .empty;
        try indent_stack.append(allocator, 0); // base level

        return .{
            .source = source,
            .pos = 0,
            .line = 0,
            .column = 0,
            .indent_stack = indent_stack,
            .allocator = allocator,
            .pending_dedents = 0,
            .pending_tokens = .empty,
            .at_line_start = true,
            .done = false,
            .diagnostics = errors.DiagnosticList.init(allocator, source),
        };
    }

    pub fn deinit(self: *Lexer) void {
        self.indent_stack.deinit(self.allocator);
        self.pending_tokens.deinit(self.allocator);
        self.diagnostics.deinit();
    }

    // -- public API --

    /// return the next token.
    pub fn nextToken(self: *Lexer) !Token {
        // emit queued dedent tokens first
        if (self.pending_dedents > 0) {
            self.pending_dedents -= 1;
            return self.makeToken(.dedent, self.pos, 0);
        }

        // emit queued tokens from string interpolation
        if (self.pending_tokens.items.len > 0) {
            return self.pending_tokens.orderedRemove(0);
        }

        // end of file — emit remaining dedents then EOF
        if (self.pos >= self.source.len) {
            if (!self.done) {
                if (self.indent_stack.items.len > 1) {
                    const remaining = @as(u32, @intCast(self.indent_stack.items.len)) - 1;
                    self.indent_stack.shrinkRetainingCapacity(1);
                    if (remaining > 1) {
                        self.pending_dedents = remaining - 1;
                    }
                    return self.makeToken(.dedent, self.pos, 0);
                }
                self.done = true;
            }
            return self.makeToken(.eof, self.pos, 0);
        }

        // indentation state machine:
        // we maintain a stack of indent levels (starts at [0]). on each new
        // line, measure the leading whitespace and compare to the stack top:
        //   deeper  → push the level, emit INDENT
        //   shallower → pop levels until we match, emit one DEDENT per pop
        //   same    → no token, continue to content
        // blank lines (whitespace-only or comment-only) are skipped entirely
        // so they don't generate spurious indent/dedent noise.
        if (self.at_line_start) {
            self.at_line_start = false;

            // skip blank lines (lines that are only whitespace or only a comment)
            if (self.isBlankLine()) {
                self.skipToNextLine();
                self.at_line_start = true;
                return self.nextToken();
            }

            const indent_result = self.measureIndent();
            if (indent_result.has_tab) {
                try self.diagnostics.addErrorWithFix(
                    self.currentLocation(1),
                    "tabs are not allowed for indentation, use spaces",
                    "replace tabs with spaces (4 spaces per indent level)",
                );
                return self.makeToken(.err, self.pos, 1);
            }

            const current_indent = self.indent_stack.items[self.indent_stack.items.len - 1];

            if (indent_result.level > current_indent) {
                try self.indent_stack.append(self.allocator, indent_result.level);
                return self.makeToken(.indent, self.pos, 0);
            } else if (indent_result.level < current_indent) {
                // pop indent levels and emit dedents
                var dedents: u32 = 0;
                while (self.indent_stack.items.len > 1) {
                    const top = self.indent_stack.items[self.indent_stack.items.len - 1];
                    if (top <= indent_result.level) break;
                    _ = self.indent_stack.pop();
                    dedents += 1;
                }

                // check that we landed on a valid indent level
                const new_top = self.indent_stack.items[self.indent_stack.items.len - 1];
                if (new_top != indent_result.level) {
                    try self.diagnostics.addError(
                        self.currentLocation(1),
                        "inconsistent indentation",
                    );
                    return self.makeToken(.err, self.pos, 1);
                }

                if (dedents > 0) {
                    self.pending_dedents = dedents - 1;
                    return self.makeToken(.dedent, self.pos, 0);
                }
            }
            // else: same indent level, no token needed
        }

        // skip spaces (but not newlines)
        self.skipSpaces();

        if (self.pos >= self.source.len) {
            return self.nextToken();
        }

        const ch = self.current();

        if (ch == '\n') {
            const tok = self.makeToken(.newline, self.pos, 1);
            self.advance();
            self.at_line_start = true;
            return tok;
        }

        if (ch == '#') return self.scanComment();
        if (ch == '"') return self.scanString();
        if (std.ascii.isDigit(ch)) return self.scanNumber();
        if (std.ascii.isAlphabetic(ch) or ch == '_') return self.scanIdentifier();

        return self.scanOperator();
    }

    /// collect all tokens into a slice. caller owns the returned memory.
    pub fn tokenize(self: *Lexer) ![]Token {
        var tokens: std.ArrayList(Token) = .empty;
        while (true) {
            const tok = try self.nextToken();
            try tokens.append(self.allocator, tok);
            if (tok.kind == .eof) break;
        }
        return tokens.toOwnedSlice(self.allocator);
    }

    // -- scanning helpers --

    fn scanComment(self: *Lexer) Token {
        const start = self.pos;
        while (self.pos < self.source.len and self.current() != '\n') {
            self.advance();
        }
        return self.makeToken(.comment, start, self.pos - start);
    }

    // string scanning has two paths:
    //   simple:       "hello world" → single STRING_LIT token
    //   interpolated: "hi {name}!"  → STRING_START, INTERPOLATION_EXPR,
    //                                  STRING_MID (if more text), ..., STRING_END
    // we peek ahead first to detect interpolation braces, then commit to
    // the appropriate path. both paths skip \-escaped characters so that
    // \" and \{ don't terminate or split the string prematurely.
    fn scanString(self: *Lexer) !Token {
        const start = self.pos;
        self.advance(); // skip opening "

        var has_interpolation = false;

        // scan ahead to see if there's an interpolation
        var preview = self.pos;
        while (preview < self.source.len) {
            if (self.source[preview] == '\\') {
                preview += 2; // skip escape
                continue;
            }
            if (self.source[preview] == '{') {
                has_interpolation = true;
                break;
            }
            if (self.source[preview] == '"' or self.source[preview] == '\n') break;
            preview += 1;
        }

        if (!has_interpolation) {
            // simple string — scan to closing quote
            while (self.pos < self.source.len and self.current() != '"' and self.current() != '\n') {
                if (self.current() == '\\') {
                    self.advance(); // skip backslash
                    if (self.pos < self.source.len) self.advance(); // skip escaped char
                } else {
                    self.advance();
                }
            }

            if (self.pos >= self.source.len or self.current() == '\n') {
                try self.diagnostics.addError(
                    self.locationAt(start, self.pos - start),
                    "unterminated string literal",
                );
                return self.makeToken(.err, start, self.pos - start);
            }

            self.advance(); // skip closing "
            return self.makeToken(.string_lit, start, self.pos - start);
        }

        // interpolated string — emit string_start for text up to first {
        while (self.pos < self.source.len and self.current() != '{' and self.current() != '"' and self.current() != '\n') {
            if (self.current() == '\\') {
                self.advance();
                if (self.pos < self.source.len) self.advance();
            } else {
                self.advance();
            }
        }

        if (self.pos >= self.source.len or self.current() == '\n') {
            try self.diagnostics.addError(
                self.locationAt(start, self.pos - start),
                "unterminated string literal",
            );
            return self.makeToken(.err, start, self.pos - start);
        }

        if (self.current() == '"') {
            // no interpolation after all (edge case)
            self.advance();
            return self.makeToken(.string_lit, start, self.pos - start);
        }

        // we hit { — this is string_start
        const start_tok = self.makeToken(.string_start, start, self.pos - start);

        // now scan the interpolation expression and remaining string parts
        // queue them all as pending tokens
        try self.scanInterpolationParts();

        return start_tok;
    }

    fn scanInterpolationParts(self: *Lexer) !void {
        while (self.pos < self.source.len) {
            if (self.current() != '{') break;

            self.advance(); // skip {
            const lbrace_pos = self.pos - 1;

            // scan expression tokens until matching }
            var brace_depth: u32 = 1;
            const expr_start = self.pos;
            while (self.pos < self.source.len and brace_depth > 0) {
                if (self.current() == '{') {
                    brace_depth += 1;
                } else if (self.current() == '}') {
                    brace_depth -= 1;
                    if (brace_depth == 0) break;
                }
                self.advance();
            }

            // emit the raw expression text. the parser will re-lex this
            // content to parse the actual expression.
            if (self.pos > expr_start) {
                try self.pending_tokens.append(self.allocator, self.makeToken(
                    .interpolation_expr,
                    expr_start,
                    self.pos - expr_start,
                ));
            } else {
                try self.diagnostics.addError(
                    self.locationAt(lbrace_pos, 1),
                    "empty interpolation expression",
                );
                try self.pending_tokens.append(self.allocator, self.makeToken(
                    .err,
                    lbrace_pos,
                    1,
                ));
            }

            if (self.pos >= self.source.len) {
                try self.diagnostics.addError(
                    self.locationAt(lbrace_pos, 1),
                    "unterminated string interpolation",
                );
                try self.pending_tokens.append(self.allocator, self.makeToken(
                    .err,
                    self.pos,
                    0,
                ));
                return;
            }

            self.advance(); // skip }

            // scan text after } — either more text, another {, or closing "
            const mid_start = self.pos;
            while (self.pos < self.source.len and self.current() != '{' and self.current() != '"' and self.current() != '\n') {
                if (self.current() == '\\') {
                    self.advance();
                    if (self.pos < self.source.len) self.advance();
                } else {
                    self.advance();
                }
            }

            if (self.pos >= self.source.len or self.current() == '\n') {
                try self.diagnostics.addError(
                    self.locationAt(mid_start, self.pos - mid_start),
                    "unterminated string literal",
                );
                try self.pending_tokens.append(self.allocator, self.makeToken(
                    .err,
                    mid_start,
                    self.pos - mid_start,
                ));
                return;
            }

            if (self.current() == '"') {
                // string_end — includes text from } to closing "
                self.advance(); // skip "
                try self.pending_tokens.append(self.allocator, self.makeToken(
                    .string_end,
                    mid_start,
                    self.pos - mid_start,
                ));
                return;
            }

            // another { — emit string_mid for text between } and {
            try self.pending_tokens.append(self.allocator, self.makeToken(
                .string_mid,
                mid_start,
                self.pos - mid_start,
            ));
        }
    }

    fn scanNumber(self: *Lexer) Token {
        const start = self.pos;

        // check for hex, binary, octal prefixes
        if (self.current() == '0' and self.pos + 1 < self.source.len) {
            const next = self.source[self.pos + 1];
            if (next == 'x' or next == 'X') return self.scanBaseNumber(start, isHexDigit);
            if (next == 'b' or next == 'B') return self.scanBaseNumber(start, isBinaryDigit);
            if (next == 'o' or next == 'O') return self.scanBaseNumber(start, isOctalDigit);
        }

        // decimal integer or float
        self.skipDigitsAndUnderscores();

        // check for float
        if (self.pos < self.source.len and self.current() == '.') {
            // make sure it's not a method call like 123.to_string()
            if (self.pos + 1 < self.source.len and std.ascii.isDigit(self.source[self.pos + 1])) {
                self.advance(); // skip .
                self.skipDigitsAndUnderscores();

                // exponent
                if (self.pos < self.source.len and (self.current() == 'e' or self.current() == 'E')) {
                    self.advance();
                    if (self.pos < self.source.len and (self.current() == '+' or self.current() == '-')) {
                        self.advance();
                    }
                    self.skipDigitsAndUnderscores();
                }

                return self.makeToken(.float_lit, start, self.pos - start);
            }
        }

        return self.makeToken(.int_lit, start, self.pos - start);
    }

    /// scan a prefixed integer literal (0x, 0b, 0o).
    /// skips the two-char prefix, then consumes digits matching the predicate
    /// (plus underscores for readability).
    fn scanBaseNumber(self: *Lexer, start: u32, isDigit: *const fn (u8) bool) Token {
        self.advance(); // skip 0
        self.advance(); // skip prefix char
        while (self.pos < self.source.len and (isDigit(self.current()) or self.current() == '_')) {
            self.advance();
        }
        return self.makeToken(.int_lit, start, self.pos - start);
    }

    fn scanIdentifier(self: *Lexer) Token {
        const start = self.pos;

        while (self.pos < self.source.len and (std.ascii.isAlphanumeric(self.current()) or self.current() == '_')) {
            self.advance();
        }

        const text = self.source[start..self.pos];

        // check if it's a keyword
        if (keyword_map.get(text)) |kw_kind| {
            // special case: lone underscore is the wildcard token, not an identifier
            return self.makeToken(kw_kind, start, self.pos - start);
        }

        // lone underscore
        if (text.len == 1 and text[0] == '_') {
            return self.makeToken(.underscore, start, 1);
        }

        return self.makeToken(.identifier, start, self.pos - start);
    }

    fn scanOperator(self: *Lexer) !Token {
        const start = self.pos;
        const ch = self.current();

        self.advance();

        const kind: TokenKind = switch (ch) {
            '+' => if (self.match('=')) .plus_eq else .plus,
            '-' => if (self.match('>')) .arrow else if (self.match('=')) .minus_eq else .minus,
            '*' => if (self.match('=')) .star_eq else .star,
            '/' => if (self.match('=')) .slash_eq else .slash,
            '%' => .percent,
            '=' => if (self.match('=')) .eq_eq else if (self.match('>')) .fat_arrow else .eq,
            '!' => if (self.match('=')) .bang_eq else .bang,
            '<' => if (self.match('=')) .less_eq else .less,
            '>' => if (self.match('=')) .greater_eq else .greater,
            ':' => if (self.match('=')) .colon_eq else .colon,
            '?' => .question,
            '.' => .dot,
            ',' => .comma,
            '(' => .lparen,
            ')' => .rparen,
            '[' => .lbracket,
            ']' => .rbracket,
            '{' => .lbrace,
            '}' => .rbrace,
            '|' => .pipe,
            else => {
                try self.diagnostics.addError(
                    self.locationAt(start, 1),
                    "unexpected character",
                );
                return self.makeToken(.err, start, 1);
            },
        };

        return self.makeToken(kind, start, self.pos - start);
    }

    // -- character-level helpers --

    fn current(self: *const Lexer) u8 {
        return self.source[self.pos];
    }

    fn advance(self: *Lexer) void {
        if (self.pos < self.source.len) {
            if (self.source[self.pos] == '\n') {
                self.line += 1;
                self.column = 0;
            } else {
                self.column += 1;
            }
            self.pos += 1;
        }
    }

    /// if the current char matches, advance and return true.
    fn match(self: *Lexer, expected: u8) bool {
        if (self.pos < self.source.len and self.source[self.pos] == expected) {
            self.advance();
            return true;
        }
        return false;
    }

    fn skipSpaces(self: *Lexer) void {
        while (self.pos < self.source.len and self.source[self.pos] == ' ') {
            self.advance();
        }
    }

    fn isHexDigit(ch: u8) bool {
        return (ch >= '0' and ch <= '9') or (ch >= 'a' and ch <= 'f') or (ch >= 'A' and ch <= 'F');
    }

    fn isBinaryDigit(ch: u8) bool {
        return ch == '0' or ch == '1';
    }

    fn isOctalDigit(ch: u8) bool {
        return ch >= '0' and ch <= '7';
    }

    fn skipDigitsAndUnderscores(self: *Lexer) void {
        while (self.pos < self.source.len and (std.ascii.isDigit(self.current()) or self.current() == '_')) {
            self.advance();
        }
    }

    fn skipToNextLine(self: *Lexer) void {
        while (self.pos < self.source.len and self.current() != '\n') {
            self.advance();
        }
        if (self.pos < self.source.len) {
            self.advance(); // skip the newline itself
        }
    }

    // -- indentation helpers --

    const IndentResult = struct {
        level: u32,
        has_tab: bool,
    };

    fn measureIndent(self: *Lexer) IndentResult {
        var level: u32 = 0;
        var has_tab = false;

        while (self.pos + level < self.source.len) {
            const ch = self.source[self.pos + level];
            if (ch == ' ') {
                level += 1;
            } else if (ch == '\t') {
                has_tab = true;
                level += 1;
            } else {
                break;
            }
        }

        return .{ .level = level, .has_tab = has_tab };
    }

    fn isBlankLine(self: *Lexer) bool {
        var i = self.pos;
        while (i < self.source.len) {
            const ch = self.source[i];
            if (ch == '\n') return true;
            if (ch == '#') return true; // comment-only line counts as blank
            if (ch != ' ' and ch != '\t') return false;
            i += 1;
        }
        // reached end of file — treat as blank if only whitespace
        return true;
    }

    // -- token construction --

    fn makeToken(self: *const Lexer, kind: TokenKind, start: u32, length: u32) Token {
        return .{
            .kind = kind,
            .lexeme = if (length > 0) self.source[start .. start + length] else "",
            .location = self.locationAt(start, length),
        };
    }

    fn locationAt(self: *const Lexer, offset: u32, length: u32) errors.Location {
        // compute line/column for the given offset
        var line: u32 = 0;
        var col: u32 = 0;
        var i: u32 = 0;
        while (i < offset and i < self.source.len) : (i += 1) {
            if (self.source[i] == '\n') {
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        return .{
            .line = line,
            .column = col,
            .offset = offset,
            .length = length,
        };
    }

    fn currentLocation(self: *const Lexer, length: u32) errors.Location {
        return .{
            .line = self.line,
            .column = self.column,
            .offset = self.pos,
            .length = length,
        };
    }
};

// ---------------------------------------------------------------
// tests
// ---------------------------------------------------------------

const testing = std.testing;

fn expectTokens(source: []const u8, expected_kinds: []const TokenKind) !void {
    var lexer = try Lexer.init(source, testing.allocator);
    defer lexer.deinit();

    for (expected_kinds) |expected| {
        const tok = try lexer.nextToken();
        try testing.expectEqual(expected, tok.kind);
    }
}

// -- keyword tests --

test "lex keywords" {
    try expectTokens("fn", &.{ .kw_fn, .eof });
    try expectTokens("if", &.{ .kw_if, .eof });
    try expectTokens("elif", &.{ .kw_elif, .eof });
    try expectTokens("else", &.{ .kw_else, .eof });
    try expectTokens("for", &.{ .kw_for, .eof });
    try expectTokens("in", &.{ .kw_in, .eof });
    try expectTokens("while", &.{ .kw_while, .eof });
    try expectTokens("match", &.{ .kw_match, .eof });
    try expectTokens("return", &.{ .kw_return, .eof });
    try expectTokens("fail", &.{ .kw_fail, .eof });
    try expectTokens("struct", &.{ .kw_struct, .eof });
    try expectTokens("enum", &.{ .kw_enum, .eof });
    try expectTokens("pub", &.{ .kw_pub, .eof });
    try expectTokens("mut", &.{ .kw_mut, .eof });
    try expectTokens("true", &.{ .kw_true, .eof });
    try expectTokens("false", &.{ .kw_false, .eof });
    try expectTokens("none", &.{ .kw_none, .eof });
    try expectTokens("and", &.{ .kw_and, .eof });
    try expectTokens("or", &.{ .kw_or, .eof });
    try expectTokens("not", &.{ .kw_not, .eof });
}

test "lex identifier vs keyword" {
    try expectTokens("function", &.{ .identifier, .eof });
    try expectTokens("iff", &.{ .identifier, .eof });
    try expectTokens("true_value", &.{ .identifier, .eof });
    try expectTokens("my_fn", &.{ .identifier, .eof });
}

test "lex underscore wildcard" {
    try expectTokens("_", &.{ .underscore, .eof });
}

test "lex identifier starting with underscore" {
    try expectTokens("_foo", &.{ .identifier, .eof });
    try expectTokens("__init", &.{ .identifier, .eof });
}

// -- number tests --

test "lex integer literals" {
    try expectTokens("0", &.{ .int_lit, .eof });
    try expectTokens("42", &.{ .int_lit, .eof });
    try expectTokens("1_000_000", &.{ .int_lit, .eof });
}

test "lex float literals" {
    try expectTokens("3.14", &.{ .float_lit, .eof });
    try expectTokens("0.5", &.{ .float_lit, .eof });
    try expectTokens("1_000.5", &.{ .float_lit, .eof });
    try expectTokens("1.0e10", &.{ .float_lit, .eof });
    try expectTokens("2.5E-3", &.{ .float_lit, .eof });
    try expectTokens("1.0e+5", &.{ .float_lit, .eof });
}

test "lex hex literals" {
    try expectTokens("0xff", &.{ .int_lit, .eof });
    try expectTokens("0xFF", &.{ .int_lit, .eof });
    try expectTokens("0xDEAD_BEEF", &.{ .int_lit, .eof });
}

test "lex binary literals" {
    try expectTokens("0b1010", &.{ .int_lit, .eof });
    try expectTokens("0b1111_0000", &.{ .int_lit, .eof });
}

test "lex octal literals" {
    try expectTokens("0o777", &.{ .int_lit, .eof });
    try expectTokens("0o644", &.{ .int_lit, .eof });
}

// -- string tests --

test "lex simple string" {
    try expectTokens("\"hello\"", &.{ .string_lit, .eof });
    try expectTokens("\"\"", &.{ .string_lit, .eof });
}

test "lex string with escapes" {
    try expectTokens("\"hello\\nworld\"", &.{ .string_lit, .eof });
    try expectTokens("\"tab\\there\"", &.{ .string_lit, .eof });
    try expectTokens("\"quote\\\"inside\"", &.{ .string_lit, .eof });
}

test "lex unterminated string" {
    var lexer = try Lexer.init("\"oops", testing.allocator);
    defer lexer.deinit();
    const tok = try lexer.nextToken();
    try testing.expectEqual(TokenKind.err, tok.kind);
    try testing.expect(lexer.diagnostics.hasErrors());
}

test "lex string with interpolation" {
    try expectTokens("\"hello {name}!\"", &.{
        .string_start,
        .interpolation_expr,
        .string_end,
        .eof,
    });
}

test "lex string with multiple interpolations" {
    try expectTokens("\"{a} and {b}\"", &.{
        .string_start,
        .interpolation_expr,
        .string_mid,
        .interpolation_expr,
        .string_end,
        .eof,
    });
}

// -- operator tests --

test "lex single-char operators" {
    try expectTokens("+", &.{ .plus, .eof });
    try expectTokens("-", &.{ .minus, .eof });
    try expectTokens("*", &.{ .star, .eof });
    try expectTokens("/", &.{ .slash, .eof });
    try expectTokens("%", &.{ .percent, .eof });
    try expectTokens("?", &.{ .question, .eof });
    try expectTokens("!", &.{ .bang, .eof });
    try expectTokens(".", &.{ .dot, .eof });
}

test "lex multi-char operators" {
    try expectTokens("==", &.{ .eq_eq, .eof });
    try expectTokens("!=", &.{ .bang_eq, .eof });
    try expectTokens("<=", &.{ .less_eq, .eof });
    try expectTokens(">=", &.{ .greater_eq, .eof });
    try expectTokens(":=", &.{ .colon_eq, .eof });
    try expectTokens("+=", &.{ .plus_eq, .eof });
    try expectTokens("-=", &.{ .minus_eq, .eof });
    try expectTokens("*=", &.{ .star_eq, .eof });
    try expectTokens("/=", &.{ .slash_eq, .eof });
    try expectTokens("=>", &.{ .fat_arrow, .eof });
    try expectTokens("->", &.{ .arrow, .eof });
}

test "lex delimiters" {
    try expectTokens("(", &.{ .lparen, .eof });
    try expectTokens(")", &.{ .rparen, .eof });
    try expectTokens("[", &.{ .lbracket, .eof });
    try expectTokens("]", &.{ .rbracket, .eof });
    try expectTokens("{", &.{ .lbrace, .eof });
    try expectTokens("}", &.{ .rbrace, .eof });
    try expectTokens(",", &.{ .comma, .eof });
    try expectTokens(":", &.{ .colon, .eof });
    try expectTokens("|", &.{ .pipe, .eof });
}

// -- comment tests --

test "comment-only line is skipped" {
    // comment-only lines are treated as blank lines — no tokens emitted
    try expectTokens("# this is a comment", &.{.eof});
}

test "comment-only line between code is skipped" {
    // the comment line doesn't produce tokens, just like a blank line
    try expectTokens("# comment\nx", &.{ .identifier, .eof });
}

test "inline comment" {
    try expectTokens("x # comment", &.{ .identifier, .comment, .eof });
}

// -- indentation tests --

test "lex indent" {
    const source = "if true:\n    x";
    try expectTokens(source, &.{
        .kw_if,
        .kw_true,
        .colon,
        .newline,
        .indent,
        .identifier,
        .dedent,
        .eof,
    });
}

test "lex indent and dedent" {
    const source = "if true:\n    x\ny";
    try expectTokens(source, &.{
        .kw_if,
        .kw_true,
        .colon,
        .newline,
        .indent,
        .identifier,
        .newline,
        .dedent,
        .identifier,
        .eof,
    });
}

test "lex multiple dedents" {
    const source = "a:\n    b:\n        c\nd";
    try expectTokens(source, &.{
        .identifier,
        .colon,
        .newline,
        .indent,
        .identifier,
        .colon,
        .newline,
        .indent,
        .identifier,
        .newline,
        .dedent,
        .dedent,
        .identifier,
        .eof,
    });
}

test "lex blank lines are skipped" {
    const source = "a\n\n\nb";
    try expectTokens(source, &.{
        .identifier,
        .newline,
        .identifier,
        .eof,
    });
}

// -- edge cases --

test "lex empty input" {
    try expectTokens("", &.{.eof});
}

test "lex whitespace only" {
    try expectTokens("   ", &.{.eof});
}

test "lex invalid character" {
    var lexer = try Lexer.init("@", testing.allocator);
    defer lexer.deinit();
    const tok = try lexer.nextToken();
    try testing.expectEqual(TokenKind.err, tok.kind);
    try testing.expect(lexer.diagnostics.hasErrors());
}

test "lex tab produces error" {
    var lexer = try Lexer.init("x\n\ty", testing.allocator);
    defer lexer.deinit();

    // first line: x, newline
    const tok1 = try lexer.nextToken();
    try testing.expectEqual(TokenKind.identifier, tok1.kind);
    const tok2 = try lexer.nextToken();
    try testing.expectEqual(TokenKind.newline, tok2.kind);
    // indented line with tab: error
    const tok3 = try lexer.nextToken();
    try testing.expectEqual(TokenKind.err, tok3.kind);
}

// -- multi-line programs --

test "lex simple function" {
    const source =
        \\fn add(x: Int, y: Int) -> Int:
        \\    return x + y
    ;
    try expectTokens(source, &.{
        .kw_fn,
        .identifier, // add
        .lparen,
        .identifier, // x
        .colon,
        .identifier, // Int
        .comma,
        .identifier, // y
        .colon,
        .identifier, // Int
        .rparen,
        .arrow,
        .identifier, // Int
        .colon,
        .newline,
        .indent,
        .kw_return,
        .identifier, // x
        .plus,
        .identifier, // y
        .dedent,
        .eof,
    });
}

test "lex binding" {
    try expectTokens("x := 42", &.{
        .identifier,
        .colon_eq,
        .int_lit,
        .eof,
    });
}

test "lex mutable binding" {
    try expectTokens("mut count := 0", &.{
        .kw_mut,
        .identifier,
        .colon_eq,
        .int_lit,
        .eof,
    });
}

test "lex method call chain" {
    try expectTokens("x.foo().bar", &.{
        .identifier,
        .dot,
        .identifier,
        .lparen,
        .rparen,
        .dot,
        .identifier,
        .eof,
    });
}

test "lex match expression" {
    const source =
        \\match x:
        \\    1 => "one"
        \\    _ => "other"
    ;
    try expectTokens(source, &.{
        .kw_match,
        .identifier,
        .colon,
        .newline,
        .indent,
        .int_lit,
        .fat_arrow,
        .string_lit,
        .newline,
        .underscore,
        .fat_arrow,
        .string_lit,
        .dedent,
        .eof,
    });
}

test "lex result type syntax" {
    try expectTokens("fn foo() -> Int!:", &.{
        .kw_fn,
        .identifier,
        .lparen,
        .rparen,
        .arrow,
        .identifier,
        .bang,
        .colon,
        .eof,
    });
}

test "lex optional type syntax" {
    try expectTokens("x: Int?", &.{
        .identifier,
        .colon,
        .identifier,
        .question,
        .eof,
    });
}

test "lex generic type" {
    try expectTokens("List[Int]", &.{
        .identifier,
        .lbracket,
        .identifier,
        .rbracket,
        .eof,
    });
}

test "lex lambda" {
    try expectTokens("fn(x) => x * 2", &.{
        .kw_fn,
        .lparen,
        .identifier,
        .rparen,
        .fat_arrow,
        .identifier,
        .star,
        .int_lit,
        .eof,
    });
}

test "lex struct declaration" {
    const source =
        \\pub struct Point:
        \\    x: Float
        \\    y: Float
    ;
    try expectTokens(source, &.{
        .kw_pub,
        .kw_struct,
        .identifier,
        .colon,
        .newline,
        .indent,
        .identifier,
        .colon,
        .identifier,
        .newline,
        .identifier,
        .colon,
        .identifier,
        .dedent,
        .eof,
    });
}

test "lex import statement" {
    try expectTokens("import std.io", &.{
        .kw_import,
        .identifier,
        .dot,
        .identifier,
        .eof,
    });
}

test "lex from import" {
    try expectTokens("from std.io import read_file, write_file", &.{
        .kw_from,
        .identifier,
        .dot,
        .identifier,
        .kw_import,
        .identifier,
        .comma,
        .identifier,
        .eof,
    });
}

test "lex comparison operators in context" {
    try expectTokens("x >= 10 and y < 20", &.{
        .identifier,
        .greater_eq,
        .int_lit,
        .kw_and,
        .identifier,
        .less,
        .int_lit,
        .eof,
    });
}

test "lex multiple statements" {
    const source = "a := 1\nb := 2\nc := a + b";
    try expectTokens(source, &.{
        .identifier, .colon_eq,   .int_lit,
        .newline,    .identifier, .colon_eq,
        .int_lit,    .newline,    .identifier,
        .colon_eq,   .identifier, .plus,
        .identifier, .eof,
    });
}

test "lex comment-only lines between code" {
    const source = "a\n# comment\nb";
    try expectTokens(source, &.{
        .identifier,
        .newline,
        .identifier,
        .eof,
    });
}

test "lex lexeme content is correct" {
    var lexer = try Lexer.init("hello := 42", testing.allocator);
    defer lexer.deinit();

    const ident = try lexer.nextToken();
    try testing.expectEqualStrings("hello", ident.lexeme);

    const bind = try lexer.nextToken();
    try testing.expectEqualStrings(":=", bind.lexeme);

    const num = try lexer.nextToken();
    try testing.expectEqualStrings("42", num.lexeme);
}

test "lex location tracking" {
    var lexer = try Lexer.init("x := 42", testing.allocator);
    defer lexer.deinit();

    const x = try lexer.nextToken();
    try testing.expectEqual(@as(u32, 0), x.location.line);
    try testing.expectEqual(@as(u32, 0), x.location.column);

    const bind = try lexer.nextToken();
    try testing.expectEqual(@as(u32, 0), bind.location.line);
    try testing.expectEqual(@as(u32, 2), bind.location.column);
}

test "lex for loop" {
    try expectTokens("for x in items:", &.{
        .kw_for,
        .identifier,
        .kw_in,
        .identifier,
        .colon,
        .eof,
    });
}

test "lex while loop" {
    try expectTokens("while x > 0:", &.{
        .kw_while,
        .identifier,
        .greater,
        .int_lit,
        .colon,
        .eof,
    });
}

test "lex pipe operator" {
    try expectTokens("x | y", &.{
        .identifier,
        .pipe,
        .identifier,
        .eof,
    });
}

test "lex nested indentation" {
    const source = "a:\n    b:\n        c\n    d";
    try expectTokens(source, &.{
        .identifier, .colon,      .newline,
        .indent,     .identifier, .colon,
        .newline,    .indent,     .identifier,
        .newline,    .dedent,     .identifier,
        .dedent,     .eof,
    });
}

test "lex number followed by dot is not float" {
    // 42.method should lex as int, dot, identifier — not a float
    try expectTokens("42.method", &.{
        .int_lit,
        .dot,
        .identifier,
        .eof,
    });
}

test "lex spawn keyword" {
    try expectTokens("spawn", &.{ .kw_spawn, .eof });
    try expectTokens("spawner", &.{ .identifier, .eof });
}

test "lex await keyword" {
    try expectTokens("await", &.{ .kw_await, .eof });
    try expectTokens("awaiting", &.{ .identifier, .eof });
}

test "lex all assignment operators" {
    try expectTokens("a = b", &.{ .identifier, .eq, .identifier, .eof });
    try expectTokens("a += b", &.{ .identifier, .plus_eq, .identifier, .eof });
    try expectTokens("a -= b", &.{ .identifier, .minus_eq, .identifier, .eof });
    try expectTokens("a *= b", &.{ .identifier, .star_eq, .identifier, .eof });
    try expectTokens("a /= b", &.{ .identifier, .slash_eq, .identifier, .eof });
}
