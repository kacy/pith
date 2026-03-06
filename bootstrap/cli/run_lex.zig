const std = @import("std");
const Lexer = @import("../lexer.zig").Lexer;
const pipeline = @import("../pipeline.zig");
const io = @import("../io.zig");

pub fn run(allocator: std.mem.Allocator, path: []const u8) !void {
    const source = pipeline.readSourceFile(allocator, path) orelse return;
    defer allocator.free(source);

    var lexer = try Lexer.init(source, allocator);
    defer lexer.deinit();

    while (true) {
        const token = try lexer.nextToken();
        switch (token.kind) {
            .newline => io.write("{s:<16}  \\n\n", .{@tagName(token.kind)}),
            .indent => io.write("{s:<16}  >>>\n", .{@tagName(token.kind)}),
            .dedent => io.write("{s:<16}  <<<\n", .{@tagName(token.kind)}),
            .eof => {
                io.write("{s:<16}  <eof>\n", .{@tagName(token.kind)});
                break;
            },
            else => {
                if (token.lexeme.len > 0) {
                    io.write("{s:<16}  {s}\n", .{ @tagName(token.kind), token.lexeme });
                } else {
                    io.write("{s:<16}\n", .{@tagName(token.kind)});
                }
            },
        }
    }

    if (lexer.diagnostics.hasErrors()) {
        pipeline.renderDiagnostics(&lexer.diagnostics, false);
    }
}
