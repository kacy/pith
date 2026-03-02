// printer — AST pretty-printer
//
// renders a parsed AST as indented text for debugging.
// extracted from main.zig to keep the CLI entry point lean.

const ast = @import("ast.zig");
const io = @import("io.zig");

pub fn printModule(module: ast.Module) void {
    io.write("module\n", .{});

    for (module.imports) |imp| {
        printImport(imp, 1);
    }

    for (module.decls) |decl| {
        printDecl(decl, 1);
    }
}

fn printImport(imp: ast.ImportDecl, indent: u32) void {
    switch (imp.kind) {
        .simple => |s| {
            printIndent(indent);
            io.write("import", .{});
            for (s.path) |part| {
                io.write(" {s}", .{part});
            }
            if (s.alias) |alias| {
                io.write(" as {s}", .{alias});
            }
            io.write("\n", .{});
        },
        .from => |f| {
            printIndent(indent);
            io.write("from", .{});
            for (f.path) |part| {
                io.write(" {s}", .{part});
            }
            io.write(" import", .{});
            for (f.names, 0..) |name, i| {
                if (i > 0) io.write(",", .{});
                io.write(" {s}", .{name.name});
                if (name.alias) |alias| {
                    io.write(" as {s}", .{alias});
                }
            }
            io.write("\n", .{});
        },
    }
}

fn printDecl(decl: ast.Decl, indent: u32) void {
    if (decl.is_pub) {
        printIndent(indent);
        io.write("pub\n", .{});
    }

    switch (decl.kind) {
        .fn_decl => |f| printFnDecl(f, indent),
        .struct_decl => |s| printStructDecl(s, indent),
        .enum_decl => |e| printEnumDecl(e, indent),
        .interface_decl => |i| printInterfaceDecl(i, indent),
        .impl_decl => |i| printImplDecl(i, indent),
        .type_alias => |t| {
            printIndent(indent);
            io.write("type_alias {s}\n", .{t.name});
            printTypeExpr(t.type_expr, indent + 1);
        },
        .binding => |b| printBinding(b, indent),
    }
}

fn printFnDecl(f: ast.FnDecl, indent: u32) void {
    printIndent(indent);
    io.write("fn {s}", .{f.name});
    printGenericParams(f.generic_params);
    io.write("\n", .{});

    for (f.params) |p| {
        printIndent(indent + 1);
        io.write("param {s}", .{p.name});
        if (p.is_mut) io.write(" mut", .{});
        if (p.is_ref) io.write(" ref", .{});
        io.write("\n", .{});
        if (p.type_expr) |te| {
            printTypeExpr(te, indent + 2);
        }
    }

    if (f.return_type) |ret| {
        printIndent(indent + 1);
        io.write("returns\n", .{});
        printTypeExpr(ret, indent + 2);
    }

    printIndent(indent + 1);
    io.write("body\n", .{});
    printBlock(f.body, indent + 2);
}

fn printStructDecl(s: ast.StructDecl, indent: u32) void {
    printIndent(indent);
    io.write("struct {s}\n", .{s.name});
    for (s.fields) |field| {
        printIndent(indent + 1);
        io.write("field {s}", .{field.name});
        if (field.is_pub) io.write(" pub", .{});
        if (field.is_mut) io.write(" mut", .{});
        if (field.is_weak) io.write(" weak", .{});
        io.write("\n", .{});
        printTypeExpr(field.type_expr, indent + 2);
    }
}

fn printEnumDecl(e: ast.EnumDecl, indent: u32) void {
    printIndent(indent);
    io.write("enum {s}\n", .{e.name});
    for (e.variants) |v| {
        printIndent(indent + 1);
        io.write("variant {s}", .{v.name});
        if (v.fields.len > 0) {
            io.write("(", .{});
            for (v.fields, 0..) |field, i| {
                if (i > 0) io.write(", ", .{});
                printTypeExprInline(field);
            }
            io.write(")", .{});
        }
        io.write("\n", .{});
    }
}

fn printInterfaceDecl(i: ast.InterfaceDecl, indent: u32) void {
    printIndent(indent);
    io.write("interface {s}\n", .{i.name});
    for (i.methods) |m| {
        printIndent(indent + 1);
        io.write("fn {s}\n", .{m.name});
    }
}

