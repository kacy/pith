const std = @import("std");
const formatter = @import("../formatter.zig");
const pipeline = @import("../pipeline.zig");
const io = @import("../io.zig");
const build_support = @import("../build_support.zig");

pub fn run(allocator: std.mem.Allocator, path: []const u8, check_only: bool) !void {
    const source = pipeline.readSourceFile(allocator, path) orelse return;
    defer allocator.free(source);

    const formatted = formatter.format(allocator, source) catch {
        io.writeErr("error: formatting failed (out of memory)\n", .{});
        return;
    };
    defer allocator.free(formatted);

    if (check_only) {
        if (!std.mem.eql(u8, source, formatted)) {
            io.write("{s}\n", .{path});
            std.process.exit(1);
        }
        return;
    }

    if (!std.mem.eql(u8, source, formatted)) {
        build_support.writeGeneratedC(path, formatted) catch |err| {
            io.writeErr("error: could not write '{s}': {}\n", .{ path, err });
        };
    }
}
