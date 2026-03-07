// lint — convention enforcement for forge source code
//
// runs after type checking to access both AST structure and type
// information. each rule visits relevant AST nodes and emits
// diagnostics with E3xx codes.
//
// lint rules:
//   E300 — snake_case for functions and variables
//   E301 — PascalCase for types (structs, enums, interfaces, aliases)
//   E302 — unused variable
//   E304 — public function missing doc comment
//   E305 — indentation depth exceeds 4 levels

const std = @import("std");
const ast = @import("ast.zig");
const errors = @import("errors.zig");

const Location = errors.Location;
const ErrorCode = errors.ErrorCode;
const DiagnosticList = errors.DiagnosticList;

/// run all lint rules on a checked module. diagnostics are appended
/// to the provided list. source is used to detect doc comments.
pub fn lint(module: *const ast.Module, diagnostics: *DiagnosticList, source: []const u8) void {
    for (module.decls) |*decl| {
        lintDecl(decl, diagnostics, source);
    }
}

fn lintDecl(decl: *const ast.Decl, diagnostics: *DiagnosticList, source: []const u8) void {
    switch (decl.kind) {
        .fn_decl => |fn_d| {
            // E300: function name must be snake_case (skip main)
            if (!std.mem.eql(u8, fn_d.name, "main")) {
                checkSnakeCase(fn_d.name, decl.location, "function", diagnostics);
            }

            // E304: public function missing doc comment
            if (decl.is_pub) {
                checkDocComment(decl.location, source, fn_d.name, diagnostics);
            }

            // lint function body
            lintBlock(&fn_d.body, diagnostics, 1);

            // E302: unused variables in function body
            checkUnusedBindings(&fn_d.body, fn_d.params, diagnostics);
        },
        .struct_decl => |sd| {
            // E301: struct name must be PascalCase
            checkPascalCase(sd.name, decl.location, "struct", diagnostics);
        },
        .enum_decl => |ed| {
            // E301: enum name must be PascalCase
            checkPascalCase(ed.name, decl.location, "enum", diagnostics);
        },
        .interface_decl => |id| {
            // E301: interface name must be PascalCase
            checkPascalCase(id.name, decl.location, "interface", diagnostics);
        },
        .type_alias => |ta| {
            // E301: type alias must be PascalCase
            checkPascalCase(ta.name, decl.location, "type alias", diagnostics);
        },
        .impl_decl => |impl_d| {
            for (impl_d.methods) |*method| {
                // E300: method name must be snake_case
                checkSnakeCase(method.decl.name, method.location, "method", diagnostics);

                // E304: public method missing doc comment
                if (method.is_pub) {
                    checkDocComment(method.location, source, method.decl.name, diagnostics);
                }

                lintBlock(&method.decl.body, diagnostics, 1);
                checkUnusedBindings(&method.decl.body, method.decl.params, diagnostics);
            }
        },
        .binding => |b| {
            // E300: top-level binding must be snake_case
            checkSnakeCase(b.name, decl.location, "variable", diagnostics);
        },
        .test_decl => |td| {
            lintBlock(&td.body, diagnostics, 1);
        },
    }
}

// ---------------------------------------------------------------
// E300: snake_case for functions and variables
// ---------------------------------------------------------------

/// check that a name follows snake_case: lowercase letters, digits,
/// underscores. must start with a letter or underscore.
fn checkSnakeCase(name: []const u8, location: Location, kind: []const u8, diagnostics: *DiagnosticList) void {
    if (name.len == 0 or std.mem.eql(u8, name, "_")) return;

    if (!isSnakeCase(name)) {
        const msg = std.fmt.allocPrint(diagnostics.allocator, "{s} '{s}' should be snake_case", .{ kind, name }) catch return;
        diagnostics.addCodedError(.E300, location, msg) catch {};
    }
}

