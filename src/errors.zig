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

/// stable error codes for machine-readable diagnostics.
/// category-based: E0xx = lexer, E1xx = parser, E2xx = type checker.
/// codes are stable across versions — never reuse a retired code.
pub const ErrorCode = enum {
    // -- lexer (E0xx) --
    E001, // unexpected character
    E002, // unterminated string
    E003, // invalid escape sequence
    E004, // invalid number literal
    E005, // indentation error
    E006, // string interpolation error

    // -- parser (E1xx) --
    E100, // unexpected token
    E101, // expected expression
    E102, // expected type annotation
    E103, // expected identifier
    E104, // expected block

    // -- type checker (E2xx) --
    E200, // type mismatch
    E201, // undefined variable
    E202, // undefined type
    E203, // duplicate definition
    E204, // non-exhaustive match
    E205, // unreachable pattern
    E206, // missing return type
    E207, // wrong number of arguments
    E208, // not callable
    E209, // field not found
    E210, // not a struct type
    E211, // not an enum type
    E212, // unknown variant
    E213, // wrong field count in pattern
    E214, // break/continue outside loop
    E215, // match arm type mismatch
    E216, // assignment to immutable binding
    E217, // invalid operand types
    E218, // match guard must be Bool

    pub fn label(self: ErrorCode) []const u8 {
        return @tagName(self);
    }
};

