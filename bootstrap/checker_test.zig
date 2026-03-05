// checker tests — moved from checker.zig for readability
//
// all 171 test blocks exercise the Checker, Scope, and type system.
// run with: zig build test

const std = @import("std");
const ast = @import("ast.zig");
const errors = @import("errors.zig");
const types = @import("types.zig");

const checker_mod = @import("checker.zig");
const Checker = checker_mod.Checker;
const Scope = checker_mod.Scope;

const TypeId = types.TypeId;
const Location = errors.Location;

test "scope define and lookup" {
    var scope = Scope.init(std.testing.allocator, null);
    defer scope.deinit();

    try scope.define("x", .{ .type_id = .int, .is_mut = false });

    const b = scope.lookup("x").?;
    try std.testing.expectEqual(TypeId.int, b.type_id);
    try std.testing.expect(!b.is_mut);
}

test "scope lookup walks parent chain" {
    var parent = Scope.init(std.testing.allocator, null);
    defer parent.deinit();

    try parent.define("x", .{ .type_id = .int, .is_mut = false });

    var child = Scope.init(std.testing.allocator, &parent);
    defer child.deinit();

    try child.define("y", .{ .type_id = .string, .is_mut = true });

    // child sees its own binding
    try std.testing.expectEqual(TypeId.string, child.lookup("y").?.type_id);
    // child sees parent's binding
    try std.testing.expectEqual(TypeId.int, child.lookup("x").?.type_id);
    // parent doesn't see child's binding
    try std.testing.expect(parent.lookup("y") == null);
}

test "scope lookup returns null for undefined name" {
    var scope = Scope.init(std.testing.allocator, null);
    defer scope.deinit();

    try std.testing.expect(scope.lookup("missing") == null);
}

test "checker init registers builtins" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // builtin types should be in the type table
    try std.testing.expect(checker.type_table.lookup("Int") != null);
    try std.testing.expect(checker.type_table.lookup("String") != null);

    // print should be in the module scope
    const print_binding = checker.module_scope.lookup("print").?;
    try std.testing.expect(!print_binding.type_id.isErr());
}

test "resolveTypeExpr resolves named types" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_type_expr = ast.TypeExpr{
        .kind = .{ .named = "Int" },
        .location = Location.zero,
    };
    try std.testing.expectEqual(TypeId.int, checker.resolveTypeExpr(&int_type_expr));

    const string_type_expr = ast.TypeExpr{
        .kind = .{ .named = "String" },
        .location = Location.zero,
    };
    try std.testing.expectEqual(TypeId.string, checker.resolveTypeExpr(&string_type_expr));
}

test "resolveTypeExpr rejects deeply nested types" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // build a chain of 200 optional wrappings: Int?????...
    const depth = 200;
    var nodes: [depth + 1]ast.TypeExpr = undefined;
    nodes[0] = .{ .kind = .{ .named = "Int" }, .location = Location.zero };
    for (1..depth + 1) |i| {
        nodes[i] = .{ .kind = .{ .optional = &nodes[i - 1] }, .location = Location.zero };
    }

    const id = checker.resolveTypeExpr(&nodes[depth]);
    try std.testing.expect(id.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "resolveTypeExpr reports unknown types" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const bad = ast.TypeExpr{
        .kind = .{ .named = "Nonexistent" },
        .location = Location.zero,
    };
    const id = checker.resolveTypeExpr(&bad);
    try std.testing.expect(id.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "resolveTypeExpr resolves optional types" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const inner = ast.TypeExpr{
        .kind = .{ .named = "Int" },
        .location = Location.zero,
    };
    const optional = ast.TypeExpr{
        .kind = .{ .optional = &inner },
        .location = Location.zero,
    };
    const id = checker.resolveTypeExpr(&optional);
    try std.testing.expect(!id.isErr());

    const ty = checker.type_table.get(id).?;
    try std.testing.expectEqual(TypeId.int, ty.optional.inner);
}

test "undeclared generic type with zero args errors" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // List is not declared as a generic, so List[] should error
    const generic = ast.TypeExpr{
        .kind = .{ .generic = .{ .name = "List", .args = &.{} } },
        .location = Location.zero,
    };
    const id = checker.resolveTypeExpr(&generic);
    try std.testing.expect(id.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "Task[Int] resolves to task type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const inner = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const generic = ast.TypeExpr{
        .kind = .{ .generic = .{ .name = "Task", .args = &.{&inner} } },
        .location = Location.zero,
    };
    const id = checker.resolveTypeExpr(&generic);
    try std.testing.expect(!id.isErr());

    const ty = checker.type_table.get(id).?;
    try std.testing.expectEqual(TypeId.int, ty.task.inner);
}

test "Channel[String] resolves to channel type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const inner = ast.TypeExpr{ .kind = .{ .named = "String" }, .location = Location.zero };
    const generic = ast.TypeExpr{
        .kind = .{ .generic = .{ .name = "Channel", .args = &.{&inner} } },
        .location = Location.zero,
    };
    const id = checker.resolveTypeExpr(&generic);
    try std.testing.expect(!id.isErr());

    const ty = checker.type_table.get(id).?;
    try std.testing.expectEqual(TypeId.string, ty.channel.inner);
}

test "Task[Unknown] produces error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const inner = ast.TypeExpr{ .kind = .{ .named = "Unknown" }, .location = Location.zero };
    const generic = ast.TypeExpr{
        .kind = .{ .generic = .{ .name = "Task", .args = &.{&inner} } },
        .location = Location.zero,
    };
    const id = checker.resolveTypeExpr(&generic);
    try std.testing.expect(id.isErr());
}

test "List[Int] resolves to list type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const inner = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const generic = ast.TypeExpr{
        .kind = .{ .generic = .{ .name = "List", .args = &.{&inner} } },
        .location = Location.zero,
    };
    const id = checker.resolveTypeExpr(&generic);
    try std.testing.expect(!id.isErr());

    const ty = checker.type_table.get(id).?;
    try std.testing.expectEqual(TypeId.int, ty.list.element);
}

test "Map[String, Int] resolves to map type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const key_te = ast.TypeExpr{ .kind = .{ .named = "String" }, .location = Location.zero };
    const val_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const generic = ast.TypeExpr{
        .kind = .{ .generic = .{ .name = "Map", .args = &.{ &key_te, &val_te } } },
        .location = Location.zero,
    };
    const id = checker.resolveTypeExpr(&generic);
    try std.testing.expect(!id.isErr());

    const ty = checker.type_table.get(id).?;
    try std.testing.expectEqual(TypeId.string, ty.map.key);
    try std.testing.expectEqual(TypeId.int, ty.map.value);
}

test "Set[Bool] resolves to set type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const inner = ast.TypeExpr{ .kind = .{ .named = "Bool" }, .location = Location.zero };
    const generic = ast.TypeExpr{
        .kind = .{ .generic = .{ .name = "Set", .args = &.{&inner} } },
        .location = Location.zero,
    };
    const id = checker.resolveTypeExpr(&generic);
    try std.testing.expect(!id.isErr());

    const ty = checker.type_table.get(id).?;
    try std.testing.expectEqual(TypeId.bool, ty.set.element);
}

test "undeclared generic Foo[Int] errors" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const inner = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const generic = ast.TypeExpr{
        .kind = .{ .generic = .{ .name = "Foo", .args = &.{&inner} } },
        .location = Location.zero,
    };
    const id = checker.resolveTypeExpr(&generic);
    try std.testing.expect(id.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "scope inherits return type from parent" {
    var parent = Scope.init(std.testing.allocator, null);
    defer parent.deinit();
    parent.return_type = .int;

    var child = Scope.init(std.testing.allocator, &parent);
    defer child.deinit();

    try std.testing.expectEqual(TypeId.int, child.return_type.?);
}

// -- expression checking tests --

test "checkExpr: literals" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const int_expr = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    try std.testing.expectEqual(TypeId.int, checker.checkExpr(&int_expr, scope));

    const float_expr = ast.Expr{ .kind = .{ .float_lit = "3.14" }, .location = Location.zero };
    try std.testing.expectEqual(TypeId.float, checker.checkExpr(&float_expr, scope));

    const str_expr = ast.Expr{ .kind = .{ .string_lit = "hello" }, .location = Location.zero };
    try std.testing.expectEqual(TypeId.string, checker.checkExpr(&str_expr, scope));

    const bool_expr = ast.Expr{ .kind = .{ .bool_lit = true }, .location = Location.zero };
    try std.testing.expectEqual(TypeId.bool, checker.checkExpr(&bool_expr, scope));
}

test "checkExpr: identifier lookup" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    try checker.module_scope.define("x", .{ .type_id = .int, .is_mut = false });
    const scope = &checker.module_scope;

    const ident = ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };
    try std.testing.expectEqual(TypeId.int, checker.checkExpr(&ident, scope));
}

test "checkExpr: undefined variable" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const ident = ast.Expr{ .kind = .{ .ident = "unknown" }, .location = Location.zero };
    try std.testing.expect(checker.checkExpr(&ident, scope).isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "checkExpr: binary arithmetic" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const left = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const right = ast.Expr{ .kind = .{ .int_lit = "2" }, .location = Location.zero };
    const add = ast.Expr{
        .kind = .{ .binary = .{ .left = &left, .op = .add, .right = &right } },
        .location = Location.zero,
    };
    try std.testing.expectEqual(TypeId.int, checker.checkExpr(&add, scope));
}

test "checkExpr: string concatenation" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const left = ast.Expr{ .kind = .{ .string_lit = "hello" }, .location = Location.zero };
    const right = ast.Expr{ .kind = .{ .string_lit = " world" }, .location = Location.zero };
    const add = ast.Expr{
        .kind = .{ .binary = .{ .left = &left, .op = .add, .right = &right } },
        .location = Location.zero,
    };
    try std.testing.expectEqual(TypeId.string, checker.checkExpr(&add, scope));
}

test "checkExpr: type mismatch in binary" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const int_e = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const float_e = ast.Expr{ .kind = .{ .float_lit = "2.0" }, .location = Location.zero };
    const add = ast.Expr{
        .kind = .{ .binary = .{ .left = &int_e, .op = .add, .right = &float_e } },
        .location = Location.zero,
    };
    try std.testing.expect(checker.checkExpr(&add, scope).isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "checkExpr: comparison returns Bool" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const left = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const right = ast.Expr{ .kind = .{ .int_lit = "2" }, .location = Location.zero };
    const eq = ast.Expr{
        .kind = .{ .binary = .{ .left = &left, .op = .eq, .right = &right } },
        .location = Location.zero,
    };
    try std.testing.expectEqual(TypeId.bool, checker.checkExpr(&eq, scope));
}

test "checkExpr: logical operators need Bool" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const t = ast.Expr{ .kind = .{ .bool_lit = true }, .location = Location.zero };
    const f = ast.Expr{ .kind = .{ .bool_lit = false }, .location = Location.zero };
    const and_expr = ast.Expr{
        .kind = .{ .binary = .{ .left = &t, .op = .@"and", .right = &f } },
        .location = Location.zero,
    };
    try std.testing.expectEqual(TypeId.bool, checker.checkExpr(&and_expr, scope));
}

test "checkExpr: unary negate" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const num = ast.Expr{ .kind = .{ .int_lit = "5" }, .location = Location.zero };
    const neg = ast.Expr{
        .kind = .{ .unary = .{ .op = .negate, .operand = &num } },
        .location = Location.zero,
    };
    try std.testing.expectEqual(TypeId.int, checker.checkExpr(&neg, scope));

    // negate on string should fail
    const s = ast.Expr{ .kind = .{ .string_lit = "hi" }, .location = Location.zero };
    const neg_s = ast.Expr{
        .kind = .{ .unary = .{ .op = .negate, .operand = &s } },
        .location = Location.zero,
    };
    try std.testing.expect(checker.checkExpr(&neg_s, scope).isErr());
}

test "checkExpr: unary not" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const b = ast.Expr{ .kind = .{ .bool_lit = true }, .location = Location.zero };
    const not_expr = ast.Expr{
        .kind = .{ .unary = .{ .op = .not, .operand = &b } },
        .location = Location.zero,
    };
    try std.testing.expectEqual(TypeId.bool, checker.checkExpr(&not_expr, scope));
}

test "checkExpr: string interpolation" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const inner = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    const interp = ast.Expr{
        .kind = .{ .string_interp = .{ .parts = &.{
            .{ .literal = "value: " },
            .{ .expr = &inner },
        } } },
        .location = Location.zero,
    };
    try std.testing.expectEqual(TypeId.string, checker.checkExpr(&interp, scope));
}

test "checkExpr: grouped expression is transparent" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const inner = ast.Expr{ .kind = .{ .int_lit = "7" }, .location = Location.zero };
    const grouped = ast.Expr{
        .kind = .{ .grouped = &inner },
        .location = Location.zero,
    };
    try std.testing.expectEqual(TypeId.int, checker.checkExpr(&grouped, scope));
}

test "checkExpr: error sentinel suppresses cascading" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    // an undefined variable + 1 should only produce one error (the undefined var),
    // not a second cascading error about type mismatch
    const bad = ast.Expr{ .kind = .{ .ident = "missing" }, .location = Location.zero };
    const num = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const add = ast.Expr{
        .kind = .{ .binary = .{ .left = &bad, .op = .add, .right = &num } },
        .location = Location.zero,
    };
    try std.testing.expect(checker.checkExpr(&add, scope).isErr());
    try std.testing.expectEqual(@as(usize, 1), checker.diagnostics.errorCount());
}

// -- function and call checking tests --

test "checkCall: correct call to print" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const callee = ast.Expr{ .kind = .{ .ident = "print" }, .location = Location.zero };
    const arg_val = ast.Expr{ .kind = .{ .string_lit = "hello" }, .location = Location.zero };
    const call = ast.Expr{
        .kind = .{ .call = .{
            .callee = &callee,
            .args = &.{.{ .name = null, .value = &arg_val, .location = Location.zero }},
        } },
        .location = Location.zero,
    };
    try std.testing.expectEqual(TypeId.void, checker.checkExpr(&call, scope));
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "checkCall: wrong argument type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const callee = ast.Expr{ .kind = .{ .ident = "print" }, .location = Location.zero };
    const arg_val = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    const call = ast.Expr{
        .kind = .{ .call = .{
            .callee = &callee,
            .args = &.{.{ .name = null, .value = &arg_val, .location = Location.zero }},
        } },
        .location = Location.zero,
    };
    _ = checker.checkExpr(&call, scope);
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "checkCall: wrong argument count" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const callee = ast.Expr{ .kind = .{ .ident = "print" }, .location = Location.zero };
    const call = ast.Expr{
        .kind = .{ .call = .{
            .callee = &callee,
            .args = &.{},
        } },
        .location = Location.zero,
    };
    _ = checker.checkExpr(&call, scope);
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "registerFnDecl and checkFnDecl" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // build an AST for: fn add(a: Int, b: Int) -> Int: return a + b
    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };

    const a_expr = ast.Expr{ .kind = .{ .ident = "a" }, .location = Location.zero };
    const b_expr = ast.Expr{ .kind = .{ .ident = "b" }, .location = Location.zero };
    const add_expr = ast.Expr{
        .kind = .{ .binary = .{ .left = &a_expr, .op = .add, .right = &b_expr } },
        .location = Location.zero,
    };
    const return_stmt = ast.Stmt{
        .kind = .{ .return_stmt = .{ .value = &add_expr } },
        .location = Location.zero,
    };

    const fn_decl = ast.FnDecl{
        .name = "add",
        .generic_params = &.{},
        .params = &.{
            .{ .name = "a", .type_expr = &int_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
            .{ .name = "b", .type_expr = &int_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
        },
        .return_type = &int_te,
        .body = .{ .stmts = &.{return_stmt}, .location = Location.zero },
    };

    const decl = ast.Decl{
        .kind = .{ .fn_decl = fn_decl },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{
        .imports = &.{},
        .decls = &.{decl},
    };

    checker.check(&module);
    try std.testing.expect(!checker.diagnostics.hasErrors());

    // the function should be registered in module scope
    const binding = checker.module_scope.lookup("add").?;
    const fn_type = checker.type_table.get(binding.type_id).?;
    const func = fn_type.function;
    try std.testing.expectEqual(@as(usize, 2), func.param_types.len);
    try std.testing.expectEqual(TypeId.int, func.return_type);
}

test "checkFnDecl: return type mismatch" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const string_te = ast.TypeExpr{ .kind = .{ .named = "String" }, .location = Location.zero };

    // fn bad() -> String: return 42
    const int_expr = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    const return_stmt = ast.Stmt{
        .kind = .{ .return_stmt = .{ .value = &int_expr } },
        .location = Location.zero,
    };

    const fn_decl = ast.FnDecl{
        .name = "bad",
        .generic_params = &.{},
        .params = &.{},
        .return_type = &string_te,
        .body = .{ .stmts = &.{return_stmt}, .location = Location.zero },
    };

    const decl = ast.Decl{
        .kind = .{ .fn_decl = fn_decl },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{
        .imports = &.{},
        .decls = &.{decl},
    };

    checker.check(&module);
    try std.testing.expect(checker.diagnostics.hasErrors());
}

// -- statement checking tests --

test "checkStmt: binding with type annotation" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    var scope = Scope.init(std.testing.allocator, &checker.module_scope);
    defer scope.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const val = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    const stmt = ast.Stmt{
        .kind = .{ .binding = .{ .name = "x", .type_expr = &int_te, .value = &val, .is_mut = false } },
        .location = Location.zero,
    };

    checker.checkStmt(&stmt, &scope);
    try std.testing.expect(!checker.diagnostics.hasErrors());
    try std.testing.expectEqual(TypeId.int, scope.lookup("x").?.type_id);
}

test "checkStmt: binding type mismatch" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    var scope = Scope.init(std.testing.allocator, &checker.module_scope);
    defer scope.deinit();

    // x: String := 42
    const str_te = ast.TypeExpr{ .kind = .{ .named = "String" }, .location = Location.zero };
    const val = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    const stmt = ast.Stmt{
        .kind = .{ .binding = .{ .name = "x", .type_expr = &str_te, .value = &val, .is_mut = false } },
        .location = Location.zero,
    };

    checker.checkStmt(&stmt, &scope);
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "checkStmt: binding infers type from value" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    var scope = Scope.init(std.testing.allocator, &checker.module_scope);
    defer scope.deinit();

    // x := "hello"
    const val = ast.Expr{ .kind = .{ .string_lit = "hello" }, .location = Location.zero };
    const stmt = ast.Stmt{
        .kind = .{ .binding = .{ .name = "x", .type_expr = null, .value = &val, .is_mut = false } },
        .location = Location.zero,
    };

    checker.checkStmt(&stmt, &scope);
    try std.testing.expect(!checker.diagnostics.hasErrors());
    try std.testing.expectEqual(TypeId.string, scope.lookup("x").?.type_id);
}

