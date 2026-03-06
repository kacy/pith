// main — CLI entry point for the forge compiler

const std = @import("std");
const cli = @import("cli.zig");

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
    _ = @import("pipeline.zig");
    _ = @import("cli.zig");
}

pub fn main() !void {
    var gpa: std.heap.GeneralPurposeAllocator(.{}) = .init;
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    const args = std.process.argsAlloc(allocator) catch {
        cli.printUsage();
        return;
    };
    defer std.process.argsFree(allocator, args);

    const request = cli.parseArgs(allocator, args) catch |err| {
        switch (err) {
            error.MissingCommand => cli.printUsage(),
            error.MissingFilePath => {},
            error.UnknownCommand => {},
            else => return err,
        }
        return;
    };

    const exit_code = try cli.run(request, allocator);
    if (exit_code != 0) {
        std.process.exit(exit_code);
    }
}