fn isSnakeCase(name: []const u8) bool {
    if (name.len == 0) return true;

    // must start with lowercase letter or underscore
    if (!std.ascii.isLower(name[0]) and name[0] != '_') return false;

    for (name) |ch| {
        if (!std.ascii.isLower(ch) and !std.ascii.isDigit(ch) and ch != '_') return false;
    }
    return true;
}

// ---------------------------------------------------------------
// E301: PascalCase for types
// ---------------------------------------------------------------

/// check that a type name follows PascalCase: starts with uppercase,
/// contains only letters and digits (no underscores).
fn checkPascalCase(name: []const u8, location: Location, kind: []const u8, diagnostics: *DiagnosticList) void {
    if (name.len == 0) return;

    if (!isPascalCase(name)) {
        const msg = std.fmt.allocPrint(diagnostics.allocator, "{s} '{s}' should be PascalCase", .{ kind, name }) catch return;
        diagnostics.addCodedError(.E301, location, msg) catch {};
    }
}

fn isPascalCase(name: []const u8) bool {
    if (name.len == 0) return true;

    // must start with uppercase letter
    if (!std.ascii.isUpper(name[0])) return false;

    // no underscores or other non-alphanumeric chars
    var has_lower = false;
    for (name) |ch| {
        if (!std.ascii.isAlphanumeric(ch)) return false;
        if (std.ascii.isLower(ch)) has_lower = true;
    }

    // must have at least one lowercase letter (HELLO is not PascalCase)
    return name.len == 1 or has_lower;
}

// ---------------------------------------------------------------
// E302: unused variable
// ---------------------------------------------------------------

/// check for bindings declared in a function body that are never
/// referenced in any expression. skips names starting with '_'.
fn checkUnusedBindings(body: *const ast.Block, params: []const ast.Param, diagnostics: *DiagnosticList) void {
    // collect all local bindings
    var bindings_buf: [128]BindingInfo = undefined;
    var binding_count: usize = 0;

    // check params (except self)
    for (params) |p| {
        if (std.mem.eql(u8, p.name, "self")) continue;
        if (p.name.len > 0 and p.name[0] == '_') continue;
        if (binding_count < bindings_buf.len) {
            bindings_buf[binding_count] = .{ .name = p.name, .location = p.location };
            binding_count += 1;
        }
    }

    // collect bindings from the body
    collectBindings(body, &bindings_buf, &binding_count);

    if (binding_count == 0) return;

    // walk all expressions to find which names are used
    var used_buf: [256][]const u8 = undefined;
    var used_count: usize = 0;
    collectUsedIdents(body, &used_buf, &used_count);

    // report unused
    for (bindings_buf[0..binding_count]) |b| {
        var found = false;
        for (used_buf[0..used_count]) |used| {
            if (std.mem.eql(u8, b.name, used)) {
                found = true;
                break;
            }
        }
        if (!found) {
            const msg = std.fmt.allocPrint(diagnostics.allocator, "unused variable '{s}'", .{b.name}) catch continue;
            diagnostics.addCodedWarning(.E302, b.location, msg) catch {};
        }
    }
}

const BindingInfo = struct {
    name: []const u8,
    location: Location,
};

/// collect all binding names from a block (non-recursive into nested fns).
fn collectBindings(block: *const ast.Block, buf: []BindingInfo, count: *usize) void {
    for (block.stmts) |*stmt| {
        switch (stmt.kind) {
            .binding => |b| {
                if (b.name.len > 0 and b.name[0] == '_') continue;
                if (count.* < buf.len) {
                    buf[count.*] = .{ .name = b.name, .location = stmt.location };
                    count.* += 1;
                }
            },
            .if_stmt => |ifs| {
                collectBindings(&ifs.then_block, buf, count);
                for (ifs.elif_branches) |*br| {
                    collectBindings(&br.block, buf, count);
                }
                if (ifs.else_block) |*eb| {
                    collectBindings(eb, buf, count);
                }
            },
            .for_stmt => |fs| {
                // for loop binding is implicitly used
                collectBindings(&fs.body, buf, count);
            },
            .while_stmt => |ws| {
                collectBindings(&ws.body, buf, count);
            },
            .match_stmt => |ms| {
                for (ms.arms) |*arm| {
                    switch (arm.body) {
                        .block => |*b| collectBindings(b, buf, count),
                        .expr => {},
                    }
                }
            },
            else => {},
        }
    }
}

