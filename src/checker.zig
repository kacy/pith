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
        // check for generic params — not yet supported
        if (fn_d.generic_params.len > 0) {
            self.diagnostics.addError(location, "generic functions are not yet supported") catch {};
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
            self.diagnostics.addError(location, "generic structs are not yet supported") catch {};
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
            self.diagnostics.addError(location, "generic enums are not yet supported") catch {};
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
            .match_stmt => |m| _ = self.checkExpr(m.subject, scope),
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

    fn checkIfStmt(self: *Checker, if_s: ast.IfStmt, scope: *Scope) void {
        const cond = self.checkExpr(if_s.condition, scope);
        if (!cond.isErr() and cond != .bool) {
            self.diagnostics.addError(if_s.condition.location, self.fmt(
                "expected Bool in condition, got {s}",
                .{self.type_table.typeName(cond)},
            )) catch {};
        }

        var then_scope = Scope.init(self.allocator, scope);
        defer then_scope.deinit();
        self.checkBlock(if_s.then_block, &then_scope);

        for (if_s.elif_branches) |branch| {
            const elif_cond = self.checkExpr(branch.condition, scope);
            if (!elif_cond.isErr() and elif_cond != .bool) {
                self.diagnostics.addError(branch.condition.location, self.fmt(
                    "expected Bool in condition, got {s}",
                    .{self.type_table.typeName(elif_cond)},
                )) catch {};
            }
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
        if (!cond.isErr() and cond != .bool) {
            self.diagnostics.addError(w.condition.location, self.fmt(
                "expected Bool in condition, got {s}",
                .{self.type_table.typeName(cond)},
            )) catch {};
        }

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

            .call => |call| self.checkCall(call, expr.location, scope),
            .method_call => .err,
            .field_access => |fa| self.checkFieldAccess(fa, expr.location, scope),
            .index => .err,
            .unwrap => .err,
            .try_expr => .err,
            .match_expr => .err,
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

    fn checkCall(self: *Checker, call: ast.CallExpr, location: Location, scope: *const Scope) TypeId {
        // check if the callee is a struct type name — route to constructor
        if (call.callee.kind == .ident) {
            const name = call.callee.kind.ident;
            if (self.type_table.lookup(name)) |type_id| {
                if (self.type_table.get(type_id)) |ty| {
                    if (ty == .@"struct") {
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
