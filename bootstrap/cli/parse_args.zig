const std = @import("std");
const io = @import("../io.zig");
const Command = @import("command.zig").Command;

pub const ParseError = error{
    InvalidArgs,
    OutOfMemory,
    UnknownCommand,
};

pub const FileCommand = struct {
    path: []const u8,
    json: bool,
};

pub const FmtCommand = struct {
    path: []const u8,
    check_only: bool,
};

pub const RunCommand = struct {
    path: []const u8,
    extra_args: []const []const u8,
};

pub const Request = union(Command) {
    build: FileCommand,
    check: FileCommand,
    fmt: FmtCommand,
    help: void,
    lex: []const u8,
    lint: FileCommand,
    parse: []const u8,
    run: RunCommand,
    @"test": FileCommand,
    version: void,

    pub fn deinit(self: *Request, allocator: std.mem.Allocator) void {
        switch (self.*) {
            .run => |run_cmd| allocator.free(run_cmd.extra_args),
            else => {},
        }
    }
};

pub fn parse(allocator: std.mem.Allocator, args: []const []const u8) ParseError!Request {
    if (args.len == 0) return .{ .help = {} };

    const command = Command.fromString(args[0]) orelse {
        io.writeErr("error: unknown command '{s}'\n", .{args[0]});
        return error.UnknownCommand;
    };

    return switch (command) {
        .build => .{ .build = try parseFileCommand("build", args[1..]) },
        .check => .{ .check = try parseFileCommand("check", args[1..]) },
        .fmt => .{ .fmt = try parseFmtCommand(args[1..]) },
        .help => .{ .help = {} },
        .lex => .{ .lex = try parseRequiredPath("lex", args[1..]) },
        .lint => .{ .lint = try parseFileCommand("lint", args[1..]) },
        .parse => .{ .parse = try parseRequiredPath("parse", args[1..]) },
        .run => .{ .run = try parseRunCommand(allocator, args[1..]) },
        .@"test" => .{ .@"test" = try parseFileCommand("test", args[1..]) },
        .version => .{ .version = {} },
    };
}

fn parseFileCommand(command_name: []const u8, args: []const []const u8) ParseError!FileCommand {
    const path = try parseRequiredPath(command_name, args);
    var json = false;
    for (args[1..]) |arg| {
        if (std.mem.eql(u8, arg, "--json")) json = true;
    }
    return .{
        .path = path,
        .json = json,
    };
}

fn parseFmtCommand(args: []const []const u8) ParseError!FmtCommand {
    var check_only = false;
    var path: ?[]const u8 = null;

    for (args) |arg| {
        if (std.mem.eql(u8, arg, "--check")) {
            check_only = true;
            continue;
        }

        if (path == null) {
            path = arg;
        }
    }

    return .{
        .path = path orelse {
            io.writeErr("error: forge fmt requires a file path\n", .{});
            return error.InvalidArgs;
        },
        .check_only = check_only,
    };
}

fn parseRunCommand(allocator: std.mem.Allocator, args: []const []const u8) ParseError!RunCommand {
    const path = try parseRequiredPath("run", args);
    return .{
        .path = path,
        .extra_args = try allocator.dupe([]const u8, args[1..]),
    };
}

fn parseRequiredPath(command_name: []const u8, args: []const []const u8) ParseError![]const u8 {
    if (args.len == 0) {
        io.writeErr("error: forge {s} requires a file path\n", .{command_name});
        return error.InvalidArgs;
    }

    return args[0];
}

test "parse run command keeps trailing argv" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var request = try parse(arena.allocator(), &.{ "run", "examples/hello.fg", "--", "a", "b" });
    defer request.deinit(arena.allocator());

    switch (request) {
        .run => |run_cmd| {
            try std.testing.expectEqualStrings("examples/hello.fg", run_cmd.path);
            try std.testing.expectEqual(@as(usize, 3), run_cmd.extra_args.len);
            try std.testing.expectEqualStrings("--", run_cmd.extra_args[0]);
        },
        else => return error.UnexpectedCommand,
    }
}

test "parse fmt command recognizes check flag" {
    var request = try parse(std.testing.allocator, &.{ "fmt", "--check", "bootstrap/main.zig" });
    defer request.deinit(std.testing.allocator);

    switch (request) {
        .fmt => |fmt_cmd| {
            try std.testing.expect(fmt_cmd.check_only);
            try std.testing.expectEqualStrings("bootstrap/main.zig", fmt_cmd.path);
        },
        else => return error.UnexpectedCommand,
    }
}
