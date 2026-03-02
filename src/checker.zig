// checker — semantic analysis and type checking
//
// two-pass approach:
//   pass 1 (register): walk top-level declarations, record names and
//     type signatures in the module scope.
//   pass 2 (check): walk function bodies and top-level bindings,
//     creating child scopes for functions and blocks.
//
// uses an error sentinel pattern: when a sub-expression has type
// TypeId.err, further checks are skipped to prevent cascading noise.

const std = @import("std");
const ast = @import("ast.zig");
const errors = @import("errors.zig");
const types = @import("types.zig");

const TypeId = types.TypeId;
const TypeTable = types.TypeTable;
const Type = types.Type;
const Location = errors.Location;

// ---------------------------------------------------------------
// scope
// ---------------------------------------------------------------

pub const Binding = struct {
    type_id: TypeId,
    is_mut: bool,
};

/// a lexical scope. linked-list: each scope has an optional parent.
/// lookups walk the parent chain until a match is found.
pub const Scope = struct {
    bindings: std.StringHashMap(Binding),
    parent: ?*Scope,
    /// the return type of the enclosing function (if any).
    /// used to check return statements.
    return_type: ?TypeId,

    pub fn init(allocator: std.mem.Allocator, parent: ?*Scope) Scope {
        return .{
            .bindings = std.StringHashMap(Binding).init(allocator),
            .parent = parent,
            .return_type = if (parent) |p| p.return_type else null,
        };
    }

    pub fn deinit(self: *Scope) void {
        self.bindings.deinit();
    }

    pub fn define(self: *Scope, name: []const u8, binding: Binding) !void {
        try self.bindings.put(name, binding);
    }

    pub fn lookup(self: *const Scope, name: []const u8) ?Binding {
        if (self.bindings.get(name)) |b| return b;
        if (self.parent) |p| return p.lookup(name);
        return null;
    }
};

// ---------------------------------------------------------------
// checker
// ---------------------------------------------------------------

