const std = @import("std");
const io = @import("../io.zig");
const pipeline = @import("../pipeline.zig");

pub fn run(allocator: std.mem.Allocator, path: []const u8, json: bool) !void {
    var checked = try pipeline.checkFile(allocator, path, json) orelse return;
    defer checked.deinit();

    if (json) {
        pipeline.renderDiagnostics(&checked.checker.diagnostics, true);
    } else if (checked.checker.diagnostics.hasErrors()) {
        pipeline.renderDiagnostics(&checked.checker.diagnostics, false);
    } else {
        io.write("ok\n", .{});
    }
}
