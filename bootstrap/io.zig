// io — shared I/O helpers
//
// zig's buffered writer API requires a buffer + writer + flush for every
// print. these helpers cut that ceremony down to a single call.

const std = @import("std");

/// buffer size for stdout/stderr writers. 8 KiB is enough for any single
/// print in the compiler — diagnostics, AST nodes, usage text, etc.
pub const write_buf_size = 8192;

pub fn writeChecked(comptime fmt: []const u8, args: anytype) !void {
    var buf: [write_buf_size]u8 = undefined;
    var w = std.fs.File.stdout().writer(&buf);
    const out = &w.interface;
    try out.print(fmt, args);
    try out.flush();
}

pub fn writeErrChecked(comptime fmt: []const u8, args: anytype) !void {
    var buf: [write_buf_size]u8 = undefined;
    var w = std.fs.File.stderr().writer(&buf);
    const out = &w.interface;
    try out.print(fmt, args);
    try out.flush();
}

pub fn write(comptime fmt: []const u8, args: anytype) void {
    writeChecked(fmt, args) catch {};
}

pub fn writeErr(comptime fmt: []const u8, args: anytype) void {
    writeErrChecked(fmt, args) catch {};
}
