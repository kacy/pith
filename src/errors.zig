// errors — source locations, diagnostics, and error formatting
//
// provides structured error reporting with source context,
// fix suggestions, and both human-readable and JSON output.

const std = @import("std");

/// a position in source code.
pub const Location = struct {
    line: u32,
    column: u32,
    offset: u32,
    length: u32,

    pub const zero = Location{ .line = 0, .column = 0, .offset = 0, .length = 0 };

    /// create a location spanning from start to end.
    pub fn span(start: Location, end: Location) Location {
        return .{
            .line = start.line,
            .column = start.column,
            .offset = start.offset,
            .length = end.offset + end.length - start.offset,
        };
    }
};

pub const Severity = enum {
    @"error",
    warning,
    note,

    pub fn label(self: Severity) []const u8 {
        return switch (self) {
            .@"error" => "error",
            .warning => "warning",
            .note => "note",
        };
    }
};

/// a compiler diagnostic — an error, warning, or note with location info.
pub const Diagnostic = struct {
    severity: Severity,
    location: Location,
    message: []const u8,

    /// optional suggestion for how to fix the error.
    fix: ?[]const u8 = null,
};

/// accumulates diagnostics during a compilation phase.
/// the compiler never stops at the first error — it collects
/// as many as it can and reports them all.
pub const DiagnosticList = struct {
    diagnostics: std.ArrayList(Diagnostic),
    allocator: std.mem.Allocator,
    source: []const u8,

    pub fn init(allocator: std.mem.Allocator, source: []const u8) DiagnosticList {
        return .{
            .diagnostics = .empty,
            .allocator = allocator,
            .source = source,
        };
    }

    pub fn deinit(self: *DiagnosticList) void {
        self.diagnostics.deinit(self.allocator);
    }

    pub fn addError(self: *DiagnosticList, location: Location, message: []const u8) !void {
        try self.diagnostics.append(self.allocator, .{
            .severity = .@"error",
            .location = location,
            .message = message,
        });
    }

    pub fn addErrorWithFix(self: *DiagnosticList, location: Location, message: []const u8, fix: []const u8) !void {
        try self.diagnostics.append(self.allocator, .{
            .severity = .@"error",
            .location = location,
            .message = message,
            .fix = fix,
        });
    }

    pub fn hasErrors(self: *const DiagnosticList) bool {
        for (self.diagnostics.items) |d| {
            if (d.severity == .@"error") return true;
        }
        return false;
    }

    pub fn errorCount(self: *const DiagnosticList) usize {
        var count: usize = 0;
        for (self.diagnostics.items) |d| {
            if (d.severity == .@"error") count += 1;
        }
        return count;
    }

    /// render all diagnostics as human-readable text.
    pub fn render(self: *const DiagnosticList, writer: anytype) !void {
        for (self.diagnostics.items) |d| {
            try renderDiagnostic(d, self.source, writer);
        }
    }
};

/// render a single diagnostic with source context and underline.
fn renderDiagnostic(d: Diagnostic, source: []const u8, writer: anytype) !void {
    // header: severity and message
    try writer.print("{s}: {s}\n", .{ d.severity.label(), d.message });

    // source line + underline
    if (d.location.offset < source.len) {
        const line_start = findLineStart(source, d.location.offset);
        const line_end = findLineEnd(source, d.location.offset);
        const source_line = source[line_start..line_end];

        try writer.print("  {d} | {s}\n", .{ d.location.line + 1, source_line });

        // underline: spaces for margin + caret for the error location
        const margin_width = digitCount(d.location.line + 1) + 4; // " N | "
        try writeSpaces(writer, margin_width + d.location.column);

        const underline_len = @max(d.location.length, 1);
        try writeChars(writer, '^', underline_len);
        try writer.print("\n", .{});
    }

    // fix suggestion
    if (d.fix) |fix| {
        try writer.print("  fix: {s}\n", .{fix});
    }

    try writer.print("\n", .{});
}

fn findLineStart(source: []const u8, offset: u32) u32 {
    var i: u32 = offset;
    while (i > 0) : (i -= 1) {
        if (source[i - 1] == '\n') return i;
    }
    return 0;
}

fn findLineEnd(source: []const u8, offset: u32) u32 {
    var i: u32 = offset;
    while (i < source.len) : (i += 1) {
        if (source[i] == '\n') return i;
    }
    return @intCast(source.len);
}

fn digitCount(n: u32) u32 {
    if (n == 0) return 1;
    var count: u32 = 0;
    var v = n;
    while (v > 0) : (v /= 10) {
        count += 1;
    }
    return count;
}

fn writeSpaces(writer: anytype, count: u32) !void {
    var i: u32 = 0;
    while (i < count) : (i += 1) {
        try writer.print(" ", .{});
    }
}

fn writeChars(writer: anytype, ch: u8, count: u32) !void {
    var i: u32 = 0;
    while (i < count) : (i += 1) {
        try writer.writeByte(ch);
    }
}

// -- tests --

test "location span" {
    const start = Location{ .line = 1, .column = 5, .offset = 10, .length = 3 };
    const end = Location{ .line = 1, .column = 12, .offset = 17, .length = 2 };
    const s = Location.span(start, end);

    try std.testing.expectEqual(@as(u32, 1), s.line);
    try std.testing.expectEqual(@as(u32, 5), s.column);
    try std.testing.expectEqual(@as(u32, 10), s.offset);
    try std.testing.expectEqual(@as(u32, 9), s.length);
}

test "diagnostic list tracks errors" {
    var list = DiagnosticList.init(std.testing.allocator, "x := 42");
    defer list.deinit();

    try list.addError(Location.zero, "test error");
    try std.testing.expect(list.hasErrors());
    try std.testing.expectEqual(@as(usize, 1), list.errorCount());
}

test "diagnostic list with no errors" {
    var list = DiagnosticList.init(std.testing.allocator, "");
    defer list.deinit();

    try std.testing.expect(!list.hasErrors());
}

test "diagnostic renders with source context" {
    var list = DiagnosticList.init(std.testing.allocator, "x := @bad");
    defer list.deinit();

    try list.addError(
        .{ .line = 0, .column = 5, .offset = 5, .length = 1 },
        "unexpected character: @",
    );

    var output: std.ArrayList(u8) = .empty;
    defer output.deinit(std.testing.allocator);
    try list.render(output.writer(std.testing.allocator));

    const rendered = output.items;
    try std.testing.expect(std.mem.indexOf(u8, rendered, "error: unexpected character: @") != null);
    try std.testing.expect(std.mem.indexOf(u8, rendered, "x := @bad") != null);
    try std.testing.expect(std.mem.indexOf(u8, rendered, "^") != null);
}