test "checkStmt: assignment to mutable variable" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    var scope = Scope.init(std.testing.allocator, &checker.module_scope);
    defer scope.deinit();
    try scope.define("x", .{ .type_id = .int, .is_mut = true });

    const target = ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };
    const val = ast.Expr{ .kind = .{ .int_lit = "10" }, .location = Location.zero };
    const stmt = ast.Stmt{
        .kind = .{ .assignment = .{ .target = &target, .op = .assign, .value = &val } },
        .location = Location.zero,
    };

    checker.checkStmt(&stmt, &scope);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "checkStmt: assignment to immutable variable" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    var scope = Scope.init(std.testing.allocator, &checker.module_scope);
    defer scope.deinit();
    try scope.define("x", .{ .type_id = .int, .is_mut = false });

    const target = ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };
    const val = ast.Expr{ .kind = .{ .int_lit = "10" }, .location = Location.zero };
    const stmt = ast.Stmt{
        .kind = .{ .assignment = .{ .target = &target, .op = .assign, .value = &val } },
        .location = Location.zero,
    };

    checker.checkStmt(&stmt, &scope);
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "checkStmt: if statement checks condition" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    var scope = Scope.init(std.testing.allocator, &checker.module_scope);
    defer scope.deinit();

    // if 42: (should fail — condition is Int, not Bool)
    const cond = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    const stmt = ast.Stmt{
        .kind = .{ .if_stmt = .{
            .condition = &cond,
            .then_block = .{ .stmts = &.{}, .location = Location.zero },
            .elif_branches = &.{},
            .else_block = null,
        } },
        .location = Location.zero,
    };

    checker.checkStmt(&stmt, &scope);
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "checkStmt: while statement checks condition" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    var scope = Scope.init(std.testing.allocator, &checker.module_scope);
    defer scope.deinit();

    const cond = ast.Expr{ .kind = .{ .bool_lit = true }, .location = Location.zero };
    const stmt = ast.Stmt{
        .kind = .{ .while_stmt = .{
            .condition = &cond,
            .body = .{ .stmts = &.{}, .location = Location.zero },
        } },
        .location = Location.zero,
    };

    checker.checkStmt(&stmt, &scope);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

// -- struct, enum, and field access tests --

test "registerStructDecl registers type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const string_te = ast.TypeExpr{ .kind = .{ .named = "String" }, .location = Location.zero };

    const struct_decl = ast.StructDecl{
        .name = "Point",
        .generic_params = &.{},
        .fields = &.{
            .{ .name = "x", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            .{ .name = "y", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            .{ .name = "label", .type_expr = &string_te, .default = null, .is_pub = false, .is_mut = false, .is_weak = false, .location = Location.zero },
        },
    };

    const decl = ast.Decl{
        .kind = .{ .struct_decl = struct_decl },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{decl} };
    checker.check(&module);

    try std.testing.expect(!checker.diagnostics.hasErrors());

    // Point should be registered in the type table
    const point_id = checker.type_table.lookup("Point").?;
    const ty = checker.type_table.get(point_id).?;
    const s = ty.@"struct";
    try std.testing.expectEqualStrings("Point", s.name);
    try std.testing.expectEqual(@as(usize, 3), s.fields.len);
    try std.testing.expectEqual(TypeId.int, s.fields[0].type_id);
}

test "registerEnumDecl registers type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };

    const enum_decl = ast.EnumDecl{
        .name = "Shape",
        .generic_params = &.{},
        .variants = &.{
            .{ .name = "Circle", .fields = &.{&int_te}, .location = Location.zero },
            .{ .name = "Square", .fields = &.{ &int_te, &int_te }, .location = Location.zero },
            .{ .name = "Point", .fields = &.{}, .location = Location.zero },
        },
    };

    const decl = ast.Decl{
        .kind = .{ .enum_decl = enum_decl },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{decl} };
    checker.check(&module);

    try std.testing.expect(!checker.diagnostics.hasErrors());

    const shape_id = checker.type_table.lookup("Shape").?;
    const ty = checker.type_table.get(shape_id).?;
    const e = ty.@"enum";
    try std.testing.expectEqualStrings("Shape", e.name);
    try std.testing.expectEqual(@as(usize, 3), e.variants.len);
}

