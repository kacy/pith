// main — CLI entry point for the forge compiler

const std = @import("std");
const Lexer = @import("lexer.zig").Lexer;
const Parser = @import("parser.zig").Parser;
const errors = @import("errors.zig");
const printer = @import("printer.zig");
const Checker = @import("checker.zig").Checker;
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

fn renderDiagnostics(diags: *const errors.DiagnosticList) void {
    var buf: [8192]u8 = undefined;
    var w = std.fs.File.stderr().writer(&buf);
    const out = &w.interface;
    diags.render(out) catch {};
    out.flush() catch {};
}

fn readSourceFile(allocator: std.mem.Allocator, path: []const u8) ?[]const u8 {
    return std.fs.cwd().readFileAlloc(allocator, path, 1024 * 1024 * 10) catch |err| {
        io.writeErr("error: could not read '{s}': {}\n", .{ path, err });
        return null;
    };
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

fn runParse(allocator: std.mem.Allocator, path: []const u8) !void {
    const source = readSourceFile(allocator, path) orelse return;
    defer allocator.free(source);

    // lex
    var lexer = try Lexer.init(source, allocator);
    defer lexer.deinit();
    const tokens = try lexer.tokenize();
    defer allocator.free(tokens);

    if (lexer.diagnostics.hasErrors()) {
        renderDiagnostics(&lexer.diagnostics);
        return;
    }

    // parse
    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();

    var parser = Parser.init(tokens, source, arena.allocator());
    defer parser.deinit();

    const module = parser.parseModule() catch {
        io.writeErr("error: parse failed (out of memory)\n", .{});
        return;
    };

    if (parser.diagnostics.hasErrors()) {
        renderDiagnostics(&parser.diagnostics);
        return;
    }

    // print the AST
    printer.printModule(module);
}

fn runCheck(allocator: std.mem.Allocator, path: []const u8) !void {
    const source = readSourceFile(allocator, path) orelse return;
    defer allocator.free(source);

    // lex
    var lexer = try Lexer.init(source, allocator);
    defer lexer.deinit();
    const tokens = try lexer.tokenize();
    defer allocator.free(tokens);

    if (lexer.diagnostics.hasErrors()) {
        renderDiagnostics(&lexer.diagnostics);
        return;
    }

    // parse
    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();

    var parser = Parser.init(tokens, source, arena.allocator());
    defer parser.deinit();

    const module = parser.parseModule() catch {
        io.writeErr("error: parse failed (out of memory)\n", .{});
        return;
    };

    if (parser.diagnostics.hasErrors()) {
        renderDiagnostics(&parser.diagnostics);
        return;
    }

    // check
    var checker = Checker.init(allocator, source) catch {
        io.writeErr("error: checker init failed (out of memory)\n", .{});
        return;
    };
    defer checker.deinit();

    checker.check(&module);

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
