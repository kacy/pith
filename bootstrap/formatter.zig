// formatter — canonical source code formatter for forge
//
// uses a source-preserving approach: walks the token stream and copies
// source text between tokens verbatim, only normalizing whitespace
// around operators, delimiters, and between tokens on the same line.
//
// this preserves comments and string content exactly, since the
// formatter works with the original source text rather than trying
// to reconstruct it from tokens.
//
// canonical rules:
//   1. 4-space indentation (already enforced by lexer)
//   2. single space around binary operators
//   3. no space before colon, space after (in type annotations)
//   4. no trailing whitespace on lines
//   5. single blank line between top-level declarations (max)
//   6. single space after # in comments
//   7. trailing newline at end of file
//   8. no space inside parens/brackets
//   9. single space after commas

const std = @import("std");
const lexer_mod = @import("lexer.zig");
const TokenKind = lexer_mod.TokenKind;
const Token = lexer_mod.Token;

/// format forge source code, returning a new string with canonical formatting.
/// works by scanning source lines and normalizing whitespace patterns.
pub fn format(allocator: std.mem.Allocator, source: []const u8) ![]const u8 {
    var output: std.ArrayList(u8) = .empty;
    defer output.deinit(allocator);

    var consecutive_blank: u32 = 0;
    var lines = std.mem.splitScalar(u8, source, '\n');

    while (lines.next()) |raw_line| {
        // trim trailing whitespace
        const line = std.mem.trimRight(u8, raw_line, " \t\r");

        // handle blank lines — allow at most one
        if (line.len == 0) {
            consecutive_blank += 1;
            if (consecutive_blank <= 1) {
                try output.append(allocator, '\n');
            }
            continue;
        }
        consecutive_blank = 0;

        // detect if line is a comment
        const trimmed = std.mem.trimLeft(u8, line, " ");
        if (trimmed.len > 0 and trimmed[0] == '#') {
            // preserve indentation, normalize comment spacing
            const indent_len = line.len - trimmed.len;
            try output.appendSlice(allocator, line[0..indent_len]);
            try formatComment(&output, allocator, trimmed);
            try output.append(allocator, '\n');
            continue;
        }

        // for code lines, normalize spacing
        try formatCodeLine(&output, allocator, line);
        try output.append(allocator, '\n');
    }

    // ensure exactly one trailing newline
    while (output.items.len > 1 and
        output.items[output.items.len - 1] == '\n' and
        output.items[output.items.len - 2] == '\n')
    {
        _ = output.pop();
    }

    // ensure at least one trailing newline
    if (output.items.len > 0 and output.items[output.items.len - 1] != '\n') {
        try output.append(allocator, '\n');
    }

    return try allocator.dupe(u8, output.items);
}

/// normalize a comment line: ensure single space after #
fn formatComment(output: *std.ArrayList(u8), allocator: std.mem.Allocator, comment: []const u8) !void {
    if (comment.len == 1 and comment[0] == '#') {
        try output.append(allocator, '#');
        return;
    }
    try output.append(allocator, '#');
    if (comment.len >= 2 and comment[1] != ' ') {
        try output.append(allocator, ' ');
        try output.appendSlice(allocator, comment[1..]);
    } else {
        try output.appendSlice(allocator, comment[1..]);
    }
}

