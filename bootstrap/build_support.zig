const std = @import("std");
const io = @import("io.zig");

const runtime_header = @embedFile("forge_runtime.h");

pub const ArtifactKind = enum {
    build,
    @"test",
};

pub const BuildPaths = struct {
    allocator: std.mem.Allocator,
    build_dir: []const u8,
    header_path: []const u8,
    c_path: []const u8,
    out_path: []const u8,

    pub fn init(allocator: std.mem.Allocator, source_path: []const u8, kind: ArtifactKind) !BuildPaths {
        const stem = stripExtension(std.fs.path.basename(source_path));
        const dir = std.fs.path.dirname(source_path) orelse ".";
        const build_dir = try std.fs.path.join(allocator, &.{ dir, ".forge-build" });
        errdefer allocator.free(build_dir);

        const header_path = try std.fs.path.join(allocator, &.{ build_dir, "forge_runtime.h" });
        errdefer allocator.free(header_path);

        const c_suffix = switch (kind) {
            .build => ".c",
            .@"test" => "_test.c",
        };
        const c_filename = try std.fmt.allocPrint(allocator, "{s}{s}", .{ stem, c_suffix });
        defer allocator.free(c_filename);

        const c_path = try std.fs.path.join(allocator, &.{ build_dir, c_filename });
        errdefer allocator.free(c_path);

        const out_name = switch (kind) {
            .build => try allocator.dupe(u8, stem),
            .@"test" => try std.fmt.allocPrint(allocator, "{s}_test", .{stem}),
        };
        errdefer allocator.free(out_name);

        const out_path = try std.fs.path.join(allocator, &.{ dir, out_name });
        allocator.free(out_name);

        return .{
            .allocator = allocator,
            .build_dir = build_dir,
            .header_path = header_path,
            .c_path = c_path,
            .out_path = out_path,
        };
    }

    pub fn deinit(self: *BuildPaths) void {
        self.allocator.free(self.build_dir);
        self.allocator.free(self.header_path);
        self.allocator.free(self.c_path);
        self.allocator.free(self.out_path);
    }
};

pub fn ensureBuildDir(build_dir: []const u8) !void {
    try std.fs.cwd().makePath(build_dir);
}

pub fn writeRuntimeHeader(path: []const u8) !void {
    try writeFile(path, runtime_header);
}

pub fn writeGeneratedC(path: []const u8, content: []const u8) !void {
    try writeFile(path, content);
}

pub fn compileGeneratedC(allocator: std.mem.Allocator, paths: *const BuildPaths) !std.process.Child.RunResult {
    return std.process.Child.run(.{
        .allocator = allocator,
        .argv = &.{ "zig", "cc", "-w", "-o", paths.out_path, "-I", paths.build_dir, paths.c_path, "-lpthread", "-lm" },
    });
}

pub fn runBinary(allocator: std.mem.Allocator, argv: []const []const u8) !std.process.Child.RunResult {
    return std.process.Child.run(.{
        .allocator = allocator,
        .argv = argv,
    });
}

pub fn printCapturedOutput(stdout: []const u8, stderr: []const u8) void {
    if (stdout.len > 0) {
        var buf: [io.write_buf_size]u8 = undefined;
        var writer = std.fs.File.stdout().writer(&buf);
        writer.interface.writeAll(stdout) catch {};
        writer.interface.flush() catch {};
    }

    if (stderr.len > 0) {
        var buf: [io.write_buf_size]u8 = undefined;
        var writer = std.fs.File.stderr().writer(&buf);
        writer.interface.writeAll(stderr) catch {};
        writer.interface.flush() catch {};
    }
}

fn writeFile(path: []const u8, content: []const u8) !void {
    const file = try std.fs.cwd().createFile(path, .{});
    defer file.close();
    try file.writeAll(content);
}

fn stripExtension(filename: []const u8) []const u8 {
    if (std.mem.lastIndexOf(u8, filename, ".")) |dot| {
        return filename[0..dot];
    }
    return filename;
}
