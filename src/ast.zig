// ast — abstract syntax tree node definitions
//
// defines all AST node types produced by the parser.
// organized by grammar section: module, declarations, statements,
// expressions, types, and patterns.
//
// uses zig tagged unions for type-safe node variants. every node
// carries a location for error reporting.

const std = @import("std");
const errors = @import("errors.zig");

pub const Location = errors.Location;

// ---------------------------------------------------------------
// module
// ---------------------------------------------------------------

pub const Module = struct {
    imports: []const ImportDecl,
    decls: []const Decl,
};

pub const ImportDecl = struct {
    kind: ImportKind,
    location: Location,
};

pub const ImportKind = union(enum) {
    /// import std.io [as alias]
    simple: struct {
        path: []const []const u8,
        alias: ?[]const u8,
    },
    /// from std.io import read_file, write_file
    from: struct {
        path: []const []const u8,
        names: []const ImportName,
    },
};

pub const ImportName = struct {
    name: []const u8,
    alias: ?[]const u8,
    location: Location,
};

// ---------------------------------------------------------------
// declarations
// ---------------------------------------------------------------

pub const Decl = struct {
    kind: DeclKind,
    is_pub: bool,
    location: Location,
};

/// the kind of a top-level declaration.
pub const DeclKind = union(enum) {
    fn_decl: FnDecl,
    struct_decl: StructDecl,
    enum_decl: EnumDecl,
    interface_decl: InterfaceDecl,
    impl_decl: ImplDecl,
    type_alias: TypeAlias,
    binding: Binding,
};

pub const FnDecl = struct {
    name: []const u8,
    generic_params: []const GenericParam,
    params: []const Param,
    return_type: ?*const TypeExpr,
    body: Block,
};

pub const Param = struct {
    name: []const u8,
    type_expr: ?*const TypeExpr,
    default: ?*const Expr,
    is_mut: bool,
    is_ref: bool,
    location: Location,
};

pub const GenericParam = struct {
    name: []const u8,
    bounds: []const *const TypeExpr,
    location: Location,
};

pub const StructDecl = struct {
    name: []const u8,
    generic_params: []const GenericParam,
    fields: []const StructField,
};

pub const StructField = struct {
    name: []const u8,
    type_expr: *const TypeExpr,
    default: ?*const Expr,
    is_pub: bool,
    is_mut: bool,
    is_weak: bool,
    location: Location,
};

pub const EnumDecl = struct {
    name: []const u8,
    generic_params: []const GenericParam,
    variants: []const EnumVariant,
};

pub const EnumVariant = struct {
    name: []const u8,
    fields: []const *const TypeExpr,
    location: Location,
};

pub const InterfaceDecl = struct {
    name: []const u8,
    generic_params: []const GenericParam,
    methods: []const FnSig,
};

pub const FnSig = struct {
    name: []const u8,
    generic_params: []const GenericParam,
    params: []const Param,
    return_type: ?*const TypeExpr,
    location: Location,
};

pub const ImplDecl = struct {
    target: *const TypeExpr,
    interface: ?*const TypeExpr,
    methods: []const ImplMethod,
};

pub const ImplMethod = struct {
    is_pub: bool,
    decl: FnDecl,
    location: Location,
};

pub const TypeAlias = struct {
    name: []const u8,
    generic_params: []const GenericParam,
    type_expr: *const TypeExpr,
};

// ---------------------------------------------------------------
// statements
// ---------------------------------------------------------------

pub const Block = struct {
    stmts: []const Stmt,
    location: Location,
};

pub const Stmt = struct {
    kind: StmtKind,
    location: Location,
};

/// the kind of a statement inside a block.
pub const StmtKind = union(enum) {
    binding: Binding,
    assignment: Assignment,
    if_stmt: IfStmt,
    for_stmt: ForStmt,
    while_stmt: WhileStmt,
    match_stmt: MatchExpr,
    return_stmt: ReturnStmt,
    fail_stmt: FailStmt,
    break_stmt,
    continue_stmt,
    expr_stmt: *const Expr,
};

pub const Binding = struct {
    name: []const u8,
    type_expr: ?*const TypeExpr,
    value: *const Expr,
    is_mut: bool,
};

pub const Assignment = struct {
    target: *const Expr,
    op: AssignOp,
    value: *const Expr,
};

pub const AssignOp = enum {
    assign, // =
    add, // +=
    sub, // -=
    mul, // *=
    div, // /=
};

pub const IfStmt = struct {
    condition: *const Expr,
    then_block: Block,
    elif_branches: []const ElifBranch,
    else_block: ?Block,
};

pub const ElifBranch = struct {
    condition: *const Expr,
    block: Block,
    location: Location,
};

pub const ForStmt = struct {
    binding: []const u8,
    index: ?[]const u8,
    iterable: *const Expr,
    body: Block,
};

pub const WhileStmt = struct {
    condition: *const Expr,
    body: Block,
};

pub const ReturnStmt = struct {
    value: ?*const Expr,
};

pub const FailStmt = struct {
    value: *const Expr,
};

// ---------------------------------------------------------------
// expressions
// ---------------------------------------------------------------

pub const Expr = struct {
    kind: ExprKind,
    location: Location,
};