fn printImplDecl(i: ast.ImplDecl, indent: u32) void {
    printIndent(indent);
    io.write("impl\n", .{});
    printTypeExpr(i.target, indent + 1);
    if (i.interface) |iface| {
        printIndent(indent + 1);
        io.write("for\n", .{});
        printTypeExpr(iface, indent + 2);
    }
    for (i.methods) |m| {
        if (m.is_pub) {
            printIndent(indent + 1);
            io.write("pub\n", .{});
        }
        printFnDecl(m.decl, indent + 1);
    }
}

fn printBlock(block: ast.Block, indent: u32) void {
    for (block.stmts) |stmt| {
        printStmt(stmt, indent);
    }
}

fn printStmt(stmt: ast.Stmt, indent: u32) void {
    switch (stmt.kind) {
        .binding => |b| printBinding(b, indent),
        .assignment => |a| {
            printIndent(indent);
            const op_str: []const u8 = switch (a.op) {
                .assign => "=",
                .add => "+=",
                .sub => "-=",
                .mul => "*=",
                .div => "/=",
            };
            io.write("assign {s}\n", .{op_str});
            printExpr(a.target, indent + 1);
            printExpr(a.value, indent + 1);
        },
        .if_stmt => |i| {
            printIndent(indent);
            io.write("if\n", .{});
            printExpr(i.condition, indent + 1);
            printIndent(indent + 1);
            io.write("then\n", .{});
            printBlock(i.then_block, indent + 2);
            for (i.elif_branches) |elif| {
                printIndent(indent + 1);
                io.write("elif\n", .{});
                printExpr(elif.condition, indent + 2);
                printBlock(elif.block, indent + 2);
            }
            if (i.else_block) |eb| {
                printIndent(indent + 1);
                io.write("else\n", .{});
                printBlock(eb, indent + 2);
            }
        },
        .for_stmt => |f| {
            printIndent(indent);
            io.write("for {s}", .{f.binding});
            if (f.index) |idx| io.write(", {s}", .{idx});
            io.write("\n", .{});
            printExpr(f.iterable, indent + 1);
            printBlock(f.body, indent + 1);
        },
        .while_stmt => |wh| {
            printIndent(indent);
            io.write("while\n", .{});
            printExpr(wh.condition, indent + 1);
            printBlock(wh.body, indent + 1);
        },
        .match_stmt => |m| {
            printIndent(indent);
            io.write("match\n", .{});
            printExpr(m.subject, indent + 1);
            for (m.arms) |arm| {
                printMatchArm(arm, indent + 1);
            }
        },
        .return_stmt => |r| {
            printIndent(indent);
            io.write("return\n", .{});
            if (r.value) |v| printExpr(v, indent + 1);
        },
        .fail_stmt => |f| {
            printIndent(indent);
            io.write("fail\n", .{});
            printExpr(f.value, indent + 1);
        },
        .break_stmt => {
            printIndent(indent);
            io.write("break\n", .{});
        },
        .continue_stmt => {
            printIndent(indent);
            io.write("continue\n", .{});
        },
        .expr_stmt => |e| {
            printExpr(e, indent);
        },
    }
}

fn printBinding(b: ast.Binding, indent: u32) void {
    printIndent(indent);
    io.write("bind {s}", .{b.name});
    if (b.is_mut) io.write(" mut", .{});
    io.write("\n", .{});
    if (b.type_expr) |te| {
        printTypeExpr(te, indent + 1);
    }
    printExpr(b.value, indent + 1);
}