/// collect all identifier names referenced in expressions within a block.
fn collectUsedIdents(block: *const ast.Block, buf: [][]const u8, count: *usize) void {
    for (block.stmts) |*stmt| {
        collectUsedIdentsFromStmt(stmt, buf, count);
    }
}

fn collectUsedIdentsFromStmt(stmt: *const ast.Stmt, buf: [][]const u8, count: *usize) void {
    switch (stmt.kind) {
        .binding => |b| {
            collectUsedIdentsFromExpr(b.value, buf, count);
        },
        .assignment => |a| {
            collectUsedIdentsFromExpr(a.target, buf, count);
            collectUsedIdentsFromExpr(a.value, buf, count);
        },
        .if_stmt => |ifs| {
            collectUsedIdentsFromExpr(ifs.condition, buf, count);
            collectUsedIdents(&ifs.then_block, buf, count);
            for (ifs.elif_branches) |*br| {
                collectUsedIdentsFromExpr(br.condition, buf, count);
                collectUsedIdents(&br.block, buf, count);
            }
            if (ifs.else_block) |*eb| {
                collectUsedIdents(eb, buf, count);
            }
        },
        .for_stmt => |fs| {
            collectUsedIdentsFromExpr(fs.iterable, buf, count);
            collectUsedIdents(&fs.body, buf, count);
        },
        .while_stmt => |ws| {
            collectUsedIdentsFromExpr(ws.condition, buf, count);
            collectUsedIdents(&ws.body, buf, count);
        },
        .match_stmt => |ms| {
            collectUsedIdentsFromExpr(ms.subject, buf, count);
            for (ms.arms) |*arm| {
                if (arm.guard) |g| collectUsedIdentsFromExpr(g, buf, count);
                switch (arm.body) {
                    .expr => |e| collectUsedIdentsFromExpr(e, buf, count),
                    .block => |*b| collectUsedIdents(b, buf, count),
                }
            }
        },
        .return_stmt => |rs| {
            if (rs.value) |v| collectUsedIdentsFromExpr(v, buf, count);
        },
        .fail_stmt => |fs| {
            collectUsedIdentsFromExpr(fs.value, buf, count);
        },
        .expr_stmt => |e| {
            collectUsedIdentsFromExpr(e, buf, count);
        },
        .break_stmt, .continue_stmt => {},
    }
}