/// format a code line by normalizing operator spacing.
/// preserves string contents exactly.
fn formatCodeLine(output: *std.ArrayList(u8), allocator: std.mem.Allocator, line: []const u8) !void {
    var i: usize = 0;
    var in_string = false;
    var brace_depth: u32 = 0; // for tracking interpolation inside strings

    // preserve leading whitespace (indentation)
    while (i < line.len and (line[i] == ' ' or line[i] == '\t')) {
        try output.append(allocator, line[i]);
        i += 1;
    }

    while (i < line.len) {
        const ch = line[i];

        // string handling — preserve contents exactly
        if (ch == '"' and !in_string) {
            in_string = true;
            try output.append(allocator, ch);
            i += 1;
            continue;
        }
        if (in_string) {
            if (ch == '\\' and i + 1 < line.len) {
                // escape sequence — copy both chars
                try output.append(allocator, ch);
                try output.append(allocator, line[i + 1]);
                i += 2;
                continue;
            }
            if (ch == '{') {
                brace_depth += 1;
                try output.append(allocator, ch);
                i += 1;
                continue;
            }
            if (ch == '}' and brace_depth > 0) {
                brace_depth -= 1;
                try output.append(allocator, ch);
                i += 1;
                continue;
            }
            if (ch == '"' and brace_depth == 0) {
                in_string = false;
                try output.append(allocator, ch);
                i += 1;
                continue;
            }
            try output.append(allocator, ch);
            i += 1;
            continue;
        }

        // inline comment — normalize and copy rest
        if (ch == '#') {
            // ensure space before comment
            trimTrailingSpaces(output);
            try output.append(allocator, ' ');
            try formatComment(output, allocator, line[i..]);
            break;
        }

        // two-character operators
        if (i + 1 < line.len) {
            const next = line[i + 1];
            const two = [2]u8{ ch, next };

            // := — space around
            if (two[0] == ':' and two[1] == '=') {
                ensureSpace(output, allocator);
                try output.appendSlice(allocator, ":= ");
                i += 2;
                skipSpaces(line, &i);
                continue;
            }

            // ==, !=, <=, >=, +=, -=, *=, /=, =>, ->  — space around
            if ((two[0] == '=' and two[1] == '=') or
                (two[0] == '!' and two[1] == '=') or
                (two[0] == '<' and two[1] == '=') or
                (two[0] == '>' and two[1] == '=') or
                (two[0] == '+' and two[1] == '=') or
                (two[0] == '-' and two[1] == '=') or
                (two[0] == '*' and two[1] == '=') or
                (two[0] == '/' and two[1] == '=') or
                (two[0] == '=' and two[1] == '>') or
                (two[0] == '-' and two[1] == '>'))
            {
                ensureSpace(output, allocator);
                try output.append(allocator, two[0]);
                try output.append(allocator, two[1]);
                try output.append(allocator, ' ');
                i += 2;
                skipSpaces(line, &i);
                continue;
            }
        }

        // single-char binary operators: + - * / % = < > |
        if (ch == '+' or ch == '*' or ch == '/' or ch == '%') {
            ensureSpace(output, allocator);
            try output.append(allocator, ch);
            try output.append(allocator, ' ');
            i += 1;
            skipSpaces(line, &i);
            continue;
        }

        // minus is tricky — could be binary or unary
        // if preceded by an operand (identifier, number, ), ]), it's binary
        if (ch == '-') {
            if (isPrecededByOperand(output)) {
                ensureSpace(output, allocator);
                try output.append(allocator, '-');
                try output.append(allocator, ' ');
                i += 1;
                skipSpaces(line, &i);
                continue;
            }
            // else it's unary — no space after
            try output.append(allocator, '-');
            i += 1;
            continue;
        }

        // = (assignment, not ==)
        if (ch == '=') {
            ensureSpace(output, allocator);
            try output.append(allocator, '=');
            try output.append(allocator, ' ');
            i += 1;
            skipSpaces(line, &i);
            continue;
        }

        // < and > as comparison (not part of <=, >=)
        if (ch == '<' or ch == '>') {
            ensureSpace(output, allocator);
            try output.append(allocator, ch);
            try output.append(allocator, ' ');
            i += 1;
            skipSpaces(line, &i);
            continue;
        }

        // | (pipe operator)
        if (ch == '|') {
            ensureSpace(output, allocator);
            try output.append(allocator, '|');
            try output.append(allocator, ' ');
            i += 1;
            skipSpaces(line, &i);
            continue;
        }

        // colon — no space before, space after (unless followed by newline or is block start)
        if (ch == ':') {
            trimTrailingSpaces(output);
            try output.append(allocator, ':');
            i += 1;
            // space after unless at end of line (block start) or before newline
            if (i < line.len and line[i] != '\n') {
                skipSpaces(line, &i);
                if (i < line.len) {
                    try output.append(allocator, ' ');
                }
            }
            continue;
        }

        // comma — no space before, space after
        if (ch == ',') {
            trimTrailingSpaces(output);
            try output.append(allocator, ',');
            i += 1;
            skipSpaces(line, &i);
            if (i < line.len) {
                try output.append(allocator, ' ');
            }
            continue;
        }

        // dot — no spaces around
        if (ch == '.') {
            trimTrailingSpaces(output);
            try output.append(allocator, '.');
            i += 1;
            skipSpaces(line, &i);
            continue;
        }

        // opening delimiters — no space after
        if (ch == '(' or ch == '[' or ch == '{') {
            try output.append(allocator, ch);
            i += 1;
            skipSpaces(line, &i);
            continue;
        }

        // closing delimiters — no space before
        if (ch == ')' or ch == ']' or ch == '}') {
            trimTrailingSpaces(output);
            try output.append(allocator, ch);
            i += 1;
            continue;
        }

        // regular character — just copy
        try output.append(allocator, ch);
        i += 1;
    }
}