test "checkFieldAccess: valid field" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // register a struct type
    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const struct_decl = ast.StructDecl{
        .name = "Point",
        .generic_params = &.{},
        .fields = &.{
            .{ .name = "x", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            .{ .name = "y", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
        },
    };

    const decl = ast.Decl{
        .kind = .{ .struct_decl = struct_decl },
        .is_pub = false,
        .location = Location.zero,
    };
    const module = ast.Module{ .imports = &.{}, .decls = &.{decl} };
    checker.check(&module);

    // add a binding for p: Point
    const point_id = checker.type_table.lookup("Point").?;
    try checker.module_scope.define("p", .{ .type_id = point_id, .is_mut = false });

    // check p.x
    const p_expr = ast.Expr{ .kind = .{ .ident = "p" }, .location = Location.zero };
    const field_access = ast.Expr{
        .kind = .{ .field_access = .{ .object = &p_expr, .field = "x" } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&field_access, &checker.module_scope);
    try std.testing.expectEqual(TypeId.int, result);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "checkFieldAccess: unknown field" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const struct_decl = ast.StructDecl{
        .name = "Point",
        .generic_params = &.{},
        .fields = &.{
            .{ .name = "x", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
        },
    };

    const decl = ast.Decl{
        .kind = .{ .struct_decl = struct_decl },
        .is_pub = false,
        .location = Location.zero,
    };
    const module = ast.Module{ .imports = &.{}, .decls = &.{decl} };
    checker.check(&module);

    const point_id = checker.type_table.lookup("Point").?;
    try checker.module_scope.define("p", .{ .type_id = point_id, .is_mut = false });

    // check p.z (doesn't exist)
    const p_expr = ast.Expr{ .kind = .{ .ident = "p" }, .location = Location.zero };
    const field_access = ast.Expr{
        .kind = .{ .field_access = .{ .object = &p_expr, .field = "z" } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&field_access, &checker.module_scope);
    try std.testing.expect(result.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "checkFieldAccess: non-struct type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    try checker.module_scope.define("x", .{ .type_id = .int, .is_mut = false });

    // check x.foo (Int is not a struct)
    const x_expr = ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };
    const field_access = ast.Expr{
        .kind = .{ .field_access = .{ .object = &x_expr, .field = "foo" } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&field_access, &checker.module_scope);
    try std.testing.expect(result.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

// -- break/continue validation tests --

test "break inside while loop is ok" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // while true: break
    const cond = ast.Expr{ .kind = .{ .bool_lit = true }, .location = Location.zero };
    const break_stmt = ast.Stmt{ .kind = .break_stmt, .location = Location.zero };
    const stmt = ast.Stmt{
        .kind = .{ .while_stmt = .{
            .condition = &cond,
            .body = .{ .stmts = &.{break_stmt}, .location = Location.zero },
        } },
        .location = Location.zero,
    };

    var scope = Scope.init(std.testing.allocator, &checker.module_scope);
    defer scope.deinit();
    checker.checkStmt(&stmt, &scope);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "continue inside for loop is ok" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // for item in items: continue
    const items = ast.Expr{ .kind = .{ .ident = "items" }, .location = Location.zero };
    const cont_stmt = ast.Stmt{ .kind = .continue_stmt, .location = Location.zero };
    const stmt = ast.Stmt{
        .kind = .{ .for_stmt = .{
            .binding = "item",
            .index = null,
            .iterable = &items,
            .body = .{ .stmts = &.{cont_stmt}, .location = Location.zero },
        } },
        .location = Location.zero,
    };

    // define 'items' so it doesn't error on the iterable
    try checker.module_scope.define("items", .{ .type_id = .err, .is_mut = false });
    var scope = Scope.init(std.testing.allocator, &checker.module_scope);
    defer scope.deinit();
    checker.checkStmt(&stmt, &scope);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "break at top level is an error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    var scope = Scope.init(std.testing.allocator, &checker.module_scope);
    defer scope.deinit();

    const stmt = ast.Stmt{ .kind = .break_stmt, .location = Location.zero };
    checker.checkStmt(&stmt, &scope);
    try std.testing.expect(checker.diagnostics.hasErrors());
}

// -- match expression tests --

test "checkMatchExpr: literal patterns with type agreement" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    // match 1: 1 => "one", 2 => "two", _ => "other"
    const subject = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const one_result = ast.Expr{ .kind = .{ .string_lit = "one" }, .location = Location.zero };
    const two_result = ast.Expr{ .kind = .{ .string_lit = "two" }, .location = Location.zero };
    const other_result = ast.Expr{ .kind = .{ .string_lit = "other" }, .location = Location.zero };

    const match_expr = ast.Expr{
        .kind = .{ .match_expr = .{
            .subject = &subject,
            .arms = &.{
                .{
                    .pattern = .{ .kind = .{ .int_lit = "1" }, .location = Location.zero },
                    .guard = null,
                    .body = .{ .expr = &one_result },
                    .location = Location.zero,
                },
                .{
                    .pattern = .{ .kind = .{ .int_lit = "2" }, .location = Location.zero },
                    .guard = null,
                    .body = .{ .expr = &two_result },
                    .location = Location.zero,
                },
                .{
                    .pattern = .{ .kind = .wildcard, .location = Location.zero },
                    .guard = null,
                    .body = .{ .expr = &other_result },
                    .location = Location.zero,
                },
            },
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&match_expr, scope);
    try std.testing.expectEqual(TypeId.string, result);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "checkMatchExpr: binding pattern defines variable in arm" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    // match 42: x => x (binding pattern, arm body uses x)
    const subject = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    const x_expr = ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };

    const match_expr = ast.Expr{
        .kind = .{ .match_expr = .{
            .subject = &subject,
            .arms = &.{
                .{
                    .pattern = .{ .kind = .{ .binding = "x" }, .location = Location.zero },
                    .guard = null,
                    .body = .{ .expr = &x_expr },
                    .location = Location.zero,
                },
            },
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&match_expr, scope);
    try std.testing.expectEqual(TypeId.int, result);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "checkMatchExpr: guard must be Bool" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const subject = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const result_expr = ast.Expr{ .kind = .{ .string_lit = "yes" }, .location = Location.zero };
    const bad_guard = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };

    const match_expr = ast.Expr{
        .kind = .{ .match_expr = .{
            .subject = &subject,
            .arms = &.{
                .{
                    .pattern = .{ .kind = .wildcard, .location = Location.zero },
                    .guard = &bad_guard,
                    .body = .{ .expr = &result_expr },
                    .location = Location.zero,
                },
            },
        } },
        .location = Location.zero,
    };

    _ = checker.checkExpr(&match_expr, scope);
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "checkMatchExpr: mismatched arm types" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    // match 1: 1 => "string", 2 => 42 (type mismatch)
    const subject = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const str_result = ast.Expr{ .kind = .{ .string_lit = "one" }, .location = Location.zero };
    const int_result = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };

    const match_expr = ast.Expr{
        .kind = .{ .match_expr = .{
            .subject = &subject,
            .arms = &.{
                .{
                    .pattern = .{ .kind = .{ .int_lit = "1" }, .location = Location.zero },
                    .guard = null,
                    .body = .{ .expr = &str_result },
                    .location = Location.zero,
                },
                .{
                    .pattern = .{ .kind = .{ .int_lit = "2" }, .location = Location.zero },
                    .guard = null,
                    .body = .{ .expr = &int_result },
                    .location = Location.zero,
                },
            },
        } },
        .location = Location.zero,
    };

    _ = checker.checkExpr(&match_expr, scope);
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "checkMatchExpr: variant pattern binds field" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // register enum Shape with Circle(Float)
    const float_te = ast.TypeExpr{ .kind = .{ .named = "Float" }, .location = Location.zero };
    const enum_decl = ast.Decl{
        .kind = .{ .enum_decl = .{
            .name = "Shape",
            .generic_params = &.{},
            .variants = &.{
                .{ .name = "Circle", .fields = &.{&float_te}, .location = Location.zero },
                .{ .name = "Point", .fields = &.{}, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };
    const module = ast.Module{ .imports = &.{}, .decls = &.{enum_decl} };
    checker.check(&module);

    // define s: Shape
    const shape_id = checker.type_table.lookup("Shape").?;
    try checker.module_scope.define("s", .{ .type_id = shape_id, .is_mut = false });

    // match s: Shape.Circle(r) => r, Shape.Point => 0.0
    const subject = ast.Expr{ .kind = .{ .ident = "s" }, .location = Location.zero };
    const r_expr = ast.Expr{ .kind = .{ .ident = "r" }, .location = Location.zero };
    const zero_expr = ast.Expr{ .kind = .{ .float_lit = "0.0" }, .location = Location.zero };

    const match_expr = ast.Expr{
        .kind = .{ .match_expr = .{
            .subject = &subject,
            .arms = &.{
                .{
                    .pattern = .{
                        .kind = .{ .variant = .{
                            .type_name = "Shape",
                            .variant = "Circle",
                            .fields = &.{
                                .{ .kind = .{ .binding = "r" }, .location = Location.zero },
                            },
                        } },
                        .location = Location.zero,
                    },
                    .guard = null,
                    .body = .{ .expr = &r_expr },
                    .location = Location.zero,
                },
                .{
                    .pattern = .{
                        .kind = .{ .variant = .{
                            .type_name = "Shape",
                            .variant = "Point",
                            .fields = &.{},
                        } },
                        .location = Location.zero,
                    },
                    .guard = null,
                    .body = .{ .expr = &zero_expr },
                    .location = Location.zero,
                },
            },
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&match_expr, &checker.module_scope);
    try std.testing.expectEqual(TypeId.float, result);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "checkMatchExpr: variant pattern wrong field count" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const float_te = ast.TypeExpr{ .kind = .{ .named = "Float" }, .location = Location.zero };
    const enum_decl = ast.Decl{
        .kind = .{ .enum_decl = .{
            .name = "Shape",
            .generic_params = &.{},
            .variants = &.{
                .{ .name = "Circle", .fields = &.{&float_te}, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };
    const module = ast.Module{ .imports = &.{}, .decls = &.{enum_decl} };
    checker.check(&module);

    const shape_id = checker.type_table.lookup("Shape").?;
    try checker.module_scope.define("s", .{ .type_id = shape_id, .is_mut = false });

    // Shape.Circle(a, b) — too many fields
    const subject = ast.Expr{ .kind = .{ .ident = "s" }, .location = Location.zero };
    const result_expr = ast.Expr{ .kind = .{ .int_lit = "0" }, .location = Location.zero };

    const match_expr = ast.Expr{
        .kind = .{ .match_expr = .{
            .subject = &subject,
            .arms = &.{
                .{
                    .pattern = .{
                        .kind = .{ .variant = .{
                            .type_name = "Shape",
                            .variant = "Circle",
                            .fields = &.{
                                .{ .kind = .{ .binding = "a" }, .location = Location.zero },
                                .{ .kind = .{ .binding = "b" }, .location = Location.zero },
                            },
                        } },
                        .location = Location.zero,
                    },
                    .guard = null,
                    .body = .{ .expr = &result_expr },
                    .location = Location.zero,
                },
            },
        } },
        .location = Location.zero,
    };

    _ = checker.checkExpr(&match_expr, &checker.module_scope);
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "checkMatchExpr: wildcard matches anything" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const subject = ast.Expr{ .kind = .{ .string_lit = "hello" }, .location = Location.zero };
    const result_expr = ast.Expr{ .kind = .{ .int_lit = "0" }, .location = Location.zero };

    const match_expr = ast.Expr{
        .kind = .{ .match_expr = .{
            .subject = &subject,
            .arms = &.{
                .{
                    .pattern = .{ .kind = .wildcard, .location = Location.zero },
                    .guard = null,
                    .body = .{ .expr = &result_expr },
                    .location = Location.zero,
                },
            },
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&match_expr, scope);
    try std.testing.expectEqual(TypeId.int, result);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "checkMatchStmt: no arm type agreement needed" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // match 1: 1 => "string", 2 => 42, _ => 0 (as statement, no type agreement needed)
    const subject = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const str_result = ast.Expr{ .kind = .{ .string_lit = "one" }, .location = Location.zero };
    const int_result = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    const fallback = ast.Expr{ .kind = .{ .int_lit = "0" }, .location = Location.zero };

    const stmt = ast.Stmt{
        .kind = .{ .match_stmt = .{
            .subject = &subject,
            .arms = &.{
                .{
                    .pattern = .{ .kind = .{ .int_lit = "1" }, .location = Location.zero },
                    .guard = null,
                    .body = .{ .expr = &str_result },
                    .location = Location.zero,
                },
                .{
                    .pattern = .{ .kind = .{ .int_lit = "2" }, .location = Location.zero },
                    .guard = null,
                    .body = .{ .expr = &int_result },
                    .location = Location.zero,
                },
                .{
                    .pattern = .{ .kind = .wildcard, .location = Location.zero },
                    .guard = null,
                    .body = .{ .expr = &fallback },
                    .location = Location.zero,
                },
            },
        } },
        .location = Location.zero,
    };

    var scope = Scope.init(std.testing.allocator, &checker.module_scope);
    defer scope.deinit();
    checker.checkStmt(&stmt, &scope);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

// -- exhaustiveness tests --

test "exhaustiveness: enum with all variants covered" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // register enum Direction { North, South, East, West }
    const enum_decl = ast.Decl{
        .kind = .{ .enum_decl = .{
            .name = "Direction",
            .generic_params = &.{},
            .variants = &.{
                .{ .name = "North", .fields = &.{}, .location = Location.zero },
                .{ .name = "South", .fields = &.{}, .location = Location.zero },
                .{ .name = "East", .fields = &.{}, .location = Location.zero },
                .{ .name = "West", .fields = &.{}, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };
    const module = ast.Module{ .imports = &.{}, .decls = &.{enum_decl} };
    checker.check(&module);

    const dir_id = checker.type_table.lookup("Direction").?;
    try checker.module_scope.define("d", .{ .type_id = dir_id, .is_mut = false });

    const subject = ast.Expr{ .kind = .{ .ident = "d" }, .location = Location.zero };
    const result_expr = ast.Expr{ .kind = .{ .string_lit = "ok" }, .location = Location.zero };

    const match_expr = ast.Expr{
        .kind = .{ .match_expr = .{
            .subject = &subject,
            .arms = &.{
                .{ .pattern = .{ .kind = .{ .variant = .{ .type_name = "Direction", .variant = "North", .fields = &.{} } }, .location = Location.zero }, .guard = null, .body = .{ .expr = &result_expr }, .location = Location.zero },
                .{ .pattern = .{ .kind = .{ .variant = .{ .type_name = "Direction", .variant = "South", .fields = &.{} } }, .location = Location.zero }, .guard = null, .body = .{ .expr = &result_expr }, .location = Location.zero },
                .{ .pattern = .{ .kind = .{ .variant = .{ .type_name = "Direction", .variant = "East", .fields = &.{} } }, .location = Location.zero }, .guard = null, .body = .{ .expr = &result_expr }, .location = Location.zero },
                .{ .pattern = .{ .kind = .{ .variant = .{ .type_name = "Direction", .variant = "West", .fields = &.{} } }, .location = Location.zero }, .guard = null, .body = .{ .expr = &result_expr }, .location = Location.zero },
            },
        } },
        .location = Location.zero,
    };

    _ = checker.checkExpr(&match_expr, &checker.module_scope);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "exhaustiveness: enum missing variant produces error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // register enum Direction { North, South, East, West }
    const enum_decl = ast.Decl{
        .kind = .{ .enum_decl = .{
            .name = "Direction",
            .generic_params = &.{},
            .variants = &.{
                .{ .name = "North", .fields = &.{}, .location = Location.zero },
                .{ .name = "South", .fields = &.{}, .location = Location.zero },
                .{ .name = "East", .fields = &.{}, .location = Location.zero },
                .{ .name = "West", .fields = &.{}, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };
    const module = ast.Module{ .imports = &.{}, .decls = &.{enum_decl} };
    checker.check(&module);

    const dir_id = checker.type_table.lookup("Direction").?;
    try checker.module_scope.define("d2", .{ .type_id = dir_id, .is_mut = false });

    const subject = ast.Expr{ .kind = .{ .ident = "d2" }, .location = Location.zero };
    const result_expr = ast.Expr{ .kind = .{ .string_lit = "ok" }, .location = Location.zero };

    // only North and South — missing East and West
    const match_expr = ast.Expr{
        .kind = .{ .match_expr = .{
            .subject = &subject,
            .arms = &.{
                .{ .pattern = .{ .kind = .{ .variant = .{ .type_name = "Direction", .variant = "North", .fields = &.{} } }, .location = Location.zero }, .guard = null, .body = .{ .expr = &result_expr }, .location = Location.zero },
                .{ .pattern = .{ .kind = .{ .variant = .{ .type_name = "Direction", .variant = "South", .fields = &.{} } }, .location = Location.zero }, .guard = null, .body = .{ .expr = &result_expr }, .location = Location.zero },
            },
        } },
        .location = Location.zero,
    };

    _ = checker.checkExpr(&match_expr, &checker.module_scope);
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "exhaustiveness: wildcard makes enum match exhaustive" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const enum_decl = ast.Decl{
        .kind = .{ .enum_decl = .{
            .name = "Color",
            .generic_params = &.{},
            .variants = &.{
                .{ .name = "Red", .fields = &.{}, .location = Location.zero },
                .{ .name = "Green", .fields = &.{}, .location = Location.zero },
                .{ .name = "Blue", .fields = &.{}, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };
    const module = ast.Module{ .imports = &.{}, .decls = &.{enum_decl} };
    checker.check(&module);

    const color_id = checker.type_table.lookup("Color").?;
    try checker.module_scope.define("c", .{ .type_id = color_id, .is_mut = false });

    const subject = ast.Expr{ .kind = .{ .ident = "c" }, .location = Location.zero };
    const result_expr = ast.Expr{ .kind = .{ .int_lit = "0" }, .location = Location.zero };

    // only Red + wildcard — should be exhaustive
    const match_expr = ast.Expr{
        .kind = .{ .match_expr = .{
            .subject = &subject,
            .arms = &.{
                .{ .pattern = .{ .kind = .{ .variant = .{ .type_name = "Color", .variant = "Red", .fields = &.{} } }, .location = Location.zero }, .guard = null, .body = .{ .expr = &result_expr }, .location = Location.zero },
                .{ .pattern = .{ .kind = .wildcard, .location = Location.zero }, .guard = null, .body = .{ .expr = &result_expr }, .location = Location.zero },
            },
        } },
        .location = Location.zero,
    };

    _ = checker.checkExpr(&match_expr, &checker.module_scope);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "exhaustiveness: binding pattern makes match exhaustive" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const subject = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const x_expr = ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };

    // match 1: x => x (binding pattern catches everything)
    const match_expr = ast.Expr{
        .kind = .{ .match_expr = .{
            .subject = &subject,
            .arms = &.{
                .{ .pattern = .{ .kind = .{ .binding = "x" }, .location = Location.zero }, .guard = null, .body = .{ .expr = &x_expr }, .location = Location.zero },
            },
        } },
        .location = Location.zero,
    };

    _ = checker.checkExpr(&match_expr, &checker.module_scope);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "exhaustiveness: bool with both true and false" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    try checker.module_scope.define("flag", .{ .type_id = .bool, .is_mut = false });
    const subject = ast.Expr{ .kind = .{ .ident = "flag" }, .location = Location.zero };
    const result_expr = ast.Expr{ .kind = .{ .string_lit = "ok" }, .location = Location.zero };

    const match_expr = ast.Expr{
        .kind = .{ .match_expr = .{
            .subject = &subject,
            .arms = &.{
                .{ .pattern = .{ .kind = .{ .bool_lit = true }, .location = Location.zero }, .guard = null, .body = .{ .expr = &result_expr }, .location = Location.zero },
                .{ .pattern = .{ .kind = .{ .bool_lit = false }, .location = Location.zero }, .guard = null, .body = .{ .expr = &result_expr }, .location = Location.zero },
            },
        } },
        .location = Location.zero,
    };

    _ = checker.checkExpr(&match_expr, &checker.module_scope);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "exhaustiveness: bool missing one value produces error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    try checker.module_scope.define("flag2", .{ .type_id = .bool, .is_mut = false });
    const subject = ast.Expr{ .kind = .{ .ident = "flag2" }, .location = Location.zero };
    const result_expr = ast.Expr{ .kind = .{ .string_lit = "yes" }, .location = Location.zero };

    // only true — missing false
    const match_expr = ast.Expr{
        .kind = .{ .match_expr = .{
            .subject = &subject,
            .arms = &.{
                .{ .pattern = .{ .kind = .{ .bool_lit = true }, .location = Location.zero }, .guard = null, .body = .{ .expr = &result_expr }, .location = Location.zero },
            },
        } },
        .location = Location.zero,
    };

    _ = checker.checkExpr(&match_expr, &checker.module_scope);
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "exhaustiveness: int with only literals requires wildcard" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const subject = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const result_expr = ast.Expr{ .kind = .{ .string_lit = "ok" }, .location = Location.zero };

    // only literal arms — no wildcard
    const match_expr = ast.Expr{
        .kind = .{ .match_expr = .{
            .subject = &subject,
            .arms = &.{
                .{ .pattern = .{ .kind = .{ .int_lit = "1" }, .location = Location.zero }, .guard = null, .body = .{ .expr = &result_expr }, .location = Location.zero },
                .{ .pattern = .{ .kind = .{ .int_lit = "2" }, .location = Location.zero }, .guard = null, .body = .{ .expr = &result_expr }, .location = Location.zero },
            },
        } },
        .location = Location.zero,
    };

    _ = checker.checkExpr(&match_expr, &checker.module_scope);
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "exhaustiveness: guarded arms don't count toward coverage" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const subject = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const result_expr = ast.Expr{ .kind = .{ .string_lit = "ok" }, .location = Location.zero };
    const guard = ast.Expr{ .kind = .{ .bool_lit = true }, .location = Location.zero };

    // wildcard with guard — doesn't count as exhaustive
    const match_expr = ast.Expr{
        .kind = .{ .match_expr = .{
            .subject = &subject,
            .arms = &.{
                .{ .pattern = .{ .kind = .wildcard, .location = Location.zero }, .guard = &guard, .body = .{ .expr = &result_expr }, .location = Location.zero },
            },
        } },
        .location = Location.zero,
    };

    _ = checker.checkExpr(&match_expr, &checker.module_scope);
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "exhaustiveness: guarded enum variant doesn't count" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const enum_decl = ast.Decl{
        .kind = .{ .enum_decl = .{
            .name = "AB",
            .generic_params = &.{},
            .variants = &.{
                .{ .name = "A", .fields = &.{}, .location = Location.zero },
                .{ .name = "B", .fields = &.{}, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };
    const module = ast.Module{ .imports = &.{}, .decls = &.{enum_decl} };
    checker.check(&module);

    const ab_id = checker.type_table.lookup("AB").?;
    try checker.module_scope.define("ab", .{ .type_id = ab_id, .is_mut = false });

    const subject = ast.Expr{ .kind = .{ .ident = "ab" }, .location = Location.zero };
    const result_expr = ast.Expr{ .kind = .{ .string_lit = "ok" }, .location = Location.zero };
    const guard = ast.Expr{ .kind = .{ .bool_lit = true }, .location = Location.zero };

    // A (unguarded) + B (guarded) — B doesn't count
    const match_expr = ast.Expr{
        .kind = .{ .match_expr = .{
            .subject = &subject,
            .arms = &.{
                .{ .pattern = .{ .kind = .{ .variant = .{ .type_name = "AB", .variant = "A", .fields = &.{} } }, .location = Location.zero }, .guard = null, .body = .{ .expr = &result_expr }, .location = Location.zero },
                .{ .pattern = .{ .kind = .{ .variant = .{ .type_name = "AB", .variant = "B", .fields = &.{} } }, .location = Location.zero }, .guard = &guard, .body = .{ .expr = &result_expr }, .location = Location.zero },
            },
        } },
        .location = Location.zero,
    };

    _ = checker.checkExpr(&match_expr, &checker.module_scope);
    try std.testing.expect(checker.diagnostics.hasErrors());
}

// -- lambda tests --

test "checkExpr: short lambda fn(x: Int) => x * 2" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const x_expr = ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };
    const two = ast.Expr{ .kind = .{ .int_lit = "2" }, .location = Location.zero };
    const body = ast.Expr{
        .kind = .{ .binary = .{ .left = &x_expr, .op = .mul, .right = &two } },
        .location = Location.zero,
    };

    const lambda = ast.Expr{
        .kind = .{ .lambda = .{
            .params = &.{
                .{ .name = "x", .type_expr = &int_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
            },
            .body = .{ .expr = &body },
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&lambda, scope);
    try std.testing.expect(!result.isErr());

    const ty = checker.type_table.get(result).?;
    const func = ty.function;
    try std.testing.expectEqual(@as(usize, 1), func.param_types.len);
    try std.testing.expectEqual(TypeId.int, func.param_types[0]);
    try std.testing.expectEqual(TypeId.int, func.return_type);
}

test "checkExpr: lambda with two params" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const a_expr = ast.Expr{ .kind = .{ .ident = "a" }, .location = Location.zero };
    const b_expr = ast.Expr{ .kind = .{ .ident = "b" }, .location = Location.zero };
    const body = ast.Expr{
        .kind = .{ .binary = .{ .left = &a_expr, .op = .add, .right = &b_expr } },
        .location = Location.zero,
    };

    const lambda = ast.Expr{
        .kind = .{ .lambda = .{
            .params = &.{
                .{ .name = "a", .type_expr = &int_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
                .{ .name = "b", .type_expr = &int_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
            },
            .body = .{ .expr = &body },
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&lambda, scope);
    try std.testing.expect(!result.isErr());

    const ty = checker.type_table.get(result).?;
    const func = ty.function;
    try std.testing.expectEqual(@as(usize, 2), func.param_types.len);
    try std.testing.expectEqual(TypeId.int, func.return_type);
}

test "checkExpr: block lambda returns Void" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const x_expr = ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };
    const ret_stmt = ast.Stmt{
        .kind = .{ .return_stmt = .{ .value = null } },
        .location = Location.zero,
    };
    // use x in an expression statement to avoid unused variable (but we don't check that yet)
    _ = x_expr;

    const lambda = ast.Expr{
        .kind = .{ .lambda = .{
            .params = &.{
                .{ .name = "x", .type_expr = &int_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
            },
            .body = .{ .block = .{ .stmts = &.{ret_stmt}, .location = Location.zero } },
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&lambda, scope);
    try std.testing.expect(!result.isErr());

    const ty = checker.type_table.get(result).?;
    try std.testing.expectEqual(TypeId.void, ty.function.return_type);
}

test "checkExpr: lambda param without annotation is error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const x_expr = ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };
    const lambda = ast.Expr{
        .kind = .{ .lambda = .{
            .params = &.{
                .{ .name = "x", .type_expr = null, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
            },
            .body = .{ .expr = &x_expr },
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&lambda, scope);
    try std.testing.expect(result.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

// -- struct constructor tests --

test "checkCall: struct constructor with correct args" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // register struct Point { x: Int, y: Int }
    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const struct_decl = ast.Decl{
        .kind = .{ .struct_decl = .{
            .name = "Point",
            .generic_params = &.{},
            .fields = &.{
                .{ .name = "x", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
                .{ .name = "y", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{struct_decl} };
    checker.check(&module);

    // Point(1, 2) should return Point type
    const callee = ast.Expr{ .kind = .{ .ident = "Point" }, .location = Location.zero };
    const arg1 = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const arg2 = ast.Expr{ .kind = .{ .int_lit = "2" }, .location = Location.zero };
    const call = ast.Expr{
        .kind = .{ .call = .{
            .callee = &callee,
            .args = &.{
                .{ .name = null, .value = &arg1, .location = Location.zero },
                .{ .name = null, .value = &arg2, .location = Location.zero },
            },
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&call, &checker.module_scope);
    const point_id = checker.type_table.lookup("Point").?;
    try std.testing.expectEqual(point_id, result);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "checkCall: struct constructor wrong arg count" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const struct_decl = ast.Decl{
        .kind = .{ .struct_decl = .{
            .name = "Point",
            .generic_params = &.{},
            .fields = &.{
                .{ .name = "x", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
                .{ .name = "y", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{struct_decl} };
    checker.check(&module);

    // Point(1) — wrong arg count
    const callee = ast.Expr{ .kind = .{ .ident = "Point" }, .location = Location.zero };
    const arg1 = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const call = ast.Expr{
        .kind = .{ .call = .{
            .callee = &callee,
            .args = &.{
                .{ .name = null, .value = &arg1, .location = Location.zero },
            },
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&call, &checker.module_scope);
    try std.testing.expect(result.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "checkCall: struct constructor wrong arg type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const struct_decl = ast.Decl{
        .kind = .{ .struct_decl = .{
            .name = "Point",
            .generic_params = &.{},
            .fields = &.{
                .{ .name = "x", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{struct_decl} };
    checker.check(&module);

    // Point("hello") — wrong type
    const callee = ast.Expr{ .kind = .{ .ident = "Point" }, .location = Location.zero };
    const arg1 = ast.Expr{ .kind = .{ .string_lit = "hello" }, .location = Location.zero };
    const call = ast.Expr{
        .kind = .{ .call = .{
            .callee = &callee,
            .args = &.{
                .{ .name = null, .value = &arg1, .location = Location.zero },
            },
        } },
        .location = Location.zero,
    };

    _ = checker.checkExpr(&call, &checker.module_scope);
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "checkCall: non-struct type name falls through to normal call" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // Int(42) — Int is a builtin, not a struct, not a function → "undefined variable"
    const callee = ast.Expr{ .kind = .{ .ident = "Int" }, .location = Location.zero };
    const arg1 = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    const call = ast.Expr{
        .kind = .{ .call = .{
            .callee = &callee,
            .args = &.{
                .{ .name = null, .value = &arg1, .location = Location.zero },
            },
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&call, &checker.module_scope);
    try std.testing.expect(result.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "checkCall: struct constructor result used in field access" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const struct_decl = ast.Decl{
        .kind = .{ .struct_decl = .{
            .name = "Point",
            .generic_params = &.{},
            .fields = &.{
                .{ .name = "x", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{struct_decl} };
    checker.check(&module);

    // bind p := Point(1), then check p.x
    const callee = ast.Expr{ .kind = .{ .ident = "Point" }, .location = Location.zero };
    const arg1 = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const call_expr = ast.Expr{
        .kind = .{ .call = .{
            .callee = &callee,
            .args = &.{
                .{ .name = null, .value = &arg1, .location = Location.zero },
            },
        } },
        .location = Location.zero,
    };

    // simulate binding p := Point(1)
    const call_type = checker.checkExpr(&call_expr, &checker.module_scope);
    try checker.module_scope.define("p", .{ .type_id = call_type, .is_mut = false });

    // now check p.x
    const p_expr = ast.Expr{ .kind = .{ .ident = "p" }, .location = Location.zero };
    const field = ast.Expr{
        .kind = .{ .field_access = .{ .object = &p_expr, .field = "x" } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&field, &checker.module_scope);
    try std.testing.expectEqual(TypeId.int, result);
}

// -- type alias tests --

test "registerTypeAlias: alias of builtin type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const alias_decl = ast.Decl{
        .kind = .{ .type_alias = .{
            .name = "Meters",
            .generic_params = &.{},
            .type_expr = &int_te,
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{alias_decl} };
    checker.check(&module);

    try std.testing.expect(!checker.diagnostics.hasErrors());
    // Meters should resolve to the same TypeId as Int
    try std.testing.expectEqual(TypeId.int, checker.type_table.lookup("Meters").?);
}

test "registerTypeAlias: alias of struct type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const struct_decl = ast.Decl{
        .kind = .{ .struct_decl = .{
            .name = "Point",
            .generic_params = &.{},
            .fields = &.{
                .{ .name = "x", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const point_te = ast.TypeExpr{ .kind = .{ .named = "Point" }, .location = Location.zero };
    const alias_decl = ast.Decl{
        .kind = .{ .type_alias = .{
            .name = "P",
            .generic_params = &.{},
            .type_expr = &point_te,
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{ struct_decl, alias_decl } };
    checker.check(&module);

    try std.testing.expect(!checker.diagnostics.hasErrors());
    const point_id = checker.type_table.lookup("Point").?;
    const p_id = checker.type_table.lookup("P").?;
    try std.testing.expectEqual(point_id, p_id);
}

test "registerTypeAlias: alias of unknown type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const bad_te = ast.TypeExpr{ .kind = .{ .named = "Nonexistent" }, .location = Location.zero };
    const alias_decl = ast.Decl{
        .kind = .{ .type_alias = .{
            .name = "Bad",
            .generic_params = &.{},
            .type_expr = &bad_te,
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{alias_decl} };
    checker.check(&module);

    try std.testing.expect(checker.diagnostics.hasErrors());
    try std.testing.expect(checker.type_table.lookup("Bad") == null);
}

// -- tuple literal tests --

test "checkExpr: tuple literal" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const int_e = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const str_e = ast.Expr{ .kind = .{ .string_lit = "hello" }, .location = Location.zero };
    const tuple = ast.Expr{
        .kind = .{ .tuple = &.{ &int_e, &str_e } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&tuple, scope);
    try std.testing.expect(!result.isErr());

    const ty = checker.type_table.get(result).?;
    const elems = ty.tuple.elements;
    try std.testing.expectEqual(@as(usize, 2), elems.len);
    try std.testing.expectEqual(TypeId.int, elems[0]);
    try std.testing.expectEqual(TypeId.string, elems[1]);
}

test "checkExpr: tuple with three elements" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const bool_e = ast.Expr{ .kind = .{ .bool_lit = true }, .location = Location.zero };
    const int_e = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    const float_e = ast.Expr{ .kind = .{ .float_lit = "3.14" }, .location = Location.zero };
    const tuple = ast.Expr{
        .kind = .{ .tuple = &.{ &bool_e, &int_e, &float_e } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&tuple, scope);
    try std.testing.expect(!result.isErr());

    const ty = checker.type_table.get(result).?;
    const elems = ty.tuple.elements;
    try std.testing.expectEqual(@as(usize, 3), elems.len);
    try std.testing.expectEqual(TypeId.bool, elems[0]);
    try std.testing.expectEqual(TypeId.int, elems[1]);
    try std.testing.expectEqual(TypeId.float, elems[2]);
}

test "checkExpr: tuple with error element propagates err" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const int_e = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const bad_e = ast.Expr{ .kind = .{ .ident = "missing" }, .location = Location.zero };
    const tuple = ast.Expr{
        .kind = .{ .tuple = &.{ &int_e, &bad_e } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&tuple, scope);
    try std.testing.expect(result.isErr());
}

test "continue in function body outside loop is an error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    var scope = Scope.init(std.testing.allocator, &checker.module_scope);
    defer scope.deinit();
    scope.return_type = .void; // simulate being inside a function

    const stmt = ast.Stmt{ .kind = .continue_stmt, .location = Location.zero };
    checker.checkStmt(&stmt, &scope);
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "spawn wraps expression type in Task" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    // spawn of an int literal → Task[Int]
    const int_expr = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    const spawn = ast.Expr{ .kind = .{ .spawn_expr = &int_expr }, .location = Location.zero };

    const result = checker.checkExpr(&spawn, scope);
    try std.testing.expect(!result.isErr());

    const ty = checker.type_table.get(result).?;
    try std.testing.expectEqual(TypeId.int, ty.task.inner);
}

test "spawn of error-typed expr propagates error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    // spawn of an undefined variable → err
    const bad = ast.Expr{ .kind = .{ .ident = "undefined" }, .location = Location.zero };
    const spawn = ast.Expr{ .kind = .{ .spawn_expr = &bad }, .location = Location.zero };

    const result = checker.checkExpr(&spawn, scope);
    try std.testing.expect(result.isErr());
}

test "nested spawn is an error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    // spawn(spawn(42)) — inner spawn produces Task[Int], outer spawn should error
    const int_expr = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    const inner_spawn = ast.Expr{ .kind = .{ .spawn_expr = &int_expr }, .location = Location.zero };
    const outer_spawn = ast.Expr{ .kind = .{ .spawn_expr = &inner_spawn }, .location = Location.zero };

    const result = checker.checkExpr(&outer_spawn, scope);
    try std.testing.expect(result.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "await unwraps Task to inner type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    // await(spawn(42)) → Int
    const int_expr = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    const spawn = ast.Expr{ .kind = .{ .spawn_expr = &int_expr }, .location = Location.zero };
    const await_e = ast.Expr{ .kind = .{ .await_expr = &spawn }, .location = Location.zero };

    const result = checker.checkExpr(&await_e, scope);
    try std.testing.expectEqual(TypeId.int, result);
}

test "await on non-Task is an error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    // await 42 → error (Int is not a Task)
    const int_expr = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    const await_e = ast.Expr{ .kind = .{ .await_expr = &int_expr }, .location = Location.zero };

    const result = checker.checkExpr(&await_e, scope);
    try std.testing.expect(result.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "await of error-typed expr propagates error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const bad = ast.Expr{ .kind = .{ .ident = "undefined" }, .location = Location.zero };
    const await_e = ast.Expr{ .kind = .{ .await_expr = &bad }, .location = Location.zero };

    const result = checker.checkExpr(&await_e, scope);
    try std.testing.expect(result.isErr());
}

test "Mutex() returns Mutex type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const callee = ast.Expr{ .kind = .{ .ident = "Mutex" }, .location = Location.zero };
    const call = ast.Expr{
        .kind = .{ .call = .{ .callee = &callee, .args = &.{} } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&call, scope);
    try std.testing.expect(!result.isErr());
    try std.testing.expectEqualStrings("Mutex", checker.type_table.typeName(result));
}

test "WaitGroup() returns WaitGroup type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const callee = ast.Expr{ .kind = .{ .ident = "WaitGroup" }, .location = Location.zero };
    const call = ast.Expr{
        .kind = .{ .call = .{ .callee = &callee, .args = &.{} } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&call, scope);
    try std.testing.expect(!result.isErr());
    try std.testing.expectEqualStrings("WaitGroup", checker.type_table.typeName(result));
}

test "Semaphore(Int) returns Semaphore type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const callee = ast.Expr{ .kind = .{ .ident = "Semaphore" }, .location = Location.zero };
    const arg_val = ast.Expr{ .kind = .{ .int_lit = "10" }, .location = Location.zero };
    const arg = ast.Arg{ .name = null, .value = &arg_val, .location = Location.zero };
    const call = ast.Expr{
        .kind = .{ .call = .{ .callee = &callee, .args = &.{arg} } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&call, scope);
    try std.testing.expect(!result.isErr());
    try std.testing.expectEqualStrings("Semaphore", checker.type_table.typeName(result));
}

// -- generic declaration tests --

test "generic struct is stored without error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const t_te = ast.TypeExpr{ .kind = .{ .named = "A" }, .location = Location.zero };
    const u_te = ast.TypeExpr{ .kind = .{ .named = "B" }, .location = Location.zero };

    const struct_decl = ast.StructDecl{
        .name = "Pair",
        .generic_params = &.{
            .{ .name = "A", .bounds = &.{}, .location = Location.zero },
            .{ .name = "B", .bounds = &.{}, .location = Location.zero },
        },
        .fields = &.{
            .{ .name = "first", .type_expr = &t_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            .{ .name = "second", .type_expr = &u_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
        },
    };

    const decl = ast.Decl{
        .kind = .{ .struct_decl = struct_decl },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{decl} };
    checker.check(&module);

    // no errors — generic struct stored, not rejected
    try std.testing.expect(!checker.diagnostics.hasErrors());
    // should be in generic_decls, not in the type table
    try std.testing.expect(checker.generic_decls.contains("Pair"));
    try std.testing.expect(checker.type_table.lookup("Pair") == null);
}

test "generic enum is stored without error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const t_te = ast.TypeExpr{ .kind = .{ .named = "T" }, .location = Location.zero };

    const enum_decl = ast.EnumDecl{
        .name = "Option",
        .generic_params = &.{
            .{ .name = "T", .bounds = &.{}, .location = Location.zero },
        },
        .variants = &.{
            .{ .name = "Some", .fields = &.{&t_te}, .location = Location.zero },
            .{ .name = "None", .fields = &.{}, .location = Location.zero },
        },
    };

    const decl = ast.Decl{
        .kind = .{ .enum_decl = enum_decl },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{decl} };
    checker.check(&module);

    try std.testing.expect(!checker.diagnostics.hasErrors());
    try std.testing.expect(checker.generic_decls.contains("Option"));
    try std.testing.expect(checker.type_table.lookup("Option") == null);
}

test "non-generic struct still registers normally" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };

    const struct_decl = ast.StructDecl{
        .name = "Point",
        .generic_params = &.{},
        .fields = &.{
            .{ .name = "x", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
        },
    };

    const decl = ast.Decl{
        .kind = .{ .struct_decl = struct_decl },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{decl} };
    checker.check(&module);

    try std.testing.expect(!checker.diagnostics.hasErrors());
    // non-generic goes into the type table, not generic_decls
    try std.testing.expect(checker.type_table.lookup("Point") != null);
    try std.testing.expect(!checker.generic_decls.contains("Point"));
}

test "checkIdent: generic type name returns err silently" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // register a generic decl manually
    const t_te = ast.TypeExpr{ .kind = .{ .named = "T" }, .location = Location.zero };
    try checker.generic_decls.put("Box", .{ .@"struct" = .{
        .name = "Box",
        .generic_params = &.{.{ .name = "T", .bounds = &.{}, .location = Location.zero }},
        .fields = &.{
            .{ .name = "value", .type_expr = &t_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
        },
    } });

    const scope = &checker.module_scope;
    const ident = ast.Expr{ .kind = .{ .ident = "Box" }, .location = Location.zero };
    const result = checker.checkExpr(&ident, scope);

    // returns err but does NOT emit a diagnostic
    try std.testing.expect(result.isErr());
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

// -- generic struct instantiation tests --

test "Pair[Int,String] resolves to concrete struct" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // register a generic Pair[A, B] struct
    const a_te = ast.TypeExpr{ .kind = .{ .named = "A" }, .location = Location.zero };
    const b_te = ast.TypeExpr{ .kind = .{ .named = "B" }, .location = Location.zero };
    try checker.generic_decls.put("Pair", .{ .@"struct" = .{
        .name = "Pair",
        .generic_params = &.{
            .{ .name = "A", .bounds = &.{}, .location = Location.zero },
            .{ .name = "B", .bounds = &.{}, .location = Location.zero },
        },
        .fields = &.{
            .{ .name = "first", .type_expr = &a_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            .{ .name = "second", .type_expr = &b_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
        },
    } });

    // resolve Pair[Int, String]
    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const str_te = ast.TypeExpr{ .kind = .{ .named = "String" }, .location = Location.zero };
    const generic = ast.TypeExpr{
        .kind = .{ .generic = .{ .name = "Pair", .args = &.{ &int_te, &str_te } } },
        .location = Location.zero,
    };

    const id = checker.resolveTypeExpr(&generic);
    try std.testing.expect(!id.isErr());
    try std.testing.expect(!checker.diagnostics.hasErrors());

    // check the resulting concrete struct
    const ty = checker.type_table.get(id).?;
    const s = ty.@"struct";
    try std.testing.expectEqualStrings("Pair[Int,String]", s.name);
    try std.testing.expectEqual(@as(usize, 2), s.fields.len);
    try std.testing.expectEqual(TypeId.int, s.fields[0].type_id);
    try std.testing.expectEqual(TypeId.string, s.fields[1].type_id);
}

test "generic struct deduplication" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const t_te = ast.TypeExpr{ .kind = .{ .named = "T" }, .location = Location.zero };
    try checker.generic_decls.put("Box", .{ .@"struct" = .{
        .name = "Box",
        .generic_params = &.{.{ .name = "T", .bounds = &.{}, .location = Location.zero }},
        .fields = &.{
            .{ .name = "value", .type_expr = &t_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
        },
    } });

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const g1 = ast.TypeExpr{
        .kind = .{ .generic = .{ .name = "Box", .args = &.{&int_te} } },
        .location = Location.zero,
    };
    const g2 = ast.TypeExpr{
        .kind = .{ .generic = .{ .name = "Box", .args = &.{&int_te} } },
        .location = Location.zero,
    };

    const id1 = checker.resolveTypeExpr(&g1);
    const id2 = checker.resolveTypeExpr(&g2);

    // same instantiation should return the same TypeId
    try std.testing.expectEqual(id1, id2);
}

test "generic struct wrong arg count" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const a_te = ast.TypeExpr{ .kind = .{ .named = "A" }, .location = Location.zero };
    const b_te = ast.TypeExpr{ .kind = .{ .named = "B" }, .location = Location.zero };
    try checker.generic_decls.put("Pair", .{ .@"struct" = .{
        .name = "Pair",
        .generic_params = &.{
            .{ .name = "A", .bounds = &.{}, .location = Location.zero },
            .{ .name = "B", .bounds = &.{}, .location = Location.zero },
        },
        .fields = &.{
            .{ .name = "first", .type_expr = &a_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            .{ .name = "second", .type_expr = &b_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
        },
    } });

    // only provide 1 arg for a 2-param generic
    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const generic = ast.TypeExpr{
        .kind = .{ .generic = .{ .name = "Pair", .args = &.{&int_te} } },
        .location = Location.zero,
    };

    const id = checker.resolveTypeExpr(&generic);
    try std.testing.expect(id.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "unknown generic type errors" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const generic = ast.TypeExpr{
        .kind = .{ .generic = .{ .name = "Nope", .args = &.{&int_te} } },
        .location = Location.zero,
    };

    const id = checker.resolveTypeExpr(&generic);
    try std.testing.expect(id.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "Task[Int] still works after generic refactor" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const inner = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const generic = ast.TypeExpr{
        .kind = .{ .generic = .{ .name = "Task", .args = &.{&inner} } },
        .location = Location.zero,
    };
    const id = checker.resolveTypeExpr(&generic);
    try std.testing.expect(!id.isErr());

    const ty = checker.type_table.get(id).?;
    try std.testing.expectEqual(TypeId.int, ty.task.inner);
}

// -- generic enum instantiation tests --

test "Option[Int] resolves to concrete enum" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const t_te = ast.TypeExpr{ .kind = .{ .named = "T" }, .location = Location.zero };
    try checker.generic_decls.put("Option", .{ .@"enum" = .{
        .name = "Option",
        .generic_params = &.{.{ .name = "T", .bounds = &.{}, .location = Location.zero }},
        .variants = &.{
            .{ .name = "Some", .fields = &.{&t_te}, .location = Location.zero },
            .{ .name = "None", .fields = &.{}, .location = Location.zero },
        },
    } });

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const generic = ast.TypeExpr{
        .kind = .{ .generic = .{ .name = "Option", .args = &.{&int_te} } },
        .location = Location.zero,
    };

    const id = checker.resolveTypeExpr(&generic);
    try std.testing.expect(!id.isErr());
    try std.testing.expect(!checker.diagnostics.hasErrors());

    const ty = checker.type_table.get(id).?;
    const e = ty.@"enum";
    try std.testing.expectEqualStrings("Option[Int]", e.name);
    try std.testing.expectEqual(@as(usize, 2), e.variants.len);
    // Some variant should have field type Int
    try std.testing.expectEqualStrings("Some", e.variants[0].name);
    try std.testing.expectEqual(@as(usize, 1), e.variants[0].fields.len);
    try std.testing.expectEqual(TypeId.int, e.variants[0].fields[0]);
    // None variant should have no fields
    try std.testing.expectEqualStrings("None", e.variants[1].name);
    try std.testing.expectEqual(@as(usize, 0), e.variants[1].fields.len);
}

test "generic enum deduplication" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const t_te = ast.TypeExpr{ .kind = .{ .named = "T" }, .location = Location.zero };
    try checker.generic_decls.put("Option", .{ .@"enum" = .{
        .name = "Option",
        .generic_params = &.{.{ .name = "T", .bounds = &.{}, .location = Location.zero }},
        .variants = &.{
            .{ .name = "Some", .fields = &.{&t_te}, .location = Location.zero },
            .{ .name = "None", .fields = &.{}, .location = Location.zero },
        },
    } });

    const str_te = ast.TypeExpr{ .kind = .{ .named = "String" }, .location = Location.zero };
    const g1 = ast.TypeExpr{
        .kind = .{ .generic = .{ .name = "Option", .args = &.{&str_te} } },
        .location = Location.zero,
    };
    const g2 = ast.TypeExpr{
        .kind = .{ .generic = .{ .name = "Option", .args = &.{&str_te} } },
        .location = Location.zero,
    };

    const id1 = checker.resolveTypeExpr(&g1);
    const id2 = checker.resolveTypeExpr(&g2);
    try std.testing.expectEqual(id1, id2);
}

test "nested generic: Option[Option[Int]]" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const t_te = ast.TypeExpr{ .kind = .{ .named = "T" }, .location = Location.zero };
    try checker.generic_decls.put("Option", .{ .@"enum" = .{
        .name = "Option",
        .generic_params = &.{.{ .name = "T", .bounds = &.{}, .location = Location.zero }},
        .variants = &.{
            .{ .name = "Some", .fields = &.{&t_te}, .location = Location.zero },
            .{ .name = "None", .fields = &.{}, .location = Location.zero },
        },
    } });

    // Option[Option[Int]]
    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const inner_generic = ast.TypeExpr{
        .kind = .{ .generic = .{ .name = "Option", .args = &.{&int_te} } },
        .location = Location.zero,
    };
    const outer_generic = ast.TypeExpr{
        .kind = .{ .generic = .{ .name = "Option", .args = &.{&inner_generic} } },
        .location = Location.zero,
    };

    const id = checker.resolveTypeExpr(&outer_generic);
    try std.testing.expect(!id.isErr());
    try std.testing.expect(!checker.diagnostics.hasErrors());

    // the outer type should be Option[Option[Int]]
    const ty = checker.type_table.get(id).?;
    try std.testing.expectEqualStrings("Option[Option[Int]]", ty.@"enum".name);

    // the Some variant's field should be Option[Int]
    const inner_id = ty.@"enum".variants[0].fields[0];
    const inner_ty = checker.type_table.get(inner_id).?;
    try std.testing.expectEqualStrings("Option[Int]", inner_ty.@"enum".name);
}

// -- generic function tests --

test "generic function is stored without error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // fn identity[T](x: T) -> T: return x
    const t_te = ast.TypeExpr{ .kind = .{ .named = "T" }, .location = Location.zero };
    const x_expr = ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };
    const ret = ast.Stmt{
        .kind = .{ .return_stmt = .{ .value = &x_expr } },
        .location = Location.zero,
    };

    const fn_decl = ast.FnDecl{
        .name = "identity",
        .generic_params = &.{
            .{ .name = "T", .bounds = &.{}, .location = Location.zero },
        },
        .params = &.{
            .{ .name = "x", .type_expr = &t_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
        },
        .return_type = &t_te,
        .body = .{ .stmts = &.{ret}, .location = Location.zero },
    };

    const decl = ast.Decl{
        .kind = .{ .fn_decl = fn_decl },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{decl} };
    checker.check(&module);

    // should be in generic_decls, not in module scope, no errors
    try std.testing.expect(!checker.diagnostics.hasErrors());
    try std.testing.expect(checker.generic_decls.contains("identity"));
    try std.testing.expect(checker.module_scope.lookup("identity") == null);
}

test "non-generic function still registers normally" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const val = ast.Expr{ .kind = .{ .int_lit = "0" }, .location = Location.zero };
    const ret = ast.Stmt{
        .kind = .{ .return_stmt = .{ .value = &val } },
        .location = Location.zero,
    };

    const fn_decl = ast.FnDecl{
        .name = "zero",
        .generic_params = &.{},
        .params = &.{},
        .return_type = &int_te,
        .body = .{ .stmts = &.{ret}, .location = Location.zero },
    };

    const decl = ast.Decl{
        .kind = .{ .fn_decl = fn_decl },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{decl} };
    checker.check(&module);

    try std.testing.expect(!checker.diagnostics.hasErrors());
    try std.testing.expect(checker.module_scope.lookup("zero") != null);
    try std.testing.expect(!checker.generic_decls.contains("zero"));
}

test "checkIdent: generic function name returns err silently" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // store a generic function decl directly
    const t_te = ast.TypeExpr{ .kind = .{ .named = "T" }, .location = Location.zero };
    const x_expr = ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };
    const ret = ast.Stmt{
        .kind = .{ .return_stmt = .{ .value = &x_expr } },
        .location = Location.zero,
    };

    try checker.generic_decls.put("identity", .{ .function = .{
        .name = "identity",
        .generic_params = &.{
            .{ .name = "T", .bounds = &.{}, .location = Location.zero },
        },
        .params = &.{
            .{ .name = "x", .type_expr = &t_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
        },
        .return_type = &t_te,
        .body = .{ .stmts = &.{ret}, .location = Location.zero },
    } });

    // looking up "identity" should return .err but not emit a diagnostic
    const ident = ast.Expr{ .kind = .{ .ident = "identity" }, .location = Location.zero };
    const result = checker.checkExpr(&ident, &checker.module_scope);
    try std.testing.expect(result.isErr());
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

// -- type inference tests --

test "inferTypeArgs: single type param" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // fn id[T](x: T)
    const t_te = ast.TypeExpr{ .kind = .{ .named = "T" }, .location = Location.zero };
    const fn_d = ast.FnDecl{
        .name = "id",
        .generic_params = &.{
            .{ .name = "T", .bounds = &.{}, .location = Location.zero },
        },
        .params = &.{
            .{ .name = "x", .type_expr = &t_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
        },
        .return_type = &t_te,
        .body = .{ .stmts = &.{}, .location = Location.zero },
    };

    var subst = checker.inferTypeArgs(fn_d, &.{TypeId.int}, Location.zero).?;
    defer subst.deinit();

    try std.testing.expectEqual(TypeId.int, subst.get("T").?);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "inferTypeArgs: two type params" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // fn swap[A, B](x: A, y: B)
    const a_te = ast.TypeExpr{ .kind = .{ .named = "A" }, .location = Location.zero };
    const b_te = ast.TypeExpr{ .kind = .{ .named = "B" }, .location = Location.zero };
    const fn_d = ast.FnDecl{
        .name = "swap",
        .generic_params = &.{
            .{ .name = "A", .bounds = &.{}, .location = Location.zero },
            .{ .name = "B", .bounds = &.{}, .location = Location.zero },
        },
        .params = &.{
            .{ .name = "x", .type_expr = &a_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
            .{ .name = "y", .type_expr = &b_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
        },
        .return_type = &a_te,
        .body = .{ .stmts = &.{}, .location = Location.zero },
    };

    var subst = checker.inferTypeArgs(fn_d, &.{ TypeId.int, TypeId.string }, Location.zero).?;
    defer subst.deinit();

    try std.testing.expectEqual(TypeId.int, subst.get("A").?);
    try std.testing.expectEqual(TypeId.string, subst.get("B").?);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "inferTypeArgs: consistent same param" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // fn eq[T](a: T, b: T) — both args are Int, should succeed
    const t_te = ast.TypeExpr{ .kind = .{ .named = "T" }, .location = Location.zero };
    const fn_d = ast.FnDecl{
        .name = "eq",
        .generic_params = &.{
            .{ .name = "T", .bounds = &.{}, .location = Location.zero },
        },
        .params = &.{
            .{ .name = "a", .type_expr = &t_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
            .{ .name = "b", .type_expr = &t_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
        },
        .return_type = &t_te,
        .body = .{ .stmts = &.{}, .location = Location.zero },
    };

    var subst = checker.inferTypeArgs(fn_d, &.{ TypeId.int, TypeId.int }, Location.zero).?;
    defer subst.deinit();

    try std.testing.expectEqual(TypeId.int, subst.get("T").?);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "inferTypeArgs: conflicting inference" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // fn eq[T](a: T, b: T) — Int and String should conflict
    const t_te = ast.TypeExpr{ .kind = .{ .named = "T" }, .location = Location.zero };
    const fn_d = ast.FnDecl{
        .name = "eq",
        .generic_params = &.{
            .{ .name = "T", .bounds = &.{}, .location = Location.zero },
        },
        .params = &.{
            .{ .name = "a", .type_expr = &t_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
            .{ .name = "b", .type_expr = &t_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
        },
        .return_type = &t_te,
        .body = .{ .stmts = &.{}, .location = Location.zero },
    };

    const result = checker.inferTypeArgs(fn_d, &.{ TypeId.int, TypeId.string }, Location.zero);
    try std.testing.expect(result == null);
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "inferTypeArgs: uninferred param" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // fn foo[T, U](x: T) — U can't be inferred from args
    const t_te = ast.TypeExpr{ .kind = .{ .named = "T" }, .location = Location.zero };
    const fn_d = ast.FnDecl{
        .name = "foo",
        .generic_params = &.{
            .{ .name = "T", .bounds = &.{}, .location = Location.zero },
            .{ .name = "U", .bounds = &.{}, .location = Location.zero },
        },
        .params = &.{
            .{ .name = "x", .type_expr = &t_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
        },
        .return_type = &t_te,
        .body = .{ .stmts = &.{}, .location = Location.zero },
    };

    const result = checker.inferTypeArgs(fn_d, &.{TypeId.int}, Location.zero);
    try std.testing.expect(result == null);
    try std.testing.expect(checker.diagnostics.hasErrors());
}

// -- generic function instantiation tests --

test "instantiateGenericFn: concrete function type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // fn identity[T](x: T) -> T
    const t_te = ast.TypeExpr{ .kind = .{ .named = "T" }, .location = Location.zero };
    const fn_d = ast.FnDecl{
        .name = "identity",
        .generic_params = &.{
            .{ .name = "T", .bounds = &.{}, .location = Location.zero },
        },
        .params = &.{
            .{ .name = "x", .type_expr = &t_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
        },
        .return_type = &t_te,
        .body = .{ .stmts = &.{}, .location = Location.zero },
    };

    var subst = std.StringHashMap(TypeId).init(std.testing.allocator);
    defer subst.deinit();
    try subst.put("T", TypeId.int);

    const fn_type_id = checker.instantiateGenericFn(fn_d, &subst, &.{TypeId.int});
    try std.testing.expect(!fn_type_id.isErr());

    const ty = checker.type_table.get(fn_type_id).?;
    const func = ty.function;
    try std.testing.expectEqual(@as(usize, 1), func.param_types.len);
    try std.testing.expectEqual(TypeId.int, func.param_types[0]);
    try std.testing.expectEqual(TypeId.int, func.return_type);
}

test "instantiateGenericFn: deduplication" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const t_te = ast.TypeExpr{ .kind = .{ .named = "T" }, .location = Location.zero };
    const fn_d = ast.FnDecl{
        .name = "identity",
        .generic_params = &.{
            .{ .name = "T", .bounds = &.{}, .location = Location.zero },
        },
        .params = &.{
            .{ .name = "x", .type_expr = &t_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
        },
        .return_type = &t_te,
        .body = .{ .stmts = &.{}, .location = Location.zero },
    };

    var subst = std.StringHashMap(TypeId).init(std.testing.allocator);
    defer subst.deinit();
    try subst.put("T", TypeId.int);

    const first = checker.instantiateGenericFn(fn_d, &subst, &.{TypeId.int});
    const second = checker.instantiateGenericFn(fn_d, &subst, &.{TypeId.int});
    try std.testing.expectEqual(first, second);
}

test "instantiateGenericFn: multiple type params" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // fn swap[A, B](x: A, y: B) -> B
    const a_te = ast.TypeExpr{ .kind = .{ .named = "A" }, .location = Location.zero };
    const b_te = ast.TypeExpr{ .kind = .{ .named = "B" }, .location = Location.zero };
    const fn_d = ast.FnDecl{
        .name = "swap",
        .generic_params = &.{
            .{ .name = "A", .bounds = &.{}, .location = Location.zero },
            .{ .name = "B", .bounds = &.{}, .location = Location.zero },
        },
        .params = &.{
            .{ .name = "x", .type_expr = &a_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
            .{ .name = "y", .type_expr = &b_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
        },
        .return_type = &b_te,
        .body = .{ .stmts = &.{}, .location = Location.zero },
    };

    var subst = std.StringHashMap(TypeId).init(std.testing.allocator);
    defer subst.deinit();
    try subst.put("A", TypeId.int);
    try subst.put("B", TypeId.string);

    const fn_type_id = checker.instantiateGenericFn(fn_d, &subst, &.{ TypeId.int, TypeId.string });
    try std.testing.expect(!fn_type_id.isErr());

    const ty = checker.type_table.get(fn_type_id).?;
    const func = ty.function;
    try std.testing.expectEqual(@as(usize, 2), func.param_types.len);
    try std.testing.expectEqual(TypeId.int, func.param_types[0]);
    try std.testing.expectEqual(TypeId.string, func.param_types[1]);
    try std.testing.expectEqual(TypeId.string, func.return_type);
}

// -- generic function call (end-to-end) tests --

fn makeGenericIdentityModule() ast.Module {
    const t_te = &ast.TypeExpr{ .kind = .{ .named = "T" }, .location = Location.zero };
    const x_expr = &ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };
    const ret = ast.Stmt{
        .kind = .{ .return_stmt = .{ .value = x_expr } },
        .location = Location.zero,
    };

    return .{
        .imports = &.{},
        .decls = &.{ast.Decl{
            .kind = .{ .fn_decl = .{
                .name = "identity",
                .generic_params = &.{
                    .{ .name = "T", .bounds = &.{}, .location = Location.zero },
                },
                .params = &.{
                    .{ .name = "x", .type_expr = t_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
                },
                .return_type = t_te,
                .body = .{ .stmts = &.{ret}, .location = Location.zero },
            } },
            .is_pub = false,
            .location = Location.zero,
        }},
    };
}

test "generic function call: identity(42) returns Int" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const module = makeGenericIdentityModule();
    checker.check(&module);

    const callee = ast.Expr{ .kind = .{ .ident = "identity" }, .location = Location.zero };
    const arg_val = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    const call = ast.Expr{
        .kind = .{ .call = .{
            .callee = &callee,
            .args = &.{.{ .name = null, .value = &arg_val, .location = Location.zero }},
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&call, &checker.module_scope);
    try std.testing.expectEqual(TypeId.int, result);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "generic function call: identity(\"hello\") returns String" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const module = makeGenericIdentityModule();
    checker.check(&module);

    const callee = ast.Expr{ .kind = .{ .ident = "identity" }, .location = Location.zero };
    const arg_val = ast.Expr{ .kind = .{ .string_lit = "hello" }, .location = Location.zero };
    const call = ast.Expr{
        .kind = .{ .call = .{
            .callee = &callee,
            .args = &.{.{ .name = null, .value = &arg_val, .location = Location.zero }},
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&call, &checker.module_scope);
    try std.testing.expectEqual(TypeId.string, result);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "generic function call: wrong arg count" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const module = makeGenericIdentityModule();
    checker.check(&module);

    // identity(1, 2) — too many args
    const callee = ast.Expr{ .kind = .{ .ident = "identity" }, .location = Location.zero };
    const arg1 = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const arg2 = ast.Expr{ .kind = .{ .int_lit = "2" }, .location = Location.zero };
    const call = ast.Expr{
        .kind = .{ .call = .{
            .callee = &callee,
            .args = &.{
                .{ .name = null, .value = &arg1, .location = Location.zero },
                .{ .name = null, .value = &arg2, .location = Location.zero },
            },
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&call, &checker.module_scope);
    try std.testing.expect(result.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "generic function call: conflicting type params" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // fn eq[T](a: T, b: T) -> T: return a
    const t_te = ast.TypeExpr{ .kind = .{ .named = "T" }, .location = Location.zero };
    const a_expr = ast.Expr{ .kind = .{ .ident = "a" }, .location = Location.zero };
    const ret = ast.Stmt{
        .kind = .{ .return_stmt = .{ .value = &a_expr } },
        .location = Location.zero,
    };

    const decl = ast.Decl{
        .kind = .{ .fn_decl = .{
            .name = "eq",
            .generic_params = &.{
                .{ .name = "T", .bounds = &.{}, .location = Location.zero },
            },
            .params = &.{
                .{ .name = "a", .type_expr = &t_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
                .{ .name = "b", .type_expr = &t_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
            },
            .return_type = &t_te,
            .body = .{ .stmts = &.{ret}, .location = Location.zero },
        } },
        .is_pub = false,
        .location = Location.zero,
    };
    const module = ast.Module{ .imports = &.{}, .decls = &.{decl} };
    checker.check(&module);

    // eq(42, "hello") — T can't be both Int and String
    const callee = ast.Expr{ .kind = .{ .ident = "eq" }, .location = Location.zero };
    const arg1 = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    const arg2 = ast.Expr{ .kind = .{ .string_lit = "hello" }, .location = Location.zero };
    const call = ast.Expr{
        .kind = .{ .call = .{
            .callee = &callee,
            .args = &.{
                .{ .name = null, .value = &arg1, .location = Location.zero },
                .{ .name = null, .value = &arg2, .location = Location.zero },
            },
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&call, &checker.module_scope);
    try std.testing.expect(result.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "generic function call: two type params" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // fn second[A, B](x: A, y: B) -> B: return y
    const a_te = ast.TypeExpr{ .kind = .{ .named = "A" }, .location = Location.zero };
    const b_te = ast.TypeExpr{ .kind = .{ .named = "B" }, .location = Location.zero };
    const y_expr = ast.Expr{ .kind = .{ .ident = "y" }, .location = Location.zero };
    const ret = ast.Stmt{
        .kind = .{ .return_stmt = .{ .value = &y_expr } },
        .location = Location.zero,
    };

    const decl = ast.Decl{
        .kind = .{ .fn_decl = .{
            .name = "second",
            .generic_params = &.{
                .{ .name = "A", .bounds = &.{}, .location = Location.zero },
                .{ .name = "B", .bounds = &.{}, .location = Location.zero },
            },
            .params = &.{
                .{ .name = "x", .type_expr = &a_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
                .{ .name = "y", .type_expr = &b_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
            },
            .return_type = &b_te,
            .body = .{ .stmts = &.{ret}, .location = Location.zero },
        } },
        .is_pub = false,
        .location = Location.zero,
    };
    const module = ast.Module{ .imports = &.{}, .decls = &.{decl} };
    checker.check(&module);

    // second(42, "hello") should return String
    const callee = ast.Expr{ .kind = .{ .ident = "second" }, .location = Location.zero };
    const arg1 = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    const arg2 = ast.Expr{ .kind = .{ .string_lit = "hello" }, .location = Location.zero };
    const call = ast.Expr{
        .kind = .{ .call = .{
            .callee = &callee,
            .args = &.{
                .{ .name = null, .value = &arg1, .location = Location.zero },
                .{ .name = null, .value = &arg2, .location = Location.zero },
            },
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&call, &checker.module_scope);
    try std.testing.expectEqual(TypeId.string, result);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "registerInterfaceDecl: registers interface name" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const decl = ast.Decl{
        .kind = .{ .interface_decl = .{
            .name = "Display",
            .generic_params = &.{},
            .methods = &.{},
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{decl} };
    checker.check(&module);

    try std.testing.expect(!checker.diagnostics.hasErrors());
    // should be in both the type table and interface_decls
    try std.testing.expect(checker.type_table.lookup("Display") != null);
    try std.testing.expect(checker.interface_decls.contains("Display"));
}

test "registerInterfaceDecl: generic interface errors" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const decl = ast.Decl{
        .kind = .{ .interface_decl = .{
            .name = "Iter",
            .generic_params = &.{
                .{ .name = "T", .bounds = &.{}, .location = Location.zero },
            },
            .methods = &.{},
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{decl} };
    checker.check(&module);

    try std.testing.expect(checker.diagnostics.hasErrors());
    // should NOT be registered
    try std.testing.expect(checker.type_table.lookup("Iter") == null);
    try std.testing.expect(!checker.interface_decls.contains("Iter"));
}

test "registerImplDecl: records impl relationship" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const display_te = ast.TypeExpr{ .kind = .{ .named = "Display" }, .location = Location.zero };
    const point_te = ast.TypeExpr{ .kind = .{ .named = "Point" }, .location = Location.zero };

    // interface Display:
    const iface_decl = ast.Decl{
        .kind = .{ .interface_decl = .{
            .name = "Display",
            .generic_params = &.{},
            .methods = &.{},
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    // struct Point: pub x: Int
    const struct_decl = ast.Decl{
        .kind = .{ .struct_decl = .{
            .name = "Point",
            .generic_params = &.{},
            .fields = &.{
                .{ .name = "x", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    // impl Display for Point:
    //   fn show() -> String: return "point"
    const impl_decl = ast.Decl{
        .kind = .{ .impl_decl = .{
            .target = &display_te,
            .interface = &point_te,
            .methods = &.{},
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{ iface_decl, struct_decl, impl_decl } };
    checker.check(&module);

    try std.testing.expect(!checker.diagnostics.hasErrors());
    try std.testing.expect(checker.typeImplements("Point", "Display"));
    // Point should not "implement" something it doesn't
    try std.testing.expect(!checker.typeImplements("Point", "Hash"));
}

test "registerImplDecl: unknown interface errors" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const unknown_te = ast.TypeExpr{ .kind = .{ .named = "Unknown" }, .location = Location.zero };
    const point_te = ast.TypeExpr{ .kind = .{ .named = "Point" }, .location = Location.zero };

    // struct Point: pub x: Int
    const struct_decl = ast.Decl{
        .kind = .{ .struct_decl = .{
            .name = "Point",
            .generic_params = &.{},
            .fields = &.{
                .{ .name = "x", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    // impl Unknown for Point: — Unknown is not a declared interface
    const impl_decl = ast.Decl{
        .kind = .{ .impl_decl = .{
            .target = &unknown_te,
            .interface = &point_te,
            .methods = &.{},
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{ struct_decl, impl_decl } };
    checker.check(&module);

    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "registerImplDecl: plain impl with no methods" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const point_te = ast.TypeExpr{ .kind = .{ .named = "Point" }, .location = Location.zero };

    // struct Point: pub x: Int
    const struct_decl = ast.Decl{
        .kind = .{ .struct_decl = .{
            .name = "Point",
            .generic_params = &.{},
            .fields = &.{
                .{ .name = "x", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    // impl Point: — no interface, just methods
    const impl_decl = ast.Decl{
        .kind = .{ .impl_decl = .{
            .target = &point_te,
            .interface = null,
            .methods = &.{},
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{ struct_decl, impl_decl } };
    checker.check(&module);

    // plain impl shouldn't produce errors
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "registerImplDecl: plain impl registers method" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const point_te = ast.TypeExpr{ .kind = .{ .named = "Point" }, .location = Location.zero };

    // struct Point: pub x: Int
    const struct_decl = ast.Decl{
        .kind = .{ .struct_decl = .{
            .name = "Point",
            .generic_params = &.{},
            .fields = &.{
                .{ .name = "x", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const ret_expr = ast.Expr{ .kind = .{ .ident = "a" }, .location = Location.zero };
    const ret_stmt = ast.Stmt{
        .kind = .{ .return_stmt = .{ .value = &ret_expr } },
        .location = Location.zero,
    };

    // impl Point:
    //   fn magnitude(a: Int) -> Int: return a
    const impl_decl = ast.Decl{
        .kind = .{ .impl_decl = .{
            .target = &point_te,
            .interface = null,
            .methods = &.{.{
                .is_pub = false,
                .decl = .{
                    .name = "magnitude",
                    .generic_params = &.{},
                    .params = &.{
                        .{ .name = "a", .type_expr = &int_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
                    },
                    .return_type = &int_te,
                    .body = .{ .stmts = &.{ret_stmt}, .location = Location.zero },
                },
                .location = Location.zero,
            }},
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{ struct_decl, impl_decl } };
    checker.check(&module);

    try std.testing.expect(!checker.diagnostics.hasErrors());
    // method should be registered
    const key = checker.buildMethodKey("Point", "magnitude");
    const entry = checker.method_types.get(key);
    try std.testing.expect(entry != null);
    // method should be a function type returning Int
    const fn_type = checker.type_table.get(entry.?.type_id).?;
    try std.testing.expectEqual(TypeId.int, fn_type.function.return_type);
}

test "registerImplDecl: interface impl registers method" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const string_te = ast.TypeExpr{ .kind = .{ .named = "String" }, .location = Location.zero };
    const display_te = ast.TypeExpr{ .kind = .{ .named = "Display" }, .location = Location.zero };
    const point_te = ast.TypeExpr{ .kind = .{ .named = "Point" }, .location = Location.zero };

    const ret_expr = ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };
    const ret_stmt = ast.Stmt{
        .kind = .{ .return_stmt = .{ .value = &ret_expr } },
        .location = Location.zero,
    };

    // interface Display:
    //   fn show(x: String) -> String
    const iface_decl = ast.Decl{
        .kind = .{ .interface_decl = .{
            .name = "Display",
            .generic_params = &.{},
            .methods = &.{},
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    // struct Point: pub x: Int
    const struct_decl = ast.Decl{
        .kind = .{ .struct_decl = .{
            .name = "Point",
            .generic_params = &.{},
            .fields = &.{
                .{ .name = "x", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    // impl Display for Point:
    //   fn show(x: String) -> String: return x
    const impl_decl = ast.Decl{
        .kind = .{ .impl_decl = .{
            .target = &display_te,
            .interface = &point_te,
            .methods = &.{.{
                .is_pub = false,
                .decl = .{
                    .name = "show",
                    .generic_params = &.{},
                    .params = &.{
                        .{ .name = "x", .type_expr = &string_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
                    },
                    .return_type = &string_te,
                    .body = .{ .stmts = &.{ret_stmt}, .location = Location.zero },
                },
                .location = Location.zero,
            }},
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{ iface_decl, struct_decl, impl_decl } };
    checker.check(&module);

    try std.testing.expect(!checker.diagnostics.hasErrors());
    // impl relationship should still be tracked
    try std.testing.expect(checker.typeImplements("Point", "Display"));
    // method should be registered
    const key = checker.buildMethodKey("Point", "show");
    const entry = checker.method_types.get(key);
    try std.testing.expect(entry != null);
    const fn_type = checker.type_table.get(entry.?.type_id).?;
    try std.testing.expectEqual(TypeId.string, fn_type.function.return_type);
}

test "registerImplDecl: plain impl for unknown type errors" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const unknown_te = ast.TypeExpr{ .kind = .{ .named = "Unknown" }, .location = Location.zero };

    // impl Unknown: — Unknown is not a declared type
    const impl_decl = ast.Decl{
        .kind = .{ .impl_decl = .{
            .target = &unknown_te,
            .interface = null,
            .methods = &.{},
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{impl_decl} };
    checker.check(&module);

    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "checkMethodCall: resolves correctly" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const point_te = ast.TypeExpr{ .kind = .{ .named = "Point" }, .location = Location.zero };

    const ret_expr = ast.Expr{ .kind = .{ .ident = "a" }, .location = Location.zero };
    const ret_stmt = ast.Stmt{
        .kind = .{ .return_stmt = .{ .value = &ret_expr } },
        .location = Location.zero,
    };

    const struct_decl = ast.Decl{
        .kind = .{ .struct_decl = .{
            .name = "Point",
            .generic_params = &.{},
            .fields = &.{
                .{ .name = "x", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    // impl Point: fn magnitude(a: Int) -> Int: return a
    const impl_decl = ast.Decl{
        .kind = .{ .impl_decl = .{
            .target = &point_te,
            .interface = null,
            .methods = &.{.{
                .is_pub = false,
                .decl = .{
                    .name = "magnitude",
                    .generic_params = &.{},
                    .params = &.{
                        .{ .name = "a", .type_expr = &int_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
                    },
                    .return_type = &int_te,
                    .body = .{ .stmts = &.{ret_stmt}, .location = Location.zero },
                },
                .location = Location.zero,
            }},
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    // fn main(): p := Point(1) \n m := p.magnitude(5)
    const one = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const five = ast.Expr{ .kind = .{ .int_lit = "5" }, .location = Location.zero };
    const p_ident = ast.Expr{ .kind = .{ .ident = "Point" }, .location = Location.zero };
    const p_call = ast.Expr{
        .kind = .{ .call = .{ .callee = &p_ident, .args = &.{.{ .name = null, .value = &one, .location = Location.zero }} } },
        .location = Location.zero,
    };
    const p_bind = ast.Stmt{
        .kind = .{ .binding = .{ .name = "p", .type_expr = null, .value = &p_call, .is_mut = false } },
        .location = Location.zero,
    };

    const p_ref = ast.Expr{ .kind = .{ .ident = "p" }, .location = Location.zero };
    const method_call = ast.Expr{
        .kind = .{ .method_call = .{
            .receiver = &p_ref,
            .method = "magnitude",
            .args = &.{.{ .name = null, .value = &five, .location = Location.zero }},
        } },
        .location = Location.zero,
    };
    const m_bind = ast.Stmt{
        .kind = .{ .binding = .{ .name = "m", .type_expr = null, .value = &method_call, .is_mut = false } },
        .location = Location.zero,
    };

    const fn_decl = ast.Decl{
        .kind = .{ .fn_decl = .{
            .name = "main",
            .generic_params = &.{},
            .params = &.{},
            .return_type = null,
            .body = .{ .stmts = &.{ p_bind, m_bind }, .location = Location.zero },
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{ struct_decl, impl_decl, fn_decl } };
    checker.check(&module);

    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "checkMethodCall: unknown method errors" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const point_te = ast.TypeExpr{ .kind = .{ .named = "Point" }, .location = Location.zero };

    const struct_decl = ast.Decl{
        .kind = .{ .struct_decl = .{
            .name = "Point",
            .generic_params = &.{},
            .fields = &.{
                .{ .name = "x", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    // impl Point: (no methods)
    const impl_decl = ast.Decl{
        .kind = .{ .impl_decl = .{
            .target = &point_te,
            .interface = null,
            .methods = &.{},
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    // fn main(): p := Point(1) \n p.nonexistent(5)
    const one = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const five = ast.Expr{ .kind = .{ .int_lit = "5" }, .location = Location.zero };
    const p_ident = ast.Expr{ .kind = .{ .ident = "Point" }, .location = Location.zero };
    const p_call = ast.Expr{
        .kind = .{ .call = .{ .callee = &p_ident, .args = &.{.{ .name = null, .value = &one, .location = Location.zero }} } },
        .location = Location.zero,
    };
    const p_bind = ast.Stmt{
        .kind = .{ .binding = .{ .name = "p", .type_expr = null, .value = &p_call, .is_mut = false } },
        .location = Location.zero,
    };

    const p_ref = ast.Expr{ .kind = .{ .ident = "p" }, .location = Location.zero };
    const method_call = ast.Expr{
        .kind = .{ .method_call = .{
            .receiver = &p_ref,
            .method = "nonexistent",
            .args = &.{.{ .name = null, .value = &five, .location = Location.zero }},
        } },
        .location = Location.zero,
    };
    const call_stmt = ast.Stmt{
        .kind = .{ .expr_stmt = &method_call },
        .location = Location.zero,
    };

    const fn_decl = ast.Decl{
        .kind = .{ .fn_decl = .{
            .name = "main",
            .generic_params = &.{},
            .params = &.{},
            .return_type = null,
            .body = .{ .stmts = &.{ p_bind, call_stmt }, .location = Location.zero },
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{ struct_decl, impl_decl, fn_decl } };
    checker.check(&module);

    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "checkMethodCall: wrong arg count errors" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const point_te = ast.TypeExpr{ .kind = .{ .named = "Point" }, .location = Location.zero };

    const ret_expr = ast.Expr{ .kind = .{ .ident = "a" }, .location = Location.zero };
    const ret_stmt = ast.Stmt{
        .kind = .{ .return_stmt = .{ .value = &ret_expr } },
        .location = Location.zero,
    };

    const struct_decl = ast.Decl{
        .kind = .{ .struct_decl = .{
            .name = "Point",
            .generic_params = &.{},
            .fields = &.{
                .{ .name = "x", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    // impl Point: fn magnitude(a: Int) -> Int: return a
    const impl_decl = ast.Decl{
        .kind = .{ .impl_decl = .{
            .target = &point_te,
            .interface = null,
            .methods = &.{.{
                .is_pub = false,
                .decl = .{
                    .name = "magnitude",
                    .generic_params = &.{},
                    .params = &.{
                        .{ .name = "a", .type_expr = &int_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
                    },
                    .return_type = &int_te,
                    .body = .{ .stmts = &.{ret_stmt}, .location = Location.zero },
                },
                .location = Location.zero,
            }},
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    // p.magnitude() — missing argument
    const one = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const p_ident = ast.Expr{ .kind = .{ .ident = "Point" }, .location = Location.zero };
    const p_call = ast.Expr{
        .kind = .{ .call = .{ .callee = &p_ident, .args = &.{.{ .name = null, .value = &one, .location = Location.zero }} } },
        .location = Location.zero,
    };
    const p_bind = ast.Stmt{
        .kind = .{ .binding = .{ .name = "p", .type_expr = null, .value = &p_call, .is_mut = false } },
        .location = Location.zero,
    };

    const p_ref = ast.Expr{ .kind = .{ .ident = "p" }, .location = Location.zero };
    const method_call = ast.Expr{
        .kind = .{
            .method_call = .{
                .receiver = &p_ref,
                .method = "magnitude",
                .args = &.{}, // no args — should be 1
            },
        },
        .location = Location.zero,
    };
    const call_stmt = ast.Stmt{
        .kind = .{ .expr_stmt = &method_call },
        .location = Location.zero,
    };

    const fn_decl = ast.Decl{
        .kind = .{ .fn_decl = .{
            .name = "main",
            .generic_params = &.{},
            .params = &.{},
            .return_type = null,
            .body = .{ .stmts = &.{ p_bind, call_stmt }, .location = Location.zero },
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{ struct_decl, impl_decl, fn_decl } };
    checker.check(&module);

    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "checkMethodCall: wrong arg type errors" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const point_te = ast.TypeExpr{ .kind = .{ .named = "Point" }, .location = Location.zero };

    const ret_expr = ast.Expr{ .kind = .{ .ident = "a" }, .location = Location.zero };
    const ret_stmt = ast.Stmt{
        .kind = .{ .return_stmt = .{ .value = &ret_expr } },
        .location = Location.zero,
    };

    const struct_decl = ast.Decl{
        .kind = .{ .struct_decl = .{
            .name = "Point",
            .generic_params = &.{},
            .fields = &.{
                .{ .name = "x", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    // impl Point: fn magnitude(a: Int) -> Int: return a
    const impl_decl = ast.Decl{
        .kind = .{ .impl_decl = .{
            .target = &point_te,
            .interface = null,
            .methods = &.{.{
                .is_pub = false,
                .decl = .{
                    .name = "magnitude",
                    .generic_params = &.{},
                    .params = &.{
                        .{ .name = "a", .type_expr = &int_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
                    },
                    .return_type = &int_te,
                    .body = .{ .stmts = &.{ret_stmt}, .location = Location.zero },
                },
                .location = Location.zero,
            }},
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    // p.magnitude("hello") — String instead of Int
    const one = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const wrong_arg = ast.Expr{ .kind = .{ .string_lit = "hello" }, .location = Location.zero };
    const p_ident = ast.Expr{ .kind = .{ .ident = "Point" }, .location = Location.zero };
    const p_call = ast.Expr{
        .kind = .{ .call = .{ .callee = &p_ident, .args = &.{.{ .name = null, .value = &one, .location = Location.zero }} } },
        .location = Location.zero,
    };
    const p_bind = ast.Stmt{
        .kind = .{ .binding = .{ .name = "p", .type_expr = null, .value = &p_call, .is_mut = false } },
        .location = Location.zero,
    };

    const p_ref = ast.Expr{ .kind = .{ .ident = "p" }, .location = Location.zero };
    const method_call = ast.Expr{
        .kind = .{ .method_call = .{
            .receiver = &p_ref,
            .method = "magnitude",
            .args = &.{.{ .name = null, .value = &wrong_arg, .location = Location.zero }},
        } },
        .location = Location.zero,
    };
    const call_stmt = ast.Stmt{
        .kind = .{ .expr_stmt = &method_call },
        .location = Location.zero,
    };

    const fn_decl = ast.Decl{
        .kind = .{ .fn_decl = .{
            .name = "main",
            .generic_params = &.{},
            .params = &.{},
            .return_type = null,
            .body = .{ .stmts = &.{ p_bind, call_stmt }, .location = Location.zero },
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{ struct_decl, impl_decl, fn_decl } };
    checker.check(&module);

    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "checkImplDecl: method body type checks correctly" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const point_te = ast.TypeExpr{ .kind = .{ .named = "Point" }, .location = Location.zero };

    // method body: return a (where a is Int, return type is Int) — valid
    const ret_expr = ast.Expr{ .kind = .{ .ident = "a" }, .location = Location.zero };
    const ret_stmt = ast.Stmt{
        .kind = .{ .return_stmt = .{ .value = &ret_expr } },
        .location = Location.zero,
    };

    const struct_decl = ast.Decl{
        .kind = .{ .struct_decl = .{
            .name = "Point",
            .generic_params = &.{},
            .fields = &.{
                .{ .name = "x", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const impl_decl = ast.Decl{
        .kind = .{ .impl_decl = .{
            .target = &point_te,
            .interface = null,
            .methods = &.{.{
                .is_pub = false,
                .decl = .{
                    .name = "magnitude",
                    .generic_params = &.{},
                    .params = &.{
                        .{ .name = "a", .type_expr = &int_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
                    },
                    .return_type = &int_te,
                    .body = .{ .stmts = &.{ret_stmt}, .location = Location.zero },
                },
                .location = Location.zero,
            }},
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{ struct_decl, impl_decl } };
    checker.check(&module);

    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "checkImplDecl: method body return type mismatch errors" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const string_te = ast.TypeExpr{ .kind = .{ .named = "String" }, .location = Location.zero };
    const point_te = ast.TypeExpr{ .kind = .{ .named = "Point" }, .location = Location.zero };

    // method body: return "hello" (String) but return type is Int — mismatch
    const ret_expr = ast.Expr{ .kind = .{ .string_lit = "hello" }, .location = Location.zero };
    const ret_stmt = ast.Stmt{
        .kind = .{ .return_stmt = .{ .value = &ret_expr } },
        .location = Location.zero,
    };

    const struct_decl = ast.Decl{
        .kind = .{ .struct_decl = .{
            .name = "Point",
            .generic_params = &.{},
            .fields = &.{
                .{ .name = "x", .type_expr = &int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            },
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const impl_decl = ast.Decl{
        .kind = .{ .impl_decl = .{
            .target = &point_te,
            .interface = null,
            .methods = &.{.{
                .is_pub = false,
                .decl = .{
                    .name = "magnitude",
                    .generic_params = &.{},
                    .params = &.{
                        .{ .name = "a", .type_expr = &string_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
                    },
                    .return_type = &int_te,
                    .body = .{ .stmts = &.{ret_stmt}, .location = Location.zero },
                },
                .location = Location.zero,
            }},
        } },
        .is_pub = false,
        .location = Location.zero,
    };

    const module = ast.Module{ .imports = &.{}, .decls = &.{ struct_decl, impl_decl } };
    checker.check(&module);

    try std.testing.expect(checker.diagnostics.hasErrors());
}

// helper: build a module with an interface, a struct, an impl, and a bounded generic function.
// interface Display:
//   fn show(self) -> String
// struct Point: pub x: Int
// impl Display for Point:
//   (methods omitted — we only track the relationship)
// fn show[T: Display](x: T) -> String: return "shown"
fn makeBoundedGenericModule() ast.Module {
    const int_te = &ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const string_te = &ast.TypeExpr{ .kind = .{ .named = "String" }, .location = Location.zero };
    const t_te = &ast.TypeExpr{ .kind = .{ .named = "T" }, .location = Location.zero };
    const display_te = &ast.TypeExpr{ .kind = .{ .named = "Display" }, .location = Location.zero };
    const point_te = &ast.TypeExpr{ .kind = .{ .named = "Point" }, .location = Location.zero };

    const ret_expr = &ast.Expr{ .kind = .{ .string_lit = "shown" }, .location = Location.zero };
    const ret_stmt = ast.Stmt{
        .kind = .{ .return_stmt = .{ .value = ret_expr } },
        .location = Location.zero,
    };

    return .{
        .imports = &.{},
        .decls = &.{
            // interface Display:
            ast.Decl{
                .kind = .{ .interface_decl = .{
                    .name = "Display",
                    .generic_params = &.{},
                    .methods = &.{},
                } },
                .is_pub = false,
                .location = Location.zero,
            },
            // struct Point: pub x: Int
            ast.Decl{
                .kind = .{ .struct_decl = .{
                    .name = "Point",
                    .generic_params = &.{},
                    .fields = &.{
                        .{ .name = "x", .type_expr = int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
                    },
                } },
                .is_pub = false,
                .location = Location.zero,
            },
            // impl Display for Point:
            ast.Decl{
                .kind = .{ .impl_decl = .{
                    .target = display_te,
                    .interface = point_te,
                    .methods = &.{},
                } },
                .is_pub = false,
                .location = Location.zero,
            },
            // fn show[T: Display](x: T) -> String: return "shown"
            ast.Decl{
                .kind = .{ .fn_decl = .{
                    .name = "show",
                    .generic_params = &.{
                        .{ .name = "T", .bounds = &.{display_te}, .location = Location.zero },
                    },
                    .params = &.{
                        .{ .name = "x", .type_expr = t_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
                    },
                    .return_type = string_te,
                    .body = .{ .stmts = &.{ret_stmt}, .location = Location.zero },
                } },
                .is_pub = false,
                .location = Location.zero,
            },
        },
    };
}

test "generic fn call: bound satisfied" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const module = makeBoundedGenericModule();
    checker.check(&module);

    // show(Point(1)) — Point implements Display, so this should work
    const callee = ast.Expr{ .kind = .{ .ident = "show" }, .location = Location.zero };
    const arg_inner_callee = ast.Expr{ .kind = .{ .ident = "Point" }, .location = Location.zero };
    const arg_inner_val = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const arg_val = ast.Expr{
        .kind = .{ .call = .{
            .callee = &arg_inner_callee,
            .args = &.{.{ .name = null, .value = &arg_inner_val, .location = Location.zero }},
        } },
        .location = Location.zero,
    };
    const call = ast.Expr{
        .kind = .{ .call = .{
            .callee = &callee,
            .args = &.{.{ .name = null, .value = &arg_val, .location = Location.zero }},
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&call, &checker.module_scope);
    try std.testing.expectEqual(TypeId.string, result);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "generic fn call: bound not satisfied" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const module = makeBoundedGenericModule();
    checker.check(&module);

    // show(42) — Int does not implement Display
    const callee = ast.Expr{ .kind = .{ .ident = "show" }, .location = Location.zero };
    const arg_val = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    const call = ast.Expr{
        .kind = .{ .call = .{
            .callee = &callee,
            .args = &.{.{ .name = null, .value = &arg_val, .location = Location.zero }},
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&call, &checker.module_scope);
    try std.testing.expect(result.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "generic fn call: multiple bounds satisfied" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = &ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const string_te = &ast.TypeExpr{ .kind = .{ .named = "String" }, .location = Location.zero };
    const t_te = &ast.TypeExpr{ .kind = .{ .named = "T" }, .location = Location.zero };
    const display_te = &ast.TypeExpr{ .kind = .{ .named = "Display" }, .location = Location.zero };
    const hash_te = &ast.TypeExpr{ .kind = .{ .named = "Hash" }, .location = Location.zero };
    const point_te = &ast.TypeExpr{ .kind = .{ .named = "Point" }, .location = Location.zero };

    const ret_expr = &ast.Expr{ .kind = .{ .string_lit = "ok" }, .location = Location.zero };
    const ret_stmt = ast.Stmt{
        .kind = .{ .return_stmt = .{ .value = ret_expr } },
        .location = Location.zero,
    };

    const module = ast.Module{
        .imports = &.{},
        .decls = &.{
            // interface Display:
            ast.Decl{ .kind = .{ .interface_decl = .{ .name = "Display", .generic_params = &.{}, .methods = &.{} } }, .is_pub = false, .location = Location.zero },
            // interface Hash:
            ast.Decl{ .kind = .{ .interface_decl = .{ .name = "Hash", .generic_params = &.{}, .methods = &.{} } }, .is_pub = false, .location = Location.zero },
            // struct Point: pub x: Int
            ast.Decl{
                .kind = .{ .struct_decl = .{
                    .name = "Point",
                    .generic_params = &.{},
                    .fields = &.{
                        .{ .name = "x", .type_expr = int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
                    },
                } },
                .is_pub = false,
                .location = Location.zero,
            },
            // impl Display for Point:
            ast.Decl{ .kind = .{ .impl_decl = .{ .target = display_te, .interface = point_te, .methods = &.{} } }, .is_pub = false, .location = Location.zero },
            // impl Hash for Point:
            ast.Decl{ .kind = .{ .impl_decl = .{ .target = hash_te, .interface = point_te, .methods = &.{} } }, .is_pub = false, .location = Location.zero },
            // fn show_hash[T: Display + Hash](x: T) -> String: return "ok"
            ast.Decl{
                .kind = .{ .fn_decl = .{
                    .name = "show_hash",
                    .generic_params = &.{
                        .{ .name = "T", .bounds = &.{ display_te, hash_te }, .location = Location.zero },
                    },
                    .params = &.{
                        .{ .name = "x", .type_expr = t_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
                    },
                    .return_type = string_te,
                    .body = .{ .stmts = &.{ret_stmt}, .location = Location.zero },
                } },
                .is_pub = false,
                .location = Location.zero,
            },
        },
    };
    checker.check(&module);

    // show_hash(Point(1)) — Point implements both Display and Hash
    const callee = ast.Expr{ .kind = .{ .ident = "show_hash" }, .location = Location.zero };
    const ctor_callee = ast.Expr{ .kind = .{ .ident = "Point" }, .location = Location.zero };
    const ctor_arg = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const arg_val = ast.Expr{
        .kind = .{ .call = .{
            .callee = &ctor_callee,
            .args = &.{.{ .name = null, .value = &ctor_arg, .location = Location.zero }},
        } },
        .location = Location.zero,
    };
    const call = ast.Expr{
        .kind = .{ .call = .{
            .callee = &callee,
            .args = &.{.{ .name = null, .value = &arg_val, .location = Location.zero }},
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&call, &checker.module_scope);
    try std.testing.expectEqual(TypeId.string, result);
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "generic fn call: one of multiple bounds not satisfied" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = &ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const string_te = &ast.TypeExpr{ .kind = .{ .named = "String" }, .location = Location.zero };
    const t_te = &ast.TypeExpr{ .kind = .{ .named = "T" }, .location = Location.zero };
    const display_te = &ast.TypeExpr{ .kind = .{ .named = "Display" }, .location = Location.zero };
    const hash_te = &ast.TypeExpr{ .kind = .{ .named = "Hash" }, .location = Location.zero };
    const point_te = &ast.TypeExpr{ .kind = .{ .named = "Point" }, .location = Location.zero };

    const ret_expr = &ast.Expr{ .kind = .{ .string_lit = "ok" }, .location = Location.zero };
    const ret_stmt = ast.Stmt{
        .kind = .{ .return_stmt = .{ .value = ret_expr } },
        .location = Location.zero,
    };

    const module = ast.Module{
        .imports = &.{},
        .decls = &.{
            // interface Display:
            ast.Decl{ .kind = .{ .interface_decl = .{ .name = "Display", .generic_params = &.{}, .methods = &.{} } }, .is_pub = false, .location = Location.zero },
            // interface Hash:
            ast.Decl{ .kind = .{ .interface_decl = .{ .name = "Hash", .generic_params = &.{}, .methods = &.{} } }, .is_pub = false, .location = Location.zero },
            // struct Point: pub x: Int
            ast.Decl{
                .kind = .{ .struct_decl = .{
                    .name = "Point",
                    .generic_params = &.{},
                    .fields = &.{
                        .{ .name = "x", .type_expr = int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
                    },
                } },
                .is_pub = false,
                .location = Location.zero,
            },
            // impl Display for Point: — only Display, NOT Hash
            ast.Decl{ .kind = .{ .impl_decl = .{ .target = display_te, .interface = point_te, .methods = &.{} } }, .is_pub = false, .location = Location.zero },
            // fn show_hash[T: Display + Hash](x: T) -> String: return "ok"
            ast.Decl{
                .kind = .{ .fn_decl = .{
                    .name = "show_hash",
                    .generic_params = &.{
                        .{ .name = "T", .bounds = &.{ display_te, hash_te }, .location = Location.zero },
                    },
                    .params = &.{
                        .{ .name = "x", .type_expr = t_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
                    },
                    .return_type = string_te,
                    .body = .{ .stmts = &.{ret_stmt}, .location = Location.zero },
                } },
                .is_pub = false,
                .location = Location.zero,
            },
        },
    };
    checker.check(&module);

    // show_hash(Point(1)) — Point only implements Display, not Hash
    const callee = ast.Expr{ .kind = .{ .ident = "show_hash" }, .location = Location.zero };
    const ctor_callee = ast.Expr{ .kind = .{ .ident = "Point" }, .location = Location.zero };
    const ctor_arg = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const arg_val = ast.Expr{
        .kind = .{ .call = .{
            .callee = &ctor_callee,
            .args = &.{.{ .name = null, .value = &ctor_arg, .location = Location.zero }},
        } },
        .location = Location.zero,
    };
    const call = ast.Expr{
        .kind = .{ .call = .{
            .callee = &callee,
            .args = &.{.{ .name = null, .value = &arg_val, .location = Location.zero }},
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&call, &checker.module_scope);
    try std.testing.expect(result.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "generic struct with bound: satisfied" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = &ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const t_te = &ast.TypeExpr{ .kind = .{ .named = "T" }, .location = Location.zero };
    const printable_te = &ast.TypeExpr{ .kind = .{ .named = "Printable" }, .location = Location.zero };
    const point_te_name = &ast.TypeExpr{ .kind = .{ .named = "Point" }, .location = Location.zero };

    const module = ast.Module{
        .imports = &.{},
        .decls = &.{
            // interface Printable:
            ast.Decl{ .kind = .{ .interface_decl = .{ .name = "Printable", .generic_params = &.{}, .methods = &.{} } }, .is_pub = false, .location = Location.zero },
            // struct Point: pub x: Int
            ast.Decl{
                .kind = .{ .struct_decl = .{
                    .name = "Point",
                    .generic_params = &.{},
                    .fields = &.{
                        .{ .name = "x", .type_expr = int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
                    },
                } },
                .is_pub = false,
                .location = Location.zero,
            },
            // impl Printable for Point:
            ast.Decl{ .kind = .{ .impl_decl = .{ .target = printable_te, .interface = point_te_name, .methods = &.{} } }, .is_pub = false, .location = Location.zero },
            // struct Wrapper[T: Printable]: pub value: T
            ast.Decl{
                .kind = .{ .struct_decl = .{
                    .name = "Wrapper",
                    .generic_params = &.{
                        .{ .name = "T", .bounds = &.{printable_te}, .location = Location.zero },
                    },
                    .fields = &.{
                        .{ .name = "value", .type_expr = t_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
                    },
                } },
                .is_pub = false,
                .location = Location.zero,
            },
            // fn use_wrapper(w: Wrapper[Point]) -> Int: return 0
            ast.Decl{
                .kind = .{ .fn_decl = .{
                    .name = "use_wrapper",
                    .generic_params = &.{},
                    .params = &.{
                        .{ .name = "w", .type_expr = &ast.TypeExpr{
                            .kind = .{ .generic = .{ .name = "Wrapper", .args = &.{point_te_name} } },
                            .location = Location.zero,
                        }, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
                    },
                    .return_type = int_te,
                    .body = .{
                        .stmts = &.{ast.Stmt{
                            .kind = .{ .return_stmt = .{ .value = &ast.Expr{ .kind = .{ .int_lit = "0" }, .location = Location.zero } } },
                            .location = Location.zero,
                        }},
                        .location = Location.zero,
                    },
                } },
                .is_pub = false,
                .location = Location.zero,
            },
        },
    };
    checker.check(&module);

    // Wrapper[Point] should succeed — Point implements Printable
    try std.testing.expect(!checker.diagnostics.hasErrors());
    try std.testing.expect(checker.type_table.lookup("Wrapper[Point]") != null);
}

test "generic struct with bound: not satisfied" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = &ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const t_te = &ast.TypeExpr{ .kind = .{ .named = "T" }, .location = Location.zero };
    const printable_te = &ast.TypeExpr{ .kind = .{ .named = "Printable" }, .location = Location.zero };

    const module = ast.Module{
        .imports = &.{},
        .decls = &.{
            // interface Printable:
            ast.Decl{ .kind = .{ .interface_decl = .{ .name = "Printable", .generic_params = &.{}, .methods = &.{} } }, .is_pub = false, .location = Location.zero },
            // struct Wrapper[T: Printable]: pub value: T
            ast.Decl{
                .kind = .{ .struct_decl = .{
                    .name = "Wrapper",
                    .generic_params = &.{
                        .{ .name = "T", .bounds = &.{printable_te}, .location = Location.zero },
                    },
                    .fields = &.{
                        .{ .name = "value", .type_expr = t_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
                    },
                } },
                .is_pub = false,
                .location = Location.zero,
            },
            // fn use_wrapper(w: Wrapper[Int]) -> Int: return 0
            // Int does NOT implement Printable
            ast.Decl{
                .kind = .{ .fn_decl = .{
                    .name = "use_wrapper",
                    .generic_params = &.{},
                    .params = &.{
                        .{ .name = "w", .type_expr = &ast.TypeExpr{
                            .kind = .{ .generic = .{ .name = "Wrapper", .args = &.{int_te} } },
                            .location = Location.zero,
                        }, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
                    },
                    .return_type = int_te,
                    .body = .{
                        .stmts = &.{ast.Stmt{
                            .kind = .{ .return_stmt = .{ .value = &ast.Expr{ .kind = .{ .int_lit = "0" }, .location = Location.zero } } },
                            .location = Location.zero,
                        }},
                        .location = Location.zero,
                    },
                } },
                .is_pub = false,
                .location = Location.zero,
            },
        },
    };
    checker.check(&module);

    // Wrapper[Int] should fail — Int does not implement Printable
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "index list with integer returns element type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // create a List[String] value in scope
    const list_type = checker.internCollectionType("List", &.{TypeId.string}, .{ .list = .{ .element = .string } });
    try checker.module_scope.define("names", .{ .type_id = list_type, .is_mut = false });

    const obj = &ast.Expr{ .kind = .{ .ident = "names" }, .location = Location.zero };
    const idx = &ast.Expr{ .kind = .{ .int_lit = "0" }, .location = Location.zero };
    const expr = ast.Expr{
        .kind = .{ .index = .{ .object = obj, .index = idx } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&expr, &checker.module_scope);
    try std.testing.expectEqual(TypeId.string, result);
}

test "index list with non-integer errors" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const list_type = checker.internCollectionType("List", &.{TypeId.int}, .{ .list = .{ .element = .int } });
    try checker.module_scope.define("nums", .{ .type_id = list_type, .is_mut = false });

    const obj = &ast.Expr{ .kind = .{ .ident = "nums" }, .location = Location.zero };
    const idx = &ast.Expr{ .kind = .{ .string_lit = "bad" }, .location = Location.zero };
    const expr = ast.Expr{
        .kind = .{ .index = .{ .object = obj, .index = idx } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&expr, &checker.module_scope);
    try std.testing.expect(result.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "index map with correct key type returns value type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const map_type = checker.internCollectionType("Map", &.{ TypeId.string, TypeId.int }, .{ .map = .{ .key = .string, .value = .int } });
    try checker.module_scope.define("ages", .{ .type_id = map_type, .is_mut = false });

    const obj = &ast.Expr{ .kind = .{ .ident = "ages" }, .location = Location.zero };
    const idx = &ast.Expr{ .kind = .{ .string_lit = "alice" }, .location = Location.zero };
    const expr = ast.Expr{
        .kind = .{ .index = .{ .object = obj, .index = idx } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&expr, &checker.module_scope);
    try std.testing.expectEqual(TypeId.int, result);
}

test "index map with wrong key type errors" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const map_type = checker.internCollectionType("Map", &.{ TypeId.string, TypeId.int }, .{ .map = .{ .key = .string, .value = .int } });
    try checker.module_scope.define("ages", .{ .type_id = map_type, .is_mut = false });

    const obj = &ast.Expr{ .kind = .{ .ident = "ages" }, .location = Location.zero };
    const idx = &ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    const expr = ast.Expr{
        .kind = .{ .index = .{ .object = obj, .index = idx } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&expr, &checker.module_scope);
    try std.testing.expect(result.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "index non-indexable type errors" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    try checker.module_scope.define("x", .{ .type_id = .int, .is_mut = false });

    const obj = &ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };
    const idx = &ast.Expr{ .kind = .{ .int_lit = "0" }, .location = Location.zero };
    const expr = ast.Expr{
        .kind = .{ .index = .{ .object = obj, .index = idx } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&expr, &checker.module_scope);
    try std.testing.expect(result.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "list literal: homogeneous elements" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const a = &ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const b = &ast.Expr{ .kind = .{ .int_lit = "2" }, .location = Location.zero };
    const c = &ast.Expr{ .kind = .{ .int_lit = "3" }, .location = Location.zero };
    const list_expr = ast.Expr{
        .kind = .{ .list = &.{ a, b, c } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&list_expr, &checker.module_scope);
    try std.testing.expect(!result.isErr());

    const ty = checker.type_table.get(result).?;
    try std.testing.expectEqual(TypeId.int, ty.list.element);
}

test "list literal: mixed types error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const a = &ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const b = &ast.Expr{ .kind = .{ .string_lit = "hello" }, .location = Location.zero };
    const list_expr = ast.Expr{
        .kind = .{ .list = &.{ a, b } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&list_expr, &checker.module_scope);
    try std.testing.expect(result.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "list literal: empty list errors" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const list_expr = ast.Expr{
        .kind = .{ .list = &.{} },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&list_expr, &checker.module_scope);
    try std.testing.expect(result.isErr());
}

test "map literal: homogeneous entries" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const k1 = &ast.Expr{ .kind = .{ .string_lit = "a" }, .location = Location.zero };
    const v1 = &ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const k2 = &ast.Expr{ .kind = .{ .string_lit = "b" }, .location = Location.zero };
    const v2 = &ast.Expr{ .kind = .{ .int_lit = "2" }, .location = Location.zero };
    const map_expr = ast.Expr{
        .kind = .{ .map = &.{
            .{ .key = k1, .value = v1, .location = Location.zero },
            .{ .key = k2, .value = v2, .location = Location.zero },
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&map_expr, &checker.module_scope);
    try std.testing.expect(!result.isErr());

    const ty = checker.type_table.get(result).?;
    try std.testing.expectEqual(TypeId.string, ty.map.key);
    try std.testing.expectEqual(TypeId.int, ty.map.value);
}

test "map literal: mixed value types error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const k1 = &ast.Expr{ .kind = .{ .string_lit = "a" }, .location = Location.zero };
    const v1 = &ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const k2 = &ast.Expr{ .kind = .{ .string_lit = "b" }, .location = Location.zero };
    const v2 = &ast.Expr{ .kind = .{ .string_lit = "hello" }, .location = Location.zero };
    const map_expr = ast.Expr{
        .kind = .{ .map = &.{
            .{ .key = k1, .value = v1, .location = Location.zero },
            .{ .key = k2, .value = v2, .location = Location.zero },
        } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&map_expr, &checker.module_scope);
    try std.testing.expect(result.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "set literal: homogeneous elements" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const a = &ast.Expr{ .kind = .{ .string_lit = "x" }, .location = Location.zero };
    const b = &ast.Expr{ .kind = .{ .string_lit = "y" }, .location = Location.zero };
    const set_expr = ast.Expr{
        .kind = .{ .set = &.{ a, b } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&set_expr, &checker.module_scope);
    try std.testing.expect(!result.isErr());

    const ty = checker.type_table.get(result).?;
    try std.testing.expectEqual(TypeId.string, ty.set.element);
}

test "set literal: mixed types error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const a = &ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const b = &ast.Expr{ .kind = .{ .bool_lit = true }, .location = Location.zero };
    const set_expr = ast.Expr{
        .kind = .{ .set = &.{ a, b } },
        .location = Location.zero,
    };

    const result = checker.checkExpr(&set_expr, &checker.module_scope);
    try std.testing.expect(result.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "self in method body resolves to impl target type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = &ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const point_te = &ast.TypeExpr{ .kind = .{ .named = "Point" }, .location = Location.zero };

    // self.x — field access on self
    const self_e = &ast.Expr{ .kind = .self_expr, .location = Location.zero };
    const field = &ast.Expr{
        .kind = .{ .field_access = .{ .object = self_e, .field = "x" } },
        .location = Location.zero,
    };
    const ret_stmt = ast.Stmt{
        .kind = .{ .return_stmt = .{ .value = field } },
        .location = Location.zero,
    };

    const module = ast.Module{
        .imports = &.{},
        .decls = &.{
            // struct Point: pub x: Int
            ast.Decl{
                .kind = .{ .struct_decl = .{
                    .name = "Point",
                    .generic_params = &.{},
                    .fields = &.{
                        .{ .name = "x", .type_expr = int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
                    },
                } },
                .is_pub = false,
                .location = Location.zero,
            },
            // impl Point:
            //   fn get_x() -> Int:
            //     return self.x
            ast.Decl{
                .kind = .{ .impl_decl = .{
                    .target = point_te,
                    .interface = null,
                    .methods = &.{.{
                        .is_pub = false,
                        .decl = .{
                            .name = "get_x",
                            .generic_params = &.{},
                            .params = &.{},
                            .return_type = int_te,
                            .body = .{ .stmts = &.{ret_stmt}, .location = Location.zero },
                        },
                        .location = Location.zero,
                    }},
                } },
                .is_pub = false,
                .location = Location.zero,
            },
        },
    };
    checker.check(&module);

    // no errors — self.x resolves to Int, matching return type
    try std.testing.expect(!checker.diagnostics.hasErrors());
}

test "self outside method body errors" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const self_e = ast.Expr{ .kind = .self_expr, .location = Location.zero };
    const result = checker.checkExpr(&self_e, &checker.module_scope);

    try std.testing.expect(result.isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "self field access type mismatch in method body" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const int_te = &ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const string_te = &ast.TypeExpr{ .kind = .{ .named = "String" }, .location = Location.zero };
    const point_te = &ast.TypeExpr{ .kind = .{ .named = "Point" }, .location = Location.zero };

    // method returns self.x (Int) but declares return type String — mismatch
    const self_e = &ast.Expr{ .kind = .self_expr, .location = Location.zero };
    const field = &ast.Expr{
        .kind = .{ .field_access = .{ .object = self_e, .field = "x" } },
        .location = Location.zero,
    };
    const ret_stmt = ast.Stmt{
        .kind = .{ .return_stmt = .{ .value = field } },
        .location = Location.zero,
    };

    const module = ast.Module{
        .imports = &.{},
        .decls = &.{
            ast.Decl{
                .kind = .{ .struct_decl = .{
                    .name = "Point",
                    .generic_params = &.{},
                    .fields = &.{
                        .{ .name = "x", .type_expr = int_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
                    },
                } },
                .is_pub = false,
                .location = Location.zero,
            },
            // impl Point:
            //   fn get_x() -> String:
            //     return self.x    ← type mismatch: self.x is Int, not String
            ast.Decl{
                .kind = .{ .impl_decl = .{
                    .target = point_te,
                    .interface = null,
                    .methods = &.{.{
                        .is_pub = false,
                        .decl = .{
                            .name = "get_x",
                            .generic_params = &.{},
                            .params = &.{},
                            .return_type = string_te,
                            .body = .{ .stmts = &.{ret_stmt}, .location = Location.zero },
                        },
                        .location = Location.zero,
                    }},
                } },
                .is_pub = false,
                .location = Location.zero,
            },
        },
    };
    checker.check(&module);

    try std.testing.expect(checker.diagnostics.hasErrors());
}

// -- unwrap operator tests --

test "unwrap Optional[Int] yields Int" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    // create an Optional[Int] type
    const opt_id = try checker.type_table.addType(.{ .optional = .{ .inner = .int } });
    try scope.define("x", .{ .type_id = opt_id, .is_mut = false });

    const ident = &ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };
    const unwrap_expr = ast.Expr{ .kind = .{ .unwrap = ident }, .location = Location.zero };
    try std.testing.expectEqual(TypeId.int, checker.checkExpr(&unwrap_expr, scope));
}

test "unwrap Optional[String] yields String" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const opt_id = try checker.type_table.addType(.{ .optional = .{ .inner = .string } });
    try scope.define("x", .{ .type_id = opt_id, .is_mut = false });

    const ident = &ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };
    const unwrap_expr = ast.Expr{ .kind = .{ .unwrap = ident }, .location = Location.zero };
    try std.testing.expectEqual(TypeId.string, checker.checkExpr(&unwrap_expr, scope));
}

test "unwrap non-optional type is an error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;
    try scope.define("x", .{ .type_id = .int, .is_mut = false });

    const ident = &ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };
    const unwrap_expr = ast.Expr{ .kind = .{ .unwrap = ident }, .location = Location.zero };
    try std.testing.expect(checker.checkExpr(&unwrap_expr, scope).isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "unwrap on error-typed expr suppresses cascade" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    // inner expression resolves to .err (unknown variable)
    const ident = &ast.Expr{ .kind = .{ .ident = "unknown" }, .location = Location.zero };
    const unwrap_expr = ast.Expr{ .kind = .{ .unwrap = ident }, .location = Location.zero };

    const error_count_before = checker.diagnostics.errorCount();
    const result = checker.checkExpr(&unwrap_expr, scope);
    try std.testing.expect(result.isErr());
    // only one error (undefined variable), not two (no "cannot unwrap" cascade)
    try std.testing.expectEqual(error_count_before + 1, checker.diagnostics.errorCount());
}

// -- try operator tests --

test "try Result[Int, E] in result-returning fn yields Int" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    var scope = Scope.init(std.testing.allocator, &checker.module_scope);
    defer scope.deinit();

    // set up scope to be inside a result-returning function
    const ret_type = try checker.type_table.addType(.{ .result = .{ .ok_type = .string, .err_type = .err } });
    scope.return_type = ret_type;

    // create a Result[Int, E] value
    const res_id = try checker.type_table.addType(.{ .result = .{ .ok_type = .int, .err_type = .err } });
    try scope.define("x", .{ .type_id = res_id, .is_mut = false });

    const ident = &ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };
    const try_expr = ast.Expr{ .kind = .{ .try_expr = ident }, .location = Location.zero };
    try std.testing.expectEqual(TypeId.int, checker.checkExpr(&try_expr, &scope));
}

test "try Result[String, E] yields String" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    var scope = Scope.init(std.testing.allocator, &checker.module_scope);
    defer scope.deinit();

    const ret_type = try checker.type_table.addType(.{ .result = .{ .ok_type = .int, .err_type = .err } });
    scope.return_type = ret_type;

    const res_id = try checker.type_table.addType(.{ .result = .{ .ok_type = .string, .err_type = .err } });
    try scope.define("x", .{ .type_id = res_id, .is_mut = false });

    const ident = &ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };
    const try_expr = ast.Expr{ .kind = .{ .try_expr = ident }, .location = Location.zero };
    try std.testing.expectEqual(TypeId.string, checker.checkExpr(&try_expr, &scope));
}

test "try on non-result type is an error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    var scope = Scope.init(std.testing.allocator, &checker.module_scope);
    defer scope.deinit();

    const ret_type = try checker.type_table.addType(.{ .result = .{ .ok_type = .int, .err_type = .err } });
    scope.return_type = ret_type;
    try scope.define("x", .{ .type_id = .int, .is_mut = false });

    const ident = &ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };
    const try_expr = ast.Expr{ .kind = .{ .try_expr = ident }, .location = Location.zero };
    try std.testing.expect(checker.checkExpr(&try_expr, &scope).isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "try result in non-result function is an error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    var scope = Scope.init(std.testing.allocator, &checker.module_scope);
    defer scope.deinit();

    // function returns Int (not a result type)
    scope.return_type = .int;

    const res_id = try checker.type_table.addType(.{ .result = .{ .ok_type = .int, .err_type = .err } });
    try scope.define("x", .{ .type_id = res_id, .is_mut = false });

    const ident = &ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };
    const try_expr = ast.Expr{ .kind = .{ .try_expr = ident }, .location = Location.zero };
    try std.testing.expect(checker.checkExpr(&try_expr, &scope).isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "try on error-typed expr suppresses cascade" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    var scope = Scope.init(std.testing.allocator, &checker.module_scope);
    defer scope.deinit();

    const ret_type = try checker.type_table.addType(.{ .result = .{ .ok_type = .int, .err_type = .err } });
    scope.return_type = ret_type;

    const ident = &ast.Expr{ .kind = .{ .ident = "unknown" }, .location = Location.zero };
    const try_expr = ast.Expr{ .kind = .{ .try_expr = ident }, .location = Location.zero };

    const error_count_before = checker.diagnostics.errorCount();
    const result = checker.checkExpr(&try_expr, &scope);
    try std.testing.expect(result.isErr());
    // only one error (undefined variable), not two
    try std.testing.expectEqual(error_count_before + 1, checker.diagnostics.errorCount());
}

// -- pipe operator tests --

test "pipe: value |> function yields return type" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    // define double(Int) -> Int
    const fn_type = try checker.type_table.addType(.{ .function = .{
        .param_types = &.{.int},
        .return_type = .int,
    } });
    try scope.define("double", .{ .type_id = fn_type, .is_mut = false });

    const left = &ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    const right = &ast.Expr{ .kind = .{ .ident = "double" }, .location = Location.zero };
    const pipe = ast.Expr{
        .kind = .{ .binary = .{ .left = left, .op = .pipe, .right = right } },
        .location = Location.zero,
    };
    try std.testing.expectEqual(TypeId.int, checker.checkExpr(&pipe, scope));
}

test "pipe: type mismatch is an error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    // define double(Int) -> Int
    const fn_type = try checker.type_table.addType(.{ .function = .{
        .param_types = &.{.int},
        .return_type = .int,
    } });
    try scope.define("double", .{ .type_id = fn_type, .is_mut = false });

    // "hello" |> double — String vs Int
    const left = &ast.Expr{ .kind = .{ .string_lit = "hello" }, .location = Location.zero };
    const right = &ast.Expr{ .kind = .{ .ident = "double" }, .location = Location.zero };
    const pipe = ast.Expr{
        .kind = .{ .binary = .{ .left = left, .op = .pipe, .right = right } },
        .location = Location.zero,
    };
    try std.testing.expect(checker.checkExpr(&pipe, scope).isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "pipe: non-function RHS is an error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;
    try scope.define("x", .{ .type_id = .int, .is_mut = false });

    const left = &ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const right = &ast.Expr{ .kind = .{ .ident = "x" }, .location = Location.zero };
    const pipe = ast.Expr{
        .kind = .{ .binary = .{ .left = left, .op = .pipe, .right = right } },
        .location = Location.zero,
    };
    try std.testing.expect(checker.checkExpr(&pipe, scope).isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "pipe: undefined function is an error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    const left = &ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const right = &ast.Expr{ .kind = .{ .ident = "nonexistent" }, .location = Location.zero };
    const pipe = ast.Expr{
        .kind = .{ .binary = .{ .left = left, .op = .pipe, .right = right } },
        .location = Location.zero,
    };
    try std.testing.expect(checker.checkExpr(&pipe, scope).isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "pipe: multi-param function is an error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    // define add(Int, Int) -> Int
    const fn_type = try checker.type_table.addType(.{ .function = .{
        .param_types = &.{ .int, .int },
        .return_type = .int,
    } });
    try scope.define("add", .{ .type_id = fn_type, .is_mut = false });

    const left = &ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const right = &ast.Expr{ .kind = .{ .ident = "add" }, .location = Location.zero };
    const pipe = ast.Expr{
        .kind = .{ .binary = .{ .left = left, .op = .pipe, .right = right } },
        .location = Location.zero,
    };
    try std.testing.expect(checker.checkExpr(&pipe, scope).isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "pipe: non-identifier RHS is an error" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    // 1 |> 42 — RHS is a literal, not a function name
    const left = &ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const right = &ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };
    const pipe = ast.Expr{
        .kind = .{ .binary = .{ .left = left, .op = .pipe, .right = right } },
        .location = Location.zero,
    };
    try std.testing.expect(checker.checkExpr(&pipe, scope).isErr());
    try std.testing.expect(checker.diagnostics.hasErrors());
}

test "pipe: chained pipes resolve correctly" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const scope = &checker.module_scope;

    // define inc(Int) -> Int
    const inc_type = try checker.type_table.addType(.{ .function = .{
        .param_types = &.{.int},
        .return_type = .int,
    } });
    try scope.define("inc", .{ .type_id = inc_type, .is_mut = false });

    // define to_string(Int) -> String
    const to_string_type = try checker.type_table.addType(.{ .function = .{
        .param_types = &.{.int},
        .return_type = .string,
    } });
    try scope.define("to_string", .{ .type_id = to_string_type, .is_mut = false });

    // 1 |> inc |> to_string
    const one = &ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const inc_ident = &ast.Expr{ .kind = .{ .ident = "inc" }, .location = Location.zero };
    const inner_pipe = &ast.Expr{
        .kind = .{ .binary = .{ .left = one, .op = .pipe, .right = inc_ident } },
        .location = Location.zero,
    };
    const to_string_ident = &ast.Expr{ .kind = .{ .ident = "to_string" }, .location = Location.zero };
    const outer_pipe = ast.Expr{
        .kind = .{ .binary = .{ .left = inner_pipe, .op = .pipe, .right = to_string_ident } },
        .location = Location.zero,
    };
    try std.testing.expectEqual(TypeId.string, checker.checkExpr(&outer_pipe, scope));
}