fn collectUsedIdentsFromExpr(expr: *const ast.Expr, buf: [][]const u8, count: *usize) void {
    switch (expr.kind) {
        .ident => |name| {
            if (count.* < buf.len) {
                buf[count.*] = name;
                count.* += 1;
            }
        },
        .binary => |b| {
            collectUsedIdentsFromExpr(b.left, buf, count);
            collectUsedIdentsFromExpr(b.right, buf, count);
        },
        .unary => |u| {
            collectUsedIdentsFromExpr(u.operand, buf, count);
        },
        .call => |c| {
            collectUsedIdentsFromExpr(c.callee, buf, count);
            for (c.args) |*arg| collectUsedIdentsFromExpr(arg.value, buf, count);
        },
        .method_call => |mc| {
            collectUsedIdentsFromExpr(mc.receiver, buf, count);
            for (mc.args) |*arg| collectUsedIdentsFromExpr(arg.value, buf, count);
        },
        .field_access => |fa| {
            collectUsedIdentsFromExpr(fa.object, buf, count);
        },
        .index => |idx| {
            collectUsedIdentsFromExpr(idx.object, buf, count);
            collectUsedIdentsFromExpr(idx.index, buf, count);
        },
        .unwrap => |inner| collectUsedIdentsFromExpr(inner, buf, count),
        .try_expr => |inner| collectUsedIdentsFromExpr(inner, buf, count),
        .spawn_expr => |inner| collectUsedIdentsFromExpr(inner, buf, count),
        .await_expr => |inner| collectUsedIdentsFromExpr(inner, buf, count),
        .grouped => |inner| collectUsedIdentsFromExpr(inner, buf, count),
        .if_expr => |ie| {
            collectUsedIdentsFromExpr(ie.condition, buf, count);
            collectUsedIdentsFromExpr(ie.then_expr, buf, count);
            for (ie.elif_branches) |*br| {
                collectUsedIdentsFromExpr(br.condition, buf, count);
                collectUsedIdentsFromExpr(br.expr, buf, count);
            }
            collectUsedIdentsFromExpr(ie.else_expr, buf, count);
        },
        .match_expr => |me| {
            collectUsedIdentsFromExpr(me.subject, buf, count);
            for (me.arms) |*arm| {
                if (arm.guard) |g| collectUsedIdentsFromExpr(g, buf, count);
                switch (arm.body) {
                    .expr => |e| collectUsedIdentsFromExpr(e, buf, count),
                    .block => |*b| collectUsedIdents(b, buf, count),
                }
            }
        },
        .lambda => |lam| {
            switch (lam.body) {
                .expr => |e| collectUsedIdentsFromExpr(e, buf, count),
                .block => |*b| collectUsedIdents(b, buf, count),
            }
        },
        .list => |items| {
            for (items) |item| collectUsedIdentsFromExpr(item, buf, count);
        },
        .set => |items| {
            for (items) |item| collectUsedIdentsFromExpr(item, buf, count);
        },
        .tuple => |items| {
            for (items) |item| collectUsedIdentsFromExpr(item, buf, count);
        },
        .map => |entries| {
            for (entries) |*entry| {
                collectUsedIdentsFromExpr(entry.key, buf, count);
                collectUsedIdentsFromExpr(entry.value, buf, count);
            }
        },
        .string_interp => |si| {
            for (si.parts) |*part| {
                switch (part.*) {
                    .expr => |e| collectUsedIdentsFromExpr(e, buf, count),
                    .literal => {},
                }
            }
        },
        // literals and self don't reference variables
        .struct_init => |si| {
            for (si.args) |*arg| collectUsedIdentsFromExpr(arg.value, buf, count);
        },
        // literals and self don't reference variables
        .int_lit, .float_lit, .string_lit, .bool_lit, .none_lit, .self_expr, .err => {},
    }
}

// ---------------------------------------------------------------
// E304: public function missing doc comment
// ---------------------------------------------------------------

/// check that a public function has a doc comment (line starting with #)
/// immediately before it in the source.
fn checkDocComment(location: Location, source: []const u8, name: []const u8, diagnostics: *DiagnosticList) void {
    if (location.offset == 0) {
        emitMissingDocWarning(location, name, diagnostics);
        return;
    }

    // find the line before the declaration
    const prev_line = findPrevLine(source, location.offset);
    const trimmed = std.mem.trimLeft(u8, prev_line, " \t");

    if (trimmed.len == 0 or trimmed[0] != '#') {
        emitMissingDocWarning(location, name, diagnostics);
    }
}

fn emitMissingDocWarning(location: Location, name: []const u8, diagnostics: *DiagnosticList) void {
    const msg = std.fmt.allocPrint(diagnostics.allocator, "public function '{s}' is missing a doc comment", .{name}) catch return;
    diagnostics.addCodedWarning(.E304, location, msg) catch {};
}

