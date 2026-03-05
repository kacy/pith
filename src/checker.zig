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
const Lexer = @import("lexer.zig").Lexer;
const Parser = @import("parser.zig").Parser;

const TypeId = types.TypeId;
const TypeTable = types.TypeTable;
const Type = types.Type;
const Location = errors.Location;
const ErrorCode = errors.ErrorCode;

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

/// a method registered from an impl block. stores the function type,
/// visibility, and the original AST decl for pass 2 body checking.
pub const MethodEntry = struct {
    type_id: TypeId,
    is_pub: bool,
    decl: ast.FnDecl,
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
    parent: ?*const Scope,
    /// the return type of the enclosing function (if any).
    /// used to check return statements.
    return_type: ?TypeId,
    /// true when inside a while or for loop body.
    /// used to validate break/continue statements.
    in_loop: bool,

    pub fn init(allocator: std.mem.Allocator, parent: ?*const Scope) Scope {
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
// imported module data
// ---------------------------------------------------------------

/// an imported module's declarations — passed to codegen so it can
/// emit the imported functions/types into the output.
pub const ImportedModule = struct {
    /// the parsed module AST (owned by the arena in parsed_arenas)
    module: ast.Module,
    /// path used for dedup (arena-allocated)
    path: []const u8,
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
    /// interface declarations stored during pass 1, keyed by name.
    /// used to distinguish interfaces from structs in the type table
    /// and to validate impl blocks and generic bounds.
    interface_decls: std.StringHashMap(ast.InterfaceDecl),
    /// tracks which types implement which interfaces. key format is
    /// "TypeName\x00InterfaceName" (null-separated, arena-allocated).
    /// presence means the type implements the interface.
    impl_set: std.StringHashMap(void),
    /// methods registered from impl blocks. key is "TypeName.methodName"
    /// (arena-allocated). used for method call resolution and pass 2
    /// body checking.
    method_types: std.StringHashMap(MethodEntry),
    /// tracks recursion depth in resolveTypeExpr to prevent stack overflow
    /// from deeply nested types like Int??????...
    resolve_depth: u32,
    /// path to the source file being checked (used for resolving relative imports)
    source_path: ?[]const u8,
    /// tracks files currently being checked to detect import cycles
    checking_files: ?*std.StringHashMap(void),
    /// declarations from imported modules (for codegen)
    imported_modules: std.ArrayList(ImportedModule),

    /// maximum depth for type resolution. prevents stack overflow from
    /// pathological inputs like deeply nested optionals or generics.
    const max_resolve_depth: u32 = 128;

    /// create a new checker. registers builtin types and functions.
    pub fn init(allocator: std.mem.Allocator, source: []const u8) !Checker {
        var checker = Checker{
            .type_table = try TypeTable.init(allocator),
            .diagnostics = errors.DiagnosticList.init(allocator, source),
            .allocator = allocator,
            .arena = std.heap.ArenaAllocator.init(allocator),
            .module_scope = Scope.init(allocator, null),
            .generic_decls = std.StringHashMap(GenericDecl).init(allocator),
            .interface_decls = std.StringHashMap(ast.InterfaceDecl).init(allocator),
            .impl_set = std.StringHashMap(void).init(allocator),
            .method_types = std.StringHashMap(MethodEntry).init(allocator),
            .resolve_depth = 0,
            .source_path = null,
            .checking_files = null,
            .imported_modules = .empty,
        };

        // register builtins into the module scope
        try checker.registerBuiltinFunctions();

        return checker;
    }

    pub fn deinit(self: *Checker) void {
        self.module_scope.deinit();
        self.generic_decls.deinit();
        self.interface_decls.deinit();
        self.impl_set.deinit();
        self.method_types.deinit();
        self.imported_modules.deinit(self.allocator);
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

        // pre-register List[String] — used by String.split() and args()
        _ = self.internCollectionType("List", &.{.string}, .{ .list = .{ .element = .string } });

        // parse_int(String) -> Int!
        const int_result = try self.type_table.addType(.{ .result = .{
            .ok_type = .int,
            .err_type = .err,
        } });
        const parse_int_type = try self.type_table.addType(.{ .function = .{
            .param_types = &.{.string},
            .return_type = int_result,
        } });
        try self.module_scope.define("parse_int", .{ .type_id = parse_int_type, .is_mut = false });

        // parse_float(String) -> Float!
        const float_result = try self.type_table.addType(.{ .result = .{
            .ok_type = .float,
            .err_type = .err,
        } });
        const parse_float_type = try self.type_table.addType(.{ .function = .{
            .param_types = &.{.string},
            .return_type = float_result,
        } });
        try self.module_scope.define("parse_float", .{ .type_id = parse_float_type, .is_mut = false });

        // read_file(String) -> String!
        const str_result = try self.type_table.addType(.{ .result = .{
            .ok_type = .string,
            .err_type = .err,
        } });
        const read_file_type = try self.type_table.addType(.{ .function = .{
            .param_types = &.{.string},
            .return_type = str_result,
        } });
        try self.module_scope.define("read_file", .{ .type_id = read_file_type, .is_mut = false });

        // write_file(String, String) -> Bool!
        const bool_result = try self.type_table.addType(.{ .result = .{
            .ok_type = .bool,
            .err_type = .err,
        } });
        const write_file_type = try self.type_table.addType(.{ .function = .{
            .param_types = &.{ .string, .string },
            .return_type = bool_result,
        } });
        try self.module_scope.define("write_file", .{ .type_id = write_file_type, .is_mut = false });

        // args() -> List[String]
        const list_string = self.type_table.lookup("List[String]") orelse return error.OutOfMemory;
        const args_type = try self.type_table.addType(.{ .function = .{
            .param_types = &.{},
            .return_type = list_string,
        } });
        try self.module_scope.define("args", .{ .type_id = args_type, .is_mut = false });

        // env(String) -> String?
        const opt_string = try self.type_table.addType(.{ .optional = .{ .inner = .string } });
        const env_type = try self.type_table.addType(.{ .function = .{
            .param_types = &.{.string},
            .return_type = opt_string,
        } });
        try self.module_scope.define("env", .{ .type_id = env_type, .is_mut = false });

        // chr(Int) -> String
        const chr_type = try self.type_table.addType(.{ .function = .{
            .param_types = &.{.int},
            .return_type = .string,
        } });
        try self.module_scope.define("chr", .{ .type_id = chr_type, .is_mut = false });

        // exit(Int) -> Void
        const exit_type = try self.type_table.addType(.{ .function = .{
            .param_types = &.{.int},
            .return_type = .void,
        } });
        try self.module_scope.define("exit", .{ .type_id = exit_type, .is_mut = false });

        // assert(Bool) -> Void
        const assert_type = try self.type_table.addType(.{ .function = .{
            .param_types = &.{.bool},
            .return_type = .void,
        } });
        try self.module_scope.define("assert", .{ .type_id = assert_type, .is_mut = false });

        // assert_eq and assert_ne are special-cased in checkCall to accept
        // any two args of the same type. registered here as (Int, Int) -> Void
        // as a placeholder — the actual type checking happens in checkFnCall.
        const assert_eq_type = try self.type_table.addType(.{ .function = .{
            .param_types = &.{ .int, .int },
            .return_type = .void,
        } });
        try self.module_scope.define("assert_eq", .{ .type_id = assert_eq_type, .is_mut = false });
        try self.module_scope.define("assert_ne", .{ .type_id = assert_eq_type, .is_mut = false });

        // exec(String) -> Int
        const exec_type = try self.type_table.addType(.{ .function = .{
            .param_types = &.{.string},
            .return_type = .int,
        } });
        try self.module_scope.define("exec", .{ .type_id = exec_type, .is_mut = false });

        // time() -> Int
        const time_type = try self.type_table.addType(.{ .function = .{
            .param_types = &.{},
            .return_type = .int,
        } });
        try self.module_scope.define("time", .{ .type_id = time_type, .is_mut = false });

        // sleep(Int) -> Void
        const sleep_type = try self.type_table.addType(.{ .function = .{
            .param_types = &.{.int},
            .return_type = .void,
        } });
        try self.module_scope.define("sleep", .{ .type_id = sleep_type, .is_mut = false });

        // random_int(Int, Int) -> Int
        const random_int_type = try self.type_table.addType(.{ .function = .{
            .param_types = &.{ .int, .int },
            .return_type = .int,
        } });
        try self.module_scope.define("random_int", .{ .type_id = random_int_type, .is_mut = false });

        // random_float() -> Float
        const random_float_type = try self.type_table.addType(.{ .function = .{
            .param_types = &.{},
            .return_type = .float,
        } });
        try self.module_scope.define("random_float", .{ .type_id = random_float_type, .is_mut = false });

        // exec_output(String) -> String!
        const exec_output_type = try self.type_table.addType(.{ .function = .{
            .param_types = &.{.string},
            .return_type = str_result,
        } });
        try self.module_scope.define("exec_output", .{ .type_id = exec_output_type, .is_mut = false });

        // input() -> String
        const input_type = try self.type_table.addType(.{ .function = .{
            .param_types = &.{},
            .return_type = .string,
        } });
        try self.module_scope.define("input", .{ .type_id = input_type, .is_mut = false });

        // path_join(String, String) -> String
        const path_join_type = try self.type_table.addType(.{ .function = .{
            .param_types = &.{ .string, .string },
            .return_type = .string,
        } });
        try self.module_scope.define("path_join", .{ .type_id = path_join_type, .is_mut = false });

        // path_dir, path_base, path_ext, path_stem: (String) -> String
        const path_str_type = try self.type_table.addType(.{ .function = .{
            .param_types = &.{.string},
            .return_type = .string,
        } });
        try self.module_scope.define("path_dir", .{ .type_id = path_str_type, .is_mut = false });
        try self.module_scope.define("path_base", .{ .type_id = path_str_type, .is_mut = false });
        try self.module_scope.define("path_ext", .{ .type_id = path_str_type, .is_mut = false });
        try self.module_scope.define("path_stem", .{ .type_id = path_str_type, .is_mut = false });

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

    /// check a parsed module. three passes:
    ///   0. resolve imports (load, parse, check imported files)
    ///   1. register all top-level declarations (names + signatures)
    ///   2. check all function bodies and top-level bindings
    pub fn check(self: *Checker, module: *const ast.Module) void {
        // pass 0: resolve imports
        self.resolveImports(module);

        // pass 1: register declarations
        for (module.decls) |*decl| {
            self.registerDecl(decl);
        }

        // pass 2: check bodies
        for (module.decls) |*decl| {
            self.checkDecl(decl);
        }
    }

    /// resolve imports: read, lex, parse, and type-check imported modules.
    /// injects public names into the current module's scope.
    fn resolveImports(self: *Checker, module: *const ast.Module) void {
        if (module.imports.len == 0) return;

        const base_path = self.source_path orelse {
            // no source path — can't resolve relative imports
            for (module.imports) |imp| {
                self.diagnostics.addCodedError(.E234, imp.location,
                    "cannot resolve imports without a source file path",
                ) catch {};
            }
            return;
        };

        // set up cycle detection
        var owned_checking: std.StringHashMap(void) = undefined;
        const need_cleanup = self.checking_files == null;
        if (need_cleanup) {
            owned_checking = std.StringHashMap(void).init(self.allocator);
            self.checking_files = &owned_checking;
            // mark the current file as being checked
            owned_checking.put(base_path, {}) catch return;
        }
        defer if (need_cleanup) {
            owned_checking.deinit();
            self.checking_files = null;
        };

        const dir = std.fs.path.dirname(base_path) orelse ".";

        for (module.imports) |imp| {
            switch (imp.kind) {
                .simple => |simple| {
                    self.resolveSimpleImport(simple.path, simple.alias, imp.location, dir);
                },
                .from => |from| {
                    self.resolveFromImport(from.path, from.names, imp.location, dir);
                },
            }
        }
    }

    /// resolve `import foo` or `import foo.bar as baz`
    fn resolveSimpleImport(
        self: *Checker,
        path_parts: []const []const u8,
        alias: ?[]const u8,
        location: Location,
        dir: []const u8,
    ) void {
        const import_path = self.resolveImportPath(path_parts, dir) orelse {
            const path_str = self.joinPath(path_parts);
            self.diagnostics.addCodedError(.E234, location, self.fmt(
                "module not found: '{s}'", .{path_str},
            )) catch {};
            return;
        };
        defer self.allocator.free(import_path);

        const imported = self.loadAndCheckModule(import_path, location) orelse return;

        // determine the name to use in the current scope
        const mod_name = alias orelse path_parts[path_parts.len - 1];

        // create a namespace scope with all public declarations
        self.injectNamespaceImport(imported, mod_name, location);
    }

    /// resolve `from foo import bar, baz`
    fn resolveFromImport(
        self: *Checker,
        path_parts: []const []const u8,
        names: []const ast.ImportName,
        location: Location,
        dir: []const u8,
    ) void {
        const import_path = self.resolveImportPath(path_parts, dir) orelse {
            const path_str = self.joinPath(path_parts);
            self.diagnostics.addCodedError(.E234, location, self.fmt(
                "module not found: '{s}'", .{path_str},
            )) catch {};
            return;
        };
        defer self.allocator.free(import_path);

        const imported = self.loadAndCheckModule(import_path, location) orelse return;

        // build decl index once, then look up each imported name in O(1)
        var decl_index = buildDeclIndex(imported, self.allocator);
        defer decl_index.deinit();

        for (names) |name| {
            const use_name = name.alias orelse name.name;
            if (self.findPublicDeclIndexed(&decl_index, name.name)) |binding| {
                self.module_scope.define(use_name, binding) catch {};
            } else {
                // check if it exists but is not public
                if (decl_index.contains(name.name)) {
                    self.diagnostics.addCodedError(.E237, name.location, self.fmt(
                        "'{s}' is not public in the imported module", .{name.name},
                    )) catch {};
                } else {
                    self.diagnostics.addCodedError(.E236, name.location, self.fmt(
                        "name '{s}' not found in the imported module", .{name.name},
                    )) catch {};
                }
            }
        }
    }

    /// build a file path from import path parts relative to a directory.
    /// returns null if the file doesn't exist.
    fn resolveImportPath(self: *Checker, path_parts: []const []const u8, dir: []const u8) ?[]const u8 {
        // build "dir/part1/part2/.../partN.fg"
        var parts: std.ArrayList([]const u8) = .empty;
        defer parts.deinit(self.allocator);
        parts.append(self.allocator, dir) catch return null;
        for (path_parts) |p| {
            parts.append(self.allocator, p) catch return null;
        }

        const joined = std.fs.path.join(self.allocator, parts.items) catch return null;
        defer self.allocator.free(joined);

        const full = std.fmt.allocPrint(self.allocator, "{s}.fg", .{joined}) catch return null;

        // check if the file exists
        std.fs.cwd().access(full, .{}) catch {
            self.allocator.free(full);
            return null;
        };

        return full;
    }

    /// join path parts with dots for error messages
    fn joinPath(self: *Checker, parts: []const []const u8) []const u8 {
        if (parts.len == 1) return parts[0];
        var result: std.ArrayList(u8) = .empty;
        for (parts, 0..) |p, i| {
            if (i > 0) result.append(self.allocator, '.') catch {};
            result.appendSlice(self.allocator, p) catch {};
        }
        return result.toOwnedSlice(self.allocator) catch return parts[parts.len - 1];
    }

    /// load, lex, parse, and type-check an imported module file.
    /// returns the checked module, or null on error.
    fn loadAndCheckModule(self: *Checker, path: []const u8, location: Location) ?*const ast.Module {
        // check if already imported (dedup) — must come before cycle detection
        // to handle diamond imports (A imports B and C, both import D)
        for (self.imported_modules.items) |*im| {
            if (std.mem.eql(u8, im.path, path)) {
                return &im.module;
            }
        }

        // cycle detection — only triggers for genuine cycles (A → B → A)
        if (self.checking_files) |cf| {
            if (cf.get(path) != null) {
                self.diagnostics.addCodedError(.E235, location, self.fmt(
                    "import cycle detected: '{s}' is already being checked", .{path},
                )) catch {};
                return null;
            }
            // store path in arena so it survives after the caller frees import_path
            const stable_path = self.arena.allocator().dupe(u8, path) catch return null;
            cf.put(stable_path, {}) catch {};
        }

        // read the file
        const source = std.fs.cwd().readFileAlloc(self.allocator, path, 10 * 1024 * 1024) catch {
            self.diagnostics.addCodedError(.E234, location, self.fmt(
                "could not read '{s}'", .{path},
            )) catch {};
            return null;
        };
        // store source in arena so it lives as long as checker
        const arena_source = self.arena.allocator().dupe(u8, source) catch {
            self.allocator.free(source);
            return null;
        };
        self.allocator.free(source);

        // lex
        var lexer = Lexer.init(arena_source, self.allocator) catch return null;
        defer lexer.deinit();
        const tokens = lexer.tokenize() catch return null;
        defer self.allocator.free(tokens);

        if (lexer.diagnostics.hasErrors()) {
            // propagate lexer errors
            for (lexer.diagnostics.diagnostics.items) |diag| {
                self.diagnostics.diagnostics.append(self.allocator, diag) catch {};
            }
            return null;
        }

        // parse
        var parser = Parser.init(tokens, arena_source, self.arena.allocator());
        defer parser.deinit();
        const module = parser.parseModule() catch return null;

        if (parser.diagnostics.hasErrors()) {
            for (parser.diagnostics.diagnostics.items) |diag| {
                self.diagnostics.diagnostics.append(self.allocator, diag) catch {};
            }
            return null;
        }

        // store path in arena for dedup
        const arena_path = self.arena.allocator().dupe(u8, path) catch return null;

        // add to imported_modules before checking (allows dedup for diamond imports)
        self.imported_modules.append(self.allocator, .{
            .module = module,
            .path = arena_path,
        }) catch return null;

        const idx = self.imported_modules.items.len - 1;

        // type-check the imported module (reuses this checker's type table + scope)
        // save and restore source_path
        const prev_path = self.source_path;
        self.source_path = path;
        defer self.source_path = prev_path;

        // resolve any imports in the imported module (recursive)
        self.resolveImports(&module);

        // register declarations from imported module (pass 1)
        for (module.decls) |*decl| {
            self.registerDecl(decl);
        }
        // check bodies (pass 2)
        for (module.decls) |*decl| {
            self.checkDecl(decl);
        }

        return &self.imported_modules.items[idx].module;
    }

    /// metadata about a declaration: whether it's public.
    const DeclMeta = struct { is_pub: bool };

    /// build a name -> DeclMeta index for a module's declarations.
    /// used to avoid repeated linear scans when resolving multiple imports.
    fn buildDeclIndex(module: *const ast.Module, allocator: std.mem.Allocator) std.StringHashMap(DeclMeta) {
        var index = std.StringHashMap(DeclMeta).init(allocator);
        for (module.decls) |*decl| {
            const name = getDeclName(decl) orelse continue;
            index.put(name, .{ .is_pub = decl.is_pub }) catch {};
        }
        return index;
    }

    /// find a public declaration's binding in an imported module.
    fn findPublicDecl(self: *Checker, module: *const ast.Module, name: []const u8) ?Binding {
        var index = buildDeclIndex(module, self.allocator);
        defer index.deinit();
        return self.findPublicDeclIndexed(&index, name);
    }

    /// find a public declaration using a pre-built decl index.
    fn findPublicDeclIndexed(self: *Checker, index: *const std.StringHashMap(DeclMeta), name: []const u8) ?Binding {
        const meta = index.get(name) orelse return null;
        if (!meta.is_pub) return null;

        // functions and bindings are in module_scope
        if (self.module_scope.lookup(name)) |binding| return binding;

        // structs and enums are registered in type_table, not module_scope.
        if (self.type_table.lookup(name)) |type_id| {
            return Binding{ .type_id = type_id, .is_mut = false };
        }
        return null;
    }

    /// check if any declaration (pub or not) has this name.
    fn findAnyDecl(_: *Checker, module: *const ast.Module, name: []const u8) bool {
        // small linear scan is fine here — only called on error paths
        for (module.decls) |*decl| {
            const decl_name = getDeclName(decl) orelse continue;
            if (std.mem.eql(u8, decl_name, name)) return true;
        }
        return false;
    }

    /// extract the name from a declaration, if it has one.
    fn getDeclName(decl: *const ast.Decl) ?[]const u8 {
        return switch (decl.kind) {
            .fn_decl => |fd| fd.name,
            .struct_decl => |sd| sd.name,
            .enum_decl => |ed| ed.name,
            .binding => |b| b.name,
            .interface_decl => |id| id.name,
            else => null,
        };
    }

    /// inject all public names from a module as accessible via `mod_name.symbol`
    fn injectNamespaceImport(self: *Checker, module: *const ast.Module, mod_name: []const u8, location: Location) void {
        _ = location;
        for (module.decls) |*decl| {
            if (!decl.is_pub) continue;
            const decl_name = getDeclName(decl) orelse continue;
            const qualified = self.fmt("{s}.{s}", .{ mod_name, decl_name });
            if (self.module_scope.lookup(decl_name)) |binding| {
                self.module_scope.define(qualified, binding) catch {};
            }
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
            .interface_decl => |iface| self.registerInterfaceDecl(iface, decl.location),
            .impl_decl => |impl_d| self.registerImplDecl(impl_d, decl.location),
            .type_alias => |ta| self.registerTypeAlias(ta, decl.location),
            .binding => {}, // top-level bindings are checked in pass 2
            .test_decl => {}, // tests don't register names
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

        const fn_type = self.resolveFnSignature(fn_d) orelse return;
        self.module_scope.define(fn_d.name, .{ .type_id = fn_type, .is_mut = false }) catch return;
    }

    /// resolve a function's parameter and return types, returning the
    /// function's TypeId. returns null if registration fails (OOM or
    /// unresolvable types — diagnostics are emitted for the latter).
    fn resolveFnSignature(self: *Checker, fn_d: ast.FnDecl) ?TypeId {
        var param_ids = std.ArrayList(TypeId).initCapacity(self.allocator, fn_d.params.len) catch return null;
        defer param_ids.deinit(self.allocator);

        for (fn_d.params) |param| {
            if (param.type_expr) |te| {
                const id = self.resolveTypeExpr(te);
                param_ids.append(self.allocator, id) catch return null;
            } else {
                self.diagnostics.addCodedError(.E230, param.location, self.fmt(
                    "parameter '{s}' needs a type annotation",
                    .{param.name},
                )) catch {};
                param_ids.append(self.allocator, .err) catch return null;
            }
        }

        const return_type = if (fn_d.return_type) |rt| self.resolveTypeExpr(rt) else TypeId.void;

        const owned_params = self.arena.allocator().dupe(TypeId, param_ids.items) catch return null;
        return self.type_table.addType(.{ .function = .{
            .param_types = owned_params,
            .return_type = return_type,
        } }) catch null;
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
            self.diagnostics.addCodedError(.E233, location, "generic type aliases are not yet supported") catch {};
            return;
        }

        const target = self.resolveTypeExpr(ta.type_expr);
        if (target.isErr()) return;

        // transparent alias — the name maps to the same TypeId as the target
        self.type_table.register(ta.name, target) catch return;
    }

    fn registerInterfaceDecl(self: *Checker, iface: ast.InterfaceDecl, location: Location) void {
        if (iface.generic_params.len > 0) {
            self.diagnostics.addCodedError(.E233, location, "generic interfaces are not yet supported") catch {};
            return;
        }

        // register the interface name as a zero-field struct in the type table
        // so that resolveNamedType("Display") works when it appears in a bound.
        // interface_decls distinguishes it from a real struct.
        const type_id = self.type_table.addType(.{ .@"struct" = .{
            .name = iface.name,
            .fields = &.{},
        } }) catch return;

        self.type_table.register(iface.name, type_id) catch return;
        self.interface_decls.put(iface.name, iface) catch return;
    }

    fn registerImplDecl(self: *Checker, impl_d: ast.ImplDecl, location: Location) void {
        // extract the concrete type name. parser field naming is inverted:
        //   impl Display for Point:  →  target=Display, interface=Point
        //   impl Point:              →  target=Point,   interface=null
        const concrete_name: []const u8 = if (impl_d.interface) |iface_type_expr| blk: {
            // `impl X for Y:` — interface form
            const iface_name = switch (impl_d.target.kind) {
                .named => |n| n,
                else => {
                    self.diagnostics.addCodedError(.E202, location, "expected an interface name") catch {};
                    return;
                },
            };
            const type_name = switch (iface_type_expr.kind) {
                .named => |n| n,
                else => {
                    self.diagnostics.addCodedError(.E202, location, "expected a type name") catch {};
                    return;
                },
            };

            // verify the interface exists
            if (!self.interface_decls.contains(iface_name)) {
                self.diagnostics.addCodedError(.E202, location, self.fmt(
                    "unknown interface '{s}'",
                    .{iface_name},
                )) catch {};
                return;
            }

            // verify the concrete type exists
            if (self.type_table.lookup(type_name) == null) {
                self.diagnostics.addCodedError(.E202, location, self.fmt(
                    "unknown type '{s}'",
                    .{type_name},
                )) catch {};
                return;
            }

            // record the interface relationship
            const key = self.buildImplKey(type_name, iface_name);
            self.impl_set.put(key, {}) catch return;

            break :blk type_name;
        } else blk: {
            // `impl X:` — plain form
            const type_name = switch (impl_d.target.kind) {
                .named => |n| n,
                else => {
                    self.diagnostics.addCodedError(.E202, location, "expected a type name") catch {};
                    return;
                },
            };

            if (self.type_table.lookup(type_name) == null) {
                self.diagnostics.addCodedError(.E202, location, self.fmt(
                    "unknown type '{s}'",
                    .{type_name},
                )) catch {};
                return;
            }

            break :blk type_name;
        };

        // register methods from the impl block
        for (impl_d.methods) |method| {
            self.registerMethod(concrete_name, method);
        }
    }

    /// register a single method from an impl block. resolves param types
    /// and return type, creates a function type, and stores a MethodEntry.
    fn registerMethod(self: *Checker, type_name: []const u8, method: ast.ImplMethod) void {
        const fn_d = method.decl;
        const fn_type = self.resolveFnSignature(fn_d) orelse return;

        const key = self.buildMethodKey(type_name, fn_d.name);
        self.method_types.put(key, .{
            .type_id = fn_type,
            .is_pub = method.is_pub,
            .decl = fn_d,
        }) catch return;
    }

    /// build a null-separated key for the impl_set: "TypeName\x00InterfaceName".
    /// uses the arena via fmt — on OOM returns a string that can't match any
    /// valid key (valid keys always contain a null byte).
    fn buildImplKey(self: *Checker, type_name: []const u8, iface_name: []const u8) []const u8 {
        return self.fmt("{s}\x00{s}", .{ type_name, iface_name });
    }

    /// build a dot-separated key for method_types: "TypeName.methodName".
    fn buildMethodKey(self: *Checker, type_name: []const u8, method_name: []const u8) []const u8 {
        return self.fmt("{s}.{s}", .{ type_name, method_name });
    }

    /// check whether a type implements a given interface.
    fn typeImplements(self: *Checker, type_name: []const u8, iface_name: []const u8) bool {
        const key = self.buildImplKey(type_name, iface_name);
        return self.impl_set.contains(key);
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
            .impl_decl => |impl_d| self.checkImplDecl(impl_d),
            .type_alias => {},
            .test_decl => |td| self.checkTestDecl(td),
        }
    }

    fn checkTestDecl(self: *Checker, td: ast.TestDecl) void {
        var test_scope = Scope.init(self.allocator, &self.module_scope);
        defer test_scope.deinit();
        // tests have no return type — they just run statements
        test_scope.return_type = .void;
        self.checkBlock(td.body, &test_scope);
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

    /// check method bodies in an impl block. extracts the concrete type name,
    /// then delegates each method to checkMethodBody.
    fn checkImplDecl(self: *Checker, impl_d: ast.ImplDecl) void {
        // extract the concrete type name (same inversion as registerImplDecl)
        const concrete_name: []const u8 = if (impl_d.interface) |iface_type_expr|
            switch (iface_type_expr.kind) {
                .named => |n| n,
                else => return,
            }
        else switch (impl_d.target.kind) {
            .named => |n| n,
            else => return,
        };

        for (impl_d.methods) |method| {
            self.checkMethodBody(concrete_name, method.decl);
        }
    }

    /// check a single method body. mirrors checkFnDecl: looks up the
    /// MethodEntry, creates a scope with params, checks the block.
    fn checkMethodBody(self: *Checker, type_name: []const u8, fn_d: ast.FnDecl) void {
        const key = self.buildMethodKey(type_name, fn_d.name);
        const entry = self.method_types.get(key) orelse return;

        const fn_type = self.type_table.get(entry.type_id) orelse return;
        const func = switch (fn_type) {
            .function => |f| f,
            else => return,
        };

        // create a scope for the method body
        var method_scope = Scope.init(self.allocator, &self.module_scope);
        defer method_scope.deinit();
        method_scope.return_type = func.return_type;

        // bind `self` to the impl target type so method bodies can
        // access fields and call other methods on the receiver.
        if (self.type_table.lookup(type_name)) |self_type| {
            method_scope.define("self", .{
                .type_id = self_type,
                .is_mut = false,
            }) catch return;
        }

        // define parameters
        for (fn_d.params, func.param_types) |param, param_type| {
            method_scope.define(param.name, .{
                .type_id = param_type,
                .is_mut = param.is_mut,
            }) catch return;
        }

        self.checkBlock(fn_d.body, &method_scope);
    }

    fn checkTopLevelBinding(self: *Checker, b: ast.Binding) void {
        self.checkBindingStmt(b, &self.module_scope);
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
            .match_stmt => |m| self.checkMatchStmt(m, stmt.location, scope),
            .break_stmt => {
                if (!scope.in_loop) {
                    self.diagnostics.addCodedError(.E214, stmt.location, "break outside of loop") catch {};
                }
            },
            .continue_stmt => {
                if (!scope.in_loop) {
                    self.diagnostics.addCodedError(.E214, stmt.location, "continue outside of loop") catch {};
                }
            },
        }
    }

    fn checkReturnStmt(self: *Checker, ret: ast.ReturnStmt, location: Location, scope: *const Scope) void {
        const expected = scope.return_type orelse {
            self.diagnostics.addCodedErrorWithFix(.E231, location, "return statement outside of function", "'return' can only be used inside a function body") catch {};
            return;
        };

        if (ret.value) |value| {
            const actual = self.checkExpr(value, scope);
            if (!actual.isErr() and !expected.isErr() and actual != expected) {
                // allow returning the inner type from a result- or optional-returning function,
                // and structurally equivalent tuples (may have different TypeIds)
                const ok_match = if (self.type_table.get(expected)) |ety| switch (ety) {
                    .result => |r| actual == r.ok_type,
                    .optional => |o| actual == o.inner,
                    .tuple => |exp_tup| blk: {
                        const act_ty = self.type_table.get(actual) orelse break :blk false;
                        const act_tup = switch (act_ty) {
                            .tuple => |t| t,
                            else => break :blk false,
                        };
                        if (exp_tup.elements.len != act_tup.elements.len) break :blk false;
                        for (exp_tup.elements, act_tup.elements) |a, b| {
                            if (a != b) break :blk false;
                        }
                        break :blk true;
                    },
                    else => false,
                } else false;

                if (!ok_match) {
                    self.diagnostics.addCodedErrorWithFix(.E200, value.location, self.fmt(
                        "return type mismatch: expected {s}, got {s}",
                        .{ self.type_table.typeName(expected), self.type_table.typeName(actual) },
                    ), self.fmt(
                        "change the return type to {s}",
                        .{self.type_table.typeName(actual)},
                    )) catch {};
                }
            }
        } else {
            // bare return — expected type should be Void
            if (expected != .void and !expected.isErr()) {
                self.diagnostics.addCodedError(.E200, location, self.fmt(
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
                self.diagnostics.addCodedError(.E200, te.location, self.fmt(
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
                    self.diagnostics.addCodedErrorWithFix(.E216, a.target.location, self.fmt(
                        "cannot assign to immutable variable '{s}'",
                        .{name},
                    ), self.fmt(
                        "declare with 'mut' to make it mutable: mut {s} := ...",
                        .{name},
                    )) catch {};
                    return;
                }
            }
        }

        // for compound assignments (+=, -=, etc.) both sides must be numeric
        if (a.op != .assign) {
            if (!target_type.isNumeric()) {
                self.diagnostics.addCodedError(.E217, a.target.location, self.fmt(
                    "expected numeric type for compound assignment, got {s}",
                    .{self.type_table.typeName(target_type)},
                )) catch {};
                return;
            }
        }

        if (target_type != value_type) {
            self.diagnostics.addCodedError(.E200, a.value.*.location, self.fmt(
                "type mismatch: expected {s}, got {s}",
                .{ self.type_table.typeName(target_type), self.type_table.typeName(value_type) },
            )) catch {};
        }
    }

    /// emit an error if the type isn't Bool. used for if/while/elif conditions.
    fn expectBool(self: *Checker, location: Location, actual: TypeId) void {
        if (!actual.isErr() and actual != .bool) {
            self.diagnostics.addCodedError(.E200, location, self.fmt(
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
        // check the iterable and infer the element type from its collection type
        const iter_type = self.checkExpr(f.iterable, scope);
        var binding_type: TypeId = .err;
        if (!iter_type.isErr()) {
            if (self.type_table.get(iter_type)) |ty| {
                switch (ty) {
                    .list => |l| binding_type = l.element,
                    .map => |m| binding_type = m.key,
                    .set => |s| binding_type = s.element,
                    else => {
                        self.diagnostics.addCodedError(.E200, f.iterable.location,
                            "for loop requires an iterable collection (List, Map, or Set)") catch {};
                    },
                }
            }
        }

        var body_scope = Scope.init(self.allocator, scope);
        defer body_scope.deinit();
        body_scope.in_loop = true;
        body_scope.define(f.binding, .{ .type_id = binding_type, .is_mut = false }) catch return;

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
        return self.resolveTypeImpl(type_expr, null);
    }

    /// resolve a type expression with a substitution map for type parameters.
    /// used during generic instantiation to replace type parameter names
    /// (e.g. T, K, V) with their concrete types.
    fn resolveTypeExprWithSubst(
        self: *Checker,
        type_expr: *const ast.TypeExpr,
        subst: *const std.StringHashMap(TypeId),
    ) TypeId {
        return self.resolveTypeImpl(type_expr, subst);
    }

    /// shared implementation for type expression resolution. when subst is
    /// non-null, named types are checked against the substitution map first
    /// (used during generic instantiation).
    fn resolveTypeImpl(
        self: *Checker,
        type_expr: *const ast.TypeExpr,
        subst: ?*const std.StringHashMap(TypeId),
    ) TypeId {
        if (self.resolve_depth >= max_resolve_depth) {
            self.diagnostics.addCodedError(.E233, type_expr.location, "type nesting exceeds maximum depth") catch {};
            return .err;
        }
        self.resolve_depth += 1;
        defer self.resolve_depth -= 1;

        return switch (type_expr.kind) {
            .named => |name| {
                if (subst) |s| {
                    if (s.get(name)) |id| return id;
                }
                return self.resolveNamedType(name, type_expr.location);
            },
            .optional => |inner| {
                const inner_id = self.resolveTypeImpl(inner, subst);
                if (inner_id.isErr()) return .err;
                return self.type_table.addType(.{ .optional = .{ .inner = inner_id } }) catch return .err;
            },
            .result => |r| {
                const ok_id = self.resolveTypeImpl(r.ok_type, subst);
                if (ok_id.isErr()) return .err;
                const err_id = if (r.err_type) |err_type|
                    self.resolveTypeImpl(err_type, subst)
                else
                    TypeId.err; // default error type — will be refined later
                return self.type_table.addType(.{ .result = .{
                    .ok_type = ok_id,
                    .err_type = err_id,
                } }) catch return .err;
            },
            .tuple => |elems| {
                var ids = std.ArrayList(TypeId).initCapacity(self.allocator, elems.len) catch return .err;
                defer ids.deinit(self.allocator);
                for (elems) |elem| {
                    const id = self.resolveTypeImpl(elem, subst);
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
                    const id = self.resolveTypeImpl(param, subst);
                    if (id.isErr()) return .err;
                    param_ids.append(self.allocator, id) catch return .err;
                }
                const ret_id = if (f.return_type) |rt| self.resolveTypeImpl(rt, subst) else TypeId.void;
                if (ret_id.isErr()) return .err;
                const owned_params = self.arena.allocator().dupe(TypeId, param_ids.items) catch return .err;
                return self.type_table.addType(.{ .function = .{
                    .param_types = owned_params,
                    .return_type = ret_id,
                } }) catch return .err;
            },
            .generic => |g| {
                var arg_ids = std.ArrayList(TypeId).initCapacity(self.allocator, g.args.len) catch return .err;
                defer arg_ids.deinit(self.allocator);
                for (g.args) |arg| {
                    const id = self.resolveTypeImpl(arg, subst);
                    if (id.isErr()) return .err;
                    arg_ids.append(self.allocator, id) catch return .err;
                }
                return self.resolveGenericTypeWithArgs(g.name, arg_ids.items, type_expr.location);
            },
        };
    }

    fn resolveNamedType(self: *Checker, name: []const u8, location: Location) TypeId {
        if (self.type_table.lookup(name)) |id| return id;

        self.diagnostics.addCodedError(.E202, location, self.fmt("unknown type '{s}'", .{name})) catch {};
        return .err;
    }

    /// resolve a generic type by name with already-resolved type arguments.
    /// handles builtin generics (Task, Channel) and user-defined generics.
    fn resolveGenericTypeWithArgs(self: *Checker, name: []const u8, arg_ids: []const TypeId, location: Location) TypeId {
        // builtin generic types — deduplicated via name_map so that e.g.
        // two List[Int] occurrences share the same TypeId.
        if (arg_ids.len == 1) {
            if (std.mem.eql(u8, name, "Task")) {
                return self.type_table.addType(.{ .task = .{ .inner = arg_ids[0] } }) catch return .err;
            }
            if (std.mem.eql(u8, name, "Channel")) {
                return self.type_table.addType(.{ .channel = .{ .inner = arg_ids[0] } }) catch return .err;
            }
            if (std.mem.eql(u8, name, "List")) {
                return self.internCollectionType(name, arg_ids, .{ .list = .{ .element = arg_ids[0] } });
            }
            if (std.mem.eql(u8, name, "Set")) {
                return self.internCollectionType(name, arg_ids, .{ .set = .{ .element = arg_ids[0] } });
            }
        }
        if (arg_ids.len == 2 and std.mem.eql(u8, name, "Map")) {
            return self.internCollectionType(name, arg_ids, .{ .map = .{
                .key = arg_ids[0],
                .value = arg_ids[1],
            } });
        }

        // look up user-defined generic
        const decl = self.generic_decls.get(name) orelse {
            self.diagnostics.addCodedError(.E222, location, self.fmt("unknown generic type '{s}'", .{name})) catch {};
            return .err;
        };

        return switch (decl) {
            .@"struct" => |s| self.instantiateGenericStruct(s, arg_ids, location),
            .@"enum" => |e| self.instantiateGenericEnum(e, arg_ids, location),
            .function => {
                self.diagnostics.addCodedError(.E200, location, self.fmt("'{s}' is a generic function, not a type", .{name})) catch {};
                return .err;
            },
        };
    }

    /// deduplicate a builtin collection type (List, Map, Set) by registering
    /// it under a canonical name like "List[Int]". returns the existing TypeId
    /// if already registered, otherwise creates and registers a new one.
    fn internCollectionType(self: *Checker, name: []const u8, arg_ids: []const TypeId, ty: types.Type) TypeId {
        const inst_name = self.buildInstName(name, arg_ids);
        if (self.type_table.lookup(inst_name)) |existing| return existing;

        const id = self.type_table.addType(ty) catch return .err;
        self.type_table.register(inst_name, id) catch {};
        return id;
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
            self.diagnostics.addCodedError(.E221, location, self.fmt(
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

        // verify type arguments satisfy interface bounds
        if (!self.checkBounds(s.generic_params, &subst, location)) return .err;

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
            self.diagnostics.addCodedError(.E221, location, self.fmt(
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

        // verify type arguments satisfy interface bounds
        if (!self.checkBounds(e.generic_params, &subst, location)) return .err;

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
                        const prev_name = if (subst.get(param_name)) |prev|
                            self.type_table.typeName(prev)
                        else
                            "unknown";
                        self.diagnostics.addCodedError(.E222, location, self.fmt(
                            "conflicting types for generic parameter '{s}': {s} vs {s}",
                            .{
                                param_name,
                                prev_name,
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
                self.diagnostics.addCodedError(.E222, location, self.fmt(
                    "could not infer type for generic parameter '{s}'",
                    .{gp.name},
                )) catch {};
                subst.deinit();
                return null;
            }
        }

        return subst;
    }

    /// instantiate a generic function with concrete type arguments.
    /// builds a concrete function type by resolving param types and return
    /// type with the substitution map. deduplicates via buildInstName.
    fn instantiateGenericFn(
        self: *Checker,
        fn_d: ast.FnDecl,
        subst: *const std.StringHashMap(TypeId),
        arg_ids: []const TypeId,
    ) TypeId {
        // build inst name and check dedup cache
        const inst_name = self.buildInstName(fn_d.name, arg_ids);
        if (self.type_table.lookup(inst_name)) |existing| return existing;

        // resolve param types with substitution
        var param_ids = std.ArrayList(TypeId).initCapacity(self.allocator, fn_d.params.len) catch return .err;
        defer param_ids.deinit(self.allocator);

        for (fn_d.params) |param| {
            if (param.type_expr) |te| {
                const id = self.resolveTypeExprWithSubst(te, subst);
                param_ids.append(self.allocator, id) catch return .err;
            } else {
                param_ids.append(self.allocator, .err) catch return .err;
            }
        }

        // resolve return type
        const return_type = if (fn_d.return_type) |rt|
            self.resolveTypeExprWithSubst(rt, subst)
        else
            TypeId.void;

        // create the concrete function type
        const owned_params = self.arena.allocator().dupe(TypeId, param_ids.items) catch return .err;
        const fn_type = self.type_table.addType(.{ .function = .{
            .param_types = owned_params,
            .return_type = return_type,
        } }) catch return .err;

        // register for deduplication
        self.type_table.register(inst_name, fn_type) catch return .err;
        return fn_type;
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

            .method_call => |mc| self.checkMethodCall(mc, expr.location, scope),

            // index returns .err because it requires generics that aren't
            // implemented yet. returning .err suppresses cascading
            // diagnostics downstream.
            .field_access => |fa| self.checkFieldAccess(fa, expr.location, scope),
            .index => |idx| self.checkIndexExpr(idx, expr.location, scope),
            .unwrap => |inner| self.checkUnwrapExpr(inner, expr.location, scope),
            .try_expr => |inner| self.checkTryExpr(inner, expr.location, scope),
            .spawn_expr => |inner| self.checkSpawnExpr(inner, expr.location, scope),
            .await_expr => |inner| self.checkAwaitExpr(inner, expr.location, scope),
            .match_expr => |m| self.checkMatchExpr(m, expr.location, scope),
            .lambda => |lam| self.checkLambda(lam, scope),
            .list => |elems| self.checkListExpr(elems, expr.location, scope),
            .map => |entries| self.checkMapExpr(entries, expr.location, scope),
            .set => |elems| self.checkSetExpr(elems, expr.location, scope),
            .tuple => |elems| self.checkTupleExpr(elems, expr.location, scope),
            .self_expr => self.checkSelfExpr(expr.location, scope),

            .err => .err,
        };
    }

    fn checkIdent(self: *Checker, name: []const u8, location: Location, scope: *const Scope) TypeId {
        if (scope.lookup(name)) |binding| return binding.type_id;

        // generic type names used as bare identifiers (e.g. in a call like
        // Pair(1, "hello") without type args) — suppress the diagnostic.
        // the real type comes from a binding annotation or generic use site.
        if (self.generic_decls.contains(name)) return .err;

        self.diagnostics.addCodedError(.E201, location, self.fmt("undefined variable '{s}'", .{name})) catch {};
        return .err;
    }

    /// check `self` — valid only inside a method body where `self` is bound.
    fn checkSelfExpr(self: *Checker, location: Location, scope: *const Scope) TypeId {
        if (scope.lookup("self")) |binding| return binding.type_id;

        self.diagnostics.addCodedErrorWithFix(.E229, location, "'self' can only be used inside a method body", "define methods inside an 'impl' block with 'self' as the first parameter") catch {};
        return .err;
    }

    fn checkBinary(self: *Checker, bin: ast.BinaryExpr, location: Location, scope: *const Scope) TypeId {
        // pipe needs special handling — the RHS is a function name, not a value
        if (bin.op == .pipe) return self.checkPipeExpr(bin, location, scope);

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

            .pipe => {
                // pipe is handled by the early return above. if we reach
                // here, the dispatch logic has a bug — return an error
                // instead of crashing.
                self.diagnostics.addCodedError(.E220, location, "internal: unexpected pipe in binary dispatch") catch {};
                return .err;
            },
        };
    }

    fn checkPipeExpr(self: *Checker, bin: ast.BinaryExpr, location: Location, scope: *const Scope) TypeId {
        const lhs_type = self.checkExpr(bin.left, scope);
        if (lhs_type.isErr()) return .err;

        // the RHS must be a bare function name (identifier)
        const rhs_name = switch (bin.right.kind) {
            .ident => |name| name,
            else => {
                self.diagnostics.addCodedError(
                    .E220,
                    location,
                    "pipe operator requires a function name on the right-hand side",
                ) catch {};
                return .err;
            },
        };

        // look up the function in scope
        const binding = scope.lookup(rhs_name) orelse {
            self.diagnostics.addCodedError(.E201, location, self.fmt(
                "undefined variable '{s}'",
                .{rhs_name},
            )) catch {};
            return .err;
        };

        // verify it's a function
        const ty = self.type_table.get(binding.type_id) orelse return .err;
        const func = switch (ty) {
            .function => |f| f,
            else => {
                self.diagnostics.addCodedError(.E208, location, self.fmt(
                    "'{s}' is not a function",
                    .{rhs_name},
                )) catch {};
                return .err;
            },
        };

        // verify it takes exactly 1 parameter
        if (func.param_types.len != 1) {
            self.diagnostics.addCodedError(.E220, location, self.fmt(
                "pipe requires a function that takes 1 argument, '{s}' takes {d}",
                .{ rhs_name, func.param_types.len },
            )) catch {};
            return .err;
        }

        // verify the LHS type matches the parameter type
        if (lhs_type != func.param_types[0]) {
            self.diagnostics.addCodedError(.E220, location, self.fmt(
                "type mismatch in pipe: expected {s}, got {s}",
                .{ self.type_table.typeName(func.param_types[0]), self.type_table.typeName(lhs_type) },
            )) catch {};
            return .err;
        }

        return func.return_type;
    }

    fn checkArithmetic(self: *Checker, left: TypeId, right: TypeId, bin: ast.BinaryExpr, location: Location) TypeId {
        // string + string → string (concatenation)
        if (bin.op == .add and left == .string and right == .string) return .string;

        return self.checkNumericBinary(left, right, location);
    }

    fn checkNumericBinary(self: *Checker, left: TypeId, right: TypeId, location: Location) TypeId {
        if (!left.isNumeric()) {
            self.diagnostics.addCodedError(.E217, location, self.fmt(
                "expected numeric type, got {s}",
                .{self.type_table.typeName(left)},
            )) catch {};
            return .err;
        }
        if (left != right) {
            self.diagnostics.addCodedError(.E217, location, self.fmt(
                "type mismatch: {s} and {s}",
                .{ self.type_table.typeName(left), self.type_table.typeName(right) },
            )) catch {};
            return .err;
        }
        return left;
    }

    fn checkEquality(self: *Checker, left: TypeId, right: TypeId, location: Location) TypeId {
        if (left != right) {
            self.diagnostics.addCodedError(.E217, location, self.fmt(
                "cannot compare {s} and {s}",
                .{ self.type_table.typeName(left), self.type_table.typeName(right) },
            )) catch {};
            return .err;
        }
        return .bool;
    }

    fn checkOrdering(self: *Checker, left: TypeId, right: TypeId, location: Location) TypeId {
        if (!left.isNumeric() and left != .string) {
            self.diagnostics.addCodedError(.E217, location, self.fmt(
                "type {s} does not support ordering",
                .{self.type_table.typeName(left)},
            )) catch {};
            return .err;
        }
        if (left != right) {
            self.diagnostics.addCodedError(.E217, location, self.fmt(
                "cannot compare {s} and {s}",
                .{ self.type_table.typeName(left), self.type_table.typeName(right) },
            )) catch {};
            return .err;
        }
        return .bool;
    }

    fn checkLogical(self: *Checker, left: TypeId, right: TypeId, location: Location) TypeId {
        if (left != .bool) {
            self.diagnostics.addCodedError(.E217, location, self.fmt(
                "expected Bool, got {s}",
                .{self.type_table.typeName(left)},
            )) catch {};
            return .err;
        }
        if (right != .bool) {
            self.diagnostics.addCodedError(.E217, location, self.fmt(
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
                    self.diagnostics.addCodedError(.E217, location, self.fmt(
                        "cannot negate {s}",
                        .{self.type_table.typeName(operand)},
                    )) catch {};
                    return .err;
                }
                return operand;
            },
            .not => {
                if (operand != .bool) {
                    self.diagnostics.addCodedError(.E217, location, self.fmt(
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
                self.diagnostics.addCodedError(.E232, location, "cannot spawn a Task") catch {};
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

        self.diagnostics.addCodedError(.E232, location, self.fmt(
            "expected Task, got {s}",
            .{self.type_table.typeName(inner_type)},
        )) catch {};
        return .err;
    }

    fn checkUnwrapExpr(self: *Checker, inner: *const ast.Expr, location: Location, scope: *const Scope) TypeId {
        const inner_type = self.checkExpr(inner, scope);
        if (inner_type.isErr()) return .err;

        // the operand must be an Optional[T]
        if (self.type_table.get(inner_type)) |ty| {
            if (ty == .optional) {
                return ty.optional.inner;
            }
        }

        self.diagnostics.addCodedErrorWithFix(.E224, location, self.fmt(
            "cannot unwrap non-optional type {s}",
            .{self.type_table.typeName(inner_type)},
        ), "'?' can only unwrap Optional types (T?)") catch {};
        return .err;
    }

    fn checkTryExpr(self: *Checker, inner: *const ast.Expr, location: Location, scope: *const Scope) TypeId {
        const inner_type = self.checkExpr(inner, scope);
        if (inner_type.isErr()) return .err;

        // the operand must be a Result[T, E]
        if (self.type_table.get(inner_type)) |ty| {
            if (ty == .result) {
                // the enclosing function must also return a result type
                if (scope.return_type) |ret| {
                    if (self.type_table.get(ret)) |ret_ty| {
                        if (ret_ty == .result) {
                            return ty.result.ok_type;
                        }
                    }
                    self.diagnostics.addCodedError(
                        .E224,
                        location,
                        "'!' can only be used in a function that returns a result type",
                    ) catch {};
                    return .err;
                }

                // no return type at all (top-level expression)
                self.diagnostics.addCodedError(
                    .E224,
                    location,
                    "'!' can only be used in a function that returns a result type",
                ) catch {};
                return .err;
            }
        }

        self.diagnostics.addCodedErrorWithFix(.E224, location, self.fmt(
            "cannot use '!' on non-result type {s}",
            .{self.type_table.typeName(inner_type)},
        ), "'!' can only propagate errors from Result types (T!)") catch {};
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
                self.diagnostics.addCodedError(.E225, branch.location, self.fmt(
                    "branch type mismatch: expected {s}, got {s}",
                    .{ self.type_table.typeName(then_type), self.type_table.typeName(elif_type) },
                )) catch {};
            }
        }

        const else_type = self.checkExpr(if_e.else_expr, scope);
        if (!then_type.isErr() and !else_type.isErr() and then_type != else_type) {
            self.diagnostics.addCodedError(.E225, if_e.else_expr.location, self.fmt(
                "branch type mismatch: expected {s}, got {s}",
                .{ self.type_table.typeName(then_type), self.type_table.typeName(else_type) },
            )) catch {};
        }

        return then_type;
    }

    // call dispatch logic:
    // 1. if the callee is a struct type name, route to struct constructor
    // 2. if the callee is a generic function name, route to generic fn call
    // 3. otherwise fall through to normal function call checking
    //
    // some struct types (Mutex, WaitGroup, Semaphore) are registered as
    // zero-field structs but also have constructor functions in scope —
    // when the arg count doesn't match the field count and a function
    // binding exists, we fall through to normal function call checking.
    fn checkCall(self: *Checker, call: ast.CallExpr, location: Location, scope: *const Scope) TypeId {
        // dispatch only applies to named callees (e.g. Point(...), identity(...))
        const name = switch (call.callee.kind) {
            .ident => |n| n,
            else => return self.checkFnCall(call, location, scope),
        };

        // assert_eq/assert_ne: accept any two args of the same type
        if (std.mem.eql(u8, name, "assert_eq") or std.mem.eql(u8, name, "assert_ne")) {
            if (call.args.len != 2) {
                self.diagnostics.addCodedError(.E207, location, self.fmt(
                    "'{s}' expects 2 argument(s), got {d}",
                    .{ name, call.args.len },
                )) catch {};
                return .err;
            }
            const lhs = self.checkExpr(call.args[0].value, scope);
            const rhs = self.checkExpr(call.args[1].value, scope);
            if (!lhs.isErr() and !rhs.isErr() and lhs != rhs) {
                self.diagnostics.addCodedError(.E219, location, self.fmt(
                    "both arguments to '{s}' must be the same type, got {s} and {s}",
                    .{ name, self.type_table.typeName(lhs), self.type_table.typeName(rhs) },
                )) catch {};
            }
            return .void;
        }

        // struct constructor: Name(field1, field2, ...)
        if (self.type_table.lookup(name)) |type_id| {
            if (self.type_table.get(type_id)) |ty| {
                if (ty == .@"struct") {
                    // some struct types (Mutex, etc.) also have constructor functions
                    // in scope — when arg count doesn't match fields and a function
                    // binding exists, fall through to normal call checking
                    const fields_match = ty.@"struct".fields.len == call.args.len;
                    if (fields_match or scope.lookup(name) == null) {
                        return self.checkStructConstructor(type_id, call, location, scope);
                    }
                }
            }
        }

        // generic function call: infer type args from arguments
        if (self.generic_decls.get(name)) |decl| {
            if (decl == .function) {
                return self.checkGenericFnCall(decl.function, call, location, scope);
            }
        }

        return self.checkFnCall(call, location, scope);
    }

    /// verify that each generic parameter's inferred type satisfies its
    /// interface bounds. emits a diagnostic for each unsatisfied bound.
    /// returns false if any bound check fails.
    fn checkBounds(
        self: *Checker,
        generic_params: []const ast.GenericParam,
        subst: *const std.StringHashMap(TypeId),
        location: Location,
    ) bool {
        var ok = true;
        for (generic_params) |gp| {
            const inferred_id = subst.get(gp.name) orelse continue;
            const type_name = self.type_table.typeName(inferred_id);

            for (gp.bounds) |bound_te| {
                const iface_name = switch (bound_te.kind) {
                    .named => |n| n,
                    else => continue,
                };

                if (!self.interface_decls.contains(iface_name)) {
                    self.diagnostics.addCodedError(.E226, location, self.fmt(
                        "unknown interface '{s}' in bound for '{s}'",
                        .{ iface_name, gp.name },
                    )) catch {};
                    ok = false;
                    continue;
                }

                if (!self.typeImplements(type_name, iface_name)) {
                    self.diagnostics.addCodedError(.E226, location, self.fmt(
                        "type '{s}' does not implement interface '{s}'",
                        .{ type_name, iface_name },
                    )) catch {};
                    ok = false;
                }
            }
        }
        return ok;
    }

    /// check a call to a generic function. evaluates argument types,
    /// infers type arguments, instantiates the concrete function type,
    /// and validates the argument types against the concrete signature.
    fn checkGenericFnCall(
        self: *Checker,
        fn_d: ast.FnDecl,
        call: ast.CallExpr,
        location: Location,
        scope: *const Scope,
    ) TypeId {
        // check argument count
        if (call.args.len != fn_d.params.len) {
            self.diagnostics.addCodedError(.E207, location, self.fmt(
                "'{s}' expects {d} argument(s), got {d}",
                .{ fn_d.name, fn_d.params.len, call.args.len },
            )) catch {};
            return .err;
        }

        // evaluate all arg types upfront
        var arg_types = std.ArrayList(TypeId).initCapacity(self.allocator, call.args.len) catch return .err;
        defer arg_types.deinit(self.allocator);

        for (call.args) |arg| {
            const t = self.checkExpr(arg.value, scope);
            arg_types.append(self.allocator, t) catch return .err;
        }

        // bail if any arg had an error
        for (arg_types.items) |t| {
            if (t.isErr()) return .err;
        }

        // infer type arguments from the arg types
        var subst = self.inferTypeArgs(fn_d, arg_types.items, location) orelse return .err;
        defer subst.deinit();

        // verify inferred types satisfy interface bounds
        if (!self.checkBounds(fn_d.generic_params, &subst, location)) return .err;

        // collect ordered arg_ids for buildInstName (same order as generic_params)
        var ordered_ids = std.ArrayList(TypeId).initCapacity(self.allocator, fn_d.generic_params.len) catch return .err;
        defer ordered_ids.deinit(self.allocator);

        for (fn_d.generic_params) |gp| {
            ordered_ids.append(self.allocator, subst.get(gp.name) orelse return .err) catch return .err;
        }

        // instantiate the concrete function type
        const fn_type_id = self.instantiateGenericFn(fn_d, &subst, ordered_ids.items);
        if (fn_type_id.isErr()) return .err;

        // validate arg types against the concrete signature
        const ty = self.type_table.get(fn_type_id) orelse return .err;
        const func = switch (ty) {
            .function => |f| f,
            else => return .err,
        };

        for (call.args, func.param_types, arg_types.items) |arg, expected, actual| {
            if (!actual.isErr() and !expected.isErr() and actual != expected) {
                self.diagnostics.addCodedError(.E219, arg.location, self.fmt(
                    "expected {s}, got {s}",
                    .{ self.type_table.typeName(expected), self.type_table.typeName(actual) },
                )) catch {};
            }
        }

        return func.return_type;
    }

    fn checkFnCall(self: *Checker, call: ast.CallExpr, location: Location, scope: *const Scope) TypeId {
        const callee_type = self.checkExpr(call.callee, scope);
        if (callee_type.isErr()) return .err;

        // look up the function type
        const ty = self.type_table.get(callee_type) orelse return .err;
        const func = switch (ty) {
            .function => |f| f,
            else => {
                self.diagnostics.addCodedError(.E208, location, self.fmt(
                    "{s} is not callable",
                    .{self.type_table.typeName(callee_type)},
                )) catch {};
                return .err;
            },
        };

        // check argument count
        if (call.args.len != func.param_types.len) {
            self.diagnostics.addCodedError(.E207, location, self.fmt(
                "expected {d} argument(s), got {d}",
                .{ func.param_types.len, call.args.len },
            )) catch {};
            return .err;
        }

        // check argument types
        for (call.args, func.param_types) |arg, expected| {
            const actual = self.checkExpr(arg.value, scope);
            if (!actual.isErr() and !expected.isErr() and actual != expected) {
                // allow structurally equivalent function types (e.g. lambda vs declared fn type)
                if (!self.typesStructurallyEqual(expected, actual)) {
                    self.diagnostics.addCodedError(.E219, arg.location, self.fmt(
                        "expected {s}, got {s}",
                        .{ self.type_table.typeName(expected), self.type_table.typeName(actual) },
                    )) catch {};
                }
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
        const type_info = self.type_table.get(type_id) orelse return .err;
        const struct_data = switch (type_info) {
            .@"struct" => |s| s,
            else => {
                self.diagnostics.addCodedError(.E210, location, "expected struct type in constructor") catch {};
                return .err;
            },
        };

        // check argument count matches field count
        if (call.args.len != struct_data.fields.len) {
            self.diagnostics.addCodedError(.E207, location, self.fmt(
                "{s} has {d} field(s), got {d} argument(s)",
                .{ struct_data.name, struct_data.fields.len, call.args.len },
            )) catch {};
            return .err;
        }

        // check each argument type against the corresponding field
        for (call.args, struct_data.fields) |arg, field| {
            const actual = self.checkExpr(arg.value, scope);
            if (!actual.isErr() and !field.type_id.isErr() and actual != field.type_id) {
                self.diagnostics.addCodedError(.E219, arg.location, self.fmt(
                    "expected {s} for field '{s}', got {s}",
                    .{ self.type_table.typeName(field.type_id), field.name, self.type_table.typeName(actual) },
                )) catch {};
            }
        }

        return type_id;
    }

    fn checkMethodCall(self: *Checker, mc: ast.MethodCallExpr, location: Location, scope: *const Scope) TypeId {
        // evaluate the receiver to get its type
        const receiver_type = self.checkExpr(mc.receiver, scope);
        if (receiver_type.isErr()) return .err;

        // built-in methods on primitive types
        if (receiver_type == .string) return self.checkStringMethod(mc, location, scope);
        if (receiver_type == .int) return self.checkPrimitiveMethod(mc, location, "Int", &int_methods);
        if (receiver_type == .float) return self.checkPrimitiveMethod(mc, location, "Float", &float_methods);
        if (receiver_type == .bool) return self.checkPrimitiveMethod(mc, location, "Bool", &bool_methods);

        // check for built-in collection methods before user-defined lookup
        if (self.type_table.get(receiver_type)) |ty| {
            switch (ty) {
                .list => |l| {
                    if (self.checkListMethod(mc, location, scope, l.element, receiver_type)) |tid| return tid;
                },
                .map => |m| {
                    if (self.checkMapMethod(mc, location, scope, m.key, m.value, receiver_type)) |tid| return tid;
                },
                .set => |s| {
                    if (self.checkSetMethod(mc, location, scope, s.element)) |tid| return tid;
                },
                .@"struct" => |st| {
                    if (self.checkSyncMethod(mc, location, scope, st.name)) |tid| return tid;
                },
                else => {},
            }
        }

        // get the type name for method lookup
        const type_name = self.type_table.typeName(receiver_type);

        // look up the method
        const key = self.buildMethodKey(type_name, mc.method);
        const entry = self.method_types.get(key) orelse {
            self.diagnostics.addCodedError(.E227, location, self.fmt(
                "type '{s}' has no method '{s}'",
                .{ type_name, mc.method },
            )) catch {};
            return .err;
        };

        // get the function type for arg validation
        const ty = self.type_table.get(entry.type_id) orelse return .err;
        const func = switch (ty) {
            .function => |f| f,
            else => return .err,
        };

        // check argument count
        if (mc.args.len != func.param_types.len) {
            self.diagnostics.addCodedError(.E207, location, self.fmt(
                "'{s}.{s}' expects {d} argument(s), got {d}",
                .{ type_name, mc.method, func.param_types.len, mc.args.len },
            )) catch {};
            return .err;
        }

        // check argument types
        for (mc.args, func.param_types) |arg, expected| {
            const actual = self.checkExpr(arg.value, scope);
            if (!actual.isErr() and !expected.isErr() and actual != expected) {
                self.diagnostics.addCodedError(.E219, arg.location, self.fmt(
                    "expected {s}, got {s}",
                    .{ self.type_table.typeName(expected), self.type_table.typeName(actual) },
                )) catch {};
            }
        }

        return func.return_type;
    }

    /// validate a built-in method that takes no arguments.
    fn checkNoArgs(self: *Checker, mc: ast.MethodCallExpr, location: Location, label: []const u8, ret: TypeId) TypeId {
        if (mc.args.len != 0) {
            self.diagnostics.addCodedError(.E207, location, self.fmt(
                "'{s}' expects 0 arguments, got {d}", .{ label, mc.args.len },
            )) catch {};
            return .err;
        }
        return ret;
    }

    /// validate a built-in method that takes exactly one string argument.
    fn checkOneStringArg(self: *Checker, mc: ast.MethodCallExpr, location: Location, scope: *const Scope, label: []const u8, ret: TypeId) TypeId {
        if (mc.args.len != 1) {
            self.diagnostics.addCodedError(.E207, location, self.fmt(
                "'{s}' expects 1 argument, got {d}", .{ label, mc.args.len },
            )) catch {};
            return .err;
        }
        const arg_type = self.checkExpr(mc.args[0].value, scope);
        if (!arg_type.isErr() and arg_type != .string) {
            self.diagnostics.addCodedError(.E219, mc.args[0].location, self.fmt(
                "expected String, got {s}", .{self.type_table.typeName(arg_type)},
            )) catch {};
        }
        return ret;
    }

    /// type-check a method call on a String receiver.
    fn checkStringMethod(self: *Checker, mc: ast.MethodCallExpr, location: Location, scope: *const Scope) TypeId {
        const method = mc.method;

        // no-arg methods
        if (std.mem.eql(u8, method, "len")) return self.checkNoArgs(mc, location, "String.len", .int);
        if (std.mem.eql(u8, method, "trim")) return self.checkNoArgs(mc, location, "String.trim", .string);
        if (std.mem.eql(u8, method, "to_upper")) return self.checkNoArgs(mc, location, "String.to_upper", .string);
        if (std.mem.eql(u8, method, "to_lower")) return self.checkNoArgs(mc, location, "String.to_lower", .string);

        // one-string-arg methods
        if (std.mem.eql(u8, method, "contains")) return self.checkOneStringArg(mc, location, scope, "String.contains", .bool);
        if (std.mem.eql(u8, method, "starts_with")) return self.checkOneStringArg(mc, location, scope, "String.starts_with", .bool);
        if (std.mem.eql(u8, method, "ends_with")) return self.checkOneStringArg(mc, location, scope, "String.ends_with", .bool);

        // split(String) -> List[String]
        if (std.mem.eql(u8, method, "split")) {
            if (mc.args.len != 1) {
                self.diagnostics.addCodedError(.E207, location, self.fmt(
                    "'String.split' expects 1 argument, got {d}", .{mc.args.len},
                )) catch {};
                return .err;
            }
            const arg_type = self.checkExpr(mc.args[0].value, scope);
            if (!arg_type.isErr() and arg_type != .string) {
                self.diagnostics.addCodedError(.E219, mc.args[0].location, self.fmt(
                    "expected String, got {s}", .{self.type_table.typeName(arg_type)},
                )) catch {};
            }
            // List[String] was pre-registered in registerBuiltinFunctions
            return self.type_table.lookup("List[String]") orelse .err;
        }

        // substring(Int, Int) -> String
        if (std.mem.eql(u8, method, "substring")) {
            if (mc.args.len != 2) {
                self.diagnostics.addCodedError(.E207, location, self.fmt(
                    "'String.substring' expects 2 arguments, got {d}", .{mc.args.len},
                )) catch {};
                return .err;
            }
            for (mc.args) |arg| {
                const arg_type = self.checkExpr(arg.value, scope);
                if (!arg_type.isErr() and arg_type != .int) {
                    self.diagnostics.addCodedError(.E219, arg.location, self.fmt(
                        "expected Int, got {s}", .{self.type_table.typeName(arg_type)},
                    )) catch {};
                }
            }
            return .string;
        }

        // replace(String, String) -> String
        if (std.mem.eql(u8, method, "replace")) {
            if (mc.args.len != 2) {
                self.diagnostics.addCodedError(.E207, location, self.fmt(
                    "'String.replace' expects 2 arguments, got {d}", .{mc.args.len},
                )) catch {};
                return .err;
            }
            for (mc.args) |arg| {
                const arg_type = self.checkExpr(arg.value, scope);
                if (!arg_type.isErr() and arg_type != .string) {
                    self.diagnostics.addCodedError(.E219, arg.location, self.fmt(
                        "expected String, got {s}", .{self.type_table.typeName(arg_type)},
                    )) catch {};
                }
            }
            return .string;
        }

        // unknown string method
        self.diagnostics.addCodedError(.E227, location, self.fmt(
            "type 'String' has no method '{s}'", .{method},
        )) catch {};
        return .err;
    }

    /// method table entry for primitive types.
    const PrimitiveMethod = struct {
        name: []const u8,
        return_type: TypeId,
    };

    const int_methods = [_]PrimitiveMethod{
        .{ .name = "to_string", .return_type = .string },
        .{ .name = "to_float", .return_type = .float },
    };

    const float_methods = [_]PrimitiveMethod{
        .{ .name = "to_string", .return_type = .string },
        .{ .name = "to_int", .return_type = .int },
    };

    const bool_methods = [_]PrimitiveMethod{
        .{ .name = "to_string", .return_type = .string },
    };

    /// type-check a method call on a primitive type (Int, Float, Bool).
    /// uses a table-driven lookup instead of per-type functions.
    fn checkPrimitiveMethod(
        self: *Checker,
        mc: ast.MethodCallExpr,
        location: Location,
        type_name: []const u8,
        methods: []const PrimitiveMethod,
    ) TypeId {
        for (methods) |m| {
            if (std.mem.eql(u8, mc.method, m.name)) {
                const label = self.fmt("{s}.{s}", .{ type_name, m.name });
                return self.checkNoArgs(mc, location, label, m.return_type);
            }
        }
        self.diagnostics.addCodedError(.E227, location, self.fmt(
            "type '{s}' has no method '{s}'", .{ type_name, mc.method },
        )) catch {};
        return .err;
    }

    /// type-check a method call on a List receiver. returns null if the method
    /// is not a built-in list method (falls through to user-defined lookup).
    fn checkListMethod(self: *Checker, mc: ast.MethodCallExpr, location: Location, scope: *const Scope, elem_type: TypeId, receiver_type: TypeId) ?TypeId {
        const method = mc.method;
        _ = receiver_type;

        if (std.mem.eql(u8, method, "push")) {
            return self.checkOneTypedArg(mc, location, scope, "List.push", elem_type, .void);
        }
        if (std.mem.eql(u8, method, "len")) return self.checkNoArgs(mc, location, "List.len", .int);
        if (std.mem.eql(u8, method, "is_empty")) return self.checkNoArgs(mc, location, "List.is_empty", .bool);
        if (std.mem.eql(u8, method, "clear")) return self.checkNoArgs(mc, location, "List.clear", .void);
        if (std.mem.eql(u8, method, "reverse")) return self.checkNoArgs(mc, location, "List.reverse", .void);
        if (std.mem.eql(u8, method, "remove")) return self.checkNoArgs1Int(mc, location, scope, "List.remove", .void);
        if (std.mem.eql(u8, method, "contains")) {
            return self.checkOneTypedArg(mc, location, scope, "List.contains", elem_type, .bool);
        }
        if (std.mem.eql(u8, method, "join")) {
            if (elem_type != .string) {
                self.diagnostics.addCodedError(.E227, location,
                    "'join' requires List[String]",
                ) catch {};
                return .err;
            }
            return self.checkOneStringArg(mc, location, scope, "List.join", .string);
        }
        return null;
    }

    /// type-check a method call on a Map receiver.
    fn checkMapMethod(self: *Checker, mc: ast.MethodCallExpr, location: Location, scope: *const Scope, key_type: TypeId, val_type: TypeId, receiver_type: TypeId) ?TypeId {
        const method = mc.method;
        _ = receiver_type;

        if (std.mem.eql(u8, method, "insert")) {
            if (mc.args.len != 2) {
                self.diagnostics.addCodedError(.E207, location, self.fmt(
                    "'Map.insert' expects 2 arguments, got {d}", .{mc.args.len},
                )) catch {};
                return .err;
            }
            const kt = self.checkExpr(mc.args[0].value, scope);
            if (!kt.isErr() and kt != key_type) {
                self.diagnostics.addCodedError(.E219, mc.args[0].location, self.fmt(
                    "expected {s}, got {s}",
                    .{ self.type_table.typeName(key_type), self.type_table.typeName(kt) },
                )) catch {};
            }
            const vt = self.checkExpr(mc.args[1].value, scope);
            if (!vt.isErr() and vt != val_type) {
                self.diagnostics.addCodedError(.E219, mc.args[1].location, self.fmt(
                    "expected {s}, got {s}",
                    .{ self.type_table.typeName(val_type), self.type_table.typeName(vt) },
                )) catch {};
            }
            return .void;
        }
        if (std.mem.eql(u8, method, "len")) return self.checkNoArgs(mc, location, "Map.len", .int);
        if (std.mem.eql(u8, method, "is_empty")) return self.checkNoArgs(mc, location, "Map.is_empty", .bool);
        if (std.mem.eql(u8, method, "clear")) return self.checkNoArgs(mc, location, "Map.clear", .void);
        if (std.mem.eql(u8, method, "contains_key")) {
            return self.checkOneTypedArg(mc, location, scope, "Map.contains_key", key_type, .bool);
        }
        if (std.mem.eql(u8, method, "remove")) {
            return self.checkOneTypedArg(mc, location, scope, "Map.remove", key_type, .void);
        }
        if (std.mem.eql(u8, method, "keys")) {
            if (mc.args.len != 0) {
                self.diagnostics.addCodedError(.E207, location, self.fmt(
                    "'Map.keys' expects 0 arguments, got {d}", .{mc.args.len},
                )) catch {};
                return .err;
            }
            // register List[K] and return it
            const list_k = self.internCollectionType("List", &.{key_type}, .{ .list = .{ .element = key_type } });
            return list_k;
        }
        if (std.mem.eql(u8, method, "values")) {
            if (mc.args.len != 0) {
                self.diagnostics.addCodedError(.E207, location, self.fmt(
                    "'Map.values' expects 0 arguments, got {d}", .{mc.args.len},
                )) catch {};
                return .err;
            }
            // register List[V] and return it
            const list_v = self.internCollectionType("List", &.{val_type}, .{ .list = .{ .element = val_type } });
            return list_v;
        }
        return null;
    }

    /// type-check a method call on a Set receiver.
    fn checkSetMethod(self: *Checker, mc: ast.MethodCallExpr, location: Location, scope: *const Scope, elem_type: TypeId) ?TypeId {
        const method = mc.method;

        if (std.mem.eql(u8, method, "add")) {
            return self.checkOneTypedArg(mc, location, scope, "Set.add", elem_type, .void);
        }
        if (std.mem.eql(u8, method, "len")) return self.checkNoArgs(mc, location, "Set.len", .int);
        if (std.mem.eql(u8, method, "is_empty")) return self.checkNoArgs(mc, location, "Set.is_empty", .bool);
        if (std.mem.eql(u8, method, "clear")) return self.checkNoArgs(mc, location, "Set.clear", .void);
        if (std.mem.eql(u8, method, "contains")) {
            return self.checkOneTypedArg(mc, location, scope, "Set.contains", elem_type, .bool);
        }
        if (std.mem.eql(u8, method, "remove")) {
            return self.checkOneTypedArg(mc, location, scope, "Set.remove", elem_type, .void);
        }
        return null;
    }

    /// check built-in methods on sync primitive types (Mutex, WaitGroup, Semaphore).
    /// returns null if the receiver isn't a sync type or the method isn't recognized.
    fn checkSyncMethod(self: *Checker, mc: ast.MethodCallExpr, location: Location, scope: *const Scope, type_name: []const u8) ?TypeId {
        if (std.mem.eql(u8, type_name, "Mutex")) {
            if (std.mem.eql(u8, mc.method, "lock")) return self.checkNoArgs(mc, location, "Mutex.lock", .void);
            if (std.mem.eql(u8, mc.method, "unlock")) return self.checkNoArgs(mc, location, "Mutex.unlock", .void);
        } else if (std.mem.eql(u8, type_name, "WaitGroup")) {
            if (std.mem.eql(u8, mc.method, "add")) return self.checkOneTypedArg(mc, location, scope, "WaitGroup.add", .int, .void);
            if (std.mem.eql(u8, mc.method, "done")) return self.checkNoArgs(mc, location, "WaitGroup.done", .void);
            if (std.mem.eql(u8, mc.method, "wait")) return self.checkNoArgs(mc, location, "WaitGroup.wait", .void);
        } else if (std.mem.eql(u8, type_name, "Semaphore")) {
            if (std.mem.eql(u8, mc.method, "acquire")) return self.checkNoArgs(mc, location, "Semaphore.acquire", .void);
            if (std.mem.eql(u8, mc.method, "release")) return self.checkNoArgs(mc, location, "Semaphore.release", .void);
        }
        return null;
    }

    /// validate a built-in method that takes one argument of a specific type.
    fn checkOneTypedArg(self: *Checker, mc: ast.MethodCallExpr, location: Location, scope: *const Scope, label: []const u8, expected: TypeId, ret: TypeId) TypeId {
        if (mc.args.len != 1) {
            self.diagnostics.addCodedError(.E207, location, self.fmt(
                "'{s}' expects 1 argument, got {d}", .{ label, mc.args.len },
            )) catch {};
            return .err;
        }
        const arg_type = self.checkExpr(mc.args[0].value, scope);
        if (!arg_type.isErr() and arg_type != expected) {
            self.diagnostics.addCodedError(.E219, mc.args[0].location, self.fmt(
                "expected {s}, got {s}",
                .{ self.type_table.typeName(expected), self.type_table.typeName(arg_type) },
            )) catch {};
        }
        return ret;
    }

    /// validate a built-in method that takes one Int argument.
    fn checkNoArgs1Int(self: *Checker, mc: ast.MethodCallExpr, location: Location, scope: *const Scope, label: []const u8, ret: TypeId) TypeId {
        if (mc.args.len != 1) {
            self.diagnostics.addCodedError(.E207, location, self.fmt(
                "'{s}' expects 1 argument, got {d}", .{ label, mc.args.len },
            )) catch {};
            return .err;
        }
        const arg_type = self.checkExpr(mc.args[0].value, scope);
        if (!arg_type.isErr() and arg_type != .int) {
            self.diagnostics.addCodedError(.E219, mc.args[0].location, self.fmt(
                "expected Int, got {s}", .{self.type_table.typeName(arg_type)},
            )) catch {};
        }
        return ret;
    }

    fn checkFieldAccess(self: *Checker, fa: ast.FieldAccess, location: Location, scope: *const Scope) TypeId {
        const object_type = self.checkExpr(fa.object, scope);
        if (object_type.isErr()) return .err;

        const ty = self.type_table.get(object_type) orelse return .err;
        switch (ty) {
            .@"struct" => |struct_data| {
                for (struct_data.fields) |field| {
                    if (std.mem.eql(u8, field.name, fa.field)) {
                        return field.type_id;
                    }
                }
                self.diagnostics.addCodedError(.E209, location, self.fmt(
                    "struct {s} has no field '{s}'",
                    .{ struct_data.name, fa.field },
                )) catch {};
                return .err;
            },
            .tuple => |tup| {
                // numeric field access: .0, .1, etc.
                const idx = std.fmt.parseInt(usize, fa.field, 10) catch {
                    self.diagnostics.addCodedError(.E209, location, self.fmt(
                        "tuple has no field '{s}' (use numeric indices: .0, .1, ...)",
                        .{fa.field},
                    )) catch {};
                    return .err;
                };
                if (idx < tup.elements.len) {
                    return tup.elements[idx];
                }
                self.diagnostics.addCodedError(.E209, location, self.fmt(
                    "tuple index {d} out of bounds (tuple has {d} elements)",
                    .{ idx, tup.elements.len },
                )) catch {};
                return .err;
            },
            else => {
                self.diagnostics.addCodedError(.E209, location, self.fmt(
                    "{s} has no field '{s}'",
                    .{ self.type_table.typeName(object_type), fa.field },
                )) catch {};
                return .err;
            },
        }
    }

    fn checkMatchExpr(self: *Checker, m: ast.MatchExpr, location: Location, scope: *const Scope) TypeId {
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
                self.diagnostics.addCodedError(.E215, arm.location, self.fmt(
                    "match arm type mismatch: expected {s}, got {s}",
                    .{ self.type_table.typeName(expected_type), self.type_table.typeName(arm_type) },
                )) catch {};
            }
        }

        self.checkExhaustiveness(m, subject_type, location);

        return expected_type;
    }

    fn checkMatchStmt(self: *Checker, m: ast.MatchExpr, location: Location, scope: *const Scope) void {
        const subject_type = self.checkExpr(m.subject, scope);
        if (subject_type.isErr()) return;

        // match statement — no arm type agreement needed
        for (m.arms) |arm| {
            _ = self.checkMatchArm(arm, subject_type, scope);
        }

        self.checkExhaustiveness(m, subject_type, location);
    }

    // ---------------------------------------------------------------
    // match exhaustiveness
    // ---------------------------------------------------------------

    /// verify that a match expression covers all possible values of
    /// the subject type. rules:
    ///   - wildcard or binding pattern (without guard) → always exhaustive
    ///   - enum subject → all variants must be covered
    ///   - Bool subject → both true and false must be covered
    ///   - Int/Float/String → infinite domain, require wildcard or binding
    ///   - guarded arms don't count (guard can fail at runtime)
    fn checkExhaustiveness(self: *Checker, m: ast.MatchExpr, subject_type: TypeId, location: Location) void {
        // scan for a catch-all pattern (wildcard or binding) without a guard
        for (m.arms) |arm| {
            if (arm.guard != null) continue;
            switch (arm.pattern.kind) {
                .wildcard, .binding => return, // exhaustive
                else => {},
            }
        }

        // no catch-all — dispatch to type-specific checks
        if (subject_type == .bool) {
            self.checkBoolExhaustiveness(m, location);
            return;
        }

        // look up the type to see if it's an enum
        const ty = self.type_table.get(subject_type) orelse return;
        switch (ty) {
            .@"enum" => |e| self.checkEnumExhaustiveness(m, e.variants, location),
            else => {
                // Int, Float, String, etc. — infinite domain, require catch-all
                self.diagnostics.addCodedErrorWithFix(
                    .E204,
                    location,
                    self.fmt(
                        "non-exhaustive match on {s}: add a wildcard '_' or binding pattern to cover all values",
                        .{self.type_table.typeName(subject_type)},
                    ),
                    "add a wildcard '_' catch-all arm",
                ) catch {};
            },
        }
    }

    /// check that every variant of an enum is covered by at least one arm.
    fn checkEnumExhaustiveness(
        self: *Checker,
        m: ast.MatchExpr,
        variants: []const types.Variant,
        location: Location,
    ) void {
        // collect which variants are covered (by unguarded arms)
        var missing: std.ArrayList([]const u8) = .empty;
        defer missing.deinit(self.allocator);

        for (variants) |variant| {
            var covered = false;
            for (m.arms) |arm| {
                if (arm.guard != null) continue;
                switch (arm.pattern.kind) {
                    .variant => |v| {
                        if (std.mem.eql(u8, v.variant, variant.name)) {
                            covered = true;
                            break;
                        }
                    },
                    else => {},
                }
            }
            if (!covered) {
                missing.append(self.allocator, variant.name) catch {};
            }
        }

        if (missing.items.len > 0) {
            // format the list of missing variants
            var buf: std.ArrayList(u8) = .empty;
            defer buf.deinit(self.allocator);
            const w = buf.writer(self.allocator);
            for (missing.items, 0..) |name, i| {
                if (i > 0) w.writeAll(", ") catch {};
                w.writeAll(name) catch {};
            }

            const names = self.arena.allocator().dupe(u8, buf.items) catch "<format error>";
            self.diagnostics.addCodedErrorWithFix(
                .E204,
                location,
                self.fmt("non-exhaustive match: missing variant(s) {s}", .{names}),
                "add missing variant patterns or a wildcard '_' catch-all",
            ) catch {};
        }
    }

    /// check that a Bool match covers both true and false.
    fn checkBoolExhaustiveness(self: *Checker, m: ast.MatchExpr, location: Location) void {
        var has_true = false;
        var has_false = false;

        for (m.arms) |arm| {
            if (arm.guard != null) continue;
            switch (arm.pattern.kind) {
                .bool_lit => |val| {
                    if (val) has_true = true else has_false = true;
                },
                else => {},
            }
        }

        if (!has_true or !has_false) {
            const missing = if (!has_true and !has_false)
                "true, false"
            else if (!has_true)
                "true"
            else
                "false";

            self.diagnostics.addCodedErrorWithFix(
                .E204,
                location,
                self.fmt("non-exhaustive match on Bool: missing {s}", .{missing}),
                "add missing Bool patterns or a wildcard '_' catch-all",
            ) catch {};
        }
    }

    fn checkMatchArm(self: *Checker, arm: ast.MatchArm, subject_type: TypeId, scope: *const Scope) TypeId {
        // each arm gets its own scope for pattern bindings
        var arm_scope = Scope.init(self.allocator, scope);
        defer arm_scope.deinit();

        self.checkPattern(arm.pattern, subject_type, &arm_scope);

        // check guard expression if present
        if (arm.guard) |guard| {
            const guard_type = self.checkExpr(guard, &arm_scope);
            if (!guard_type.isErr() and guard_type != .bool) {
                self.diagnostics.addCodedError(.E218, guard.location, self.fmt(
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
            self.diagnostics.addCodedError(.E228, location, self.fmt(
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
            self.diagnostics.addCodedError(.E202, location, self.fmt(
                "unknown type '{s}'",
                .{v.type_name},
            )) catch {};
            return;
        };

        if (enum_type_id != subject_type) {
            self.diagnostics.addCodedError(.E228, location, self.fmt(
                "pattern type {s} does not match subject type {s}",
                .{ v.type_name, self.type_table.typeName(subject_type) },
            )) catch {};
            return;
        }

        const ty = self.type_table.get(enum_type_id) orelse return;
        const enum_data = switch (ty) {
            .@"enum" => |e| e,
            else => {
                self.diagnostics.addCodedError(.E211, location, self.fmt(
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
                    self.diagnostics.addCodedError(.E213, location, self.fmt(
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

        self.diagnostics.addCodedError(.E212, location, self.fmt(
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
                self.diagnostics.addCodedError(.E228, location, self.fmt(
                    "cannot match tuple pattern against {s}",
                    .{self.type_table.typeName(subject_type)},
                )) catch {};
                return;
            },
        };

        if (elems.len != tuple_data.elements.len) {
            self.diagnostics.addCodedError(.E213, location, self.fmt(
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
                self.diagnostics.addCodedError(.E230, param.location, self.fmt(
                    "lambda parameter '{s}' needs a type annotation",
                    .{param.name},
                )) catch {};
                return .err;
            }
        }

        // create a child scope for the lambda body
        var lambda_scope = Scope.init(self.allocator, scope);
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

    /// check a list literal [a, b, c]. all elements must have the same type.
    /// an empty list produces an error — the type can't be inferred without context.
    fn checkListExpr(self: *Checker, elems: []const *const ast.Expr, location: Location, scope: *const Scope) TypeId {
        _ = location;
        if (elems.len == 0) {
            // empty list — type will come from the binding's type annotation.
            // no error here; checkBindingStmt will use the declared type.
            return .err;
        }

        const first_type = self.checkExpr(elems[0], scope);
        if (first_type.isErr()) return .err;

        for (elems[1..]) |elem| {
            const elem_type = self.checkExpr(elem, scope);
            if (elem_type.isErr()) return .err;
            if (elem_type != first_type) {
                self.diagnostics.addCodedError(.E223, elem.location, self.fmt(
                    "list element type mismatch: expected {s}, got {s}",
                    .{ self.type_table.typeName(first_type), self.type_table.typeName(elem_type) },
                )) catch {};
                return .err;
            }
        }

        return self.internCollectionType("List", &.{first_type}, .{ .list = .{ .element = first_type } });
    }

    /// check a map literal {k: v, ...}. all keys must share a type, all values must share a type.
    /// an empty map {} is allowed — but the type can't be inferred, so we return err.
    fn checkMapExpr(self: *Checker, entries: []const ast.MapEntry, location: Location, scope: *const Scope) TypeId {
        _ = location;
        if (entries.len == 0) {
            // empty map — type will come from the binding's type annotation.
            return .err;
        }

        const first_key_type = self.checkExpr(entries[0].key, scope);
        const first_val_type = self.checkExpr(entries[0].value, scope);
        if (first_key_type.isErr() or first_val_type.isErr()) return .err;

        for (entries[1..]) |entry| {
            const key_type = self.checkExpr(entry.key, scope);
            if (!key_type.isErr() and key_type != first_key_type) {
                self.diagnostics.addCodedError(.E223, entry.location, self.fmt(
                    "map key type mismatch: expected {s}, got {s}",
                    .{ self.type_table.typeName(first_key_type), self.type_table.typeName(key_type) },
                )) catch {};
                return .err;
            }

            const val_type = self.checkExpr(entry.value, scope);
            if (!val_type.isErr() and val_type != first_val_type) {
                self.diagnostics.addCodedError(.E223, entry.location, self.fmt(
                    "map value type mismatch: expected {s}, got {s}",
                    .{ self.type_table.typeName(first_val_type), self.type_table.typeName(val_type) },
                )) catch {};
                return .err;
            }
        }

        return self.internCollectionType("Map", &.{ first_key_type, first_val_type }, .{ .map = .{
            .key = first_key_type,
            .value = first_val_type,
        } });
    }

    /// check a set literal {a, b, c}. all elements must have the same type.
    fn checkSetExpr(self: *Checker, elems: []const *const ast.Expr, location: Location, scope: *const Scope) TypeId {
        if (elems.len == 0) {
            // the parser emits empty {} as a map, not a set, so this shouldn't
            // happen in practice — but guard against it.
            self.diagnostics.addCodedError(.E223, location, "cannot infer element type of empty set") catch {};
            return .err;
        }

        const first_type = self.checkExpr(elems[0], scope);
        if (first_type.isErr()) return .err;

        for (elems[1..]) |elem| {
            const elem_type = self.checkExpr(elem, scope);
            if (elem_type.isErr()) return .err;
            if (elem_type != first_type) {
                self.diagnostics.addCodedError(.E223, elem.location, self.fmt(
                    "set element type mismatch: expected {s}, got {s}",
                    .{ self.type_table.typeName(first_type), self.type_table.typeName(elem_type) },
                )) catch {};
                return .err;
            }
        }

        return self.internCollectionType("Set", &.{first_type}, .{ .set = .{ .element = first_type } });
    }

    /// check an index expression: obj[idx]. supports List[T], Map[K, V], and tuples.
    fn checkIndexExpr(self: *Checker, idx: ast.IndexExpr, location: Location, scope: *const Scope) TypeId {
        const obj_type = self.checkExpr(idx.object, scope);
        const index_type = self.checkExpr(idx.index, scope);
        if (obj_type.isErr() or index_type.isErr()) return .err;

        // string indexing: s[n] returns a single-character string
        if (obj_type == .string) {
            if (!index_type.isInteger()) {
                self.diagnostics.addCodedError(.E217, location, self.fmt(
                    "string index must be an integer, got {s}",
                    .{self.type_table.typeName(index_type)},
                )) catch {};
                return .err;
            }
            return .string;
        }

        const ty = self.type_table.get(obj_type) orelse return .err;
        return switch (ty) {
            .list => |l| blk: {
                if (!index_type.isInteger()) {
                    self.diagnostics.addCodedError(.E217, location, self.fmt(
                        "list index must be an integer, got {s}",
                        .{self.type_table.typeName(index_type)},
                    )) catch {};
                    break :blk .err;
                }
                break :blk l.element;
            },
            .map => |m| blk: {
                if (index_type != m.key) {
                    self.diagnostics.addCodedError(.E217, location, self.fmt(
                        "map key type mismatch: expected {s}, got {s}",
                        .{ self.type_table.typeName(m.key), self.type_table.typeName(index_type) },
                    )) catch {};
                    break :blk .err;
                }
                break :blk m.value;
            },
            else => blk: {
                self.diagnostics.addCodedError(.E217, location, self.fmt(
                    "type '{s}' does not support indexing",
                    .{self.type_table.typeName(obj_type)},
                )) catch {};
                break :blk .err;
            },
        };
    }

    fn checkTupleExpr(self: *Checker, elems: []const *const ast.Expr, location: Location, scope: *const Scope) TypeId {
        if (elems.len == 0) {
            self.diagnostics.addCodedError(.E233, location, "empty tuple is not allowed") catch {};
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

    /// check if two types are structurally equal even if they have different TypeIds.
    /// handles function types and tuples where the checker creates distinct TypeIds
    /// for structurally identical types.
    fn typesStructurallyEqual(self: *Checker, a: TypeId, b: TypeId) bool {
        if (a == b) return true;
        const ty_a = self.type_table.get(a) orelse return false;
        const ty_b = self.type_table.get(b) orelse return false;

        return switch (ty_a) {
            .function => |fa| switch (ty_b) {
                .function => |fb| blk: {
                    if (fa.return_type != fb.return_type) break :blk false;
                    if (fa.param_types.len != fb.param_types.len) break :blk false;
                    for (fa.param_types, fb.param_types) |pa, pb| {
                        if (pa != pb) break :blk false;
                    }
                    break :blk true;
                },
                else => false,
            },
            .tuple => |ta| switch (ty_b) {
                .tuple => |tb| blk: {
                    if (ta.elements.len != tb.elements.len) break :blk false;
                    for (ta.elements, tb.elements) |ea, eb| {
                        if (ea != eb) break :blk false;
                    }
                    break :blk true;
                },
                else => false,
            },
            else => false,
        };
    }

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