pub const Checker = struct {
    type_table: TypeTable,
    diagnostics: errors.DiagnosticList,
    allocator: std.mem.Allocator,
    /// arena for checker-allocated data (scope storage, etc.)
    arena: std.heap.ArenaAllocator,
    module_scope: Scope,

    pub fn init(allocator: std.mem.Allocator, source: []const u8) !Checker {
        var checker = Checker{
            .type_table = try TypeTable.init(allocator),
            .diagnostics = errors.DiagnosticList.init(allocator, source),
            .allocator = allocator,
            .arena = std.heap.ArenaAllocator.init(allocator),
            .module_scope = Scope.init(allocator, null),
        };

        // register builtins into the module scope
        try checker.registerBuiltinFunctions();

        return checker;
    }

    pub fn deinit(self: *Checker) void {
        self.module_scope.deinit();
        self.arena.deinit();
        self.diagnostics.deinit();
        self.type_table.deinit();
    }

    fn registerBuiltinFunctions(self: *Checker) !void {
        // print(String) -> Void
        const print_type = try self.type_table.addType(.{ .function = .{
            .param_types = &.{.string},
            .return_type = .void,
        } });
        try self.module_scope.define("print", .{ .type_id = print_type, .is_mut = false });
    }

    // ---------------------------------------------------------------
    // type resolution — AST TypeExpr → TypeId
    // ---------------------------------------------------------------

    pub fn resolveTypeExpr(self: *Checker, type_expr: *const ast.TypeExpr) TypeId {
        return switch (type_expr.kind) {
            .named => |name| self.resolveNamedType(name, type_expr.location),
            .optional => |inner| self.resolveOptionalType(inner),
            .result => |r| self.resolveResultType(r),
            .tuple => |elems| self.resolveTupleType(elems),
            .fn_type => |f| self.resolveFnType(f),
            .generic => |g| self.resolveGenericType(g, type_expr.location),
        };
    }

    fn resolveNamedType(self: *Checker, name: []const u8, location: Location) TypeId {
        if (self.type_table.lookup(name)) |id| return id;

        self.diagnostics.addError(location, self.fmt("unknown type '{s}'", .{name})) catch {};
        return .err;
    }

    fn resolveOptionalType(self: *Checker, inner: *const ast.TypeExpr) TypeId {
        const inner_id = self.resolveTypeExpr(inner);
        if (inner_id.isErr()) return .err;

        return self.type_table.addType(.{ .optional = .{ .inner = inner_id } }) catch return .err;
    }

    fn resolveResultType(self: *Checker, r: ast.ResultType) TypeId {
        const ok_id = self.resolveTypeExpr(r.ok_type);
        if (ok_id.isErr()) return .err;

        const err_id = if (r.err_type) |err_type|
            self.resolveTypeExpr(err_type)
        else
            TypeId.err; // default error type — will be refined later

        return self.type_table.addType(.{ .result = .{
            .ok_type = ok_id,
            .err_type = err_id,
        } }) catch return .err;
    }

    fn resolveTupleType(self: *Checker, elems: []const *const ast.TypeExpr) TypeId {
        var ids = std.ArrayList(TypeId).initCapacity(self.allocator, elems.len) catch return .err;
        defer ids.deinit(self.allocator);

        for (elems) |elem| {
            const id = self.resolveTypeExpr(elem);
            if (id.isErr()) return .err;
            ids.append(self.allocator, id) catch return .err;
        }

        // copy to arena so it outlives this call
        const owned = self.arena.allocator().dupe(TypeId, ids.items) catch return .err;
        return self.type_table.addType(.{ .tuple = .{ .elements = owned } }) catch return .err;
    }

    fn resolveFnType(self: *Checker, f: ast.FnType) TypeId {
        var param_ids = std.ArrayList(TypeId).initCapacity(self.allocator, f.params.len) catch return .err;
        defer param_ids.deinit(self.allocator);

        for (f.params) |param| {
            const id = self.resolveTypeExpr(param);
            if (id.isErr()) return .err;
            param_ids.append(self.allocator, id) catch return .err;
        }

        const ret_id = if (f.return_type) |rt| self.resolveTypeExpr(rt) else TypeId.void;
        if (ret_id.isErr()) return .err;

        const owned_params = self.arena.allocator().dupe(TypeId, param_ids.items) catch return .err;
        return self.type_table.addType(.{ .function = .{
            .param_types = owned_params,
            .return_type = ret_id,
        } }) catch return .err;
    }

    fn resolveGenericType(self: *Checker, g: ast.GenericType, location: Location) TypeId {
        _ = g;
        self.diagnostics.addError(location, "generics are not yet supported") catch {};
        return .err;
    }

    // ---------------------------------------------------------------
    // expression checking
    // ---------------------------------------------------------------

    /// infer the type of an expression. returns TypeId.err for
    /// unsupported constructs (suppresses cascading errors downstream).
    pub fn checkExpr(self: *Checker, expr: *const ast.Expr, scope: *const Scope) TypeId {
        return switch (expr.kind) {
            .int_lit => .int,
            .float_lit => .float,
            .string_lit => .string,
            .bool_lit => .bool,
            .none_lit => .err, // needs optional type context
            .ident => |name| self.checkIdent(name, expr.location, scope),
            .binary => |bin| self.checkBinary(bin, expr.location, scope),
            .unary => |un| self.checkUnary(un, expr.location, scope),
            .grouped => |inner| self.checkExpr(inner, scope),
            .string_interp => |interp| self.checkStringInterp(interp, scope),
            .if_expr => |if_e| self.checkIfExpr(if_e, scope),

            // these need further infrastructure (generics, impl blocks, etc.)
            .call => .err,
            .method_call => .err,
            .field_access => .err,
            .index => .err,
            .unwrap => .err,
            .try_expr => .err,
            .match_expr => .err,
            .lambda => .err,
            .list => .err,
            .map => .err,
            .set => .err,
            .tuple => .err,
            .self_expr => .err,

            .err => .err,
        };
    }

    fn checkIdent(self: *Checker, name: []const u8, location: Location, scope: *const Scope) TypeId {
        if (scope.lookup(name)) |binding| return binding.type_id;

        self.diagnostics.addError(location, self.fmt("undefined variable '{s}'", .{name})) catch {};
        return .err;
    }

    fn checkBinary(self: *Checker, bin: ast.BinaryExpr, location: Location, scope: *const Scope) TypeId {
        const left = self.checkExpr(bin.left, scope);
        const right = self.checkExpr(bin.right, scope);

        // if either side is an error, don't cascade
        if (left.isErr() or right.isErr()) return .err;

        return switch (bin.op) {
            // arithmetic: both sides must be the same numeric type
            .add => self.checkArithmetic(left, right, bin, location),
            .sub, .mul, .div, .mod => self.checkNumericBinary(left, right, location),

            // comparison: both sides must match, result is Bool
            .eq, .neq => self.checkEquality(left, right, location),
            .lt, .gt, .lte, .gte => self.checkOrdering(left, right, location),

            // logical: both sides must be Bool
            .@"and", .@"or" => self.checkLogical(left, right, location),

            // pipe: not yet supported
            .pipe => .err,
        };
    }

    fn checkArithmetic(self: *Checker, left: TypeId, right: TypeId, bin: ast.BinaryExpr, location: Location) TypeId {
        // string + string → string (concatenation)
        if (bin.op == .add and left == .string and right == .string) return .string;

        return self.checkNumericBinary(left, right, location);
    }

    fn checkNumericBinary(self: *Checker, left: TypeId, right: TypeId, location: Location) TypeId {
        if (!left.isNumeric()) {
            self.diagnostics.addError(location, self.fmt(
                "expected numeric type, got {s}",
                .{self.type_table.typeName(left)},
            )) catch {};
            return .err;
        }
        if (left != right) {
            self.diagnostics.addError(location, self.fmt(
                "type mismatch: {s} and {s}",
                .{ self.type_table.typeName(left), self.type_table.typeName(right) },
            )) catch {};
            return .err;
        }
        return left;
    }

    fn checkEquality(self: *Checker, left: TypeId, right: TypeId, location: Location) TypeId {
        if (left != right) {
            self.diagnostics.addError(location, self.fmt(
                "cannot compare {s} and {s}",
                .{ self.type_table.typeName(left), self.type_table.typeName(right) },
            )) catch {};
            return .err;
        }
        return .bool;
    }

    fn checkOrdering(self: *Checker, left: TypeId, right: TypeId, location: Location) TypeId {
        if (!left.isNumeric() and left != .string) {
            self.diagnostics.addError(location, self.fmt(
                "type {s} does not support ordering",
                .{self.type_table.typeName(left)},
            )) catch {};
            return .err;
        }
        if (left != right) {
            self.diagnostics.addError(location, self.fmt(
                "cannot compare {s} and {s}",
                .{ self.type_table.typeName(left), self.type_table.typeName(right) },
            )) catch {};
            return .err;
        }
        return .bool;
    }

    fn checkLogical(self: *Checker, left: TypeId, right: TypeId, location: Location) TypeId {
        if (left != .bool) {
            self.diagnostics.addError(location, self.fmt(
                "expected Bool, got {s}",
                .{self.type_table.typeName(left)},
            )) catch {};
            return .err;
        }
        if (right != .bool) {
            self.diagnostics.addError(location, self.fmt(
                "expected Bool, got {s}",
                .{self.type_table.typeName(right)},
            )) catch {};
            return .err;
        }
        return .bool;
    }

    fn checkUnary(self: *Checker, un: ast.UnaryExpr, location: Location, scope: *const Scope) TypeId {
        const operand = self.checkExpr(un.operand, scope);
        if (operand.isErr()) return .err;

        return switch (un.op) {
            .negate => {
                if (!operand.isNumeric()) {
                    self.diagnostics.addError(location, self.fmt(
                        "cannot negate {s}",
                        .{self.type_table.typeName(operand)},
                    )) catch {};
                    return .err;
                }
                return operand;
            },
            .not => {
                if (operand != .bool) {
                    self.diagnostics.addError(location, self.fmt(
                        "expected Bool for 'not', got {s}",
                        .{self.type_table.typeName(operand)},
                    )) catch {};
                    return .err;
                }
                return .bool;
            },
        };
    }

    fn checkStringInterp(self: *Checker, interp: ast.StringInterp, scope: *const Scope) TypeId {
        // string interpolation always produces a String.
        // we still check sub-expressions for errors though.
        for (interp.parts) |part| {
            switch (part) {
                .literal => {},
                .expr => |e| _ = self.checkExpr(e, scope),
            }
        }
        return .string;
    }

    fn checkIfExpr(self: *Checker, if_e: ast.IfExpr, scope: *const Scope) TypeId {
        const cond = self.checkExpr(if_e.condition, scope);
        if (!cond.isErr() and cond != .bool) {
            self.diagnostics.addError(if_e.condition.location, self.fmt(
                "expected Bool in condition, got {s}",
                .{self.type_table.typeName(cond)},
            )) catch {};
        }

        const then_type = self.checkExpr(if_e.then_expr, scope);

        // check elif branches
        for (if_e.elif_branches) |branch| {
            const elif_cond = self.checkExpr(branch.condition, scope);
            if (!elif_cond.isErr() and elif_cond != .bool) {
                self.diagnostics.addError(branch.condition.location, self.fmt(
                    "expected Bool in condition, got {s}",
                    .{self.type_table.typeName(elif_cond)},
                )) catch {};
            }

            const elif_type = self.checkExpr(branch.expr, scope);
            if (!then_type.isErr() and !elif_type.isErr() and then_type != elif_type) {
                self.diagnostics.addError(branch.location, self.fmt(
                    "branch type mismatch: expected {s}, got {s}",
                    .{ self.type_table.typeName(then_type), self.type_table.typeName(elif_type) },
                )) catch {};
            }
        }

        const else_type = self.checkExpr(if_e.else_expr, scope);
        if (!then_type.isErr() and !else_type.isErr() and then_type != else_type) {
            self.diagnostics.addError(if_e.else_expr.location, self.fmt(
                "branch type mismatch: expected {s}, got {s}",
                .{ self.type_table.typeName(then_type), self.type_table.typeName(else_type) },
            )) catch {};
        }

        return then_type;
    }

    // ---------------------------------------------------------------
    // helpers
    // ---------------------------------------------------------------

    /// format a string onto the checker's arena. the returned slice lives
    /// as long as the checker does — safe to store in diagnostics.
    fn fmt(self: *Checker, comptime format: []const u8, args: anytype) []const u8 {
        return std.fmt.allocPrint(self.arena.allocator(), format, args) catch "<format error>";
    }
};

// ---------------------------------------------------------------
// tests
// ---------------------------------------------------------------

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

test "resolveTypeExpr reports generics as unsupported" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    const generic = ast.TypeExpr{
        .kind = .{ .generic = .{ .name = "List", .args = &.{} } },
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
