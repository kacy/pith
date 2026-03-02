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
