// main — CLI entry point for the forge compiler

const std = @import("std");
const builtin = @import("builtin");
const command = @import("cli/command.zig");
const parse_args = @import("cli/parse_args.zig");
const run_build = @import("cli/run_build.zig");
const run_check = @import("cli/run_check.zig");
const run_fmt = @import("cli/run_fmt.zig");
const run_lex = @import("cli/run_lex.zig");
const run_lint = @import("cli/run_lint.zig");
const run_parse = @import("cli/run_parse.zig");
const run_test = @import("cli/run_test.zig");
const io = @import("io.zig");

comptime {
    _ = command;
    _ = parse_args;
    _ = run_build;
    _ = run_check;
    _ = run_fmt;
    _ = run_lex;
    _ = run_lint;
    _ = run_parse;
    _ = run_test;
    _ = @import("ast.zig");
    _ = @import("parser.zig");
    _ = @import("errors.zig");
    _ = @import("intern.zig");
    _ = @import("printer.zig");
    _ = @import("io.zig");
    _ = @import("types.zig");
    _ = @import("checker.zig");
    _ = @import("checker_test.zig");
    _ = @import("codegen.zig");
    _ = @import("codegen_test.zig");
    _ = @import("formatter.zig");
    _ = @import("lint.zig");
    _ = @import("pipeline.zig");
    _ = @import("build_support.zig");
}

const version = "0.1.0";

pub fn main() !void {
    var gpa: std.heap.GeneralPurposeAllocator(.{}) = .init;
    defer if (builtin.mode == .Debug) {
        _ = gpa.deinit();
    };

    const allocator = if (builtin.mode == .Debug)
        gpa.allocator()
    else
        std.heap.smp_allocator;

    const argv = std.process.argsAlloc(allocator) catch {
        printUsage();
        return;
    };
    defer std.process.argsFree(allocator, argv);

    var request = parse_args.parse(allocator, if (argv.len > 0) argv[1..] else &.{}) catch |err| {
        switch (err) {
            error.InvalidArgs => return,
            error.UnknownCommand => {
                printUsage();
                return;
            },
            error.OutOfMemory => {
                io.writeErr("error: out of memory\n", .{});
                return;
            },
        }
    };
    defer request.deinit(allocator);

    try dispatch(allocator, request);
}

fn dispatch(allocator: std.mem.Allocator, request: parse_args.Request) !void {
    switch (request) {
        .build => |cmd| try run_build.run(allocator, cmd.path, false, cmd.json, &[_][]const u8{}),
        .check => |cmd| try run_check.run(allocator, cmd.path, cmd.json),
        .fmt => |cmd| try run_fmt.run(allocator, cmd.path, cmd.check_only),
        .help => printUsage(),
        .lex => |path| try run_lex.run(allocator, path),
        .lint => |cmd| try run_lint.run(allocator, cmd.path, cmd.json),
        .parse => |path| try run_parse.run(allocator, path),
        .run => |cmd| try run_build.run(allocator, cmd.path, true, false, cmd.extra_args),
        .@"test" => |cmd| try run_test.run(allocator, cmd.path, cmd.json),
        .version => printVersion(),
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
        \\  build <file>          compile to native binary
        \\  run <file>            compile and run
        \\  test <file>           run tests
        \\  fmt <file>            format source code
        \\  fmt <file> --check    check formatting (exit 1 if unformatted)
        \\  lint <file>           check conventions and best practices
        \\  lint <file> --json    lint with JSON output
        \\  check <file>          type check a source file
        \\  check <file> --json   type check with JSON output
        \\  lex <file>            tokenize a source file
        \\  parse <file>          parse and print AST
        \\  version               print version
        \\  help                  show this message
        \\
    , .{version});
}