/// find the content of the line immediately before the given offset.
fn findPrevLine(source: []const u8, offset: usize) []const u8 {
    if (offset == 0) return "";

    // find the start of the current line by going back to the previous newline
    var pos = offset;
    while (pos > 0 and source[pos - 1] != '\n') {
        pos -= 1;
    }

    // pos is now at the start of the current line. go back past the newline.
    if (pos == 0) return "";
    pos -= 1; // skip the '\n'

    // skip blank lines
    while (pos > 0 and source[pos] == '\n') {
        pos -= 1;
    }

    // find the bounds of the previous line
    const line_end = pos + 1;
    while (pos > 0 and source[pos - 1] != '\n') {
        pos -= 1;
    }

    return source[pos..line_end];
}

// ---------------------------------------------------------------
// E305: indentation depth exceeds 4 levels
// ---------------------------------------------------------------

/// walk a block tracking nesting depth. if/for/while/match each
/// add one level. report when depth exceeds 4.
fn lintBlock(block: *const ast.Block, diagnostics: *DiagnosticList, depth: u32) void {
    for (block.stmts) |*stmt| {
        switch (stmt.kind) {
            .if_stmt => |ifs| {
                if (depth >= 5) {
                    reportDeepNesting(stmt.location, depth, diagnostics);
                }
                lintBlock(&ifs.then_block, diagnostics, depth + 1);
                for (ifs.elif_branches) |*br| {
                    lintBlock(&br.block, diagnostics, depth + 1);
                }
                if (ifs.else_block) |*eb| {
                    lintBlock(eb, diagnostics, depth + 1);
                }
            },
            .for_stmt => |fs| {
                if (depth >= 5) {
                    reportDeepNesting(stmt.location, depth, diagnostics);
                }
                lintBlock(&fs.body, diagnostics, depth + 1);
            },
            .while_stmt => |ws| {
                if (depth >= 5) {
                    reportDeepNesting(stmt.location, depth, diagnostics);
                }
                lintBlock(&ws.body, diagnostics, depth + 1);
            },
            .match_stmt => |ms| {
                if (depth >= 5) {
                    reportDeepNesting(stmt.location, depth, diagnostics);
                }
                for (ms.arms) |*arm| {
                    switch (arm.body) {
                        .block => |*b| lintBlock(b, diagnostics, depth + 1),
                        .expr => {},
                    }
                }
            },
            else => {},
        }
    }
}

fn reportDeepNesting(location: Location, depth: u32, diagnostics: *DiagnosticList) void {
    const msg = std.fmt.allocPrint(diagnostics.allocator, "nesting depth {d} exceeds 4 levels — consider refactoring", .{depth}) catch return;
    diagnostics.addCodedWarning(.E305, location, msg) catch {};
}

// ---------------------------------------------------------------
// tests
// ---------------------------------------------------------------

const testing = std.testing;

test "isSnakeCase" {
    try testing.expect(isSnakeCase("hello"));
    try testing.expect(isSnakeCase("hello_world"));
    try testing.expect(isSnakeCase("_private"));
    try testing.expect(isSnakeCase("x1"));
    try testing.expect(isSnakeCase("get_item_2"));

    try testing.expect(!isSnakeCase("Hello"));
    try testing.expect(!isSnakeCase("helloWorld"));
    try testing.expect(!isSnakeCase("HELLO"));
    try testing.expect(!isSnakeCase("hello-world"));
}

test "isPascalCase" {
    try testing.expect(isPascalCase("Hello"));
    try testing.expect(isPascalCase("HelloWorld"));
    try testing.expect(isPascalCase("Http2Client"));
    try testing.expect(isPascalCase("A"));

    try testing.expect(!isPascalCase("hello"));
    try testing.expect(!isPascalCase("hello_world"));
    try testing.expect(!isPascalCase("Hello_World"));
    try testing.expect(!isPascalCase("HELLO"));
}

test "findPrevLine: basic" {
    const source = "# a comment\nfn foo():\n";
    // offset of 'f' in 'fn foo()'
    const prev = findPrevLine(source, 13);
    try testing.expectEqualStrings("# a comment", prev);
}

test "findPrevLine: no prev line" {
    const source = "fn foo():\n";
    const prev = findPrevLine(source, 0);
    try testing.expectEqualStrings("", prev);
}
