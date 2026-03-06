// pipeline — shared compiler phases used by CLI commands

const std = @import("std");
const lexer_mod = @import("lexer.zig");
const Lexer = lexer_mod.Lexer;
const Token = lexer_mod.Token;
const Parser = @import("parser.zig").Parser;
const Checker = @import("checker.zig").Checker;
const errors = @import("errors.zig");
const ast = @import("ast.zig");
const io = @import("io.zig");

/// max source file size the compiler will read (10 MiB). prevents
/// accidental reads of large binary files.
pub const max_source_size = 10 * 1024 * 1024;

pub const LexResult = struct {
    tokens: []const Token,
    had_errors: bool,

    pub fn deinit(self: *const LexResult, allocator: std.mem.Allocator) void {
        allocator.free(self.tokens);
    }
};

pub const ParseResult = struct {
    module: ast.Module,
    arena: std.heap.ArenaAllocator,
    had_errors: bool,

    pub fn deinit(self: *ParseResult) void {
        self.arena.deinit();
    }
};

pub const CheckResult = struct {
    had_errors: bool,
};

pub fn renderDiagnostics(diags: *const errors.DiagnosticList) void {
    var buf: [io.write_buf_size]u8 = undefined;
    var w = std.fs.File.stderr().writer(&buf);
    const out = &w.interface;
    diags.render(out) catch {};
    out.flush() catch {};
}

pub fn readSourceFile(allocator: std.mem.Allocator, path: []const u8) ![]const u8 {
    return std.fs.cwd().readFileAlloc(allocator, path, max_source_size) catch |err| {
        io.writeErr("error: could not read '{s}': {}\n", .{ path, err });
        return error.ReadFailed;
    };
}

pub fn lexSource(allocator: std.mem.Allocator, source: []const u8) !LexResult {
    var lexer = try Lexer.init(source, allocator);
    defer lexer.deinit();

    const tokens = try lexer.tokenize();

    const had_errors = lexer.diagnostics.hasErrors();
    if (had_errors) {
        renderDiagnostics(&lexer.diagnostics);
    }

    return .{ .tokens = tokens, .had_errors = had_errors };
}

pub fn parseSource(
    allocator: std.mem.Allocator,
    source: []const u8,
    tokens: []const Token,
) !ParseResult {
    var arena = std.heap.ArenaAllocator.init(allocator);
    var parser = Parser.init(tokens, source, arena.allocator());
    defer parser.deinit();

    const module = parser.parseModule() catch {
        io.writeErr("error: parse failed (out of memory)\n", .{});
        arena.deinit();
        return error.ParseFailed;
    };

    const had_errors = parser.diagnostics.hasErrors();
    if (had_errors) {
        renderDiagnostics(&parser.diagnostics);
    }

    return .{ .module = module, .arena = arena, .had_errors = had_errors };
}

pub fn checkModule(
    allocator: std.mem.Allocator,
    source: []const u8,
    module: *const ast.Module,
) !CheckResult {
    var checker = Checker.init(allocator, source) catch {
        io.writeErr("error: checker init failed (out of memory)\n", .{});
        return error.CheckFailed;
    };
    defer checker.deinit();

    checker.check(module);

    const had_errors = checker.diagnostics.hasErrors();
    if (had_errors) {
        renderDiagnostics(&checker.diagnostics);
    }

    return .{ .had_errors = had_errors };
}

test "lexSource surfaces lexer diagnostics" {
    const source = "x := @bad";

    var lex_result = try lexSource(std.testing.allocator, source);
    defer lex_result.deinit(std.testing.allocator);

    try std.testing.expect(lex_result.had_errors);
}

test "parseSource surfaces parser diagnostics" {
    const source = "fn main(\n";

    var lex_result = try lexSource(std.testing.allocator, source);
    defer lex_result.deinit(std.testing.allocator);
    try std.testing.expect(!lex_result.had_errors);

    var parse_result = try parseSource(std.testing.allocator, source, lex_result.tokens);
    defer parse_result.deinit();

    try std.testing.expect(parse_result.had_errors);
}

test "checkModule succeeds for a valid module" {
    const source =
        \\fn main():
        \\    print("hello")
    ;

    var lex_result = try lexSource(std.testing.allocator, source);
    defer lex_result.deinit(std.testing.allocator);
    try std.testing.expect(!lex_result.had_errors);

    var parse_result = try parseSource(std.testing.allocator, source, lex_result.tokens);
    defer parse_result.deinit();
    try std.testing.expect(!parse_result.had_errors);

    const check_result = try checkModule(std.testing.allocator, source, &parse_result.module);
    try std.testing.expect(!check_result.had_errors);
}
