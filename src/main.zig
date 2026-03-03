// main — CLI entry point for the forge compiler

const std = @import("std");
const lexer_mod = @import("lexer.zig");
const Lexer = lexer_mod.Lexer;
const Token = lexer_mod.Token;
const Parser = @import("parser.zig").Parser;
const errors = @import("errors.zig");
const printer = @import("printer.zig");
const Checker = @import("checker.zig").Checker;
const ast = @import("ast.zig");
const io = @import("io.zig");

// compiler modules — imported here so zig build sees them
comptime {
    _ = @import("ast.zig");
    _ = @import("parser.zig");
    _ = @import("errors.zig");
    _ = @import("intern.zig");
    _ = @import("printer.zig");
    _ = @import("io.zig");
    _ = @import("types.zig");
    _ = @import("checker.zig");
}

const version = "0.1.0";

/// max source file size the compiler will read (10 MiB). prevents
/// accidental reads of large binary files.
const max_source_size = 10 * 1024 * 1024;

fn renderDiagnostics(diags: *const errors.DiagnosticList) void {
    var buf: [io.write_buf_size]u8 = undefined;
    var w = std.fs.File.stderr().writer(&buf);
    const out = &w.interface;
    diags.render(out) catch {};
    out.flush() catch {};
}

fn readSourceFile(allocator: std.mem.Allocator, path: []const u8) ?[]const u8 {
    return std.fs.cwd().readFileAlloc(allocator, path, max_source_size) catch |err| {
        io.writeErr("error: could not read '{s}': {}\n", .{ path, err });
        return null;
    };
}

/// bundles the outputs of lexing + parsing so callers can clean up
/// with a single deinit call.
const ParseResult = struct {
    module: ast.Module,
    tokens: []const Token,
    arena: std.heap.ArenaAllocator,

    fn deinit(self: *ParseResult, allocator: std.mem.Allocator) void {
        allocator.free(self.tokens);
        self.arena.deinit();
    }
};

/// lex and parse source code. returns null if there are errors
/// (diagnostics are rendered to stderr before returning).
fn lexAndParse(allocator: std.mem.Allocator, source: []const u8) !?ParseResult {
    // lex
    var lexer = try Lexer.init(source, allocator);
    defer lexer.deinit();
    const tokens = try lexer.tokenize();

    if (lexer.diagnostics.hasErrors()) {
        renderDiagnostics(&lexer.diagnostics);
        allocator.free(tokens);
        return null;
    }

    // parse
    var arena = std.heap.ArenaAllocator.init(allocator);
    var parser = Parser.init(tokens, source, arena.allocator());
    defer parser.deinit();

    const module = parser.parseModule() catch {
        io.writeErr("error: parse failed (out of memory)\n", .{});
        arena.deinit();
        allocator.free(tokens);
        return null;
    };

    if (parser.diagnostics.hasErrors()) {
        renderDiagnostics(&parser.diagnostics);
        arena.deinit();
        allocator.free(tokens);
        return null;
    }

    return .{ .module = module, .tokens = tokens, .arena = arena };
}

pub fn main() !void {
    var gpa: std.heap.GeneralPurposeAllocator(.{}) = .init;
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    var args = std.process.argsWithAllocator(allocator) catch {
        printUsage();
        return;
    };
    defer args.deinit();

    // skip the program name
    _ = args.next();

    const cmd = args.next() orelse {
        printUsage();
        return;
    };

    if (std.mem.eql(u8, cmd, "version") or std.mem.eql(u8, cmd, "--version")) {
        printVersion();
    } else if (std.mem.eql(u8, cmd, "help") or std.mem.eql(u8, cmd, "--help")) {
        printUsage();
    } else if (std.mem.eql(u8, cmd, "lex")) {
        const file_path = args.next() orelse {
            io.writeErr("error: forge lex requires a file path\n", .{});
            return;
        };
        try runLex(allocator, file_path);
    } else if (std.mem.eql(u8, cmd, "parse")) {
        const file_path = args.next() orelse {
            io.writeErr("error: forge parse requires a file path\n", .{});
            return;
        };
        try runParse(allocator, file_path);
    } else if (std.mem.eql(u8, cmd, "check")) {
        const file_path = args.next() orelse {
            io.writeErr("error: forge check requires a file path\n", .{});
            return;
        };
        try runCheck(allocator, file_path);
    } else {
        io.writeErr("error: unknown command '{s}'\n", .{cmd});
        printUsage();
    }
}

/// lex a source file and print each token.
fn runLex(allocator: std.mem.Allocator, path: []const u8) !void {
    const source = readSourceFile(allocator, path) orelse return;
    defer allocator.free(source);

    var lexer = try Lexer.init(source, allocator);
    defer lexer.deinit();

    while (true) {
        const tok = try lexer.nextToken();

        switch (tok.kind) {
            .newline => io.write("{s:<16}  \\n\n", .{@tagName(tok.kind)}),
            .indent => io.write("{s:<16}  >>>\n", .{@tagName(tok.kind)}),
            .dedent => io.write("{s:<16}  <<<\n", .{@tagName(tok.kind)}),
            .eof => {
                io.write("{s:<16}  <eof>\n", .{@tagName(tok.kind)});
                break;
            },
            else => {
                if (tok.lexeme.len > 0) {
                    io.write("{s:<16}  {s}\n", .{ @tagName(tok.kind), tok.lexeme });
                } else {
                    io.write("{s:<16}\n", .{@tagName(tok.kind)});
                }
            },
        }
    }

    if (lexer.diagnostics.hasErrors()) {
        renderDiagnostics(&lexer.diagnostics);
    }
}

/// lex and parse a source file, then print the AST.
fn runParse(allocator: std.mem.Allocator, path: []const u8) !void {
    const source = readSourceFile(allocator, path) orelse return;
    defer allocator.free(source);

    var result = try lexAndParse(allocator, source) orelse return;
    defer result.deinit(allocator);

    printer.printModule(result.module);
}

/// lex, parse, and type-check a source file. prints "ok" on success.
fn runCheck(allocator: std.mem.Allocator, path: []const u8) !void {
    const source = readSourceFile(allocator, path) orelse return;
    defer allocator.free(source);

    var result = try lexAndParse(allocator, source) orelse return;
    defer result.deinit(allocator);

    var checker = Checker.init(allocator, source) catch {
        io.writeErr("error: checker init failed (out of memory)\n", .{});
        return;
    };
    defer checker.deinit();

    checker.check(&result.module);

    if (checker.diagnostics.hasErrors()) {
        renderDiagnostics(&checker.diagnostics);
    } else {
        io.write("ok\n", .{});
    }
}

fn printVersion() void {
    io.write("forge {s}\n", .{version});
}

fn printUsage() void {
    io.write(
        \\forge {s}
        \\
        \\usage: forge <command> [options]
        \\
        \\commands:
        \\  check <file>   type check a source file
        \\  lex <file>     tokenize a source file
        \\  parse <file>   parse and print AST
        \\  version        print version
        \\  help           show this message
        \\
    , .{version});
}
