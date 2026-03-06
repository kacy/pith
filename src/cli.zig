// cli — command parsing and command execution

const std = @import("std");
const io = @import("io.zig");
const printer = @import("printer.zig");
const pipeline = @import("pipeline.zig");
const Token = @import("lexer.zig").Token;

const version = "0.1.0";

pub const Command = enum {
    help,
    version,
    lex,
    parse,
    check,
};

pub const Request = struct {
    command: Command,
    file_path: ?[]const u8,
};

pub fn parseArgs(allocator: std.mem.Allocator, args: []const []const u8) !Request {
    _ = allocator;

    if (args.len <= 1) {
        return error.MissingCommand;
    }

    const cmd = args[1];

    if (std.mem.eql(u8, cmd, "help") or std.mem.eql(u8, cmd, "--help")) {
        return .{ .command = .help, .file_path = null };
    }
    if (std.mem.eql(u8, cmd, "version") or std.mem.eql(u8, cmd, "--version")) {
        return .{ .command = .version, .file_path = null };
    }

    if (std.mem.eql(u8, cmd, "lex") or std.mem.eql(u8, cmd, "parse") or std.mem.eql(u8, cmd, "check")) {
        if (args.len <= 2) {
            io.writeErr("error: forge {s} requires a file path\n", .{cmd});
            return error.MissingFilePath;
        }
        const file_path = args[2];

        const command: Command = if (std.mem.eql(u8, cmd, "lex"))
            .lex
        else if (std.mem.eql(u8, cmd, "parse"))
            .parse
        else
            .check;

        return .{ .command = command, .file_path = file_path };
    }

    io.writeErr("error: unknown command '{s}'\n", .{cmd});
    printUsage();
    return error.UnknownCommand;
}

pub fn run(request: Request, allocator: std.mem.Allocator) !u8 {
    return switch (request.command) {
        .help => blk: {
            printUsage();
            break :blk 0;
        },
        .version => blk: {
            printVersion();
            break :blk 0;
        },
        .lex => runLex(allocator, request.file_path.?),
        .parse => runParse(allocator, request.file_path.?),
        .check => runCheck(allocator, request.file_path.?),
    };
}

fn runLex(allocator: std.mem.Allocator, path: []const u8) !u8 {
    const source = pipeline.readSourceFile(allocator, path) catch {
        return 1;
    };
    defer allocator.free(source);

    var lex_result = pipeline.lexSource(allocator, source) catch {
        io.writeErr("error: lex failed (out of memory)\n", .{});
        return 1;
    };
    defer lex_result.deinit(allocator);

    printTokens(lex_result.tokens);
    return if (lex_result.had_errors) 1 else 0;
}

fn runParse(allocator: std.mem.Allocator, path: []const u8) !u8 {
    const source = pipeline.readSourceFile(allocator, path) catch {
        return 1;
    };
    defer allocator.free(source);

    var lex_result = pipeline.lexSource(allocator, source) catch {
        io.writeErr("error: lex failed (out of memory)\n", .{});
        return 1;
    };
    defer lex_result.deinit(allocator);

    if (lex_result.had_errors) {
        return 1;
    }

    var parse_result = pipeline.parseSource(allocator, source, lex_result.tokens) catch {
        return 1;
    };
    defer parse_result.deinit();

    if (parse_result.had_errors) {
        return 1;
    }

    printer.printModule(parse_result.module);
    return 0;
}

fn runCheck(allocator: std.mem.Allocator, path: []const u8) !u8 {
    const source = pipeline.readSourceFile(allocator, path) catch {
        return 1;
    };
    defer allocator.free(source);

    var lex_result = pipeline.lexSource(allocator, source) catch {
        io.writeErr("error: lex failed (out of memory)\n", .{});
        return 1;
    };
    defer lex_result.deinit(allocator);

    if (lex_result.had_errors) {
        return 1;
    }

    var parse_result = pipeline.parseSource(allocator, source, lex_result.tokens) catch {
        return 1;
    };
    defer parse_result.deinit();

    if (parse_result.had_errors) {
        return 1;
    }

    const check_result = pipeline.checkModule(allocator, source, &parse_result.module) catch {
        return 1;
    };

    if (check_result.had_errors) {
        return 1;
    }

    io.write("ok\n", .{});
    return 0;
}

