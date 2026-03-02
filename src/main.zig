const std = @import("std");
const Lexer = @import("lexer.zig").Lexer;

// compiler modules — imported here so zig build sees them
comptime {
    _ = @import("ast.zig");
    _ = @import("parser.zig");
    _ = @import("errors.zig");
    _ = @import("intern.zig");
}

const version = "0.1.0";

// -- I/O helpers --
// zig's buffered writer API requires a buffer + writer + flush for every
// print. these helpers cut that ceremony down to a single call.

fn write(comptime fmt: []const u8, args: anytype) void {
    var buf: [8192]u8 = undefined;
    var w = std.fs.File.stdout().writer(&buf);
    const out = &w.interface;
    out.print(fmt, args) catch {};
    out.flush() catch {};
}

fn writeErr(comptime fmt: []const u8, args: anytype) void {
    var buf: [4096]u8 = undefined;
    var w = std.fs.File.stderr().writer(&buf);
    const out = &w.interface;
    out.print(fmt, args) catch {};
    out.flush() catch {};
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
            writeErr("error: forge lex requires a file path\n", .{});
            return;
        };
        try runLex(allocator, file_path);
    } else {
        writeErr("error: unknown command '{s}'\n", .{cmd});
        printUsage();
    }
}

fn runLex(allocator: std.mem.Allocator, path: []const u8) !void {
    const source = std.fs.cwd().readFileAlloc(allocator, path, 1024 * 1024 * 10) catch |err| {
        writeErr("error: could not read '{s}': {}\n", .{ path, err });
        return;
    };
    defer allocator.free(source);

    var lexer = try Lexer.init(source, allocator);
    defer lexer.deinit();

    while (true) {
        const tok = try lexer.nextToken();

        switch (tok.kind) {
            .newline => write("{s:<16}  \\n\n", .{@tagName(tok.kind)}),
            .indent => write("{s:<16}  >>>\n", .{@tagName(tok.kind)}),
            .dedent => write("{s:<16}  <<<\n", .{@tagName(tok.kind)}),
            .eof => {
                write("{s:<16}  <eof>\n", .{@tagName(tok.kind)});
                break;
            },
            else => {
                if (tok.lexeme.len > 0) {
                    write("{s:<16}  {s}\n", .{ @tagName(tok.kind), tok.lexeme });
                } else {
                    write("{s:<16}\n", .{@tagName(tok.kind)});
                }
            },
        }
    }

    if (lexer.diagnostics.hasErrors()) {
        var buf: [8192]u8 = undefined;
        var w = std.fs.File.stderr().writer(&buf);
        const out = &w.interface;
        lexer.diagnostics.render(out) catch {};
        out.flush() catch {};
    }
}

fn printVersion() void {
    write("forge {s}\n", .{version});
}

fn printUsage() void {
    write(
        \\forge {s}
        \\
        \\usage: forge <command> [options]
        \\
        \\commands:
        \\  lex <file>   tokenize a source file
        \\  version      print version
        \\  help         show this message
        \\
    , .{version});
}
