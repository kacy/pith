const std = @import("std");
const errors = @import("../errors.zig");
const lint = @import("../lint.zig");
const pipeline = @import("../pipeline.zig");

pub fn run(allocator: std.mem.Allocator, path: []const u8, json: bool) !void {
    var checked = try pipeline.checkFile(allocator, path, json) orelse return;
    defer checked.deinit();

    if (checked.checker.diagnostics.hasErrors()) {
        pipeline.renderDiagnostics(&checked.checker.diagnostics, json);
        return;
    }

    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();

    var lint_diags = errors.DiagnosticList.init(arena.allocator(), checked.parsed.source);
    defer lint_diags.deinit();

    lint.lint(&checked.parsed.parse_result.module, &lint_diags, checked.parsed.source);

    if (lint_diags.diagnostics.items.len > 0) {
        pipeline.renderDiagnostics(&lint_diags, json);
        if (lint_diags.hasErrors()) {
            std.process.exit(1);
        }
    }
}
