const std = @import("std");
const pipeline = @import("../pipeline.zig");
const printer = @import("../printer.zig");

pub fn run(allocator: std.mem.Allocator, path: []const u8) !void {
    var parsed = try pipeline.parseFile(allocator, path, false) orelse return;
    defer parsed.deinit();

    printer.printModule(parsed.parse_result.module);
}
