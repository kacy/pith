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
//
// error handling convention:
//   - diagnostics.addError(...) catch {}  — losing a diagnostic under OOM
//     is acceptable; the compilation will still fail on other errors.
//   - scope.define(...) catch return  — if a name can't be registered,
//     bail out of the current declaration. proceeding without it leads to
//     confusing "undefined variable" cascades.
//   - type_table.register(...) catch return  — same reasoning. a missing
//     type makes every later reference to it an error.

const std = @import("std");
const ast = @import("ast.zig");
const errors = @import("errors.zig");
const types = @import("types.zig");

const TypeId = types.TypeId;
const TypeTable = types.TypeTable;
const Type = types.Type;
const Location = errors.Location;

// ---------------------------------------------------------------
// generic declarations
// ---------------------------------------------------------------

/// a generic type declaration stored during pass 1. the AST node is
/// kept around so we can instantiate it later with concrete type args.
pub const GenericDecl = union(enum) {
    @"struct": ast.StructDecl,
    @"enum": ast.EnumDecl,
    function: ast.FnDecl,
};

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
    /// true when inside a while or for loop body.
    /// used to validate break/continue statements.
    in_loop: bool,

    pub fn init(allocator: std.mem.Allocator, parent: ?*Scope) Scope {
        return .{
            .bindings = std.StringHashMap(Binding).init(allocator),
            .parent = parent,
            .return_type = if (parent) |p| p.return_type else null,
            .in_loop = if (parent) |p| p.in_loop else false,
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
    /// generic type declarations stored during pass 1, keyed by base name
    /// (e.g. "Pair", "Option"). instantiated on demand when a concrete
    /// use like Pair[Int, String] is encountered in type resolution.
    generic_decls: std.StringHashMap(GenericDecl),

    /// create a new checker. registers builtin types and functions.
    pub fn init(allocator: std.mem.Allocator, source: []const u8) !Checker {
        var checker = Checker{
            .type_table = try TypeTable.init(allocator),
            .diagnostics = errors.DiagnosticList.init(allocator, source),
            .allocator = allocator,
            .arena = std.heap.ArenaAllocator.init(allocator),
            .module_scope = Scope.init(allocator, null),
            .generic_decls = std.StringHashMap(GenericDecl).init(allocator),
        };

        // register builtins into the module scope
        try checker.registerBuiltinFunctions();

        return checker;
    }

    pub fn deinit(self: *Checker) void {
        self.module_scope.deinit();
        self.generic_decls.deinit();
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

        // sync primitives — opaque struct types with constructors
        try self.registerSyncType("Mutex", &.{});
        try self.registerSyncType("WaitGroup", &.{});
        try self.registerSyncType("Semaphore", &.{.int});
    }

    /// register an opaque struct type and a constructor function for it.
    fn registerSyncType(self: *Checker, name: []const u8, param_types: []const TypeId) !void {
        const type_id = try self.type_table.addType(.{ .@"struct" = .{
            .name = name,
            .fields = &.{},
        } });
        try self.type_table.register(name, type_id);

        // constructor: Name(...) -> Name
        const ctor_type = try self.type_table.addType(.{ .function = .{
            .param_types = param_types,
            .return_type = type_id,
        } });
        try self.module_scope.define(name, .{ .type_id = ctor_type, .is_mut = false });
    }

    // ---------------------------------------------------------------
    // module checking — public entry point
    // ---------------------------------------------------------------

    /// check a parsed module. two passes:
    ///   1. register all top-level declarations (names + signatures)
    ///   2. check all function bodies and top-level bindings
    pub fn check(self: *Checker, module: *const ast.Module) void {
        // pass 1: register declarations
        for (module.decls) |*decl| {
            self.registerDecl(decl);
        }

        // pass 2: check bodies
        for (module.decls) |*decl| {
            self.checkDecl(decl);
        }
    }

    // ---------------------------------------------------------------
    // pass 1 — declaration registration
    // ---------------------------------------------------------------

    fn registerDecl(self: *Checker, decl: *const ast.Decl) void {
        switch (decl.kind) {
            .fn_decl => |fn_d| self.registerFnDecl(fn_d, decl.location),
            .struct_decl => |s| self.registerStructDecl(s, decl.location),
            .enum_decl => |e| self.registerEnumDecl(e, decl.location),
            .interface_decl => {},
            .impl_decl => {},
            .type_alias => |ta| self.registerTypeAlias(ta, decl.location),
            .binding => {}, // top-level bindings are checked in pass 2
        }
    }

    fn registerFnDecl(self: *Checker, fn_d: ast.FnDecl, location: Location) void {
        // generic functions are stored for later instantiation —
        // the signature isn't resolved until a call site infers the type args
        if (fn_d.generic_params.len > 0) {
            self.generic_decls.put(fn_d.name, .{ .function = fn_d }) catch return;
            _ = location;
            return;
        }

        // resolve parameter types
        var param_ids = std.ArrayList(TypeId).initCapacity(self.allocator, fn_d.params.len) catch return;
        defer param_ids.deinit(self.allocator);

        for (fn_d.params) |param| {
            if (param.type_expr) |te| {
                const id = self.resolveTypeExpr(te);
                param_ids.append(self.allocator, id) catch return;
            } else {
                self.diagnostics.addError(param.location, self.fmt(
                    "parameter '{s}' needs a type annotation",
                    .{param.name},
                )) catch {};
                param_ids.append(self.allocator, .err) catch return;
            }
        }

        // resolve return type
        const return_type = if (fn_d.return_type) |rt| self.resolveTypeExpr(rt) else TypeId.void;

        // store param types on the arena
        const owned_params = self.arena.allocator().dupe(TypeId, param_ids.items) catch return;
        const fn_type = self.type_table.addType(.{ .function = .{
            .param_types = owned_params,
            .return_type = return_type,
        } }) catch return;

        self.module_scope.define(fn_d.name, .{ .type_id = fn_type, .is_mut = false }) catch return;
    }

    fn registerStructDecl(self: *Checker, s: ast.StructDecl, location: Location) void {
        if (s.generic_params.len > 0) {
            // store for later instantiation — fields aren't resolved until
            // a concrete use like Pair[Int, String] is encountered
            self.generic_decls.put(s.name, .{ .@"struct" = s }) catch return;
            _ = location;
            return;
        }

        // resolve field types
        var fields = std.ArrayList(types.Field).initCapacity(self.allocator, s.fields.len) catch return;
        defer fields.deinit(self.allocator);

        for (s.fields) |field| {
            const field_type = self.resolveTypeExpr(field.type_expr);
            fields.append(self.allocator, .{
                .name = field.name,
                .type_id = field_type,
                .is_pub = field.is_pub,
                .is_mut = field.is_mut,
            }) catch return;
        }

        const owned_fields = self.arena.allocator().dupe(types.Field, fields.items) catch return;
        const struct_type = self.type_table.addType(.{ .@"struct" = .{
            .name = s.name,
            .fields = owned_fields,
        } }) catch return;

        self.type_table.register(s.name, struct_type) catch return;
    }

    fn registerEnumDecl(self: *Checker, e: ast.EnumDecl, location: Location) void {
        if (e.generic_params.len > 0) {
            self.generic_decls.put(e.name, .{ .@"enum" = e }) catch return;
            _ = location;
            return;
        }

        // resolve variant field types
        var variants = std.ArrayList(types.Variant).initCapacity(self.allocator, e.variants.len) catch return;
        defer variants.deinit(self.allocator);

        for (e.variants) |variant| {
            var field_types = std.ArrayList(TypeId).initCapacity(self.allocator, variant.fields.len) catch return;
            defer field_types.deinit(self.allocator);

            for (variant.fields) |field_te| {
                const id = self.resolveTypeExpr(field_te);
                field_types.append(self.allocator, id) catch return;
            }

            const owned = self.arena.allocator().dupe(TypeId, field_types.items) catch return;
            variants.append(self.allocator, .{
                .name = variant.name,
                .fields = owned,
            }) catch return;
        }

        const owned_variants = self.arena.allocator().dupe(types.Variant, variants.items) catch return;
        const enum_type = self.type_table.addType(.{ .@"enum" = .{
            .name = e.name,
            .variants = owned_variants,
        } }) catch return;

        self.type_table.register(e.name, enum_type) catch return;
    }

    fn registerTypeAlias(self: *Checker, ta: ast.TypeAlias, location: Location) void {
        if (ta.generic_params.len > 0) {
            self.diagnostics.addError(location, "generic type aliases are not yet supported") catch {};
            return;
        }

        const target = self.resolveTypeExpr(ta.type_expr);
        if (target.isErr()) return;

        // transparent alias — the name maps to the same TypeId as the target
        self.type_table.register(ta.name, target) catch return;
    }

    // ---------------------------------------------------------------
    // pass 2 — declaration bodies
    // ---------------------------------------------------------------

    fn checkDecl(self: *Checker, decl: *const ast.Decl) void {
        switch (decl.kind) {
            .fn_decl => |fn_d| self.checkFnDecl(fn_d),
            .binding => |b| self.checkTopLevelBinding(b),
            .struct_decl => {},
            .enum_decl => {},
            .interface_decl => {},
            .impl_decl => {},
            .type_alias => {},
        }
    }

    fn checkFnDecl(self: *Checker, fn_d: ast.FnDecl) void {
        // skip generics (already reported in pass 1)
        if (fn_d.generic_params.len > 0) return;

        // look up the function's type to get param types and return type
        const fn_binding = self.module_scope.lookup(fn_d.name) orelse return;
        const fn_type = self.type_table.get(fn_binding.type_id) orelse return;
        const func = switch (fn_type) {
            .function => |f| f,
            else => return,
        };

        // create a scope for the function body
        var fn_scope = Scope.init(self.allocator, &self.module_scope);
        defer fn_scope.deinit();
        fn_scope.return_type = func.return_type;

        // define parameters in the function scope
        for (fn_d.params, func.param_types) |param, param_type| {
            fn_scope.define(param.name, .{
                .type_id = param_type,
                .is_mut = param.is_mut,
            }) catch return;
        }

        // check the body
        self.checkBlock(fn_d.body, &fn_scope);
    }

    fn checkTopLevelBinding(self: *Checker, b: ast.Binding) void {
        // infer or check the binding's type, then add to module scope
        const value_type = self.checkExpr(b.value, &self.module_scope);

        if (b.type_expr) |te| {
            const annotated = self.resolveTypeExpr(te);
            if (!annotated.isErr() and !value_type.isErr() and annotated != value_type) {
                self.diagnostics.addError(te.location, self.fmt(
                    "type mismatch: declared {s}, got {s}",
                    .{ self.type_table.typeName(annotated), self.type_table.typeName(value_type) },
                )) catch {};
            }
            self.module_scope.define(b.name, .{ .type_id = annotated, .is_mut = b.is_mut }) catch return;
        } else {
            self.module_scope.define(b.name, .{ .type_id = value_type, .is_mut = b.is_mut }) catch return;
        }
    }

    // ---------------------------------------------------------------
    // block checking
    // ---------------------------------------------------------------

    fn checkBlock(self: *Checker, block: ast.Block, scope: *Scope) void {
        for (block.stmts) |*stmt| {
            self.checkStmt(stmt, scope);
        }
    }

    fn checkStmt(self: *Checker, stmt: *const ast.Stmt, scope: *Scope) void {
        switch (stmt.kind) {
            .expr_stmt => |expr| _ = self.checkExpr(expr, scope),
            .return_stmt => |ret| self.checkReturnStmt(ret, stmt.location, scope),
            .binding => |b| self.checkBindingStmt(b, scope),
            .assignment => |a| self.checkAssignment(a, scope),
            .if_stmt => |if_s| self.checkIfStmt(if_s, scope),
            .while_stmt => |w| self.checkWhileStmt(w, scope),
            .for_stmt => |f| self.checkForStmt(f, scope),
            .fail_stmt => |f| _ = self.checkExpr(f.value, scope),
            .match_stmt => |m| self.checkMatchStmt(m, scope),
            .break_stmt => {
                if (!scope.in_loop) {
                    self.diagnostics.addError(stmt.location, "break outside of loop") catch {};
                }
            },
            .continue_stmt => {
                if (!scope.in_loop) {
                    self.diagnostics.addError(stmt.location, "continue outside of loop") catch {};
                }
            },
        }
    }

    fn checkReturnStmt(self: *Checker, ret: ast.ReturnStmt, location: Location, scope: *const Scope) void {
        const expected = scope.return_type orelse {
            self.diagnostics.addError(location, "return statement outside of function") catch {};
            return;
        };

        if (ret.value) |value| {
            const actual = self.checkExpr(value, scope);
            if (!actual.isErr() and !expected.isErr() and actual != expected) {
                self.diagnostics.addError(value.location, self.fmt(
                    "return type mismatch: expected {s}, got {s}",
                    .{ self.type_table.typeName(expected), self.type_table.typeName(actual) },
                )) catch {};
            }
        } else {
            // bare return — expected type should be Void
            if (expected != .void and !expected.isErr()) {
                self.diagnostics.addError(location, self.fmt(
                    "function expects return type {s}, got Void",
                    .{self.type_table.typeName(expected)},
                )) catch {};
            }
        }
    }

    fn checkBindingStmt(self: *Checker, b: ast.Binding, scope: *Scope) void {
        const value_type = self.checkExpr(b.value, scope);

        if (b.type_expr) |te| {
            const annotated = self.resolveTypeExpr(te);
            if (!annotated.isErr() and !value_type.isErr() and annotated != value_type) {
                self.diagnostics.addError(te.location, self.fmt(
                    "type mismatch: declared {s}, got {s}",
                    .{ self.type_table.typeName(annotated), self.type_table.typeName(value_type) },
                )) catch {};
            }
            scope.define(b.name, .{ .type_id = annotated, .is_mut = b.is_mut }) catch return;
        } else {
            scope.define(b.name, .{ .type_id = value_type, .is_mut = b.is_mut }) catch return;
        }
    }

    fn checkAssignment(self: *Checker, a: ast.Assignment, scope: *Scope) void {
        const target_type = self.checkExpr(a.target, scope);
        const value_type = self.checkExpr(a.value, scope);

        if (target_type.isErr() or value_type.isErr()) return;

        // check mutability — the target must be a mutable binding
        if (a.target.kind == .ident) {
            const name = a.target.kind.ident;
            if (scope.lookup(name)) |binding| {
                if (!binding.is_mut) {
                    self.diagnostics.addError(a.target.location, self.fmt(
                        "cannot assign to immutable variable '{s}'",
                        .{name},
                    )) catch {};
                    return;
                }
            }
        }

        // for compound assignments (+=, -=, etc.) both sides must be numeric
        if (a.op != .assign) {
            if (!target_type.isNumeric()) {
                self.diagnostics.addError(a.target.location, self.fmt(
                    "expected numeric type for compound assignment, got {s}",
                    .{self.type_table.typeName(target_type)},
                )) catch {};
                return;
            }
        }

        if (target_type != value_type) {
            self.diagnostics.addError(a.value.*.location, self.fmt(
                "type mismatch: expected {s}, got {s}",
                .{ self.type_table.typeName(target_type), self.type_table.typeName(value_type) },
            )) catch {};
        }
    }

    /// emit an error if the type isn't Bool. used for if/while/elif conditions.
    fn expectBool(self: *Checker, location: Location, actual: TypeId) void {
        if (!actual.isErr() and actual != .bool) {
            self.diagnostics.addError(location, self.fmt(
                "expected Bool in condition, got {s}",
                .{self.type_table.typeName(actual)},
            )) catch {};
        }
    }

    fn checkIfStmt(self: *Checker, if_s: ast.IfStmt, scope: *Scope) void {
        const cond = self.checkExpr(if_s.condition, scope);
        self.expectBool(if_s.condition.location, cond);

        var then_scope = Scope.init(self.allocator, scope);
        defer then_scope.deinit();
        self.checkBlock(if_s.then_block, &then_scope);

        for (if_s.elif_branches) |branch| {
            const elif_cond = self.checkExpr(branch.condition, scope);
            self.expectBool(branch.condition.location, elif_cond);
            var elif_scope = Scope.init(self.allocator, scope);
            defer elif_scope.deinit();
            self.checkBlock(branch.block, &elif_scope);
        }

        if (if_s.else_block) |else_block| {
            var else_scope = Scope.init(self.allocator, scope);
            defer else_scope.deinit();
            self.checkBlock(else_block, &else_scope);
        }
    }

    fn checkWhileStmt(self: *Checker, w: ast.WhileStmt, scope: *Scope) void {
        const cond = self.checkExpr(w.condition, scope);
        self.expectBool(w.condition.location, cond);

        var body_scope = Scope.init(self.allocator, scope);
        defer body_scope.deinit();
        body_scope.in_loop = true;
        self.checkBlock(w.body, &body_scope);
    }

    fn checkForStmt(self: *Checker, f: ast.ForStmt, scope: *Scope) void {
        // check the iterable expression
        _ = self.checkExpr(f.iterable, scope);

        // the loop variable type needs generics to determine element type,
        // so for now we give it the error sentinel to avoid false positives
        var body_scope = Scope.init(self.allocator, scope);
        defer body_scope.deinit();
        body_scope.in_loop = true;
        body_scope.define(f.binding, .{ .type_id = .err, .is_mut = false }) catch return;

        if (f.index) |idx| {
            body_scope.define(idx, .{ .type_id = .int, .is_mut = false }) catch return;
        }

        self.checkBlock(f.body, &body_scope);
    }

    // ---------------------------------------------------------------
    // type resolution — AST TypeExpr → TypeId
    // ---------------------------------------------------------------

    /// resolve an AST type expression to a TypeId in the type table.
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
        // resolve all type arguments first
        var arg_ids = std.ArrayList(TypeId).initCapacity(self.allocator, g.args.len) catch return .err;
        defer arg_ids.deinit(self.allocator);

        for (g.args) |arg| {
            const id = self.resolveTypeExpr(arg);
            if (id.isErr()) return .err;
            arg_ids.append(self.allocator, id) catch return .err;
        }

        return self.resolveGenericTypeWithArgs(g.name, arg_ids.items, location);
    }

    /// resolve a generic type by name with already-resolved type arguments.
    /// handles builtin generics (Task, Channel) and user-defined generics.
    fn resolveGenericTypeWithArgs(self: *Checker, name: []const u8, arg_ids: []const TypeId, location: Location) TypeId {
        // Task[T] and Channel[T] — builtin semantic types
        if (arg_ids.len == 1) {
            if (std.mem.eql(u8, name, "Task")) {
                return self.type_table.addType(.{ .task = .{ .inner = arg_ids[0] } }) catch return .err;
            }
            if (std.mem.eql(u8, name, "Channel")) {
                return self.type_table.addType(.{ .channel = .{ .inner = arg_ids[0] } }) catch return .err;
            }
        }

        // look up user-defined generic
        const decl = self.generic_decls.get(name) orelse {
            self.diagnostics.addError(location, self.fmt("unknown generic type '{s}'", .{name})) catch {};
            return .err;
        };

        return switch (decl) {
            .@"struct" => |s| self.instantiateGenericStruct(s, arg_ids, location),
            .@"enum" => |e| self.instantiateGenericEnum(e, arg_ids, location),
            .function => {
                self.diagnostics.addError(location, self.fmt("'{s}' is a generic function, not a type", .{name})) catch {};
                return .err;
            },
        };
    }

    /// build an instantiated name like "Pair[Int,String]" on the arena.
    fn buildInstName(self: *Checker, base: []const u8, arg_ids: []const TypeId) []const u8 {
        var buf: std.ArrayList(u8) = .empty;
        defer buf.deinit(self.allocator);

        buf.appendSlice(self.allocator, base) catch return base;
        buf.append(self.allocator, '[') catch return base;

        for (arg_ids, 0..) |id, i| {
            if (i > 0) buf.append(self.allocator, ',') catch return base;
            buf.appendSlice(self.allocator, self.type_table.typeName(id)) catch return base;
        }

        buf.append(self.allocator, ']') catch return base;

        // copy to arena for stable lifetime
        return self.arena.allocator().dupe(u8, buf.items) catch base;
    }

    /// resolve a type expression with a substitution map for type parameters.
    /// mirrors resolveTypeExpr but checks the map for named types first.
    fn resolveTypeExprWithSubst(
        self: *Checker,
        type_expr: *const ast.TypeExpr,
        subst: *const std.StringHashMap(TypeId),
    ) TypeId {
        return switch (type_expr.kind) {
            .named => |name| {
                // check substitution map before normal resolution
                if (subst.get(name)) |id| return id;
                return self.resolveNamedType(name, type_expr.location);
            },
            .optional => |inner| {
                const inner_id = self.resolveTypeExprWithSubst(inner, subst);
                if (inner_id.isErr()) return .err;
                return self.type_table.addType(.{ .optional = .{ .inner = inner_id } }) catch return .err;
            },
            .result => |r| {
                const ok_id = self.resolveTypeExprWithSubst(r.ok_type, subst);
                if (ok_id.isErr()) return .err;
                const err_id = if (r.err_type) |err_type|
                    self.resolveTypeExprWithSubst(err_type, subst)
                else
                    TypeId.err;
                return self.type_table.addType(.{ .result = .{
                    .ok_type = ok_id,
                    .err_type = err_id,
                } }) catch return .err;
            },
            .tuple => |elems| {
                var ids = std.ArrayList(TypeId).initCapacity(self.allocator, elems.len) catch return .err;
                defer ids.deinit(self.allocator);
                for (elems) |elem| {
                    const id = self.resolveTypeExprWithSubst(elem, subst);
                    if (id.isErr()) return .err;
                    ids.append(self.allocator, id) catch return .err;
                }
                const owned = self.arena.allocator().dupe(TypeId, ids.items) catch return .err;
                return self.type_table.addType(.{ .tuple = .{ .elements = owned } }) catch return .err;
            },
            .fn_type => |f| {
                var param_ids = std.ArrayList(TypeId).initCapacity(self.allocator, f.params.len) catch return .err;
                defer param_ids.deinit(self.allocator);
                for (f.params) |param| {
                    const id = self.resolveTypeExprWithSubst(param, subst);
                    if (id.isErr()) return .err;
                    param_ids.append(self.allocator, id) catch return .err;
                }
                const ret_id = if (f.return_type) |rt| self.resolveTypeExprWithSubst(rt, subst) else TypeId.void;
                if (ret_id.isErr()) return .err;
                const owned_params = self.arena.allocator().dupe(TypeId, param_ids.items) catch return .err;
                return self.type_table.addType(.{ .function = .{
                    .param_types = owned_params,
                    .return_type = ret_id,
                } }) catch return .err;
            },
            .generic => |g| {
                // resolve type args with substitution, then delegate
                var arg_ids = std.ArrayList(TypeId).initCapacity(self.allocator, g.args.len) catch return .err;
                defer arg_ids.deinit(self.allocator);
                for (g.args) |arg| {
                    const id = self.resolveTypeExprWithSubst(arg, subst);
                    if (id.isErr()) return .err;
                    arg_ids.append(self.allocator, id) catch return .err;
                }
                return self.resolveGenericTypeWithArgs(g.name, arg_ids.items, type_expr.location);
            },
        };
    }

    /// instantiate a generic struct with concrete type arguments.
    /// validates arg count, deduplicates via name_map, and resolves
    /// field types with a substitution map.
    fn instantiateGenericStruct(
        self: *Checker,
        s: ast.StructDecl,
        arg_ids: []const TypeId,
        location: Location,
    ) TypeId {
        // validate argument count
        if (arg_ids.len != s.generic_params.len) {
            self.diagnostics.addError(location, self.fmt(
                "'{s}' expects {d} type argument(s), got {d}",
                .{ s.name, s.generic_params.len, arg_ids.len },
            )) catch {};
            return .err;
        }

        // build the instantiated name and check dedup cache
        const inst_name = self.buildInstName(s.name, arg_ids);
        if (self.type_table.lookup(inst_name)) |existing| return existing;

        // build substitution map: generic param name → concrete TypeId
        var subst = std.StringHashMap(TypeId).init(self.allocator);
        defer subst.deinit();
        for (s.generic_params, arg_ids) |param, arg_id| {
            subst.put(param.name, arg_id) catch return .err;
        }

        // resolve each field type with substitution
        var fields = std.ArrayList(types.Field).initCapacity(self.allocator, s.fields.len) catch return .err;
        defer fields.deinit(self.allocator);

        for (s.fields) |field| {
            const field_type = self.resolveTypeExprWithSubst(field.type_expr, &subst);
            fields.append(self.allocator, .{
                .name = field.name,
                .type_id = field_type,
                .is_pub = field.is_pub,
                .is_mut = field.is_mut,
            }) catch return .err;
        }

        const owned_fields = self.arena.allocator().dupe(types.Field, fields.items) catch return .err;
        const type_id = self.type_table.addType(.{ .@"struct" = .{
            .name = inst_name,
            .fields = owned_fields,
        } }) catch return .err;

        self.type_table.register(inst_name, type_id) catch return .err;
        return type_id;
    }

    /// instantiate a generic enum with concrete type arguments.
    /// same pattern as instantiateGenericStruct: validate, dedup, substitute.
    fn instantiateGenericEnum(
        self: *Checker,
        e: ast.EnumDecl,
        arg_ids: []const TypeId,
        location: Location,
    ) TypeId {
        if (arg_ids.len != e.generic_params.len) {
            self.diagnostics.addError(location, self.fmt(
                "'{s}' expects {d} type argument(s), got {d}",
                .{ e.name, e.generic_params.len, arg_ids.len },
            )) catch {};
            return .err;
        }

        const inst_name = self.buildInstName(e.name, arg_ids);
        if (self.type_table.lookup(inst_name)) |existing| return existing;

        // build substitution map
        var subst = std.StringHashMap(TypeId).init(self.allocator);
        defer subst.deinit();
        for (e.generic_params, arg_ids) |param, arg_id| {
            subst.put(param.name, arg_id) catch return .err;
        }

        // resolve each variant's field types with substitution
        var variants = std.ArrayList(types.Variant).initCapacity(self.allocator, e.variants.len) catch return .err;
        defer variants.deinit(self.allocator);

        for (e.variants) |variant| {
            var field_types = std.ArrayList(TypeId).initCapacity(self.allocator, variant.fields.len) catch return .err;
            defer field_types.deinit(self.allocator);

            for (variant.fields) |field_te| {
                const id = self.resolveTypeExprWithSubst(field_te, &subst);
                field_types.append(self.allocator, id) catch return .err;
            }

            const owned = self.arena.allocator().dupe(TypeId, field_types.items) catch return .err;
            variants.append(self.allocator, .{
                .name = variant.name,
                .fields = owned,
            }) catch return .err;
        }

        const owned_variants = self.arena.allocator().dupe(types.Variant, variants.items) catch return .err;
        const type_id = self.type_table.addType(.{ .@"enum" = .{
            .name = inst_name,
            .variants = owned_variants,
        } }) catch return .err;

        self.type_table.register(inst_name, type_id) catch return .err;
        return type_id;
    }

    // ---------------------------------------------------------------
    // generic function inference + instantiation
    // ---------------------------------------------------------------

    const InferError = error{ConflictingInference};

    /// try to match a single parameter's type expression against an argument
    /// type. if the type expr is a bare name that matches a generic param,
    /// record the mapping. nested patterns (e.g. Option[T]) are deferred.
    fn matchTypeParam(
        type_expr: *const ast.TypeExpr,
        arg_type: TypeId,
        generic_params: []const ast.GenericParam,
        subst: *std.StringHashMap(TypeId),
    ) InferError!void {
        // only match direct type parameter names
        if (type_expr.kind != .named) return;
        const name = type_expr.kind.named;

        // check if this name is one of the generic params
        var is_generic = false;
        for (generic_params) |gp| {
            if (std.mem.eql(u8, gp.name, name)) {
                is_generic = true;
                break;
            }
        }
        if (!is_generic) return;

        // record or verify the mapping
        if (subst.get(name)) |existing| {
            if (existing != arg_type) return error.ConflictingInference;
        } else {
            subst.put(name, arg_type) catch return;
        }
    }

    /// infer type arguments for a generic function from call-site argument
    /// types. returns a substitution map on success, or null if inference
    /// fails (with a diagnostic emitted).
    fn inferTypeArgs(
        self: *Checker,
        fn_d: ast.FnDecl,
        arg_types: []const TypeId,
        location: Location,
    ) ?std.StringHashMap(TypeId) {
        var subst = std.StringHashMap(TypeId).init(self.allocator);

        for (fn_d.params, arg_types) |param, arg_type| {
            if (param.type_expr) |te| {
                matchTypeParam(te, arg_type, fn_d.generic_params, &subst) catch |err| switch (err) {
                    error.ConflictingInference => {
                        // find which param conflicted for the error message
                        const param_name = te.kind.named;
                        self.diagnostics.addError(location, self.fmt(
                            "conflicting types for generic parameter '{s}': {s} vs {s}",
                            .{
                                param_name,
                                self.type_table.typeName(subst.get(param_name).?),
                                self.type_table.typeName(arg_type),
                            },
                        )) catch {};
                        subst.deinit();
                        return null;
                    },
                };
            }
        }

        // verify all generic params were inferred
        for (fn_d.generic_params) |gp| {
            if (subst.get(gp.name) == null) {
                self.diagnostics.addError(location, self.fmt(
                    "could not infer type for generic parameter '{s}'",
                    .{gp.name},
                )) catch {};
                subst.deinit();
                return null;
            }
        }

        return subst;
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

            .call => |call| self.checkCall(call, expr.location, scope),

            // method_call, index, unwrap, try_expr return .err because they
            // require generics or method resolution that isn't implemented yet.
            // returning .err (the error sentinel) suppresses cascading
            // diagnostics — downstream checks skip anything typed as .err.
            .method_call => .err,
            .field_access => |fa| self.checkFieldAccess(fa, expr.location, scope),
            .index => .err,
            .unwrap => .err,
            .try_expr => .err,
            .spawn_expr => |inner| self.checkSpawnExpr(inner, expr.location, scope),
            .await_expr => |inner| self.checkAwaitExpr(inner, expr.location, scope),
            .match_expr => |m| self.checkMatchExpr(m, scope),
            .lambda => |lam| self.checkLambda(lam, scope),
            .list => .err,
            .map => .err,
            .set => .err,
            .tuple => |elems| self.checkTupleExpr(elems, expr.location, scope),
            .self_expr => .err,

            .err => .err,
        };
    }

    fn checkIdent(self: *Checker, name: []const u8, location: Location, scope: *const Scope) TypeId {
        if (scope.lookup(name)) |binding| return binding.type_id;

        // generic type names used as bare identifiers (e.g. in a call like
        // Pair(1, "hello") without type args) — suppress the diagnostic.
        // the real type comes from a binding annotation or generic use site.
        if (self.generic_decls.contains(name)) return .err;

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

    fn checkSpawnExpr(self: *Checker, inner: *const ast.Expr, location: Location, scope: *const Scope) TypeId {
        const inner_type = self.checkExpr(inner, scope);
        if (inner_type.isErr()) return .err;

        // can't spawn something that's already a task
        if (self.type_table.get(inner_type)) |ty| {
            if (ty == .task) {
                self.diagnostics.addError(location, "cannot spawn a Task") catch {};
                return .err;
            }
        }

        return self.type_table.addType(.{ .task = .{ .inner = inner_type } }) catch return .err;
    }

    fn checkAwaitExpr(self: *Checker, inner: *const ast.Expr, location: Location, scope: *const Scope) TypeId {
        const inner_type = self.checkExpr(inner, scope);
        if (inner_type.isErr()) return .err;

        // the operand must be a Task[T]
        if (self.type_table.get(inner_type)) |ty| {
            if (ty == .task) {
                return ty.task.inner;
            }
        }

        self.diagnostics.addError(location, self.fmt(
            "expected Task, got {s}",
            .{self.type_table.typeName(inner_type)},
        )) catch {};
        return .err;
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
        self.expectBool(if_e.condition.location, cond);

        const then_type = self.checkExpr(if_e.then_expr, scope);

        // check elif branches
        for (if_e.elif_branches) |branch| {
            const elif_cond = self.checkExpr(branch.condition, scope);
            self.expectBool(branch.condition.location, elif_cond);

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

    // call dispatch logic:
    // if the callee is a struct type name, route to struct constructor
    // checking. however, some struct types (Mutex, WaitGroup, Semaphore)
    // are registered as zero-field structs but also have constructor
    // functions in scope — when the arg count doesn't match the field
    // count and a function binding exists, we fall through to normal
    // function call checking instead.
    fn checkCall(self: *Checker, call: ast.CallExpr, location: Location, scope: *const Scope) TypeId {
        // check if the callee is a struct type name — route to constructor
        if (call.callee.kind == .ident) {
            const name = call.callee.kind.ident;
            if (self.type_table.lookup(name)) |type_id| {
                if (self.type_table.get(type_id)) |ty| {
                    if (ty == .@"struct") {
                        // if arg count doesn't match field count but there's
                        // a function binding in scope (e.g. sync type constructors),
                        // fall through to function call checking
                        if (ty.@"struct".fields.len != call.args.len) {
                            if (scope.lookup(name) != null) {
                                return self.checkFnCall(call, location, scope);
                            }
                        }
                        return self.checkStructConstructor(type_id, call, location, scope);
                    }
                }
            }
        }

        return self.checkFnCall(call, location, scope);
    }

    fn checkFnCall(self: *Checker, call: ast.CallExpr, location: Location, scope: *const Scope) TypeId {
        const callee_type = self.checkExpr(call.callee, scope);
        if (callee_type.isErr()) return .err;

        // look up the function type
        const ty = self.type_table.get(callee_type) orelse return .err;
        const func = switch (ty) {
            .function => |f| f,
            else => {
                self.diagnostics.addError(location, self.fmt(
                    "{s} is not callable",
                    .{self.type_table.typeName(callee_type)},
                )) catch {};
                return .err;
            },
        };

        // check argument count
        if (call.args.len != func.param_types.len) {
            self.diagnostics.addError(location, self.fmt(
                "expected {d} argument(s), got {d}",
                .{ func.param_types.len, call.args.len },
            )) catch {};
            return .err;
        }

        // check argument types
        for (call.args, func.param_types) |arg, expected| {
            const actual = self.checkExpr(arg.value, scope);
            if (!actual.isErr() and !expected.isErr() and actual != expected) {
                self.diagnostics.addError(arg.location, self.fmt(
                    "expected {s}, got {s}",
                    .{ self.type_table.typeName(expected), self.type_table.typeName(actual) },
                )) catch {};
            }
        }

        return func.return_type;
    }

    fn checkStructConstructor(
        self: *Checker,
        type_id: TypeId,
        call: ast.CallExpr,
        location: Location,
        scope: *const Scope,
    ) TypeId {
        const struct_data = self.type_table.get(type_id).?.@"struct";

        // check argument count matches field count
        if (call.args.len != struct_data.fields.len) {
            self.diagnostics.addError(location, self.fmt(
                "{s} has {d} field(s), got {d} argument(s)",
                .{ struct_data.name, struct_data.fields.len, call.args.len },
            )) catch {};
            return .err;
        }

        // check each argument type against the corresponding field
        for (call.args, struct_data.fields) |arg, field| {
            const actual = self.checkExpr(arg.value, scope);
            if (!actual.isErr() and !field.type_id.isErr() and actual != field.type_id) {
                self.diagnostics.addError(arg.location, self.fmt(
                    "expected {s} for field '{s}', got {s}",
                    .{ self.type_table.typeName(field.type_id), field.name, self.type_table.typeName(actual) },
                )) catch {};
            }
        }

        return type_id;
    }

    fn checkFieldAccess(self: *Checker, fa: ast.FieldAccess, location: Location, scope: *const Scope) TypeId {
        const object_type = self.checkExpr(fa.object, scope);
        if (object_type.isErr()) return .err;

        const ty = self.type_table.get(object_type) orelse return .err;
        const struct_data = switch (ty) {
            .@"struct" => |s| s,
            else => {
                self.diagnostics.addError(location, self.fmt(
                    "{s} has no field '{s}'",
                    .{ self.type_table.typeName(object_type), fa.field },
                )) catch {};
                return .err;
            },
        };

        // look up the field
        for (struct_data.fields) |field| {
            if (std.mem.eql(u8, field.name, fa.field)) {
                return field.type_id;
            }
        }

        self.diagnostics.addError(location, self.fmt(
            "struct {s} has no field '{s}'",
            .{ struct_data.name, fa.field },
        )) catch {};
        return .err;
    }

    fn checkMatchExpr(self: *Checker, m: ast.MatchExpr, scope: *const Scope) TypeId {
        const subject_type = self.checkExpr(m.subject, scope);
        if (subject_type.isErr()) return .err;

        var expected_type: TypeId = .err;

        for (m.arms) |arm| {
            const arm_type = self.checkMatchArm(arm, subject_type, scope);
            if (arm_type.isErr()) continue;

            if (expected_type.isErr()) {
                // first non-error arm establishes the expected type
                expected_type = arm_type;
            } else if (arm_type != expected_type) {
                self.diagnostics.addError(arm.location, self.fmt(
                    "match arm type mismatch: expected {s}, got {s}",
                    .{ self.type_table.typeName(expected_type), self.type_table.typeName(arm_type) },
                )) catch {};
            }
        }

        return expected_type;
    }

    fn checkMatchStmt(self: *Checker, m: ast.MatchExpr, scope: *const Scope) void {
        const subject_type = self.checkExpr(m.subject, scope);
        if (subject_type.isErr()) return;

        // match statement — no arm type agreement needed
        for (m.arms) |arm| {
            _ = self.checkMatchArm(arm, subject_type, scope);
        }
    }

    fn checkMatchArm(self: *Checker, arm: ast.MatchArm, subject_type: TypeId, scope: *const Scope) TypeId {
        // each arm gets its own scope for pattern bindings
        var arm_scope = Scope.init(self.allocator, @constCast(scope));
        defer arm_scope.deinit();

        self.checkPattern(arm.pattern, subject_type, &arm_scope);

        // check guard expression if present
        if (arm.guard) |guard| {
            const guard_type = self.checkExpr(guard, &arm_scope);
            if (!guard_type.isErr() and guard_type != .bool) {
                self.diagnostics.addError(guard.location, self.fmt(
                    "match guard must be Bool, got {s}",
                    .{self.type_table.typeName(guard_type)},
                )) catch {};
            }
        }

        // check arm body
        return switch (arm.body) {
            .expr => |e| self.checkExpr(e, &arm_scope),
            .block => |block| {
                var block_scope = Scope.init(self.allocator, &arm_scope);
                defer block_scope.deinit();
                self.checkBlock(block, &block_scope);
                return .void;
            },
        };
    }

    /// emit an error if the subject type doesn't match the expected literal type.
    fn checkLiteralPattern(self: *Checker, subject_type: TypeId, expected: TypeId, type_name: []const u8, location: Location) void {
        if (!subject_type.isErr() and subject_type != expected) {
            self.diagnostics.addError(location, self.fmt(
                "cannot match {s} literal against {s}",
                .{ type_name, self.type_table.typeName(subject_type) },
            )) catch {};
        }
    }

    fn checkPattern(self: *Checker, pattern: ast.Pattern, subject_type: TypeId, scope: *Scope) void {
        switch (pattern.kind) {
            .wildcard => {},
            .int_lit => self.checkLiteralPattern(subject_type, .int, "Int", pattern.location),
            .float_lit => self.checkLiteralPattern(subject_type, .float, "Float", pattern.location),
            .string_lit => self.checkLiteralPattern(subject_type, .string, "String", pattern.location),
            .bool_lit => self.checkLiteralPattern(subject_type, .bool, "Bool", pattern.location),
            .none_lit => {}, // needs Optional types — skip for now
            .binding => |name| {
                scope.define(name, .{ .type_id = subject_type, .is_mut = false }) catch {};
            },
            .variant => |v| self.checkVariantPattern(v, subject_type, pattern.location, scope),
            .tuple => |elems| self.checkTuplePattern(elems, subject_type, pattern.location, scope),
        }
    }

    fn checkVariantPattern(
        self: *Checker,
        v: ast.VariantPattern,
        subject_type: TypeId,
        location: Location,
        scope: *Scope,
    ) void {
        if (subject_type.isErr()) return;

        // look up the enum type by name
        const enum_type_id = self.type_table.lookup(v.type_name) orelse {
            self.diagnostics.addError(location, self.fmt(
                "unknown type '{s}'",
                .{v.type_name},
            )) catch {};
            return;
        };

        if (enum_type_id != subject_type) {
            self.diagnostics.addError(location, self.fmt(
                "pattern type {s} does not match subject type {s}",
                .{ v.type_name, self.type_table.typeName(subject_type) },
            )) catch {};
            return;
        }

        const ty = self.type_table.get(enum_type_id) orelse return;
        const enum_data = switch (ty) {
            .@"enum" => |e| e,
            else => {
                self.diagnostics.addError(location, self.fmt(
                    "{s} is not an enum type",
                    .{v.type_name},
                )) catch {};
                return;
            },
        };

        // find the variant
        for (enum_data.variants) |variant| {
            if (std.mem.eql(u8, variant.name, v.variant)) {
                // check field count
                if (v.fields.len != variant.fields.len) {
                    self.diagnostics.addError(location, self.fmt(
                        "variant {s}.{s} has {d} field(s), pattern has {d}",
                        .{ v.type_name, v.variant, variant.fields.len, v.fields.len },
                    )) catch {};
                    return;
                }

                // recurse into sub-patterns with field types
                for (v.fields, variant.fields) |sub_pattern, field_type| {
                    self.checkPattern(sub_pattern, field_type, scope);
                }
                return;
            }
        }

        self.diagnostics.addError(location, self.fmt(
            "enum {s} has no variant '{s}'",
            .{ v.type_name, v.variant },
        )) catch {};
    }

    fn checkTuplePattern(
        self: *Checker,
        elems: []const ast.Pattern,
        subject_type: TypeId,
        location: Location,
        scope: *Scope,
    ) void {
        if (subject_type.isErr()) return;

        const ty = self.type_table.get(subject_type) orelse return;
        const tuple_data = switch (ty) {
            .tuple => |t| t,
            else => {
                self.diagnostics.addError(location, self.fmt(
                    "cannot match tuple pattern against {s}",
                    .{self.type_table.typeName(subject_type)},
                )) catch {};
                return;
            },
        };

        if (elems.len != tuple_data.elements.len) {
            self.diagnostics.addError(location, self.fmt(
                "tuple has {d} element(s), pattern has {d}",
                .{ tuple_data.elements.len, elems.len },
            )) catch {};
            return;
        }

        for (elems, tuple_data.elements) |sub_pattern, elem_type| {
            self.checkPattern(sub_pattern, elem_type, scope);
        }
    }

    fn checkLambda(self: *Checker, lam: ast.Lambda, scope: *const Scope) TypeId {
        // resolve parameter types — require annotations (no inference yet)
        var param_ids = std.ArrayList(TypeId).initCapacity(self.allocator, lam.params.len) catch return .err;
        defer param_ids.deinit(self.allocator);

        for (lam.params) |param| {
            if (param.type_expr) |te| {
                const id = self.resolveTypeExpr(te);
                param_ids.append(self.allocator, id) catch return .err;
            } else {
                self.diagnostics.addError(param.location, self.fmt(
                    "lambda parameter '{s}' needs a type annotation",
                    .{param.name},
                )) catch {};
                return .err;
            }
        }

        // create a child scope for the lambda body
        var lambda_scope = Scope.init(self.allocator, @constCast(scope));
        defer lambda_scope.deinit();

        for (lam.params, param_ids.items) |param, param_type| {
            lambda_scope.define(param.name, .{
                .type_id = param_type,
                .is_mut = param.is_mut,
            }) catch return .err;
        }

        // determine return type based on body form
        const return_type: TypeId = switch (lam.body) {
            .expr => |body_expr| self.checkExpr(body_expr, &lambda_scope),
            .block => |block| blk: {
                lambda_scope.return_type = .void;
                self.checkBlock(block, &lambda_scope);
                break :blk .void;
            },
        };

        // build the function type
        const owned_params = self.arena.allocator().dupe(TypeId, param_ids.items) catch return .err;
        return self.type_table.addType(.{ .function = .{
            .param_types = owned_params,
            .return_type = return_type,
        } }) catch return .err;
    }

    fn checkTupleExpr(self: *Checker, elems: []const *const ast.Expr, location: Location, scope: *const Scope) TypeId {
        if (elems.len == 0) {
            self.diagnostics.addError(location, "empty tuple is not allowed") catch {};
            return .err;
        }

        var elem_types = std.ArrayList(TypeId).initCapacity(self.allocator, elems.len) catch return .err;
        defer elem_types.deinit(self.allocator);

        for (elems) |elem| {
            const id = self.checkExpr(elem, scope);
            if (id.isErr()) return .err;
            elem_types.append(self.allocator, id) catch return .err;
        }

        const owned = self.arena.allocator().dupe(TypeId, elem_types.items) catch return .err;
        return self.type_table.addType(.{ .tuple = .{ .elements = owned } }) catch return .err;
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

test "undeclared generic List[Int] errors" {
    var checker = try Checker.init(std.testing.allocator, "");
    defer checker.deinit();

    // List is not declared, so List[Int] should error with "unknown generic type"
    const inner = ast.TypeExpr{ .kind = .{ .named = "Int" }, .location = Location.zero };
    const generic = ast.TypeExpr{
        .kind = .{ .generic = .{ .name = "List", .args = &.{&inner} } },
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

    // match 1: 1 => "one", 2 => "two"
    const subject = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const one_result = ast.Expr{ .kind = .{ .string_lit = "one" }, .location = Location.zero };
    const two_result = ast.Expr{ .kind = .{ .string_lit = "two" }, .location = Location.zero };

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

    // match s: Shape.Circle(r) => r
    const subject = ast.Expr{ .kind = .{ .ident = "s" }, .location = Location.zero };
    const r_expr = ast.Expr{ .kind = .{ .ident = "r" }, .location = Location.zero };

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

    // match 1: 1 => "string", 2 => 42 (as statement, no type agreement needed)
    const subject = ast.Expr{ .kind = .{ .int_lit = "1" }, .location = Location.zero };
    const str_result = ast.Expr{ .kind = .{ .string_lit = "one" }, .location = Location.zero };
    const int_result = ast.Expr{ .kind = .{ .int_lit = "42" }, .location = Location.zero };

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
            },
        } },
        .location = Location.zero,
    };

    var scope = Scope.init(std.testing.allocator, &checker.module_scope);
    defer scope.deinit();
    checker.checkStmt(&stmt, &scope);
    try std.testing.expect(!checker.diagnostics.hasErrors());
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
    checker.generic_decls.put("Box", .{ .@"struct" = .{
        .name = "Box",
        .generic_params = &.{.{ .name = "T", .bounds = &.{}, .location = Location.zero }},
        .fields = &.{
            .{ .name = "value", .type_expr = &t_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
        },
    } }) catch unreachable;

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
    checker.generic_decls.put("Pair", .{ .@"struct" = .{
        .name = "Pair",
        .generic_params = &.{
            .{ .name = "A", .bounds = &.{}, .location = Location.zero },
            .{ .name = "B", .bounds = &.{}, .location = Location.zero },
        },
        .fields = &.{
            .{ .name = "first", .type_expr = &a_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            .{ .name = "second", .type_expr = &b_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
        },
    } }) catch unreachable;

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
    checker.generic_decls.put("Box", .{ .@"struct" = .{
        .name = "Box",
        .generic_params = &.{.{ .name = "T", .bounds = &.{}, .location = Location.zero }},
        .fields = &.{
            .{ .name = "value", .type_expr = &t_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
        },
    } }) catch unreachable;

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
    checker.generic_decls.put("Pair", .{ .@"struct" = .{
        .name = "Pair",
        .generic_params = &.{
            .{ .name = "A", .bounds = &.{}, .location = Location.zero },
            .{ .name = "B", .bounds = &.{}, .location = Location.zero },
        },
        .fields = &.{
            .{ .name = "first", .type_expr = &a_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
            .{ .name = "second", .type_expr = &b_te, .default = null, .is_pub = true, .is_mut = false, .is_weak = false, .location = Location.zero },
        },
    } }) catch unreachable;

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
    checker.generic_decls.put("Option", .{ .@"enum" = .{
        .name = "Option",
        .generic_params = &.{.{ .name = "T", .bounds = &.{}, .location = Location.zero }},
        .variants = &.{
            .{ .name = "Some", .fields = &.{&t_te}, .location = Location.zero },
            .{ .name = "None", .fields = &.{}, .location = Location.zero },
        },
    } }) catch unreachable;

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
    checker.generic_decls.put("Option", .{ .@"enum" = .{
        .name = "Option",
        .generic_params = &.{.{ .name = "T", .bounds = &.{}, .location = Location.zero }},
        .variants = &.{
            .{ .name = "Some", .fields = &.{&t_te}, .location = Location.zero },
            .{ .name = "None", .fields = &.{}, .location = Location.zero },
        },
    } }) catch unreachable;

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
    checker.generic_decls.put("Option", .{ .@"enum" = .{
        .name = "Option",
        .generic_params = &.{.{ .name = "T", .bounds = &.{}, .location = Location.zero }},
        .variants = &.{
            .{ .name = "Some", .fields = &.{&t_te}, .location = Location.zero },
            .{ .name = "None", .fields = &.{}, .location = Location.zero },
        },
    } }) catch unreachable;

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

    checker.generic_decls.put("identity", .{ .function = .{
        .name = "identity",
        .generic_params = &.{
            .{ .name = "T", .bounds = &.{}, .location = Location.zero },
        },
        .params = &.{
            .{ .name = "x", .type_expr = &t_te, .default = null, .is_mut = false, .is_ref = false, .location = Location.zero },
        },
        .return_type = &t_te,
        .body = .{ .stmts = &.{ret}, .location = Location.zero },
    } }) catch unreachable;

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