/// the kind of an expression node. covers literals, operators, calls,
/// control flow, collections, and the error recovery sentinel.
pub const ExprKind = union(enum) {
    // literals
    int_lit: []const u8,
    float_lit: []const u8,
    string_lit: []const u8,
    bool_lit: bool,
    none_lit,

    // identifiers
    ident: []const u8,
    self_expr,

    // operations
    binary: BinaryExpr,
    unary: UnaryExpr,

    // access
    call: CallExpr,
    method_call: MethodCallExpr,
    field_access: FieldAccess,
    index: IndexExpr,

    // postfix operators
    unwrap: *const Expr, // expr?
    try_expr: *const Expr, // expr!

    // concurrency
    spawn_expr: *const Expr, // spawn <expr>
    await_expr: *const Expr, // await <expr>

    // control flow expressions
    if_expr: IfExpr,
    match_expr: MatchExpr,

    // function
    lambda: Lambda,

    // collections
    list: []const *const Expr,
    map: []const MapEntry,
    set: []const *const Expr,
    tuple: []const *const Expr,

    // string interpolation
    string_interp: StringInterp,

    // grouped expression (parenthesized)
    grouped: *const Expr,

    // error recovery sentinel
    err,
};

pub const BinaryExpr = struct {
    left: *const Expr,
    op: BinaryOp,
    right: *const Expr,
};

pub const BinaryOp = enum {
    add, // +
    sub, // -
    mul, // *
    div, // /
    mod, // %
    eq, // ==
    neq, // !=
    lt, // <
    gt, // >
    lte, // <=
    gte, // >=
    @"and", // and
    @"or", // or
    pipe, // |
};

pub const UnaryExpr = struct {
    op: UnaryOp,
    operand: *const Expr,
};

pub const UnaryOp = enum {
    negate, // -
    not, // not
};

pub const CallExpr = struct {
    callee: *const Expr,
    args: []const Arg,
};

pub const MethodCallExpr = struct {
    receiver: *const Expr,
    method: []const u8,
    args: []const Arg,
};

pub const Arg = struct {
    name: ?[]const u8,
    value: *const Expr,
    location: Location,
};

pub const FieldAccess = struct {
    object: *const Expr,
    field: []const u8,
};

pub const IndexExpr = struct {
    object: *const Expr,
    index: *const Expr,
};

pub const IfExpr = struct {
    condition: *const Expr,
    then_expr: *const Expr,
    elif_branches: []const ElifExprBranch,
    else_expr: *const Expr,
};

pub const ElifExprBranch = struct {
    condition: *const Expr,
    expr: *const Expr,
    location: Location,
};

pub const MatchExpr = struct {
    subject: *const Expr,
    arms: []const MatchArm,
};

pub const MatchArm = struct {
    pattern: Pattern,
    guard: ?*const Expr,
    body: MatchBody,
    location: Location,
};

pub const MatchBody = union(enum) {
    expr: *const Expr,
    block: Block,
};

pub const Lambda = struct {
    params: []const Param,
    body: LambdaBody,
};

pub const LambdaBody = union(enum) {
    expr: *const Expr,
    block: Block,
};

pub const MapEntry = struct {
    key: *const Expr,
    value: *const Expr,
    location: Location,
};

pub const StringInterp = struct {
    parts: []const StringPart,
};

pub const StringPart = union(enum) {
    literal: []const u8,
    expr: *const Expr,
};

// ---------------------------------------------------------------
// types
// ---------------------------------------------------------------

pub const TypeExpr = struct {
    kind: TypeExprKind,
    location: Location,
};

/// the kind of a type annotation in source (Int, List[T], T?, T!, etc.)
pub const TypeExprKind = union(enum) {
    named: []const u8,
    generic: GenericType,
    optional: *const TypeExpr,
    result: ResultType,
    tuple: []const *const TypeExpr,
    fn_type: FnType,
};

pub const GenericType = struct {
    name: []const u8,
    args: []const *const TypeExpr,
};

pub const ResultType = struct {
    ok_type: *const TypeExpr,
    err_type: ?*const TypeExpr,
};

pub const FnType = struct {
    params: []const *const TypeExpr,
    return_type: ?*const TypeExpr,
};

// ---------------------------------------------------------------
// patterns
// ---------------------------------------------------------------

pub const Pattern = struct {
    kind: PatternKind,
    location: Location,
};

/// the kind of a match pattern (wildcard, literal, binding, variant, tuple).
pub const PatternKind = union(enum) {
    wildcard,
    int_lit: []const u8,
    float_lit: []const u8,
    string_lit: []const u8,
    bool_lit: bool,
    none_lit,
    binding: []const u8,
    variant: VariantPattern,
    tuple: []const Pattern,
};

pub const VariantPattern = struct {
    type_name: []const u8,
    variant: []const u8,
    fields: []const Pattern,
};

// ---------------------------------------------------------------
// tests
// ---------------------------------------------------------------

test "ast types are well-formed" {
    // verify that key tagged unions can be constructed
    const expr_kind: ExprKind = .{ .int_lit = "42" };
    try std.testing.expectEqualStrings("42", expr_kind.int_lit);

    const stmt_kind: StmtKind = .break_stmt;
    try std.testing.expect(stmt_kind == .break_stmt);

    const pattern_kind: PatternKind = .wildcard;
    try std.testing.expect(pattern_kind == .wildcard);
}

test "binary op variants exist" {
    const ops = [_]BinaryOp{ .add, .sub, .mul, .div, .mod, .eq, .neq, .lt, .gt, .lte, .gte, .@"and", .@"or", .pipe };
    try std.testing.expectEqual(@typeInfo(BinaryOp).@"enum".fields.len, ops.len);
}

test "decl kind variants exist" {
    // make sure all declaration kinds are accounted for
    const d: DeclKind = .{ .type_alias = .{
        .name = "Foo",
        .generic_params = &.{},
        .type_expr = undefined,
    } };
    try std.testing.expect(d == .type_alias);
}