fn printTokens(tokens: []const Token) void {
    for (tokens) |tok| {
        switch (tok.kind) {
            .newline => io.write("{s:<16}  \\n\n", .{@tagName(tok.kind)}),
            .indent => io.write("{s:<16}  >>>\n", .{@tagName(tok.kind)}),
            .dedent => io.write("{s:<16}  <<<\n", .{@tagName(tok.kind)}),
            .eof => io.write("{s:<16}  <eof>\n", .{@tagName(tok.kind)}),
            else => {
                if (tok.lexeme.len > 0) {
                    io.write("{s:<16}  {s}\n", .{ @tagName(tok.kind), tok.lexeme });
                } else {
                    io.write("{s:<16}\n", .{@tagName(tok.kind)});
                }
            },
        }
    }
}

fn printVersion() void {
    io.write("forge {s}\n", .{version});
}

pub fn printUsage() void {
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

test "parseArgs parses help commands" {
    const argv1 = [_][]const u8{ "forge", "help" };
    const request1 = try parseArgs(std.testing.allocator, &argv1);
    try std.testing.expectEqual(Command.help, request1.command);
    try std.testing.expect(request1.file_path == null);

    const argv2 = [_][]const u8{ "forge", "--help" };
    const request2 = try parseArgs(std.testing.allocator, &argv2);
    try std.testing.expectEqual(Command.help, request2.command);
    try std.testing.expect(request2.file_path == null);
}

test "parseArgs parses version commands" {
    const argv1 = [_][]const u8{ "forge", "version" };
    const request1 = try parseArgs(std.testing.allocator, &argv1);
    try std.testing.expectEqual(Command.version, request1.command);
    try std.testing.expect(request1.file_path == null);

    const argv2 = [_][]const u8{ "forge", "--version" };
    const request2 = try parseArgs(std.testing.allocator, &argv2);
    try std.testing.expectEqual(Command.version, request2.command);
    try std.testing.expect(request2.file_path == null);
}

test "parseArgs parses file commands" {
    const lex_argv = [_][]const u8{ "forge", "lex", "examples/hello.fg" };
    const lex_request = try parseArgs(std.testing.allocator, &lex_argv);
    try std.testing.expectEqual(Command.lex, lex_request.command);
    try std.testing.expectEqualStrings("examples/hello.fg", lex_request.file_path.?);

    const parse_argv = [_][]const u8{ "forge", "parse", "examples/hello.fg" };
    const parse_request = try parseArgs(std.testing.allocator, &parse_argv);
    try std.testing.expectEqual(Command.parse, parse_request.command);
    try std.testing.expectEqualStrings("examples/hello.fg", parse_request.file_path.?);

    const check_argv = [_][]const u8{ "forge", "check", "examples/hello.fg" };
    const check_request = try parseArgs(std.testing.allocator, &check_argv);
    try std.testing.expectEqual(Command.check, check_request.command);
    try std.testing.expectEqualStrings("examples/hello.fg", check_request.file_path.?);
}

test "parseArgs errors for missing file path" {
    const lex_argv = [_][]const u8{ "forge", "lex" };
    try std.testing.expectError(error.MissingFilePath, parseArgs(std.testing.allocator, &lex_argv));

    const parse_argv = [_][]const u8{ "forge", "parse" };
    try std.testing.expectError(error.MissingFilePath, parseArgs(std.testing.allocator, &parse_argv));

    const check_argv = [_][]const u8{ "forge", "check" };
    try std.testing.expectError(error.MissingFilePath, parseArgs(std.testing.allocator, &check_argv));
}

test "parseArgs errors for unknown command" {
    const argv = [_][]const u8{ "forge", "unknown" };
    try std.testing.expectError(error.UnknownCommand, parseArgs(std.testing.allocator, &argv));
}

test "parseArgs errors for missing command" {
    const argv = [_][]const u8{"forge"};
    try std.testing.expectError(error.MissingCommand, parseArgs(std.testing.allocator, &argv));
}

test "integration: parseArgs + run check command" {
    const argv = [_][]const u8{ "forge", "check", "examples/hello.fg" };
    const request = try parseArgs(std.testing.allocator, &argv);

    const exit_code = try run(request, std.testing.allocator);
    try std.testing.expectEqual(@as(u8, 0), exit_code);
}