fn printExpr(expr: *const ast.Expr, indent: u32) void {
    switch (expr.kind) {
        .int_lit => |v| {
            printIndent(indent);
            io.write("int {s}\n", .{v});
        },
        .float_lit => |v| {
            printIndent(indent);
            io.write("float {s}\n", .{v});
        },
        .string_lit => |v| {
            printIndent(indent);
            io.write("string {s}\n", .{v});
        },
        .bool_lit => |v| {
            printIndent(indent);
            io.write("bool {}\n", .{v});
        },
        .none_lit => {
            printIndent(indent);
            io.write("none\n", .{});
        },
        .ident => |name| {
            printIndent(indent);
            io.write("ident {s}\n", .{name});
        },
        .self_expr => {
            printIndent(indent);
            io.write("self\n", .{});
        },
        .binary => |b| {
            printIndent(indent);
            io.write("binary {s}\n", .{@tagName(b.op)});
            printExpr(b.left, indent + 1);
            printExpr(b.right, indent + 1);
        },
        .unary => |u| {
            printIndent(indent);
            io.write("unary {s}\n", .{@tagName(u.op)});
            printExpr(u.operand, indent + 1);
        },
        .call => |c| {
            printIndent(indent);
            io.write("call\n", .{});
            printExpr(c.callee, indent + 1);
            for (c.args) |arg| {
                printIndent(indent + 1);
                if (arg.name) |name| {
                    io.write("arg {s}=\n", .{name});
                } else {
                    io.write("arg\n", .{});
                }
                printExpr(arg.value, indent + 2);
            }
        },
        .method_call => |m| {
            printIndent(indent);
            io.write("method_call .{s}\n", .{m.method});
            printExpr(m.receiver, indent + 1);
            for (m.args) |arg| {
                printIndent(indent + 1);
                io.write("arg\n", .{});
                printExpr(arg.value, indent + 2);
            }
        },
        .field_access => |f| {
            printIndent(indent);
            io.write("field .{s}\n", .{f.field});
            printExpr(f.object, indent + 1);
        },
        .index => |i| {
            printIndent(indent);
            io.write("index\n", .{});
            printExpr(i.object, indent + 1);
            printExpr(i.index, indent + 1);
        },
        .unwrap => |inner| {
            printIndent(indent);
            io.write("unwrap\n", .{});
            printExpr(inner, indent + 1);
        },
        .try_expr => |inner| {
            printIndent(indent);
            io.write("try\n", .{});
            printExpr(inner, indent + 1);
        },
        .if_expr => |i| {
            printIndent(indent);
            io.write("if_expr\n", .{});
            printExpr(i.condition, indent + 1);
            printExpr(i.then_expr, indent + 1);
            printExpr(i.else_expr, indent + 1);
        },
        .match_expr => |m| {
            printIndent(indent);
            io.write("match_expr\n", .{});
            printExpr(m.subject, indent + 1);
            for (m.arms) |arm| {
                printMatchArm(arm, indent + 1);
            }
        },
        .lambda => |l| {
            printIndent(indent);
            io.write("lambda\n", .{});
            for (l.params) |p| {
                printIndent(indent + 1);
                io.write("param {s}", .{p.name});
                if (p.is_mut) io.write(" mut", .{});
                if (p.is_ref) io.write(" ref", .{});
                io.write("\n", .{});
                if (p.type_expr) |te| {
                    printTypeExpr(te, indent + 2);
                }
            }
            printIndent(indent + 1);
            io.write("body\n", .{});
            switch (l.body) {
                .expr => |e| printExpr(e, indent + 2),
                .block => |b| printBlock(b, indent + 2),
            }
        },
        .list => |items| {
            printIndent(indent);
            io.write("list ({d} items)\n", .{items.len});
            for (items) |item| printExpr(item, indent + 1);
        },
        .map => |entries| {
            printIndent(indent);
            io.write("map ({d} entries)\n", .{entries.len});
            for (entries) |entry| {
                printIndent(indent + 1);
                io.write("entry\n", .{});
                printExpr(entry.key, indent + 2);
                printExpr(entry.value, indent + 2);
            }
        },
        .set => |items| {
            printIndent(indent);
            io.write("set ({d} items)\n", .{items.len});
            for (items) |item| printExpr(item, indent + 1);
        },
        .tuple => |items| {
            printIndent(indent);
            io.write("tuple ({d} items)\n", .{items.len});
            for (items) |item| printExpr(item, indent + 1);
        },
        .string_interp => |si| {
            printIndent(indent);
            io.write("string_interp ({d} parts)\n", .{si.parts.len});
            for (si.parts) |part| {
                switch (part) {
                    .literal => |lit| {
                        printIndent(indent + 1);
                        io.write("lit {s}\n", .{lit});
                    },
                    .expr => |e| {
                        printExpr(e, indent + 1);
                    },
                }
            }
        },
        .grouped => |inner| {
            printExpr(inner, indent);
        },
        .err => {
            printIndent(indent);
            io.write("<error>\n", .{});
        },
    }
}

fn printMatchArm(arm: ast.MatchArm, indent: u32) void {
    printIndent(indent);
    io.write("arm\n", .{});
    printPattern(arm.pattern, indent + 1);
    if (arm.guard) |guard| {
        printIndent(indent + 1);
        io.write("guard\n", .{});
        printExpr(guard, indent + 2);
    }
    printIndent(indent + 1);
    io.write("body\n", .{});
    switch (arm.body) {
        .expr => |e| printExpr(e, indent + 2),
        .block => |b| printBlock(b, indent + 2),
    }
}