/// a compiler diagnostic — an error, warning, or note with location info.
pub const Diagnostic = struct {
    severity: Severity,
    location: Location,
    message: []const u8,

    /// optional suggestion for how to fix the error.
    fix: ?[]const u8 = null,

    /// optional stable error code for machine-readable output.
    code: ?ErrorCode = null,
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

    /// record an error diagnostic at the given source location.
    pub fn addError(self: *DiagnosticList, location: Location, message: []const u8) !void {
        try self.addErrorWithFix(location, message, null);
    }

    /// record an error diagnostic with an optional fix suggestion.
    pub fn addErrorWithFix(self: *DiagnosticList, location: Location, message: []const u8, fix: ?[]const u8) !void {
        try self.diagnostics.append(self.allocator, .{
            .severity = .@"error",
            .location = location,
            .message = message,
            .fix = fix,
        });
    }

    /// record an error diagnostic with a stable error code.
    pub fn addCodedError(self: *DiagnosticList, code: ErrorCode, location: Location, message: []const u8) !void {
        try self.addCodedErrorWithFix(code, location, message, null);
    }

    /// record an error diagnostic with a stable error code and fix suggestion.
    pub fn addCodedErrorWithFix(self: *DiagnosticList, code: ErrorCode, location: Location, message: []const u8, fix: ?[]const u8) !void {
        try self.diagnostics.append(self.allocator, .{
            .severity = .@"error",
            .location = location,
            .message = message,
            .fix = fix,
            .code = code,
        });
    }

    pub fn hasErrors(self: *const DiagnosticList) bool {
        return self.errorCount() > 0;
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

    /// render all diagnostics as a JSON array. outputs valid JSON even
    /// when there are no diagnostics (empty array). designed for agents
    /// that parse `forge check --json` output.
    pub fn renderJson(self: *const DiagnosticList, writer: anytype) !void {
        try writer.writeAll("[");
        for (self.diagnostics.items, 0..) |d, i| {
            if (i > 0) try writer.writeAll(",");
            try renderJsonDiagnostic(d, writer);
        }
        try writer.writeAll("]\n");
    }
};

/// render a single diagnostic with source context and underline.
fn renderDiagnostic(d: Diagnostic, source: []const u8, writer: anytype) !void {
    // header: severity and message (with error code if present)
    if (d.code) |code| {
        try writer.print("{s}[{s}]: {s}\n", .{ d.severity.label(), code.label(), d.message });
    } else {
        try writer.print("{s}: {s}\n", .{ d.severity.label(), d.message });
    }

    // source line + underline
    if (d.location.offset < source.len) {
        const line_start = findLineStart(source, d.location.offset);
        const line_end = findLineEnd(source, d.location.offset);
        const source_line = source[line_start..line_end];

        try writer.print("  {d} | {s}\n", .{ d.location.line + 1, source_line });

        // underline: spaces for margin + caret for the error location
        // margin format is "  N | " — digits for the line number, then
        // space-pipe-space (3 chars), plus a leading space (1 char) = +4
        const margin_width = digitCount(d.location.line + 1) + 4;
        for (0..margin_width + d.location.column) |_| try writer.writeByte(' ');

        const underline_len = @max(d.location.length, 1);
        for (0..underline_len) |_| try writer.writeByte('^');
        try writer.print("\n", .{});
    }

    // fix suggestion
    if (d.fix) |fix| {
        try writer.print("  fix: {s}\n", .{fix});
    }

    try writer.print("\n", .{});
}

/// render a single diagnostic as a JSON object.
fn renderJsonDiagnostic(d: Diagnostic, writer: anytype) !void {
    try writer.writeAll("{");

    // severity
    try writer.writeAll("\"severity\":\"");
    try writer.writeAll(d.severity.label());
    try writer.writeAll("\"");

    // code (null if not present)
    try writer.writeAll(",\"code\":");
    if (d.code) |code| {
        try writer.writeAll("\"");
        try writer.writeAll(code.label());
        try writer.writeAll("\"");
    } else {
        try writer.writeAll("null");
    }

    // message
    try writer.writeAll(",\"message\":\"");
    try writeJsonEscaped(writer, d.message);
    try writer.writeAll("\"");

    // location
    try writer.print(",\"line\":{d},\"col\":{d}", .{ d.location.line + 1, d.location.column + 1 });

    // fix (null if not present)
    try writer.writeAll(",\"fix\":");
    if (d.fix) |fix| {
        try writer.writeAll("\"");
        try writeJsonEscaped(writer, fix);
        try writer.writeAll("\"");
    } else {
        try writer.writeAll("null");
    }

    try writer.writeAll("}");
}

/// write a string with JSON escaping (backslash, quotes, control chars).
fn writeJsonEscaped(writer: anytype, s: []const u8) !void {
    for (s) |c| {
        switch (c) {
            '"' => try writer.writeAll("\\\""),
            '\\' => try writer.writeAll("\\\\"),
            '\n' => try writer.writeAll("\\n"),
            '\r' => try writer.writeAll("\\r"),
            '\t' => try writer.writeAll("\\t"),
            else => {
                if (c < 0x20) {
                    // other control characters as unicode escapes
                    try writer.print("\\u{x:0>4}", .{c});
                } else {
                    try writer.writeByte(c);
                }
            },
        }
    }
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

test "error code label" {
    try std.testing.expectEqualStrings("E200", ErrorCode.E200.label());
    try std.testing.expectEqualStrings("E204", ErrorCode.E204.label());
}

test "coded error renders with code in header" {
    var list = DiagnosticList.init(std.testing.allocator, "x := 42");
    defer list.deinit();

    try list.addCodedError(.E200, .{ .line = 0, .column = 0, .offset = 0, .length = 1 }, "type mismatch");

    var output: std.ArrayList(u8) = .empty;
    defer output.deinit(std.testing.allocator);
    try list.render(output.writer(std.testing.allocator));

    const rendered = output.items;
    try std.testing.expect(std.mem.indexOf(u8, rendered, "error[E200]: type mismatch") != null);
}

test "uncoded error renders without brackets" {
    var list = DiagnosticList.init(std.testing.allocator, "x := 42");
    defer list.deinit();

    try list.addError(.{ .line = 0, .column = 0, .offset = 0, .length = 1 }, "some error");

    var output: std.ArrayList(u8) = .empty;
    defer output.deinit(std.testing.allocator);
    try list.render(output.writer(std.testing.allocator));

    const rendered = output.items;
    // should be "error: some error", not "error[null]: some error"
    try std.testing.expect(std.mem.indexOf(u8, rendered, "error: some error") != null);
    try std.testing.expect(std.mem.indexOf(u8, rendered, "[") == null);
}

test "renderJson: empty diagnostics" {
    var list = DiagnosticList.init(std.testing.allocator, "");
    defer list.deinit();

    var output: std.ArrayList(u8) = .empty;
    defer output.deinit(std.testing.allocator);
    try list.renderJson(output.writer(std.testing.allocator));

    try std.testing.expectEqualStrings("[]\n", output.items);
}

test "renderJson: single coded error" {
    var list = DiagnosticList.init(std.testing.allocator, "x := 42");
    defer list.deinit();

    try list.addCodedErrorWithFix(
        .E204,
        .{ .line = 9, .column = 4, .offset = 50, .length = 5 },
        "non-exhaustive match",
        "add a wildcard '_' catch-all",
    );

    var output: std.ArrayList(u8) = .empty;
    defer output.deinit(std.testing.allocator);
    try list.renderJson(output.writer(std.testing.allocator));

    const json = output.items;
    // check key fields are present and correctly formatted
    try std.testing.expect(std.mem.indexOf(u8, json, "\"severity\":\"error\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"code\":\"E204\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"message\":\"non-exhaustive match\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"line\":10") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"col\":5") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"fix\":\"add a wildcard") != null);
}

test "renderJson: uncoded error has null code" {
    var list = DiagnosticList.init(std.testing.allocator, "x");
    defer list.deinit();

    try list.addError(Location.zero, "test error");

    var output: std.ArrayList(u8) = .empty;
    defer output.deinit(std.testing.allocator);
    try list.renderJson(output.writer(std.testing.allocator));

    const json = output.items;
    try std.testing.expect(std.mem.indexOf(u8, json, "\"code\":null") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"fix\":null") != null);
}

test "writeJsonEscaped: special characters" {
    var output: std.ArrayList(u8) = .empty;
    defer output.deinit(std.testing.allocator);
    const w = output.writer(std.testing.allocator);

    try writeJsonEscaped(&w, "hello \"world\"\nnew\\line");

    try std.testing.expectEqualStrings("hello \\\"world\\\"\\nnew\\\\line", output.items);
}
