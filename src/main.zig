const std = @import("std");
const Lexer = @import("lexer.zig").Lexer;
const TokenKind = @import("lexer.zig").TokenKind;

// compiler modules — imported here so zig build sees them
comptime {
    _ = @import("lexer.zig");
    _ = @import("ast.zig");
    _ = @import("parser.zig");
    _ = @import("errors.zig");
    _ = @import("intern.zig");
}

const version = "0.1.0";

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
            printErr("error: forge lex requires a file path\n");
            return;
        };
        try runLex(allocator, file_path);
    } else {
        printErr("error: unknown command '");
        printErr(cmd);
        printErr("'\n");
        printUsage();
    }
}

fn runLex(allocator: std.mem.Allocator, path: []const u8) !void {
    const source = std.fs.cwd().readFileAlloc(allocator, path, 1024 * 1024 * 10) catch |err| {
        var buf: [4096]u8 = undefined;
        var w = std.fs.File.stderr().writer(&buf);
        const out = &w.interface;
        out.print("error: could not read '{s}': {}\n", .{ path, err }) catch {};
        out.flush() catch {};
        return;
    };
    defer allocator.free(source);

    var lexer = try Lexer.init(source, allocator);
    defer lexer.deinit();

    var buf: [8192]u8 = undefined;
    var w = std.fs.File.stdout().writer(&buf);
    const out = &w.interface;

    while (true) {
        const tok = try lexer.nextToken();

        // print token kind and lexeme
        out.print("{s:<16}", .{@tagName(tok.kind)}) catch {};

        switch (tok.kind) {
            .newline => out.print("  \\n\n", .{}) catch {},
            .indent => out.print("  >>>\n", .{}) catch {},
            .dedent => out.print("  <<<\n", .{}) catch {},
            .eof => {
                out.print("  <eof>\n", .{}) catch {};
                out.flush() catch {};
                break;
            },
            else => {
                if (tok.lexeme.len > 0) {
                    out.print("  {s}\n", .{tok.lexeme}) catch {};
                } else {
                    out.print("\n", .{}) catch {};
                }
            },
        }
    }

    out.flush() catch {};

    // print any errors
    if (lexer.diagnostics.hasErrors()) {
        var err_buf: [8192]u8 = undefined;
        var ew = std.fs.File.stderr().writer(&err_buf);
        const err_out = &ew.interface;
        lexer.diagnostics.render(err_out) catch {};
        err_out.flush() catch {};
    }
}

fn printVersion() void {
    var buf: [4096]u8 = undefined;
    var w = std.fs.File.stdout().writer(&buf);
    const out = &w.interface;
    out.print("forge {s}\n", .{version}) catch {};
    out.flush() catch {};
}

fn printUsage() void {
    var buf: [4096]u8 = undefined;
    var w = std.fs.File.stdout().writer(&buf);
    const out = &w.interface;
    out.print(
        \\forge {s}
        \\
        \\usage: forge <command> [options]
        \\
        \\commands:
        \\  lex <file>   tokenize a source file
        \\  version      print version
        \\  help         show this message
        \\
    , .{version}) catch {};
    out.flush() catch {};
}

fn printErr(msg: []const u8) void {
    var buf: [4096]u8 = undefined;
    var w = std.fs.File.stderr().writer(&buf);
    const out = &w.interface;
    out.writeAll(msg) catch {};
    out.flush() catch {};
}