fn printPattern(pat: ast.Pattern, indent: u32) void {
    switch (pat.kind) {
        .wildcard => {
            printIndent(indent);
            io.write("pattern _\n", .{});
        },
        .int_lit => |v| {
            printIndent(indent);
            io.write("pattern int {s}\n", .{v});
        },
        .float_lit => |v| {
            printIndent(indent);
            io.write("pattern float {s}\n", .{v});
        },
        .string_lit => |v| {
            printIndent(indent);
            io.write("pattern string {s}\n", .{v});
        },
        .bool_lit => |v| {
            printIndent(indent);
            io.write("pattern bool {}\n", .{v});
        },
        .none_lit => {
            printIndent(indent);
            io.write("pattern none\n", .{});
        },
        .binding => |name| {
            printIndent(indent);
            io.write("pattern bind {s}\n", .{name});
        },
        .variant => |v| {
            printIndent(indent);
            io.write("pattern {s}.{s}", .{ v.type_name, v.variant });
            if (v.fields.len > 0) {
                io.write("(", .{});
                for (v.fields, 0..) |_, i| {
                    if (i > 0) io.write(", ", .{});
                    io.write("_", .{});
                }
                io.write(")", .{});
            }
            io.write("\n", .{});
        },
        .tuple => |fields| {
            printIndent(indent);
            io.write("pattern tuple\n", .{});
            for (fields) |f| {
                printPattern(f, indent + 1);
            }
        },
    }
}

fn printTypeExpr(te: *const ast.TypeExpr, indent: u32) void {
    switch (te.kind) {
        .named => |name| {
            printIndent(indent);
            io.write("type {s}\n", .{name});
        },
        .generic => |g| {
            printIndent(indent);
            io.write("type {s}[", .{g.name});
            for (g.args, 0..) |arg, i| {
                if (i > 0) io.write(", ", .{});
                printTypeExprInline(arg);
            }
            io.write("]\n", .{});
        },
        .optional => |inner| {
            printIndent(indent);
            io.write("optional\n", .{});
            printTypeExpr(inner, indent + 1);
        },
        .result => |r| {
            printIndent(indent);
            io.write("result\n", .{});
            printTypeExpr(r.ok_type, indent + 1);
            if (r.err_type) |et| printTypeExpr(et, indent + 1);
        },
        .tuple => |types| {
            printIndent(indent);
            io.write("tuple_type ({d})\n", .{types.len});
        },
        .fn_type => |f| {
            printIndent(indent);
            io.write("fn_type ({d} params)\n", .{f.params.len});
        },
    }
}

/// print a type expression inline (no newline, no indent).
/// used inside generic args and enum variant fields.
fn printTypeExprInline(te: *const ast.TypeExpr) void {
    switch (te.kind) {
        .named => |name| io.write("{s}", .{name}),
        .generic => |g| {
            io.write("{s}[", .{g.name});
            for (g.args, 0..) |arg, i| {
                if (i > 0) io.write(", ", .{});
                printTypeExprInline(arg);
            }
            io.write("]", .{});
        },
        .optional => |inner| {
            printTypeExprInline(inner);
            io.write("?", .{});
        },
        .result => |r| {
            printTypeExprInline(r.ok_type);
            io.write("!", .{});
            if (r.err_type) |et| printTypeExprInline(et);
        },
        .tuple => |types| {
            io.write("(", .{});
            for (types, 0..) |t, i| {
                if (i > 0) io.write(", ", .{});
                printTypeExprInline(t);
            }
            io.write(")", .{});
        },
        .fn_type => |f| {
            io.write("fn(", .{});
            for (f.params, 0..) |p, i| {
                if (i > 0) io.write(", ", .{});
                printTypeExprInline(p);
            }
            io.write(")", .{});
            if (f.return_type) |ret| {
                io.write(" -> ", .{});
                printTypeExprInline(ret);
            }
        },
    }
}

/// print generic parameter list: [T, U: Display]
fn printGenericParams(params: []const ast.GenericParam) void {
    if (params.len == 0) return;
    io.write("[", .{});
    for (params, 0..) |gp, i| {
        if (i > 0) io.write(", ", .{});
        io.write("{s}", .{gp.name});
    }
    io.write("]", .{});
}

fn printIndent(level: u32) void {
    var i: u32 = 0;
    while (i < level) : (i += 1) {
        io.write("  ", .{});
    }
}