// -- helpers --

fn ensureSpace(output: *std.ArrayList(u8), allocator: std.mem.Allocator) void {
    if (output.items.len > 0 and output.items[output.items.len - 1] != ' ' and
        output.items[output.items.len - 1] != '(' and output.items[output.items.len - 1] != '[')
    {
        output.append(allocator, ' ') catch {};
    }
}

fn trimTrailingSpaces(output: *std.ArrayList(u8)) void {
    while (output.items.len > 0 and output.items[output.items.len - 1] == ' ') {
        _ = output.pop();
    }
}

fn skipSpaces(line: []const u8, i: *usize) void {
    while (i.* < line.len and line[i.*] == ' ') {
        i.* += 1;
    }
}

fn isPrecededByOperand(output: *const std.ArrayList(u8)) bool {
    if (output.items.len == 0) return false;
    var idx = output.items.len;
    // skip trailing spaces
    while (idx > 0 and output.items[idx - 1] == ' ') idx -= 1;
    if (idx == 0) return false;
    const last = output.items[idx - 1];
    return std.ascii.isAlphanumeric(last) or last == ')' or last == ']' or last == '_' or last == '?' or last == '!';
}

// -- tests --

const testing = std.testing;

test "format: simple function unchanged" {
    const source = "fn add(a: Int, b: Int) -> Int:\n    return a + b\n";
    const result = try format(testing.allocator, source);
    defer testing.allocator.free(result);
    try testing.expectEqualStrings(source, result);
}

test "format: trailing newline added" {
    const source = "x := 42";
    const result = try format(testing.allocator, source);
    defer testing.allocator.free(result);
    try testing.expectEqualStrings("x := 42\n", result);
}

test "format: multiple blank lines collapsed" {
    const source = "x := 1\n\n\n\ny := 2\n";
    const result = try format(testing.allocator, source);
    defer testing.allocator.free(result);
    try testing.expectEqualStrings("x := 1\n\ny := 2\n", result);
}

test "format: trailing whitespace removed" {
    const source = "x := 42   \n";
    const result = try format(testing.allocator, source);
    defer testing.allocator.free(result);
    try testing.expectEqualStrings("x := 42\n", result);
}

test "format: comment space normalized" {
    const source = "#no space\n";
    const result = try format(testing.allocator, source);
    defer testing.allocator.free(result);
    try testing.expectEqualStrings("# no space\n", result);
}

test "format: comment with space preserved" {
    const source = "# already spaced\n";
    const result = try format(testing.allocator, source);
    defer testing.allocator.free(result);
    try testing.expectEqualStrings("# already spaced\n", result);
}

test "format: inline comment normalized" {
    const source = "x := 42 #comment\n";
    const result = try format(testing.allocator, source);
    defer testing.allocator.free(result);
    try testing.expectEqualStrings("x := 42 # comment\n", result);
}

test "format: operator spacing normalized" {
    const source = "x :=1+2\n";
    const result = try format(testing.allocator, source);
    defer testing.allocator.free(result);
    try testing.expectEqualStrings("x := 1 + 2\n", result);
}

test "format: string content preserved" {
    const source = "x := \"hello, {name}!\"\n";
    const result = try format(testing.allocator, source);
    defer testing.allocator.free(result);
    try testing.expectEqualStrings("x := \"hello, {name}!\"\n", result);
}
