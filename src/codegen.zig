// codegen — C transpilation backend
//
// walks the type-checked AST and emits C source code. the generated C
// is compiled by `zig cc` to produce a native binary.
//
// the emitter does a single pass over the module:
//   1. emit #include and forward declarations
//   2. emit struct/enum type definitions
//   3. emit function definitions
//
// type mapping:
//   Int    → int64_t        Float  → double
//   Bool   → bool           String → forge_string_t
//   Void   → void           sized ints → stdint types
//
// the emitter assumes the AST has been type-checked. it uses the type
// table from the checker to resolve types for expressions, struct
// fields, etc.

const std = @import("std");
const ast = @import("ast.zig");
const types = @import("types.zig");
const Checker = @import("checker.zig");

const TypeId = types.TypeId;
const TypeTable = types.TypeTable;
const Type = types.Type;
const Scope = Checker.Scope;

// builtins that emit as forge_<name>(<args>) with no special logic.
// checked via a comptime perfect-hash set for O(1) lookup.
const forge_prefix_builtins = std.StaticStringMap(void).initComptime(.{
    .{ "parse_int", {} },
    .{ "parse_float", {} },
    .{ "read_file", {} },
    .{ "write_file", {} },
    .{ "env", {} },
    .{ "chr", {} },
    .{ "exec_output", {} },
    .{ "time", {} },
    .{ "sleep", {} },
    .{ "random_int", {} },
    .{ "random_float", {} },
    .{ "input", {} },
    .{ "path_join", {} },
    .{ "path_dir", {} },
    .{ "path_base", {} },
    .{ "path_ext", {} },
    .{ "path_stem", {} },
    .{ "log_info", {} },
    .{ "log_warn", {} },
    .{ "log_error", {} },
    .{ "log_debug", {} },
    .{ "file_exists", {} },
    .{ "dir_exists", {} },
    .{ "mkdir", {} },
    .{ "remove_file", {} },
    .{ "rename_file", {} },
    .{ "append_file", {} },
    .{ "list_dir", {} },
    .{ "math_pow", {} },
    .{ "math_sqrt", {} },
    .{ "math_floor", {} },
    .{ "math_ceil", {} },
    .{ "math_round", {} },
    .{ "fmt_hex", {} },
    .{ "fmt_oct", {} },
    .{ "fmt_bin", {} },
    .{ "fmt_float", {} },
    .{ "json_parse", {} },
    .{ "json_type", {} },
    .{ "json_get_bool", {} },
    .{ "json_get_int", {} },
    .{ "json_get_float", {} },
    .{ "json_get_string", {} },
    .{ "json_array_len", {} },
    .{ "json_array_get", {} },
    .{ "json_object_get", {} },
    .{ "json_object_has", {} },
    .{ "json_object_keys", {} },
    .{ "json_encode", {} },
    .{ "json_new_null", {} },
    .{ "json_new_bool", {} },
    .{ "json_new_int", {} },
    .{ "json_new_float", {} },
    .{ "json_new_string", {} },
    .{ "json_new_array", {} },
    .{ "json_new_object", {} },
    .{ "json_array_push", {} },
    .{ "json_object_set", {} },
});

// ---------------------------------------------------------------
// C emitter
// ---------------------------------------------------------------

/// errors that can occur during C emission. limited to allocation
/// failures since we're just building a string buffer.
pub const EmitError = std.mem.Allocator.Error;

/// a variable captured from an enclosing scope by a closure.
const CapturedVar = struct {
    name: []const u8,
    type_id: TypeId,
};

/// a lambda body recorded for hoisting as a top-level C function.
const HoistedLambda = struct {
    index: u32,
    lambda: ast.Lambda,
    fn_type_id: TypeId,
    /// variables captured from the enclosing scope. empty for non-capturing lambdas.
    captures: []const CapturedVar,
};

/// a spawn expression recorded for hoisting as a static wrapper function.
/// each spawn creates a wrapper that pthread_create can call.
const HoistedSpawn = struct {
    index: u32,
    /// the function being called (name extracted from the inner call expression)
    fn_name: []const u8,
    /// argument types for the spawned call
    arg_types: []const TypeId,
    /// the return type of the spawned function (inner type of Task[T])
    return_type: TypeId,
    /// the C type name for the task struct (e.g. "forge_task_int64_t")
    task_type_name: []const u8,
};

/// key for looking up function types by signature (for lambda type inference).
const FnSigKey = struct {
    params: [16]TypeId = .{.err} ** 16,
    param_count: u8,
    return_type: TypeId,
};

/// key for looking up tuple types by element signature.
const TupleSigKey = struct {
    elements: [16]TypeId = .{.err} ** 16,
    elem_count: u8,
};

pub const CEmitter = struct {
    output: std.ArrayList(u8),
    indent_level: u32,
    type_table: *const TypeTable,
    module_scope: *const Scope,
    allocator: std.mem.Allocator,
    /// tracks variable types in the current function scope so we can
    /// choose the right C type for bindings and string operations.
    local_types: std.StringHashMap(TypeId),
    /// the return type of the current function being emitted. used to
    /// determine when match arms need `return` prepended.
    current_fn_return: TypeId,
    /// method type information from the checker. key is "TypeName.methodName",
    /// value has the function type id and original AST decl.
    method_types: *const std.StringHashMap(Checker.MethodEntry),
    /// generic declarations from the checker. used to look up original AST
    /// for generic functions when emitting monomorphized instantiations.
    generic_decls: *const std.StringHashMap(Checker.GenericDecl),
    /// counter for generating unique loop index variable names (__idx_0, __idx_1, ...)
    for_counter: u32,
    /// counter for generating unique try temporary variable names (__try_0, __try_1, ...)
    try_counter: u32,
    /// set of result type TypeIds whose C typedefs have already been emitted.
    /// (kept for compatibility but dedup now uses name-based check)
    emitted_result_types: std.AutoHashMap(TypeId, void),
    /// cached mangled names for instantiated generic types. TypeId → "Pair_Int_String".
    /// populated during the monomorphization pass, used by cTypeStringForId.
    mangled_names: std.AutoHashMap(TypeId, []const u8),
    /// counter for generating unique lambda function names (__lambda_0, __lambda_1, ...)
    lambda_counter: u32,
    /// accumulated lambda bodies to hoist as top-level C functions.
    hoisted_lambdas: std.ArrayList(HoistedLambda),
    /// counter for generating unique spawn wrapper function names
    spawn_counter: u32,
    /// accumulated spawn wrappers to hoist as static C functions.
    hoisted_spawns: std.ArrayList(HoistedSpawn),
    /// when true, emit test functions and a test runner main() instead
    /// of the user's main(). set by `forge test`.
    test_mode: bool,
    /// test names collected during emission (for the test runner summary).
    test_names: std.ArrayList([]const u8),
    /// imported module declarations to emit alongside the main module.
    imported_modules: []const Checker.ImportedModule,
    /// whether any top-level global bindings were emitted.
    has_globals: bool,
    /// counter for unique push temp variable names.
    push_counter: u32,
    /// cache: function signature → TypeId (avoids linear type table scan for lambdas)
    fn_type_cache: std.AutoHashMap(FnSigKey, TypeId),
    /// cache: tuple element signature → TypeId (avoids linear type table scan for tuples)
    tuple_type_cache: std.AutoHashMap(TupleSigKey, TypeId),

    pub fn init(
        allocator: std.mem.Allocator,
        type_table: *const TypeTable,
        module_scope: *const Scope,
        method_types: *const std.StringHashMap(Checker.MethodEntry),
        generic_decls: *const std.StringHashMap(Checker.GenericDecl),
    ) CEmitter {
        var emitter = CEmitter{
            .output = .empty,
            .indent_level = 0,
            .type_table = type_table,
            .module_scope = module_scope,
            .allocator = allocator,
            .local_types = std.StringHashMap(TypeId).init(allocator),
            .current_fn_return = .void,
            .method_types = method_types,
            .generic_decls = generic_decls,
            .for_counter = 0,
            .try_counter = 0,
            .emitted_result_types = std.AutoHashMap(TypeId, void).init(allocator),
            .mangled_names = std.AutoHashMap(TypeId, []const u8).init(allocator),
            .lambda_counter = 0,
            .hoisted_lambdas = .empty,
            .spawn_counter = 0,
            .hoisted_spawns = .empty,
            .test_mode = false,
            .test_names = .empty,
            .imported_modules = &.{},
            .has_globals = false,
            .push_counter = 0,
            .fn_type_cache = std.AutoHashMap(FnSigKey, TypeId).init(allocator),
            .tuple_type_cache = std.AutoHashMap(TupleSigKey, TypeId).init(allocator),
        };

        // eagerly populate function and tuple type caches from the type table
        const items = type_table.types.items;
        for (items, 0..) |ty, idx| {
            const tid = TypeId.fromIndex(@intCast(idx));
            switch (ty) {
                .function => |func| {
                    if (func.param_types.len <= 16) {
                        var key = FnSigKey{
                            .param_count = @intCast(func.param_types.len),
                            .return_type = func.return_type,
                        };
                        for (func.param_types, 0..) |p, i| key.params[i] = p;
                        emitter.fn_type_cache.put(key, tid) catch {};
                    }
                },
                .tuple => |tup| {
                    if (tup.elements.len <= 16) {
                        var key = TupleSigKey{
                            .elem_count = @intCast(tup.elements.len),
                        };
                        for (tup.elements, 0..) |e, i| key.elements[i] = e;
                        emitter.tuple_type_cache.put(key, tid) catch {};
                    }
                },
                else => {},
            }
        }

        return emitter;
    }

    pub fn deinit(self: *CEmitter) void {
        // free allocated mangled name strings
        var it = self.mangled_names.iterator();
        while (it.next()) |entry| {
            self.allocator.free(entry.value_ptr.*);
        }
        self.mangled_names.deinit();
        self.emitted_result_types.deinit();
        self.local_types.deinit();
        self.hoisted_lambdas.deinit(self.allocator);
        for (self.hoisted_spawns.items) |sp| {
            self.allocator.free(sp.arg_types);
        }
        self.hoisted_spawns.deinit(self.allocator);
        self.test_names.deinit(self.allocator);
        self.fn_type_cache.deinit();
        self.tuple_type_cache.deinit();
        self.output.deinit(self.allocator);
    }

    /// get the generated C source as a string slice.
    pub fn getOutput(self: *const CEmitter) []const u8 {
        return self.output.items;
    }

    // ---------------------------------------------------------------
    // module emission
    // ---------------------------------------------------------------

    /// emit a complete C translation unit from a forge module.
    pub fn emitModule(self: *CEmitter, module: *const ast.Module) EmitError!void {
        try self.emitPreamble();

        // collect all modules to emit: imported modules first, then main
        var all_modules: std.ArrayList(*const ast.Module) = .empty;
        defer all_modules.deinit(self.allocator);
        for (self.imported_modules) |*im| {
            all_modules.append(self.allocator, &im.module) catch {};
        }
        all_modules.append(self.allocator, module) catch {};

        // pass 1: struct type definitions (must come before function decls
        // since functions may use struct types in signatures)
        for (all_modules.items) |mod| {
            for (mod.decls) |*decl| {
                switch (decl.kind) {
                    .struct_decl => |*sd| try self.emitStructDef(sd),
                    else => {},
                }
            }
        }

        // pass 1b: emit instantiated generic struct types from the type table.
        // the checker creates concrete types like Pair[Int,String] with resolved
        // field types — we scan for these and emit C typedefs.
        try self.emitInstantiatedStructs(module);

        // pass 2: enum type definitions
        for (all_modules.items) |mod| {
            for (mod.decls) |*decl| {
                switch (decl.kind) {
                    .enum_decl => |*ed| try self.emitEnumDef(ed),
                    else => {},
                }
            }
        }

        // pass 2b: emit result/optional/tuple type typedefs found in the type table
        try self.emitResultTypedefs();
        try self.emitOptionalTypedefs();
        try self.emitTupleTypedefs();
        try self.emitTaskTypedefs();

        // pass 2c: emit C wrapper functions for built-in failable functions.
        // these must come after result/optional typedefs since they return those types.
        try self.emitBuiltinHelpers();

        // pass 2d: type alias typedefs
        for (all_modules.items) |mod| {
            for (mod.decls) |*decl| {
                switch (decl.kind) {
                    .type_alias => |ta| try self.emitTypeAlias(&ta),
                    else => {},
                }
            }
        }

        // pass 3: forward-declare all functions.
        // skip main() from imported modules — each module may have its own
        // main() for standalone use, but only the entry module's main is emitted.
        const imported_count = self.imported_modules.len;
        for (all_modules.items, 0..) |mod, mod_idx| {
            const is_imported = mod_idx < imported_count;
            for (mod.decls) |*decl| {
                switch (decl.kind) {
                    .fn_decl => |*fd| {
                        if (is_imported and std.mem.eql(u8, fd.name, "main")) continue;
                        try self.emitFnForwardDecl(fd);
                    },
                    else => {},
                }
            }
        }

        // pass 3b: forward-declare methods from impl blocks
        for (all_modules.items) |mod| {
            for (mod.decls) |*decl| {
                switch (decl.kind) {
                    .impl_decl => |impl_d| try self.emitImplForwardDecls(&impl_d),
                    else => {},
                }
            }
        }

        // pass 3c: forward-declare instantiated generic functions
        try self.emitInstantiatedFnForwardDecls(module);
        try self.writeByte('\n');

        // pass 3d: top-level global variable declarations and init function
        try self.emitGlobalBindings(all_modules.items);

        // mark position for lambda forward declarations — we'll insert them
        // after all function bodies have been emitted and lambdas discovered
        const lambda_fwd_insert_pos = self.output.items.len;

        // pass 4: function definitions.
        // same filtering as pass 3 — skip main() from imported modules.
        for (all_modules.items, 0..) |mod, mod_idx| {
            const is_imported = mod_idx < imported_count;
            for (mod.decls) |*decl| {
                switch (decl.kind) {
                    .fn_decl => |*fd| {
                        if (is_imported and std.mem.eql(u8, fd.name, "main")) continue;
                        try self.emitFnDef(fd);
                    },
                    else => {},
                }
            }
        }

        // pass 5: method definitions from impl blocks
        for (all_modules.items) |mod| {
            for (mod.decls) |*decl| {
                switch (decl.kind) {
                    .impl_decl => |impl_d| try self.emitImplMethodDefs(&impl_d),
                    else => {},
                }
            }
        }

        // pass 6: instantiated generic function definitions
        try self.emitInstantiatedFnDefs(module);

        // pass 6b: test function definitions (only in test mode)
        if (self.test_mode) {
            // forward-declare the global test state variable
            try self.writeStr("int __current_test_failed = 0;\n\n");
            for (module.decls) |*decl| {
                switch (decl.kind) {
                    .test_decl => |td| try self.emitTestDef(&td),
                    else => {},
                }
            }
        }

        // pass 7: hoisted lambda functions.
        // lambdas are discovered during function body emission — we emit their
        // definitions here at the end, and insert forward declarations at the
        // position we recorded earlier so C can resolve the references.
        if (self.hoisted_lambdas.items.len > 0) {
            // build forward declarations for all lambdas
            var fwd_buf: std.ArrayList(u8) = .empty;
            defer fwd_buf.deinit(self.allocator);

            for (self.hoisted_lambdas.items) |lam| {
                const func = if (self.type_table.get(lam.fn_type_id)) |ty| switch (ty) {
                    .function => |f| f,
                    else => continue,
                } else continue;

                // emit env struct forward declaration for capturing lambdas
                if (lam.captures.len > 0) {
                    const env_decl = std.fmt.allocPrint(self.allocator,
                        "typedef struct {{ ", .{}) catch continue;
                    fwd_buf.appendSlice(self.allocator, env_decl) catch continue;
                    for (lam.captures) |cap| {
                        const field = std.fmt.allocPrint(self.allocator,
                            "{s} {s}; ", .{ self.cTypeStringForId(cap.type_id), cap.name }) catch continue;
                        fwd_buf.appendSlice(self.allocator, field) catch continue;
                    }
                    const env_end = std.fmt.allocPrint(self.allocator,
                        "}} __closure_env_{d};\n", .{lam.index}) catch continue;
                    fwd_buf.appendSlice(self.allocator, env_end) catch continue;
                }

                // all lambdas take void* __env as first param (uniform closure ABI)
                const decl = std.fmt.allocPrint(self.allocator, "static {s} __lambda_{d}(void *__env", .{
                    self.cTypeStringForId(func.return_type),
                    lam.index,
                }) catch continue;
                fwd_buf.appendSlice(self.allocator, decl) catch continue;

                for (lam.lambda.params, 0..) |param, i| {
                    _ = i;
                    fwd_buf.appendSlice(self.allocator, ", ") catch continue;
                    if (param.type_expr) |te| {
                        const ptid = self.resolveTypeExprToId(te);
                        fwd_buf.appendSlice(self.allocator, self.cTypeStringForId(ptid)) catch continue;
                    }
                    fwd_buf.append(self.allocator, ' ') catch continue;
                    fwd_buf.appendSlice(self.allocator, param.name) catch continue;
                }
                fwd_buf.appendSlice(self.allocator, ");\n") catch continue;
            }

            // insert forward declarations at the marked position
            try self.output.insertSlice(self.allocator, lambda_fwd_insert_pos, fwd_buf.items);

            // emit lambda function definitions
            try self.emitHoistedLambdas();
        }

        // pass 7b: hoisted spawn wrapper functions.
        // each spawn site needs a per-spawn struct (with args + value + header)
        // and a wrapper function. both are forward-declared and inserted.
        if (self.hoisted_spawns.items.len > 0) {
            var fwd_buf: std.ArrayList(u8) = .empty;
            defer fwd_buf.deinit(self.allocator);

            for (self.hoisted_spawns.items) |sp| {
                // per-spawn struct typedef
                const struct_begin = std.fmt.allocPrint(self.allocator,
                    "typedef struct {{ forge_task_header_t __header; {s} __value; ",
                    .{self.cTypeStringForId(sp.return_type)}) catch continue;
                fwd_buf.appendSlice(self.allocator, struct_begin) catch continue;
                self.allocator.free(struct_begin);
                for (sp.arg_types, 0..) |arg_tid, i| {
                    const field = std.fmt.allocPrint(self.allocator,
                        "{s} __arg_{d}; ", .{ self.cTypeStringForId(arg_tid), i }) catch continue;
                    fwd_buf.appendSlice(self.allocator, field) catch continue;
                    self.allocator.free(field);
                }
                const struct_end = std.fmt.allocPrint(self.allocator,
                    "}} __spawn_data_{d};\n", .{sp.index}) catch continue;
                fwd_buf.appendSlice(self.allocator, struct_end) catch continue;
                self.allocator.free(struct_end);

                // wrapper forward declaration
                const decl = std.fmt.allocPrint(self.allocator,
                    "static void *__spawn_wrapper_{d}(void *__arg);\n", .{sp.index}) catch continue;
                fwd_buf.appendSlice(self.allocator, decl) catch continue;
                self.allocator.free(decl);
            }

            try self.output.insertSlice(self.allocator, lambda_fwd_insert_pos, fwd_buf.items);

            // emit spawn wrapper definitions
            try self.emitHoistedSpawns();
        }

        // pass 8: test runner main (only in test mode)
        if (self.test_mode) {
            try self.emitTestRunnerMain();
        }
    }

    pub fn emitPreamble(self: *CEmitter) EmitError!void {
        try self.writeStr("// generated by forge compiler — do not edit\n");
        try self.writeStr("#include \"forge_runtime.h\"\n\n");
    }

    // ---------------------------------------------------------------
    // test emission
    // ---------------------------------------------------------------

    /// emit a test body as a static C function: static void __test_N(void) { ... }
    fn emitTestDef(self: *CEmitter, td: *const ast.TestDecl) EmitError!void {
        const index = self.test_names.items.len;
        try self.test_names.append(self.allocator, td.name);

        self.local_types.clearRetainingCapacity();
        self.current_fn_return = .void;

        var buf: [64]u8 = undefined;
        const sig = std.fmt.bufPrint(&buf, "static void __test_{d}(void) {{\n", .{index}) catch return;
        try self.writeStr(sig);

        self.indent_level += 1;
        try self.emitBlock(&td.body);
        self.indent_level -= 1;
        try self.writeStr("}\n\n");
    }

    /// emit a test runner main() that calls each test and prints a summary.
    fn emitTestRunnerMain(self: *CEmitter) EmitError!void {
        try self.writeStr("int main(int __argc, char **__argv) {\n");
        self.indent_level += 1;

        try self.writeIndent();
        try self.writeStr("forge_set_args(__argc, __argv);\n");
        try self.writeIndent();
        try self.writeStr("int __passed = 0, __failed = 0;\n");

        for (self.test_names.items, 0..) |name, i| {
            try self.writeByte('\n');
            try self.writeIndent();
            try self.writeStr("__current_test_failed = 0;\n");
            try self.writeIndent();
            // strip surrounding quotes from test name for display
            const display_name = if (name.len >= 2 and name[0] == '"' and name[name.len - 1] == '"')
                name[1 .. name.len - 1]
            else
                name;
            try self.writeStr("fprintf(stderr, \"test \\\"");
            try self.writeStr(display_name);
            try self.writeStr("\\\"... \");\n");

            try self.writeIndent();
            var idx_buf: [64]u8 = undefined;
            const call = std.fmt.bufPrint(&idx_buf, "__test_{d}();\n", .{i}) catch continue;
            try self.writeStr(call);

            try self.writeIndent();
            try self.writeStr("if (__current_test_failed) { __failed++; fprintf(stderr, \"FAILED\\n\"); }\n");
            try self.writeIndent();
            try self.writeStr("else { __passed++; fprintf(stderr, \"ok\\n\"); }\n");
        }

        try self.writeByte('\n');
        try self.writeIndent();
        try self.writeStr("fprintf(stderr, \"\\n%d passed, %d failed\\n\", __passed, __failed);\n");
        try self.writeIndent();
        try self.writeStr("return __failed > 0 ? 1 : 0;\n");

        self.indent_level -= 1;
        try self.writeStr("}\n");
    }

    // ---------------------------------------------------------------
    // generic monomorphization
    // ---------------------------------------------------------------

    /// mangle a generic type name for C: "Pair[Int,String]" → "Pair_Int_String"
    fn mangleName(allocator: std.mem.Allocator, name: []const u8) EmitError![]const u8 {
        var result: std.ArrayList(u8) = .empty;
        for (name) |c| {
            switch (c) {
                '[', ',' => try result.append(allocator, '_'),
                ']', ' ' => {},
                else => try result.append(allocator, c),
            }
        }
        return result.toOwnedSlice(allocator);
    }

    /// emit C typedefs for all instantiated generic structs.
    /// scans the type table for struct types whose names contain '[' —
    /// these are concrete instantiations created by the checker.
    fn emitInstantiatedStructs(self: *CEmitter, module: *const ast.Module) EmitError!void {
        // find all generic struct base names so we can match instantiations
        for (module.decls) |*decl| {
            switch (decl.kind) {
                .struct_decl => |sd| {
                    if (sd.generic_params.len == 0) continue;
                    // scan type table for instantiations of this struct
                    var it = self.type_table.name_map.iterator();
                    while (it.next()) |entry| {
                        const inst_name = entry.key_ptr.*;
                        // match "Name[" prefix
                        if (!std.mem.startsWith(u8, inst_name, sd.name)) continue;
                        if (inst_name.len <= sd.name.len or inst_name[sd.name.len] != '[') continue;

                        const tid = entry.value_ptr.*;
                        const ty = self.type_table.get(tid) orelse continue;
                        const s = switch (ty) {
                            .@"struct" => |s| s,
                            else => continue,
                        };

                        const mangled = mangleName(self.allocator, inst_name) catch continue;
                        // cache the mangled name for cTypeStringForId lookups
                        self.mangled_names.put(tid, mangled) catch continue;

                        try self.writeStr("typedef struct {\n");
                        self.indent_level += 1;
                        for (s.fields) |field| {
                            try self.writeIndent();
                            try self.writeStr(self.cTypeStringForId(field.type_id));
                            try self.writeByte(' ');
                            try self.writeStr(field.name);
                            try self.writeStr(";\n");
                        }
                        self.indent_level -= 1;
                        try self.writeStr("} ");
                        try self.writeStr(mangled);
                        try self.writeStr(";\n\n");
                    }
                },
                else => {},
            }
        }
    }

    /// forward-declare all instantiated generic functions.
    fn emitInstantiatedFnForwardDecls(self: *CEmitter, module: *const ast.Module) EmitError!void {
        _ = module;
        var it = self.generic_decls.iterator();
        while (it.next()) |entry| {
            const decl = entry.value_ptr.*;
            const fn_d = switch (decl) {
                .function => |f| f,
                else => continue,
            };
            // scan type table for instantiations: "name[..."
            var tit = self.type_table.name_map.iterator();
            while (tit.next()) |te| {
                const inst_name = te.key_ptr.*;
                if (!std.mem.startsWith(u8, inst_name, fn_d.name)) continue;
                if (inst_name.len <= fn_d.name.len or inst_name[fn_d.name.len] != '[') continue;

                const tid = te.value_ptr.*;
                const fn_type = self.type_table.get(tid) orelse continue;
                const func = switch (fn_type) {
                    .function => |f| f,
                    else => continue,
                };

                // emit: return_type fg_name_T(...);
                const mangled = mangleName(self.allocator, inst_name) catch continue;
                try self.writeStr(self.cTypeStringForId(func.return_type));
                try self.writeStr(" fg_");
                try self.writeStr(mangled);
                try self.writeByte('(');
                for (fn_d.params, 0..) |param, pi| {
                    if (pi > 0) try self.writeStr(", ");
                    if (pi < func.param_types.len) {
                        try self.writeStr(self.cTypeStringForId(func.param_types[pi]));
                    }
                    try self.writeByte(' ');
                    try self.writeStr(param.name);
                }
                try self.writeStr(");\n");
            }
        }
    }

    /// emit function bodies for all instantiated generic functions.
    fn emitInstantiatedFnDefs(self: *CEmitter, module: *const ast.Module) EmitError!void {
        _ = module;
        var it = self.generic_decls.iterator();
        while (it.next()) |entry| {
            const decl = entry.value_ptr.*;
            const fn_d = switch (decl) {
                .function => |f| f,
                else => continue,
            };
            // scan type table for instantiations
            var tit = self.type_table.name_map.iterator();
            while (tit.next()) |te| {
                const inst_name = te.key_ptr.*;
                if (!std.mem.startsWith(u8, inst_name, fn_d.name)) continue;
                if (inst_name.len <= fn_d.name.len or inst_name[fn_d.name.len] != '[') continue;

                const tid = te.value_ptr.*;
                const fn_type = self.type_table.get(tid) orelse continue;
                const func = switch (fn_type) {
                    .function => |f| f,
                    else => continue,
                };

                // set up local types for the concrete instantiation
                self.local_types.clearRetainingCapacity();
                self.current_fn_return = func.return_type;
                for (fn_d.params, 0..) |param, pi| {
                    if (pi < func.param_types.len) {
                        self.local_types.put(param.name, func.param_types[pi]) catch return error.OutOfMemory;
                    }
                }

                // emit: return_type fg_name_T(...) { body }
                const mangled = mangleName(self.allocator, inst_name) catch continue;
                try self.writeStr(self.cTypeStringForId(func.return_type));
                try self.writeStr(" fg_");
                try self.writeStr(mangled);
                try self.writeByte('(');
                for (fn_d.params, 0..) |param, pi| {
                    if (pi > 0) try self.writeStr(", ");
                    if (pi < func.param_types.len) {
                        try self.writeStr(self.cTypeStringForId(func.param_types[pi]));
                    }
                    try self.writeByte(' ');
                    try self.writeStr(param.name);
                }
                try self.writeStr(") {\n");

                self.indent_level += 1;
                try self.emitBlock(&fn_d.body);
                self.indent_level -= 1;
                try self.writeStr("}\n\n");
            }
        }
    }

    /// emit C typedefs for all Result[T,E] types found in the type table.
    /// each gets a struct: typedef struct { bool is_ok; T ok; forge_string_t err; } forge_result_T;
    /// deduplicates by name so that multiple TypeIds for the same ok_type
    /// (e.g. two functions both returning Int!) only emit one typedef.
    fn emitResultTypedefs(self: *CEmitter) EmitError!void {
        var emitted_names = std.StringHashMap(void).init(self.allocator);
        defer emitted_names.deinit();

        const items = self.type_table.types.items;
        for (items, 0..) |ty, idx| {
            switch (ty) {
                .result => |r| {
                    const tid = TypeId.fromIndex(@intCast(idx));
                    const ok_c = self.cTypeStringForId(r.ok_type);

                    // build the result type name
                    const name = std.fmt.allocPrint(self.allocator, "forge_result_{s}", .{ok_c}) catch continue;

                    // skip if we already emitted a typedef with this name
                    if (emitted_names.contains(name)) {
                        // still cache the mangled name so cTypeStringForId works for this TypeId
                        if (!self.mangled_names.contains(tid)) {
                            self.mangled_names.put(tid, name) catch {};
                        }
                        continue;
                    }

                    self.mangled_names.put(tid, name) catch continue;
                    emitted_names.put(name, {}) catch continue;

                    try self.writeStr("typedef struct { bool is_ok; ");
                    try self.writeStr(ok_c);
                    try self.writeStr(" ok; forge_string_t err; } ");
                    try self.writeStr(name);
                    try self.writeStr(";\n");
                },
                else => {},
            }
        }
        try self.writeByte('\n');
    }

    /// emit C typedefs for all Optional[T] types found in the type table.
    /// each gets a struct: typedef struct { bool has_value; T value; } forge_optional_T;
    fn emitOptionalTypedefs(self: *CEmitter) EmitError!void {
        var emitted_names = std.StringHashMap(void).init(self.allocator);
        defer emitted_names.deinit();

        const items = self.type_table.types.items;
        for (items, 0..) |ty, idx| {
            switch (ty) {
                .optional => |o| {
                    const tid = TypeId.fromIndex(@intCast(idx));
                    const inner_c = self.cTypeStringForId(o.inner);

                    const name = std.fmt.allocPrint(self.allocator, "forge_optional_{s}", .{inner_c}) catch continue;

                    if (emitted_names.contains(name)) {
                        if (!self.mangled_names.contains(tid)) {
                            self.mangled_names.put(tid, name) catch {};
                        }
                        continue;
                    }

                    self.mangled_names.put(tid, name) catch continue;
                    emitted_names.put(name, {}) catch continue;

                    try self.writeStr("typedef struct { bool has_value; ");
                    try self.writeStr(inner_c);
                    try self.writeStr(" value; } ");
                    try self.writeStr(name);
                    try self.writeStr(";\n");
                },
                else => {},
            }
        }
        try self.writeByte('\n');
    }

    /// emit C typedefs for all tuple types found in the type table.
    /// each gets a struct with fields named _0, _1, etc:
    ///   typedef struct { int64_t _0; forge_string_t _1; } forge_tuple_int64_t_forge_string_t;
    fn emitTupleTypedefs(self: *CEmitter) EmitError!void {
        var emitted_names = std.StringHashMap(void).init(self.allocator);
        defer emitted_names.deinit();

        const items = self.type_table.types.items;
        for (items, 0..) |ty, idx| {
            switch (ty) {
                .tuple => |t| {
                    const tid = TypeId.fromIndex(@intCast(idx));

                    // build the tuple type name from element types
                    var name_parts: std.ArrayList(u8) = .empty;
                    name_parts.appendSlice(self.allocator, "forge_tuple_") catch continue;
                    for (t.elements, 0..) |elem, i| {
                        if (i > 0) name_parts.append(self.allocator, '_') catch continue;
                        name_parts.appendSlice(self.allocator, self.cTypeStringForId(elem)) catch continue;
                    }
                    const name = name_parts.toOwnedSlice(self.allocator) catch continue;

                    if (emitted_names.contains(name)) {
                        if (!self.mangled_names.contains(tid)) {
                            self.mangled_names.put(tid, name) catch {};
                        }
                        continue;
                    }

                    self.mangled_names.put(tid, name) catch continue;
                    emitted_names.put(name, {}) catch continue;

                    try self.writeStr("typedef struct { ");
                    for (t.elements, 0..) |elem, i| {
                        try self.writeStr(self.cTypeStringForId(elem));
                        try self.writeFmt(" _{d}; ", .{i});
                    }
                    try self.writeStr("} ");
                    try self.writeStr(name);
                    try self.writeStr(";\n");
                },
                else => {},
            }
        }
        try self.writeByte('\n');
    }

    /// emit C typedefs for all Task[T] types found in the type table.
    /// task types are represented as void pointers at the C level — the actual
    /// per-spawn struct is emitted at each spawn site. this function just registers
    /// the mangled name so cTypeStringForId returns a consistent type.
    fn emitTaskTypedefs(self: *CEmitter) EmitError!void {
        const items = self.type_table.types.items;
        for (items, 0..) |ty, idx| {
            switch (ty) {
                .task => {
                    const tid = TypeId.fromIndex(@intCast(idx));
                    if (!self.mangled_names.contains(tid)) {
                        // task pointers are void* — the actual struct varies per spawn site.
                        // allocate the string so deinit can free it.
                        const name = self.allocator.dupe(u8, "void*") catch continue;
                        self.mangled_names.put(tid, name) catch {};
                    }
                },
                else => {},
            }
        }
    }

    /// emit all hoisted spawn wrapper function bodies.
    fn emitHoistedSpawns(self: *CEmitter) EmitError!void {
        for (self.hoisted_spawns.items) |sp| {
            // static void *__spawn_wrapper_N(void *__arg) {
            //     __spawn_data_N *__data = (__spawn_data_N *)__arg;
            //     __data->__value = fg_funcname(__data->__arg_0, ...);
            //     return NULL;
            // }
            try self.writeFmt("static void *__spawn_wrapper_{d}(void *__arg) {{\n", .{sp.index});
            try self.writeFmt("    __spawn_data_{d} *__data = (__spawn_data_{d} *)__arg;\n", .{ sp.index, sp.index });
            try self.writeStr("    __data->__value = fg_");
            try self.writeStr(sp.fn_name);
            try self.writeByte('(');
            for (sp.arg_types, 0..) |_, i| {
                if (i > 0) try self.writeStr(", ");
                try self.writeFmt("__data->__arg_{d}", .{i});
            }
            try self.writeStr(");\n    return NULL;\n}\n\n");
        }
    }

    /// emit a spawn expression: allocates a per-spawn struct, copies args,
    /// and creates a thread via pthread_create. the spawned expression must
    /// be a function call (the checker validates this).
    fn emitSpawn(self: *CEmitter, inner: *const ast.Expr) EmitError!void {
        // inner must be a function call
        const call = switch (inner.kind) {
            .call => |c| c,
            else => {
                try self.writeStr("/* spawn: expected function call */");
                return;
            },
        };

        // extract function name
        const fn_name = switch (call.callee.kind) {
            .ident => |n| n,
            else => {
                try self.writeStr("/* spawn: expected named function */");
                return;
            },
        };

        // determine the return type of the spawned function
        const return_type = self.inferExprType(inner);

        // collect argument types
        var arg_types_buf: [16]TypeId = .{.err} ** 16;
        const arg_count = @min(call.args.len, 16);
        for (call.args[0..arg_count], 0..) |arg, i| {
            arg_types_buf[i] = self.inferExprType(arg.value);
        }
        const arg_types = self.allocator.dupe(TypeId, arg_types_buf[0..arg_count]) catch {
            try self.writeStr("/* spawn: out of memory */");
            return;
        };

        const idx = self.spawn_counter;
        self.spawn_counter += 1;

        // record for hoisting
        self.hoisted_spawns.append(self.allocator, .{
            .index = idx,
            .fn_name = fn_name,
            .arg_types = arg_types,
            .return_type = return_type,
            .task_type_name = "",
        }) catch {
            try self.writeStr("/* spawn: out of memory */");
            return;
        };

        // emit a gcc statement expression that allocates the data struct,
        // copies args, and spawns the thread:
        // ({
        //     __spawn_data_N *__sp = malloc(sizeof(__spawn_data_N));
        //     __sp->__arg_0 = arg0; ...
        //     pthread_create(&__sp->__header.thread, NULL, __spawn_wrapper_N, __sp);
        //     (void*)__sp;
        // })
        try self.writeFmt("({{ __spawn_data_{d} *__sp = malloc(sizeof(__spawn_data_{d})); ", .{ idx, idx });
        for (call.args[0..arg_count], 0..) |arg, i| {
            try self.writeFmt("__sp->__arg_{d} = ", .{i});
            try self.emitExpr(arg.value);
            try self.writeStr("; ");
        }
        try self.writeFmt("pthread_create(&__sp->__header.thread, NULL, __spawn_wrapper_{d}, __sp); ", .{idx});
        try self.writeStr("(void*)__sp; })");
    }

    /// emit an await expression: joins the thread and extracts the return value.
    /// uses a gcc statement expression so await works in expression position.
    fn emitAwait(self: *CEmitter, inner: *const ast.Expr) EmitError!void {
        // determine the task type to figure out the spawn data struct
        // the inner expression should be a variable holding a void* (task pointer)
        // we need to figure out which spawn data type to cast to

        // infer the type of the inner expression — should be Task[T]
        const inner_tid = self.inferExprType(inner);
        const return_type = if (self.type_table.get(inner_tid)) |ty| switch (ty) {
            .task => |t| t.inner,
            else => TypeId.err,
        } else TypeId.err;

        const ret_c = self.cTypeStringForId(return_type);

        // ({
        //     void *__aw = task_ptr;
        //     pthread_join(((forge_task_header_t *)__aw)->thread, NULL);
        //     // the value is stored right after the header in all spawn data structs
        //     *(ret_type *)((char *)__aw + sizeof(forge_task_header_t));
        // })
        try self.writeStr("({ void *__aw = ");
        try self.emitExpr(inner);
        try self.writeStr("; pthread_join(((forge_task_header_t *)__aw)->thread, NULL); *(");
        try self.writeStr(ret_c);
        try self.writeStr(" *)((char *)__aw + sizeof(forge_task_header_t)); })");
    }

    /// emit C wrapper functions for built-in failable functions.
    /// must be called after emitResultTypedefs/emitOptionalTypedefs so the
    /// result/optional struct types are already defined.
    /// emit top-level mutable bindings as C global variables and an init function.
    /// globals are declared uninitialized at file scope, then assigned their initial
    /// values in __forge_init_globals() which main() calls on startup.
    fn emitGlobalBindings(self: *CEmitter, modules: []const *const ast.Module) EmitError!void {
        // collect all top-level bindings across modules
        var has_any = false;
        for (modules) |mod| {
            for (mod.decls) |*decl| {
                switch (decl.kind) {
                    .binding => |*b| {
                        has_any = true;
                        // emit C global variable declaration
                        const tid = if (b.type_expr) |te|
                            self.resolveTypeExprToId(te)
                        else
                            self.inferExprType(b.value);
                        try self.emitCType(tid);
                        try self.writeByte(' ');
                        try self.writeStr(b.name);
                        try self.writeStr(";\n");
                        self.local_types.put(b.name, tid) catch return error.OutOfMemory;
                    },
                    else => {},
                }
            }
        }

        if (!has_any) return;

        self.has_globals = true;
        try self.writeByte('\n');

        // emit init function that assigns initial values
        try self.writeStr("void __forge_init_globals(void) {\n");
        self.indent_level += 1;
        for (modules) |mod| {
            for (mod.decls) |*decl| {
                switch (decl.kind) {
                    .binding => |*b| {
                        try self.writeIndent();
                        try self.writeStr(b.name);
                        try self.writeStr(" = ");
                        try self.emitExpr(b.value);
                        try self.writeStr(";\n");
                    },
                    else => {},
                }
            }
        }
        self.indent_level -= 1;
        try self.writeStr("}\n\n");
    }

    fn emitBuiltinHelpers(self: *CEmitter) EmitError!void {
        // parse_int(String) -> Int! — wraps strtoll
        try self.writeStr(
            \\static forge_result_int64_t forge_parse_int(forge_string_t s) {
            \\    char buf[64];
            \\    int64_t slen = s.len < 63 ? s.len : 63;
            \\    memcpy(buf, s.data, (size_t)slen);
            \\    buf[slen] = '\0';
            \\    char *end;
            \\    int64_t val = strtoll(buf, &end, 10);
            \\    if (end == buf || *end != '\0') {
            \\        forge_result_int64_t r; r.is_ok = false;
            \\        r.err = FORGE_STRING_LIT("invalid integer");
            \\        return r;
            \\    }
            \\    forge_result_int64_t r; r.is_ok = true; r.ok = val;
            \\    return r;
            \\}
            \\
        );

        // parse_float(String) -> Float! — wraps strtod
        try self.writeStr(
            \\static forge_result_double forge_parse_float(forge_string_t s) {
            \\    char buf[128];
            \\    int64_t slen = s.len < 127 ? s.len : 127;
            \\    memcpy(buf, s.data, (size_t)slen);
            \\    buf[slen] = '\0';
            \\    char *end;
            \\    double val = strtod(buf, &end);
            \\    if (end == buf || *end != '\0') {
            \\        forge_result_double r; r.is_ok = false;
            \\        r.err = FORGE_STRING_LIT("invalid float");
            \\        return r;
            \\    }
            \\    forge_result_double r; r.is_ok = true; r.ok = val;
            \\    return r;
            \\}
            \\
        );

        // read_file(String) -> String!
        try self.writeStr(
            \\static forge_result_forge_string_t forge_read_file(forge_string_t path) {
            \\    forge_string_t content;
            \\    if (forge_read_file_impl(path.data, path.len, &content)) {
            \\        forge_result_forge_string_t r; r.is_ok = true; r.ok = content;
            \\        return r;
            \\    }
            \\    forge_result_forge_string_t r; r.is_ok = false;
            \\    r.err = FORGE_STRING_LIT("failed to read file");
            \\    return r;
            \\}
            \\
        );

        // write_file(String, String) -> Bool!
        try self.writeStr(
            \\static forge_result_bool forge_write_file(forge_string_t path, forge_string_t data) {
            \\    if (forge_write_file_impl(path.data, path.len, data.data, data.len)) {
            \\        forge_result_bool r; r.is_ok = true; r.ok = true;
            \\        return r;
            \\    }
            \\    forge_result_bool r; r.is_ok = false;
            \\    r.err = FORGE_STRING_LIT("failed to write file");
            \\    return r;
            \\}
            \\
        );

        // env(String) -> String?
        try self.writeStr(
            \\static forge_optional_forge_string_t forge_env(forge_string_t name) {
            \\    forge_string_t val;
            \\    if (forge_env_impl(name.data, name.len, &val)) {
            \\        forge_optional_forge_string_t r; r.has_value = true; r.value = val;
            \\        return r;
            \\    }
            \\    forge_optional_forge_string_t r; r.has_value = false;
            \\    return r;
            \\}
            \\
        );

        // exec_output(String) -> String!
        try self.writeStr(
            \\static forge_result_forge_string_t forge_exec_output(forge_string_t cmd) {
            \\    forge_string_t output;
            \\    if (forge_exec_output_impl(cmd, &output)) {
            \\        forge_result_forge_string_t r; r.is_ok = true; r.ok = output;
            \\        return r;
            \\    }
            \\    forge_result_forge_string_t r; r.is_ok = false;
            \\    r.err = FORGE_STRING_LIT("failed to execute command");
            \\    return r;
            \\}
            \\
        );

        // append_file(String, String) -> Bool!
        try self.writeStr(
            \\static forge_result_bool forge_append_file(forge_string_t path, forge_string_t data) {
            \\    if (forge_append_file_impl(path.data, path.len, data.data, data.len)) {
            \\        forge_result_bool r; r.is_ok = true; r.ok = true;
            \\        return r;
            \\    }
            \\    forge_result_bool r; r.is_ok = false;
            \\    r.err = FORGE_STRING_LIT("failed to append to file");
            \\    return r;
            \\}
            \\
        );

        try self.writeByte('\n');
    }

    /// emit all hoisted lambda function bodies.
    /// lambdas are accumulated during expression emission and emitted as
    /// static top-level C functions after all other function definitions.
    /// all lambdas take `void* __env` as the first parameter (uniform closure ABI).
    fn emitHoistedLambdas(self: *CEmitter) EmitError!void {
        for (self.hoisted_lambdas.items) |lam| {
            // resolve the function type to get param/return types
            const func = if (self.type_table.get(lam.fn_type_id)) |ty| switch (ty) {
                .function => |f| f,
                else => continue,
            } else continue;

            // env struct typedefs are emitted in forward declarations
            // emit: static return_type __lambda_N(void* __env, param_types...) { body }
            try self.writeStr("static ");
            try self.writeStr(self.cTypeStringForId(func.return_type));
            try self.writeFmt(" __lambda_{d}(void *__env", .{lam.index});

            for (lam.lambda.params, 0..) |param, i| {
                _ = i;
                try self.writeStr(", ");
                if (param.type_expr) |te| {
                    const ptid = self.resolveTypeExprToId(te);
                    try self.writeStr(self.cTypeStringForId(ptid));
                } else {
                    try self.writeStr("/* unknown */");
                }
                try self.writeByte(' ');
                try self.writeStr(param.name);
            }
            try self.writeStr(") {\n");

            self.indent_level += 1;

            // for capturing lambdas, unpack the env struct
            if (lam.captures.len > 0) {
                try self.writeIndent();
                try self.writeFmt("__closure_env_{d} *__captures = (__closure_env_{d} *)__env;\n", .{ lam.index, lam.index });
                // create local aliases for captured variables
                for (lam.captures) |cap| {
                    try self.writeIndent();
                    try self.writeStr(self.cTypeStringForId(cap.type_id));
                    try self.writeFmt(" {s} = __captures->{s};\n", .{ cap.name, cap.name });
                }
            }

            switch (lam.lambda.body) {
                .expr => |body_expr| {
                    try self.writeIndent();
                    try self.writeStr("return ");
                    try self.emitExpr(body_expr);
                    try self.writeStr(";\n");
                },
                .block => |*blk| try self.emitBlock(blk),
            }
            self.indent_level -= 1;
            try self.writeStr("}\n\n");
        }
    }

    /// build a generic instantiation name by inferring type args from call arguments.
    /// e.g., for `Pair(1, "hello")` returns "Pair[Int,String]".
    /// returns null if type inference fails.
    fn buildGenericInstName(self: *const CEmitter, base_name: []const u8, args: []const ast.Arg) ?[]const u8 {
        var parts: std.ArrayList(u8) = .empty;
        parts.appendSlice(self.allocator, base_name) catch return null;
        parts.append(self.allocator, '[') catch return null;

        for (args, 0..) |arg, i| {
            if (i > 0) parts.append(self.allocator, ',') catch return null;
            const tid = self.inferExprType(arg.value);
            const type_name = self.type_table.typeName(tid);
            parts.appendSlice(self.allocator, type_name) catch return null;
        }
        parts.append(self.allocator, ']') catch return null;

        const lookup = parts.toOwnedSlice(self.allocator) catch return null;

        // look up in type table — the checker must have registered this name
        // for it to be valid. return the type table key for stable lifetime.
        if (self.type_table.name_map.getKeyPtr(lookup)) |key_ptr| {
            return key_ptr.*;
        }
        return null;
    }

    // ---------------------------------------------------------------
    // struct definitions
    // ---------------------------------------------------------------

    fn emitStructDef(self: *CEmitter, sd: *const ast.StructDecl) EmitError!void {
        // skip generic (uninstantiated) structs
        if (sd.generic_params.len > 0) return;

        try self.writeStr("typedef struct {\n");
        self.indent_level += 1;
        for (sd.fields) |field| {
            try self.writeIndent();
            try self.emitCType(self.resolveFieldType(sd.name, field.name));
            try self.writeByte(' ');
            try self.writeStr(field.name);
            try self.writeStr(";\n");
        }
        self.indent_level -= 1;
        try self.writeStr("} ");
        try self.writeStr(sd.name);
        try self.writeStr(";\n\n");
    }

    /// look up the type of a struct field from the type table.
    fn resolveFieldType(self: *const CEmitter, struct_name: []const u8, field_name: []const u8) TypeId {
        const type_id = self.type_table.lookup(struct_name) orelse return .err;
        const ty = self.type_table.get(type_id) orelse return .err;
        const fields = switch (ty) {
            .@"struct" => |s| s.fields,
            else => return .err,
        };
        for (fields) |field| {
            if (std.mem.eql(u8, field.name, field_name)) return field.type_id;
        }
        return .err;
    }

    // ---------------------------------------------------------------
    // type alias typedefs
    // ---------------------------------------------------------------

    fn emitTypeAlias(self: *CEmitter, ta: *const ast.TypeAlias) EmitError!void {
        // skip generic type aliases (not supported yet, E233)
        if (ta.generic_params.len > 0) return;

        const target_tid = self.resolveTypeExprToId(ta.type_expr);
        if (target_tid.isErr()) return;

        try self.writeStr("typedef ");
        try self.emitCType(target_tid);
        try self.writeByte(' ');
        try self.writeStr(ta.name);
        try self.writeStr(";\n");
    }

    // ---------------------------------------------------------------
    // enum definitions
    // ---------------------------------------------------------------

    fn emitEnumDef(self: *CEmitter, ed: *const ast.EnumDecl) EmitError!void {
        // skip generic (uninstantiated) enums
        if (ed.generic_params.len > 0) return;

        // tag enum
        try self.writeStr("typedef enum {\n");
        self.indent_level += 1;
        for (ed.variants) |variant| {
            try self.writeIndent();
            try self.writeStr(ed.name);
            try self.writeStr("_TAG_");
            try self.writeStr(variant.name);
            try self.writeStr(",\n");
        }
        self.indent_level -= 1;
        try self.writeStr("} ");
        try self.writeStr(ed.name);
        try self.writeStr("_Tag;\n\n");

        // tagged union struct
        try self.writeStr("typedef struct {\n");
        self.indent_level += 1;
        try self.writeIndent();
        try self.writeStr(ed.name);
        try self.writeStr("_Tag tag;\n");

        // union of variant data (only if any variant has fields)
        var has_data = false;
        for (ed.variants) |variant| {
            if (variant.fields.len > 0) {
                has_data = true;
                break;
            }
        }

        if (has_data) {
            try self.writeIndent();
            try self.writeStr("union {\n");
            self.indent_level += 1;
            for (ed.variants) |variant| {
                if (variant.fields.len > 0) {
                    try self.writeIndent();
                    try self.writeStr("struct {\n");
                    self.indent_level += 1;
                    for (variant.fields, 0..) |_, i| {
                        try self.writeIndent();
                        const field_tid = self.resolveEnumFieldType(ed.name, variant.name, i);
                        try self.emitCType(field_tid);
                        try self.writeFmt(" _{d};\n", .{i});
                    }
                    self.indent_level -= 1;
                    try self.writeIndent();
                    try self.writeStr("} ");
                    try self.writeStr(variant.name);
                    try self.writeStr(";\n");
                }
            }
            self.indent_level -= 1;
            try self.writeIndent();
            try self.writeStr("} data;\n");
        }

        self.indent_level -= 1;
        try self.writeStr("} ");
        try self.writeStr(ed.name);
        try self.writeStr(";\n\n");
    }

    /// resolve the type of an enum variant field from the type table.
    fn resolveEnumFieldType(self: *const CEmitter, enum_name: []const u8, variant_name: []const u8, field_idx: usize) TypeId {
        const type_id = self.type_table.lookup(enum_name) orelse return .err;
        const ty = self.type_table.get(type_id) orelse return .err;
        const variants = switch (ty) {
            .@"enum" => |e| e.variants,
            else => return .err,
        };
        for (variants) |variant| {
            if (std.mem.eql(u8, variant.name, variant_name)) {
                if (field_idx < variant.fields.len) return variant.fields[field_idx];
                return .err;
            }
        }
        return .err;
    }

    // ---------------------------------------------------------------
    // function declarations
    // ---------------------------------------------------------------

    fn emitFnForwardDecl(self: *CEmitter, fd: *const ast.FnDecl) EmitError!void {
        // skip generic (uninstantiated) functions
        if (fd.generic_params.len > 0) return;

        // main is special — accepts argc/argv for command-line arguments.
        // in test mode, skip main entirely (test runner provides its own).
        if (std.mem.eql(u8, fd.name, "main")) {
            if (self.test_mode) return;
            try self.writeStr("int main(int __argc, char **__argv);\n");
            return;
        }

        try self.emitReturnType(fd.return_type);
        try self.writeByte(' ');
        try self.emitUserFnName(fd.name);
        try self.emitParamList(fd.params);
        try self.writeStr(";\n");
    }

    // ---------------------------------------------------------------
    // impl block emission
    // ---------------------------------------------------------------

    /// extract the concrete type name from an impl decl. the parser's
    /// naming is inverted: `impl Display for Point:` has target=Display,
    /// interface=Point. `impl Point:` has target=Point, interface=null.
    fn implTypeName(impl_d: *const ast.ImplDecl) ?[]const u8 {
        if (impl_d.interface) |iface_type_expr| {
            // `impl X for Y:` — Y is the concrete type
            return switch (iface_type_expr.kind) {
                .named => |n| n,
                else => null,
            };
        } else {
            // `impl X:` — X is the concrete type
            return switch (impl_d.target.kind) {
                .named => |n| n,
                else => null,
            };
        }
    }

    /// emit forward declarations for all methods in an impl block.
    fn emitImplForwardDecls(self: *CEmitter, impl_d: *const ast.ImplDecl) EmitError!void {
        const type_name = implTypeName(impl_d) orelse return;
        for (impl_d.methods) |method| {
            try self.emitMethodForwardDecl(type_name, &method.decl);
        }
    }

    /// emit a single method forward declaration.
    /// `fn distance_from_origin() -> Int` on Point becomes:
    /// `int64_t Point_distance_from_origin(Point self);`
    fn emitMethodForwardDecl(self: *CEmitter, type_name: []const u8, fd: *const ast.FnDecl) EmitError!void {
        if (fd.generic_params.len > 0) return;
        try self.emitReturnType(fd.return_type);
        try self.writeByte(' ');
        try self.writeStr(type_name);
        try self.writeByte('_');
        try self.writeStr(fd.name);
        // emit param list with `self` as first parameter
        try self.writeByte('(');
        try self.writeStr(type_name);
        try self.writeStr(" self");
        for (fd.params) |param| {
            try self.writeStr(", ");
            if (param.type_expr) |te| {
                if (te.kind == .fn_type) {
                    try self.emitFnPtrParam(te.kind.fn_type, param.name);
                } else {
                    try self.emitTypeExpr(te);
                    try self.writeByte(' ');
                    try self.writeStr(param.name);
                }
            } else {
                try self.writeStr("/* unknown */ ");
                try self.writeStr(param.name);
            }
        }
        try self.writeByte(')');
        try self.writeStr(";\n");
    }

    /// emit method definitions for all methods in an impl block.
    fn emitImplMethodDefs(self: *CEmitter, impl_d: *const ast.ImplDecl) EmitError!void {
        const type_name = implTypeName(impl_d) orelse return;
        for (impl_d.methods) |method| {
            try self.emitMethodDef(type_name, &method.decl);
        }
    }

    /// emit a single method definition as a C function.
    /// `self` becomes the first explicit parameter of the struct type.
    fn emitMethodDef(self: *CEmitter, type_name: []const u8, fd: *const ast.FnDecl) EmitError!void {
        if (fd.generic_params.len > 0) return;

        // clear local type tracking
        self.local_types.clearRetainingCapacity();

        // track return type for match tail-position returns
        self.current_fn_return = if (fd.return_type) |rt|
            self.resolveTypeExprToId(rt)
        else
            .void;

        // register `self` as local with the struct's type
        const self_tid = self.type_table.lookup(type_name) orelse .err;
        self.local_types.put("self", self_tid) catch return error.OutOfMemory;

        // register method parameters
        for (fd.params) |param| {
            if (param.type_expr) |te| {
                const tid = self.resolveTypeExprToId(te);
                self.local_types.put(param.name, tid) catch return error.OutOfMemory;
            }
        }

        // emit signature
        try self.emitReturnType(fd.return_type);
        try self.writeByte(' ');
        try self.writeStr(type_name);
        try self.writeByte('_');
        try self.writeStr(fd.name);
        try self.writeByte('(');
        try self.writeStr(type_name);
        try self.writeStr(" self");
        for (fd.params) |param| {
            try self.writeStr(", ");
            if (param.type_expr) |te| {
                if (te.kind == .fn_type) {
                    try self.emitFnPtrParam(te.kind.fn_type, param.name);
                } else {
                    try self.emitTypeExpr(te);
                    try self.writeByte(' ');
                    try self.writeStr(param.name);
                }
            } else {
                try self.writeStr("/* unknown */ ");
                try self.writeStr(param.name);
            }
        }
        try self.writeStr(") {\n");

        self.indent_level += 1;
        try self.emitBlock(&fd.body);
        self.indent_level -= 1;
        try self.writeStr("}\n\n");
    }

    fn emitFnDef(self: *CEmitter, fd: *const ast.FnDecl) EmitError!void {
        // skip generic functions
        if (fd.generic_params.len > 0) return;

        // in test mode, skip the user's main() — the test runner provides its own
        if (self.test_mode and std.mem.eql(u8, fd.name, "main")) return;

        // clear local type tracking for each function
        self.local_types.clearRetainingCapacity();

        // track the function's return type so match arms can emit `return`
        self.current_fn_return = if (fd.return_type) |rt|
            self.resolveTypeExprToId(rt)
        else
            .void;

        // register parameters as local variables
        for (fd.params) |param| {
            if (param.type_expr) |te| {
                const tid = self.resolveTypeExprToId(te);
                self.local_types.put(param.name, tid) catch return error.OutOfMemory;
            }
        }

        if (std.mem.eql(u8, fd.name, "main")) {
            try self.writeStr("int main(int __argc, char **__argv) {\n");
            self.indent_level += 1;
            try self.writeIndent();
            try self.writeStr("forge_set_args(__argc, __argv);\n");
            if (self.has_globals) {
                try self.writeIndent();
                try self.writeStr("__forge_init_globals();\n");
            }
            self.indent_level -= 1;
        } else {
            try self.emitReturnType(fd.return_type);
            try self.writeByte(' ');
            try self.emitUserFnName(fd.name);
            try self.emitParamList(fd.params);
            try self.writeStr(" {\n");
        }

        self.indent_level += 1;
        try self.emitBlock(&fd.body);

        // if main, add return 0
        if (std.mem.eql(u8, fd.name, "main")) {
            try self.writeIndent();
            try self.writeStr("return 0;\n");
        }

        self.indent_level -= 1;
        try self.writeStr("}\n\n");
    }

    /// emit a user-defined function name, prefixed to avoid collisions
    /// with C standard library names (abs, div, etc).
    fn emitUserFnName(self: *CEmitter, name: []const u8) EmitError!void {
        try self.writeStr("fg_");
        try self.writeStr(name);
    }

    fn emitReturnType(self: *CEmitter, return_type: ?*const ast.TypeExpr) EmitError!void {
        if (return_type) |rt| {
            try self.emitTypeExpr(rt);
        } else {
            try self.writeStr("void");
        }
    }

    fn emitParamList(self: *CEmitter, params: []const ast.Param) EmitError!void {
        try self.writeByte('(');
        if (params.len == 0) {
            try self.writeStr("void");
        } else {
            for (params, 0..) |param, i| {
                if (i > 0) try self.writeStr(", ");
                if (param.type_expr) |te| {
                    // function pointer params need special C syntax: ret (*name)(params)
                    if (te.kind == .fn_type) {
                        try self.emitFnPtrParam(te.kind.fn_type, param.name);
                    } else {
                        try self.emitTypeExpr(te);
                        try self.writeByte(' ');
                        try self.writeStr(param.name);
                    }
                } else {
                    try self.writeStr("/* unknown */ ");
                    try self.writeStr(param.name);
                }
            }
        }
        try self.writeByte(')');
    }

    /// emit a closure-typed binding from a TypeId: `forge_closure_t name`
    fn emitFnPtrBindingFromTypeId(self: *CEmitter, _: TypeId, name: []const u8) EmitError!void {
        try self.writeStr("forge_closure_t ");
        try self.writeStr(name);
    }

    /// emit a closure-typed parameter: `forge_closure_t name`
    fn emitFnPtrParam(self: *CEmitter, _: ast.FnType, name: []const u8) EmitError!void {
        try self.writeStr("forge_closure_t ");
        try self.writeStr(name);
    }

    // ---------------------------------------------------------------
    // block and statements
    // ---------------------------------------------------------------

    fn emitBlock(self: *CEmitter, block: *const ast.Block) EmitError!void {
        for (block.stmts) |*stmt| {
            try self.emitStmt(stmt);
        }
    }

    fn emitStmt(self: *CEmitter, stmt: *const ast.Stmt) EmitError!void {
        switch (stmt.kind) {
            .binding => |b| try self.emitBinding(&b),
            .assignment => |a| try self.emitAssignment(&a),
            .if_stmt => |ifs| try self.emitIfStmt(&ifs),
            .while_stmt => |ws| try self.emitWhileStmt(&ws),
            .return_stmt => |rs| try self.emitReturnStmt(&rs),
            .break_stmt => {
                try self.writeIndent();
                try self.writeStr("break;\n");
            },
            .continue_stmt => {
                try self.writeIndent();
                try self.writeStr("continue;\n");
            },
            .expr_stmt => |expr| {
                // try propagation as statement: foo()! → hoist + check + early return
                if (expr.kind == .try_expr) {
                    try self.emitTryExprStmt(expr.kind.try_expr);
                } else {
                    try self.writeIndent();
                    try self.emitExpr(expr);
                    try self.writeStr(";\n");
                }
            },
            .match_stmt => |m| try self.emitMatchStmt(&m),
            .for_stmt => |fs| try self.emitForStmt(&fs),
            .fail_stmt => |f| try self.emitFailStmt(&f),
        }
    }

    fn emitBinding(self: *CEmitter, b: *const ast.Binding) EmitError!void {
        // determine the type and track it for later lookups.
        // for lambda values, inferExprType may return .err (it doesn't
        // temporarily register params), so fall back to findLambdaType.
        var tid = if (b.type_expr) |te|
            self.resolveTypeExprToId(te)
        else
            self.inferExprType(b.value);
        if (tid.isErr() and b.value.kind == .lambda) {
            tid = self.findLambdaType(b.value.kind.lambda);
        }
        self.local_types.put(b.name, tid) catch return error.OutOfMemory;

        // try propagation: x := foo()! → hoist result to temp, check, early return
        if (b.value.kind == .try_expr) {
            try self.emitTryBinding(b, tid);
            return;
        }

        // closure bindings: forge_closure_t name = value
        if (b.type_expr) |te| {
            if (te.kind == .fn_type) {
                try self.writeIndent();
                try self.emitFnPtrParam(te.kind.fn_type, b.name);
                try self.writeStr(" = ");
                try self.emitExpr(b.value);
                try self.writeStr(";\n");
                return;
            }
        } else if (!tid.isErr()) {
            if (self.type_table.get(tid)) |ty| {
                if (ty == .function) {
                    try self.writeIndent();
                    try self.emitFnPtrBindingFromTypeId(tid, b.name);
                    try self.writeStr(" = ");
                    try self.emitExpr(b.value);
                    try self.writeStr(";\n");
                    return;
                }
            }
        }

        try self.writeIndent();
        if (b.type_expr) |te| {
            try self.emitTypeExpr(te);
        } else if (!tid.isErr()) {
            try self.emitCType(tid);
        } else {
            // fallback: infer from expression structure
            try self.emitTypeForExpr(b.value);
        }
        try self.writeByte(' ');
        try self.writeStr(b.name);
        try self.writeStr(" = ");
        try self.emitExpr(b.value);
        try self.writeStr(";\n");
    }

    /// emit a try-propagating binding: `x := foo()!`
    ///
    /// generates:
    ///   forge_result_T __try_N = fg_foo();
    ///   if (!__try_N.is_ok) return (enclosing_result){ .is_ok = false, .err = __try_N.err };
    ///   T x = __try_N.ok;
    fn emitTryBinding(self: *CEmitter, b: *const ast.Binding, ok_tid: TypeId) EmitError!void {
        const inner = b.value.kind.try_expr;
        // infer the result type of the inner expression (before unwrapping)
        const result_tid = self.inferExprType(inner);
        const result_c = self.cTypeStringForId(result_tid);
        const ok_c = self.cTypeStringForId(ok_tid);

        // unique temp name
        var name_buf: [32]u8 = undefined;
        const try_name = std.fmt.bufPrint(&name_buf, "__try_{d}", .{self.try_counter}) catch return;
        self.try_counter += 1;

        // emit: result_type __try_N = inner_expr;
        try self.writeIndent();
        try self.writeStr(result_c);
        try self.writeByte(' ');
        try self.writeStr(try_name);
        try self.writeStr(" = ");
        try self.emitExpr(inner);
        try self.writeStr(";\n");

        // emit: if (!__try_N.is_ok) return (enclosing_result){ .is_ok = false, .err = __try_N.err };
        try self.writeIndent();
        try self.writeStr("if (!");
        try self.writeStr(try_name);
        try self.writeStr(".is_ok) return (");
        try self.writeStr(self.cTypeStringForId(self.current_fn_return));
        try self.writeStr("){ .is_ok = false, .err = ");
        try self.writeStr(try_name);
        try self.writeStr(".err };\n");

        // emit: T x = __try_N.ok;
        try self.writeIndent();
        try self.writeStr(ok_c);
        try self.writeByte(' ');
        try self.writeStr(b.name);
        try self.writeStr(" = ");
        try self.writeStr(try_name);
        try self.writeStr(".ok;\n");
    }

    /// emit a try expression used as a statement: `foo()!`
    /// generates the temp + check + early return, but discards the ok value.
    fn emitTryExprStmt(self: *CEmitter, inner: *const ast.Expr) EmitError!void {
        const result_tid = self.inferExprType(inner);
        const result_c = self.cTypeStringForId(result_tid);

        var name_buf: [32]u8 = undefined;
        const try_name = std.fmt.bufPrint(&name_buf, "__try_{d}", .{self.try_counter}) catch return;
        self.try_counter += 1;

        // emit: result_type __try_N = inner_expr;
        try self.writeIndent();
        try self.writeStr(result_c);
        try self.writeByte(' ');
        try self.writeStr(try_name);
        try self.writeStr(" = ");
        try self.emitExpr(inner);
        try self.writeStr(";\n");

        // emit: if (!__try_N.is_ok) return (enclosing_result){ .is_ok = false, .err = __try_N.err };
        try self.writeIndent();
        try self.writeStr("if (!");
        try self.writeStr(try_name);
        try self.writeStr(".is_ok) return (");
        try self.writeStr(self.cTypeStringForId(self.current_fn_return));
        try self.writeStr("){ .is_ok = false, .err = ");
        try self.writeStr(try_name);
        try self.writeStr(".err };\n");
    }

    /// emit a C type string for an expression by inferring its TypeId.
    /// replaces the old emitInferredType — delegates to inferExprType
    /// which already handles all expression kinds.
    fn emitTypeForExpr(self: *CEmitter, expr: *const ast.Expr) EmitError!void {
        const tid = self.inferExprType(expr);
        if (!tid.isErr()) {
            try self.writeStr(self.cTypeStringForId(tid));
        } else {
            try self.writeStr("/* unknown */");
        }
    }

    fn emitAssignment(self: *CEmitter, a: *const ast.Assignment) EmitError!void {
        try self.writeIndent();
        try self.emitExpr(a.target);
        switch (a.op) {
            .assign => try self.writeStr(" = "),
            .add => try self.writeStr(" += "),
            .sub => try self.writeStr(" -= "),
            .mul => try self.writeStr(" *= "),
            .div => try self.writeStr(" /= "),
        }
        try self.emitExpr(a.value);
        try self.writeStr(";\n");
    }

    fn emitIfStmt(self: *CEmitter, ifs: *const ast.IfStmt) EmitError!void {
        try self.writeIndent();
        try self.writeStr("if (");
        try self.emitExpr(ifs.condition);
        try self.writeStr(") {\n");
        self.indent_level += 1;
        try self.emitBlock(&ifs.then_block);
        self.indent_level -= 1;

        for (ifs.elif_branches) |*elif| {
            try self.writeIndent();
            try self.writeStr("} else if (");
            try self.emitExpr(elif.condition);
            try self.writeStr(") {\n");
            self.indent_level += 1;
            try self.emitBlock(&elif.block);
            self.indent_level -= 1;
        }

        if (ifs.else_block) |*eb| {
            try self.writeIndent();
            try self.writeStr("} else {\n");
            self.indent_level += 1;
            try self.emitBlock(eb);
            self.indent_level -= 1;
        }

        try self.writeIndent();
        try self.writeStr("}\n");
    }

    fn emitWhileStmt(self: *CEmitter, ws: *const ast.WhileStmt) EmitError!void {
        try self.writeIndent();
        try self.writeStr("while (");
        try self.emitExpr(ws.condition);
        try self.writeStr(") {\n");
        self.indent_level += 1;
        try self.emitBlock(&ws.body);
        self.indent_level -= 1;
        try self.writeIndent();
        try self.writeStr("}\n");
    }

    fn emitReturnStmt(self: *CEmitter, rs: *const ast.ReturnStmt) EmitError!void {
        try self.writeIndent();
        if (rs.value) |val| {
            if (self.isResultType(self.current_fn_return)) {
                // check if the expression already returns the same result type —
                // if so, return it directly without wrapping
                const val_type = self.inferExprType(val);
                if (val_type == self.current_fn_return) {
                    try self.writeStr("return ");
                    try self.emitExpr(val);
                    try self.writeStr(";\n");
                } else {
                    // result-returning function: wrap in ok result
                    const ret_c = self.cTypeStringForId(self.current_fn_return);
                    try self.writeStr("return (");
                    try self.writeStr(ret_c);
                    try self.writeStr("){ .is_ok = true, .ok = ");
                    try self.emitExpr(val);
                    try self.writeStr(" };\n");
                }
            } else if (self.isOptionalType(self.current_fn_return)) {
                // optional-returning function: check if returning None or a value
                if (val.kind == .none_lit) {
                    const ret_c = self.cTypeStringForId(self.current_fn_return);
                    try self.writeStr("return (");
                    try self.writeStr(ret_c);
                    try self.writeStr("){ .has_value = false };\n");
                } else {
                    const ret_c = self.cTypeStringForId(self.current_fn_return);
                    try self.writeStr("return (");
                    try self.writeStr(ret_c);
                    try self.writeStr("){ .has_value = true, .value = ");
                    try self.emitExpr(val);
                    try self.writeStr(" };\n");
                }
            } else {
                try self.writeStr("return ");
                try self.emitExpr(val);
                try self.writeStr(";\n");
            }
        } else {
            try self.writeStr("return;\n");
        }
    }

    fn emitFailStmt(self: *CEmitter, f: *const ast.FailStmt) EmitError!void {
        // fail "msg" → return (result_type){ .is_ok = false, .err = FORGE_STRING_LIT("msg") }
        try self.writeIndent();
        const ret_c = self.cTypeStringForId(self.current_fn_return);
        try self.writeStr("return (");
        try self.writeStr(ret_c);
        try self.writeStr("){ .is_ok = false, .err = ");
        try self.emitExpr(f.value);
        try self.writeStr(" };\n");
    }

    /// find the function TypeId for a lambda by matching its parameter types
    /// and return type against function types in the type table.
    fn findLambdaType(self: *CEmitter, lam: ast.Lambda) TypeId {
        // resolve param types from AST
        var param_ids: [16]TypeId = undefined;
        if (lam.params.len > param_ids.len) return .err;
        for (lam.params, 0..) |param, i| {
            if (param.type_expr) |te| {
                param_ids[i] = self.resolveTypeExprToId(te);
                if (param_ids[i].isErr()) return .err;
            } else {
                return .err;
            }
        }
        const resolved = param_ids[0..lam.params.len];

        // temporarily register lambda params as locals to infer body type
        for (lam.params, resolved) |param, tid| {
            self.local_types.put(param.name, tid) catch return .err;
        }
        const ret_type: TypeId = switch (lam.body) {
            .expr => |body_expr| self.inferExprType(body_expr),
            .block => .void,
        };
        // clean up temp locals (they'll be shadowed by actual fn locals)
        for (lam.params) |param| {
            _ = self.local_types.fetchRemove(param.name);
        }

        // scan the type table for a function type with matching params and return
        const items = self.type_table.types.items;
        for (items, 0..) |ty, idx| {
            switch (ty) {
                .function => |func| {
                    if (func.return_type == ret_type and func.param_types.len == resolved.len) {
                        var match = true;
                        for (func.param_types, resolved) |a, b| {
                            if (a != b) {
                                match = false;
                                break;
                            }
                        }
                        if (match) return TypeId.fromIndex(@intCast(idx));
                    }
                },
                else => {},
            }
        }
        return .err;
    }

    /// detect variables captured from the enclosing scope by a lambda.
    /// walks the lambda body's expression tree and finds identifiers that
    /// are in local_types (enclosing function locals) but not lambda params.
    fn detectCaptures(self: *const CEmitter, lam: ast.Lambda) EmitError![]const CapturedVar {
        // build a set of lambda parameter names
        var param_names = std.StringHashMap(void).init(self.allocator);
        defer param_names.deinit();
        for (lam.params) |param| {
            param_names.put(param.name, {}) catch return error.OutOfMemory;
        }

        // collect all identifiers used in the lambda body
        var idents = std.StringHashMap(void).init(self.allocator);
        defer idents.deinit();
        switch (lam.body) {
            .expr => |body_expr| self.collectIdents(body_expr, &idents),
            .block => |blk| {
                for (blk.stmts) |*stmt| {
                    self.collectStmtIdents(stmt, &idents);
                }
            },
        }

        // filter: keep identifiers that are in local_types but not in params
        // and not top-level functions (module_scope)
        var captures: std.ArrayList(CapturedVar) = .empty;
        var iter = idents.iterator();
        while (iter.next()) |entry| {
            const name = entry.key_ptr.*;
            if (param_names.contains(name)) continue;
            if (self.module_scope.lookup(name) != null) continue;
            if (self.local_types.get(name)) |tid| {
                if (!tid.isErr()) {
                    captures.append(self.allocator, .{
                        .name = name,
                        .type_id = tid,
                    }) catch return error.OutOfMemory;
                }
            }
        }
        return captures.toOwnedSlice(self.allocator) catch return error.OutOfMemory;
    }

    /// recursively collect all identifier names from an expression tree.
    fn collectIdents(self: *const CEmitter, expr: *const ast.Expr, out: *std.StringHashMap(void)) void {
        switch (expr.kind) {
            .ident => |name| {
                out.put(name, {}) catch {};
            },
            .binary => |bin| {
                self.collectIdents(bin.left, out);
                self.collectIdents(bin.right, out);
            },
            .unary => |un| {
                self.collectIdents(un.operand, out);
            },
            .call => |call| {
                self.collectIdents(call.callee, out);
                for (call.args) |arg| {
                    self.collectIdents(arg.value, out);
                }
            },
            .method_call => |mc| {
                self.collectIdents(mc.receiver, out);
                for (mc.args) |arg| {
                    self.collectIdents(arg.value, out);
                }
            },
            .field_access => |fa| {
                self.collectIdents(fa.object, out);
            },
            .index => |idx| {
                self.collectIdents(idx.object, out);
                self.collectIdents(idx.index, out);
            },
            .grouped => |inner| {
                self.collectIdents(inner, out);
            },
            .if_expr => |if_e| {
                self.collectIdents(if_e.condition, out);
                self.collectIdents(if_e.then_expr, out);
                for (if_e.elif_branches) |elif| {
                    self.collectIdents(elif.condition, out);
                    self.collectIdents(elif.expr, out);
                }
                self.collectIdents(if_e.else_expr, out);
            },
            .string_interp => |interp| {
                for (interp.parts) |part| {
                    switch (part) {
                        .expr => |e| self.collectIdents(e, out),
                        .literal => {},
                    }
                }
            },
            .lambda => |inner_lam| {
                // don't descend into nested lambdas — they have their own scope
                _ = inner_lam;
            },
            .list => |elems| {
                for (elems) |e| self.collectIdents(e, out);
            },
            .map => |entries| {
                for (entries) |entry| {
                    self.collectIdents(entry.key, out);
                    self.collectIdents(entry.value, out);
                }
            },
            .set => |elems| {
                for (elems) |e| self.collectIdents(e, out);
            },
            .tuple => |elems| {
                for (elems) |e| self.collectIdents(e, out);
            },
            .try_expr => |inner| {
                self.collectIdents(inner, out);
            },
            .unwrap => |inner| {
                self.collectIdents(inner, out);
            },
            .match_expr => |m| {
                self.collectIdents(m.subject, out);
                for (m.arms) |arm| {
                    switch (arm.body) {
                        .expr => |e| self.collectIdents(e, out),
                        .block => |blk| {
                            for (blk.stmts) |*stmt| self.collectStmtIdents(stmt, out);
                        },
                    }
                }
            },
            else => {},
        }
    }

    /// collect identifiers from a statement.
    fn collectStmtIdents(self: *const CEmitter, stmt: *const ast.Stmt, out: *std.StringHashMap(void)) void {
        switch (stmt.kind) {
            .binding => |b| {
                self.collectIdents(b.value, out);
            },
            .assignment => |a| {
                self.collectIdents(a.target, out);
                self.collectIdents(a.value, out);
            },
            .expr_stmt => |e| {
                self.collectIdents(e, out);
            },
            .return_stmt => |r| {
                if (r.value) |v| self.collectIdents(v, out);
            },
            .if_stmt => |if_s| {
                self.collectIdents(if_s.condition, out);
                for (if_s.then_block.stmts) |*s| self.collectStmtIdents(s, out);
                for (if_s.elif_branches) |elif| {
                    self.collectIdents(elif.condition, out);
                    for (elif.block.stmts) |*s| self.collectStmtIdents(s, out);
                }
                if (if_s.else_block) |eb| {
                    for (eb.stmts) |*s| self.collectStmtIdents(s, out);
                }
            },
            .while_stmt => |ws| {
                self.collectIdents(ws.condition, out);
                for (ws.body.stmts) |*s| self.collectStmtIdents(s, out);
            },
            .for_stmt => |fs| {
                self.collectIdents(fs.iterable, out);
                for (fs.body.stmts) |*s| self.collectStmtIdents(s, out);
            },
            .match_stmt => |m| {
                self.collectIdents(m.subject, out);
                for (m.arms) |arm| {
                    switch (arm.body) {
                        .expr => |e| self.collectIdents(e, out),
                        .block => |blk| {
                            for (blk.stmts) |*s| self.collectStmtIdents(s, out);
                        },
                    }
                }
            },
            else => {},
        }
    }

    /// check if a TypeId represents a Result type.
    fn isResultType(self: *const CEmitter, tid: TypeId) bool {
        if (tid.isErr()) return false;
        const ty = self.type_table.get(tid) orelse return false;
        return ty == .result;
    }

    /// check if a TypeId represents an Optional type.
    fn isOptionalType(self: *const CEmitter, tid: TypeId) bool {
        if (tid.isErr()) return false;
        const ty = self.type_table.get(tid) orelse return false;
        return ty == .optional;
    }

    fn emitForStmt(self: *CEmitter, fs: *const ast.ForStmt) EmitError!void {
        const iter_tid = self.inferExprType(fs.iterable);
        const collection_type = if (!iter_tid.isErr()) self.type_table.get(iter_tid) else null;

        // determine the index variable name — user-provided or generated
        var idx_buf: [32]u8 = undefined;
        const idx_name = if (fs.index) |idx| idx else blk: {
            const name = std.fmt.bufPrint(&idx_buf, "__idx_{d}", .{self.for_counter}) catch return;
            self.for_counter += 1;
            break :blk name;
        };

        if (collection_type) |ct| {
            switch (ct) {
                .list => |l| {
                    // for item in list:  →  for (i = 0; i < list.len; i++) { T item = ...; }
                    const elem_c = self.cTypeStringForId(l.element);
                    try self.writeIndent();
                    try self.writeFmt("for (int64_t {s} = 0; {s} < ", .{ idx_name, idx_name });
                    try self.emitExpr(fs.iterable);
                    try self.writeFmt(".len; {s}++) {{\n", .{idx_name});
                    self.indent_level += 1;
                    try self.writeIndent();
                    try self.writeStr(elem_c);
                    try self.writeFmt(" {s} = FORGE_LIST_GET(", .{fs.binding});
                    try self.emitExpr(fs.iterable);
                    try self.writeStr(", ");
                    try self.writeStr(elem_c);
                    try self.writeFmt(", {s});\n", .{idx_name});
                    // track the binding type so the body can use it
                    self.local_types.put(fs.binding, l.element) catch return error.OutOfMemory;
                    if (fs.index != null) self.local_types.put(idx_name, .int) catch return error.OutOfMemory;
                    try self.emitBlock(&fs.body);
                    self.indent_level -= 1;
                    try self.writeIndent();
                    try self.writeStr("}\n");
                },
                .map => |m| {
                    // for key in map:  →  for (i = 0; i < map.len; i++) { K key = ...; }
                    const key_c = self.cTypeStringForId(m.key);
                    try self.writeIndent();
                    try self.writeFmt("for (int64_t {s} = 0; {s} < ", .{ idx_name, idx_name });
                    try self.emitExpr(fs.iterable);
                    try self.writeFmt(".len; {s}++) {{\n", .{idx_name});
                    self.indent_level += 1;
                    try self.writeIndent();
                    try self.writeStr(key_c);
                    try self.writeFmt(" {s} = (({s} *)", .{ fs.binding, key_c });
                    try self.emitExpr(fs.iterable);
                    try self.writeFmt(".keys)[{s}];\n", .{idx_name});
                    self.local_types.put(fs.binding, m.key) catch return error.OutOfMemory;
                    if (fs.index != null) self.local_types.put(idx_name, .int) catch return error.OutOfMemory;
                    try self.emitBlock(&fs.body);
                    self.indent_level -= 1;
                    try self.writeIndent();
                    try self.writeStr("}\n");
                },
                .set => |s| {
                    // set is typedef'd to list — same iteration pattern
                    const elem_c = self.cTypeStringForId(s.element);
                    try self.writeIndent();
                    try self.writeFmt("for (int64_t {s} = 0; {s} < ", .{ idx_name, idx_name });
                    try self.emitExpr(fs.iterable);
                    try self.writeFmt(".len; {s}++) {{\n", .{idx_name});
                    self.indent_level += 1;
                    try self.writeIndent();
                    try self.writeStr(elem_c);
                    try self.writeFmt(" {s} = FORGE_LIST_GET(", .{fs.binding});
                    try self.emitExpr(fs.iterable);
                    try self.writeStr(", ");
                    try self.writeStr(elem_c);
                    try self.writeFmt(", {s});\n", .{idx_name});
                    self.local_types.put(fs.binding, s.element) catch return error.OutOfMemory;
                    if (fs.index != null) self.local_types.put(idx_name, .int) catch return error.OutOfMemory;
                    try self.emitBlock(&fs.body);
                    self.indent_level -= 1;
                    try self.writeIndent();
                    try self.writeStr("}\n");
                },
                else => {
                    try self.writeIndent();
                    try self.writeStr("/* for loop: unsupported iterable type */\n");
                },
            }
        } else {
            try self.writeIndent();
            try self.writeStr("/* for loop: could not resolve iterable type */\n");
        }
    }

    fn emitMatchStmt(self: *CEmitter, m: *const ast.MatchExpr) EmitError!void {
        // when a match statement is in a function that returns non-void,
        // the arms need `return` so the value gets returned to the caller.
        const needs_return = self.current_fn_return != .void;
        try self.emitMatchAsIfChain(m, true, needs_return);
    }

    // ---------------------------------------------------------------
    // expressions
    // ---------------------------------------------------------------

    fn emitExpr(self: *CEmitter, expr: *const ast.Expr) EmitError!void {
        switch (expr.kind) {
            .int_lit => |lit| try self.writeStr(lit),
            .float_lit => |lit| try self.writeStr(lit),
            .string_lit => |lit| try self.emitStringLit(lit),
            .bool_lit => |b| try self.writeStr(if (b) "true" else "false"),
            .none_lit => {
                // None in expression context — needs the optional type from context.
                // emitBinding and emitReturnStmt handle the common cases; this is
                // a fallback that emits a generic zero-init (works for any optional).
                try self.writeStr("{ 0 }");
            },
            .ident => |name| try self.writeStr(name),
            .self_expr => try self.writeStr("self"),
            .binary => |bin| try self.emitBinary(&bin),
            .unary => |un| try self.emitUnary(&un),
            .call => |call| try self.emitCall(&call),
            .method_call => |mc| try self.emitMethodCall(&mc),
            .field_access => |fa| try self.emitFieldAccess(&fa),
            .index => |idx| try self.emitIndexExpr(&idx),
            .grouped => |inner| {
                try self.writeByte('(');
                try self.emitExpr(inner);
                try self.writeByte(')');
            },
            .if_expr => |if_e| try self.emitIfExpr(&if_e),
            .match_expr => |m| try self.emitMatchAsIfChain(&m, false, false),
            .string_interp => |interp| try self.emitStringInterp(&interp),
            .lambda => |lam| {
                // hoist body to a top-level C function, emit a closure struct
                // at the use site. captures are detected by walking the lambda
                // body and finding identifiers from the enclosing scope.
                const idx = self.lambda_counter;
                self.lambda_counter += 1;
                const fn_tid = self.findLambdaType(lam);
                const captures = self.detectCaptures(lam) catch &.{};
                try self.hoisted_lambdas.append(self.allocator, .{
                    .index = idx,
                    .lambda = lam,
                    .fn_type_id = fn_tid,
                    .captures = captures,
                });
                if (captures.len > 0) {
                    // capturing lambda: heap-allocate env struct, build closure.
                    // uses a statement-expression so it can appear in expression context.
                    try self.writeFmt("(__extension__({{ __closure_env_{d} *__env_{d} = ", .{ idx, idx });
                    try self.writeFmt("(__closure_env_{d} *)malloc(sizeof(__closure_env_{d})); ", .{ idx, idx });
                    for (captures) |cap| {
                        try self.writeFmt("__env_{d}->{s} = {s}; ", .{ idx, cap.name, cap.name });
                    }
                    try self.writeFmt("(forge_closure_t){{ (void*)__lambda_{d}, (void*)__env_{d} }}; }}))", .{ idx, idx });
                } else {
                    // non-capturing: closure with NULL env
                    try self.writeFmt("(forge_closure_t){{ (void*)__lambda_{d}, NULL }}", .{idx});
                }
            },
            .list => |elems| try self.emitListLiteral(elems),
            .map => |entries| try self.emitMapLiteral(entries),
            .set => |elems| try self.emitSetLiteral(elems),
            .tuple => |elems| {
                // emit: (forge_tuple_T0_T1){ ._0 = a, ._1 = b }
                const tuple_tid = self.inferExprType(expr);
                const tuple_c = self.cTypeStringForId(tuple_tid);
                try self.writeStr("(");
                try self.writeStr(tuple_c);
                try self.writeStr("){ ");
                for (elems, 0..) |elem, i| {
                    if (i > 0) try self.writeStr(", ");
                    try self.writeFmt("._{d} = ", .{i});
                    try self.emitExpr(elem);
                }
                try self.writeStr(" }");
            },
            .unwrap => |inner| {
                // expr? — unwrap optional, access .value
                // in expression context, just access the value field
                try self.writeStr("(");
                try self.emitExpr(inner);
                try self.writeStr(").value");
            },
            .try_expr => |inner| {
                // expr! — extract ok value from result. the error propagation
                // is handled at the statement level via emitBinding/emitExprStmt.
                // in expression position, just access .ok
                try self.writeStr("(");
                try self.emitExpr(inner);
                try self.writeStr(").ok");
            },
            .spawn_expr => |inner| try self.emitSpawn(inner),
            .await_expr => |inner| try self.emitAwait(inner),
            .err => {},
        }
    }

    fn emitStringLit(self: *CEmitter, lit: []const u8) EmitError!void {
        // the lexer includes surrounding quotes in string literals — strip them
        const content = stripQuotes(lit);
        try self.writeStr("FORGE_STRING_LIT(\"");
        try self.writeEscapedString(content);
        try self.writeStr("\")");
    }

    /// strip surrounding double quotes if present. the lexer includes
    /// them in the lexeme for string literals.
    pub fn stripQuotes(s: []const u8) []const u8 {
        if (s.len >= 2 and s[0] == '"' and s[s.len - 1] == '"') {
            return s[1 .. s.len - 1];
        }
        return s;
    }

    fn emitBinary(self: *CEmitter, bin: *const ast.BinaryExpr) EmitError!void {
        // pipe operator: x |> f  →  fg_f(x)
        if (bin.op == .pipe) {
            // the right side should be a function name
            switch (bin.right.kind) {
                .ident => |name| try self.emitUserFnName(name),
                else => try self.emitExpr(bin.right),
            }
            try self.writeByte('(');
            try self.emitExpr(bin.left);
            try self.writeByte(')');
            return;
        }

        // string operations need special handling
        const is_string_op = self.isStringExpr(bin.left);

        if (is_string_op) {
            switch (bin.op) {
                .add => {
                    try self.writeStr("forge_string_concat(");
                    try self.emitExpr(bin.left);
                    try self.writeStr(", ");
                    try self.emitExpr(bin.right);
                    try self.writeByte(')');
                    return;
                },
                .eq => return self.emitStringCmp("forge_string_eq", bin),
                .neq => return self.emitStringCmp("forge_string_neq", bin),
                .lt => return self.emitStringCmp("forge_string_lt", bin),
                .gt => return self.emitStringCmp("forge_string_gt", bin),
                .lte => return self.emitStringCmp("forge_string_lte", bin),
                .gte => return self.emitStringCmp("forge_string_gte", bin),
                else => {},
            }
        }

        try self.writeByte('(');
        try self.emitExpr(bin.left);
        try self.writeStr(switch (bin.op) {
            .add => " + ",
            .sub => " - ",
            .mul => " * ",
            .div => " / ",
            .mod => " % ",
            .eq => " == ",
            .neq => " != ",
            .lt => " < ",
            .gt => " > ",
            .lte => " <= ",
            .gte => " >= ",
            .@"and" => " && ",
            .@"or" => " || ",
            .pipe => return, // handled above
        });
        try self.emitExpr(bin.right);
        try self.writeByte(')');
    }

    fn emitStringCmp(self: *CEmitter, func: []const u8, bin: *const ast.BinaryExpr) EmitError!void {
        try self.writeStr(func);
        try self.writeByte('(');
        try self.emitExpr(bin.left);
        try self.writeStr(", ");
        try self.emitExpr(bin.right);
        try self.writeByte(')');
    }

    /// is this expression a string? checks literal types, variable types,
    /// and function return types via the module scope.
    fn isStringExpr(self: *const CEmitter, expr: *const ast.Expr) bool {
        return self.inferExprType(expr) == .string;
    }

    fn emitUnary(self: *CEmitter, un: *const ast.UnaryExpr) EmitError!void {
        switch (un.op) {
            .negate => {
                try self.writeStr("(-");
                try self.emitExpr(un.operand);
                try self.writeByte(')');
            },
            .not => {
                try self.writeStr("(!");
                try self.emitExpr(un.operand);
                try self.writeByte(')');
            },
        }
    }

    fn emitCall(self: *CEmitter, call: *const ast.CallExpr) EmitError!void {
        const name = switch (call.callee.kind) {
            .ident => |n| n,
            else => {
                // indirect calls — emit as-is
                try self.emitExpr(call.callee);
                try self.emitArgList(call.args);
                return;
            },
        };

        // print() → forge_print()
        if (std.mem.eql(u8, name, "print")) {
            try self.writeStr("forge_print(");
            if (call.args.len > 0) {
                try self.emitExpr(call.args[0].value);
            }
            try self.writeByte(')');
            return;
        }

        // built-in functions that emit as forge_<name>(<args>)
        if (forge_prefix_builtins.has(name)) {
            try self.writeStr("forge_");
            try self.writeStr(name);
            try self.emitArgList(call.args);
            return;
        }

        // args() → forge_get_args()
        if (std.mem.eql(u8, name, "args")) {
            try self.writeStr("forge_get_args()");
            return;
        }

        // exit(Int) → exit()
        if (std.mem.eql(u8, name, "exit")) {
            try self.writeStr("exit(");
            if (call.args.len > 0) {
                try self.emitExpr(call.args[0].value);
            }
            try self.writeByte(')');
            return;
        }

        // exec(String) → forge_exec()
        if (std.mem.eql(u8, name, "exec")) {
            try self.writeStr("forge_exec(");
            if (call.args.len > 0) {
                try self.emitExpr(call.args[0].value);
            }
            try self.writeByte(')');
            return;
        }

        // sync primitive constructors
        if (std.mem.eql(u8, name, "Mutex")) {
            try self.writeStr("forge_mutex_create()");
            return;
        }
        if (std.mem.eql(u8, name, "WaitGroup")) {
            try self.writeStr("forge_waitgroup_create()");
            return;
        }
        if (std.mem.eql(u8, name, "Semaphore")) {
            try self.writeStr("forge_semaphore_create(");
            if (call.args.len > 0) {
                try self.emitExpr(call.args[0].value);
            }
            try self.writeByte(')');
            return;
        }

        // assert(Bool) — test assertion
        if (std.mem.eql(u8, name, "assert")) {
            try self.writeStr("do { if (!(");
            if (call.args.len > 0) try self.emitExpr(call.args[0].value);
            try self.writeStr(")) { fprintf(stderr, \"  assertion failed\\n\"); __current_test_failed = 1; } } while(0)");
            return;
        }

        // assert_eq(a, b) / assert_ne(a, b) — test equality assertions
        if (std.mem.eql(u8, name, "assert_eq") or std.mem.eql(u8, name, "assert_ne")) {
            const is_eq = std.mem.eql(u8, name, "assert_eq");
            // determine if args are strings (need forge_string_eq)
            const is_string = if (call.args.len >= 1) self.isStringExpr(call.args[0].value) else false;
            try self.writeStr("do { ");
            if (is_string) {
                if (is_eq) {
                    try self.writeStr("if (!forge_string_eq(");
                } else {
                    try self.writeStr("if (forge_string_eq(");
                }
                try self.emitExpr(call.args[0].value);
                try self.writeStr(", ");
                try self.emitExpr(call.args[1].value);
                try self.writeStr("))");
            } else {
                try self.writeStr("if ((");
                try self.emitExpr(call.args[0].value);
                try self.writeStr(") ");
                if (is_eq) {
                    try self.writeStr("!=");
                } else {
                    try self.writeStr("==");
                }
                try self.writeStr(" (");
                try self.emitExpr(call.args[1].value);
                try self.writeStr("))");
            }
            try self.writeStr(" { fprintf(stderr, \"  assertion failed: ");
            try self.writeStr(name);
            try self.writeStr("\\n\"); __current_test_failed = 1; } } while(0)");
            return;
        }

        // struct constructor: Name(args) → (Name){ .field1 = arg1, ... }
        if (self.type_table.lookup(name)) |tid| {
            if (self.type_table.get(tid)) |ty| {
                switch (ty) {
                    .@"struct" => |s| {
                        try self.writeByte('(');
                        try self.writeStr(self.cTypeStringForId(tid));
                        try self.writeStr("){ ");
                        for (s.fields, 0..) |field, i| {
                            if (i > 0) try self.writeStr(", ");
                            try self.writeByte('.');
                            try self.writeStr(field.name);
                            try self.writeStr(" = ");
                            if (i < call.args.len) {
                                try self.emitExpr(call.args[i].value);
                            }
                        }
                        try self.writeStr(" }");
                        return;
                    },
                    else => {},
                }
            }
        }

        // generic struct constructor: infer type args from call-site arguments
        if (self.generic_decls.get(name)) |decl| {
            switch (decl) {
                .@"struct" => {
                    // build instantiation name from argument types
                    if (self.buildGenericInstName(name, call.args)) |inst_name| {
                        if (self.type_table.lookup(inst_name)) |tid| {
                            if (self.type_table.get(tid)) |ty| {
                                switch (ty) {
                                    .@"struct" => |s| {
                                        try self.writeByte('(');
                                        try self.writeStr(self.cTypeStringForId(tid));
                                        try self.writeStr("){ ");
                                        for (s.fields, 0..) |field, i| {
                                            if (i > 0) try self.writeStr(", ");
                                            try self.writeByte('.');
                                            try self.writeStr(field.name);
                                            try self.writeStr(" = ");
                                            if (i < call.args.len) {
                                                try self.emitExpr(call.args[i].value);
                                            }
                                        }
                                        try self.writeStr(" }");
                                        return;
                                    },
                                    else => {},
                                }
                            }
                        }
                    }
                },
                .function => {
                    // generic function call: emit mangled name
                    if (self.buildGenericInstName(name, call.args)) |inst_name| {
                        const mangled = mangleName(self.allocator, inst_name) catch return error.OutOfMemory;
                        try self.writeStr("fg_");
                        try self.writeStr(mangled);
                        try self.emitArgList(call.args);
                        return;
                    }
                },
                else => {},
            }
        }

        // check if this is a closure variable (e.g., lambda binding or fn parameter)
        if (self.local_types.get(name)) |tid| {
            if (!tid.isErr()) {
                if (self.type_table.get(tid)) |ty| {
                    if (ty == .function) {
                        // closure call: cast fn_ptr and invoke with env_ptr
                        const func = ty.function;
                        try self.writeStr("((");
                        try self.writeStr(self.cTypeStringForId(func.return_type));
                        try self.writeStr(" (*)(void*");
                        for (func.param_types) |pt| {
                            try self.writeStr(", ");
                            try self.writeStr(self.cTypeStringForId(pt));
                        }
                        try self.writeStr("))");
                        try self.writeStr(name);
                        try self.writeStr(".fn_ptr)(");
                        try self.writeStr(name);
                        try self.writeStr(".env_ptr");
                        for (call.args) |arg| {
                            try self.writeStr(", ");
                            try self.emitExpr(arg.value);
                        }
                        try self.writeByte(')');
                        return;
                    }
                }
            }
        }

        // regular function call — prefixed to avoid C stdlib collisions
        try self.emitUserFnName(name);
        try self.emitArgList(call.args);
    }

    fn emitMethodCall(self: *CEmitter, mc: *const ast.MethodCallExpr) EmitError!void {
        const receiver_tid = self.inferExprType(mc.receiver);

        // built-in methods on primitive types
        if (receiver_tid == .string) return self.emitStringMethod(mc);
        if (receiver_tid == .int or receiver_tid == .float or receiver_tid == .bool) {
            return self.emitPrimitiveMethod(mc, receiver_tid);
        }

        // built-in collection methods
        if (!receiver_tid.isErr()) {
            if (self.type_table.get(receiver_tid)) |ty| {
                switch (ty) {
                    .list => |l| {
                        if (try self.emitListMethod(mc, l.element)) return;
                    },
                    .map => |m| {
                        if (try self.emitMapMethod(mc, m.key, m.value)) return;
                    },
                    .set => |s| {
                        if (try self.emitSetMethod(mc, s.element)) return;
                    },
                    .@"struct" => |st| {
                        if (try self.emitSyncMethod(mc, st.name)) return;
                    },
                    else => {},
                }
            }
        }

        // user-defined methods: TypeName_methodname(receiver, args...)
        const type_name = self.typeNameFromId(receiver_tid);

        if (type_name) |name| {
            try self.writeStr(name);
            try self.writeByte('_');
            try self.writeStr(mc.method);
            try self.writeByte('(');
            try self.emitExpr(mc.receiver);
            for (mc.args) |arg| {
                try self.writeStr(", ");
                try self.emitExpr(arg.value);
            }
            try self.writeByte(')');
        } else {
            // fallback — can't resolve receiver type
            try self.writeStr("/* method call: ");
            try self.writeStr(mc.method);
            try self.writeStr(" */");
        }
    }

    /// emit a built-in string method call.
    fn emitStringMethod(self: *CEmitter, mc: *const ast.MethodCallExpr) EmitError!void {
        const method = mc.method;

        // .len() → direct field access (zero overhead)
        if (std.mem.eql(u8, method, "len")) {
            try self.emitExpr(mc.receiver);
            try self.writeStr(".len");
            return;
        }

        // one-arg string->string/bool: contains, starts_with, ends_with, split, index_of, last_index_of
        if (std.mem.eql(u8, method, "contains") or
            std.mem.eql(u8, method, "starts_with") or
            std.mem.eql(u8, method, "ends_with") or
            std.mem.eql(u8, method, "split") or
            std.mem.eql(u8, method, "index_of") or
            std.mem.eql(u8, method, "last_index_of"))
        {
            try self.writeStr("forge_string_");
            try self.writeStr(method);
            try self.writeByte('(');
            try self.emitExpr(mc.receiver);
            try self.writeStr(", ");
            try self.emitExpr(mc.args[0].value);
            try self.writeByte(')');
            return;
        }

        // no-arg runtime functions: trim, to_upper, to_lower, chars
        if (std.mem.eql(u8, method, "trim") or
            std.mem.eql(u8, method, "to_upper") or
            std.mem.eql(u8, method, "to_lower") or
            std.mem.eql(u8, method, "chars"))
        {
            try self.writeStr("forge_string_");
            try self.writeStr(method);
            try self.writeByte('(');
            try self.emitExpr(mc.receiver);
            try self.writeByte(')');
            return;
        }

        // is_empty() → inline check
        if (std.mem.eql(u8, method, "is_empty")) {
            try self.writeByte('(');
            try self.emitExpr(mc.receiver);
            try self.writeStr(".len == 0)");
            return;
        }

        // repeat(Int) -> String
        if (std.mem.eql(u8, method, "repeat")) {
            try self.writeStr("forge_string_repeat(");
            try self.emitExpr(mc.receiver);
            try self.writeStr(", ");
            try self.emitExpr(mc.args[0].value);
            try self.writeByte(')');
            return;
        }

        // two-arg string methods: substring, replace, pad_left, pad_right
        if (std.mem.eql(u8, method, "substring") or std.mem.eql(u8, method, "replace") or
            std.mem.eql(u8, method, "pad_left") or std.mem.eql(u8, method, "pad_right"))
        {
            try self.writeStr("forge_string_");
            try self.writeStr(method);
            try self.writeByte('(');
            try self.emitExpr(mc.receiver);
            try self.writeStr(", ");
            try self.emitExpr(mc.args[0].value);
            try self.writeStr(", ");
            try self.emitExpr(mc.args[1].value);
            try self.writeByte(')');
            return;
        }
    }

    /// emit method calls on Int, Float, Bool.
    fn emitPrimitiveMethod(self: *CEmitter, mc: *const ast.MethodCallExpr, receiver_tid: TypeId) EmitError!void {
        const method = mc.method;

        // to_string() — use existing runtime conversion functions
        if (std.mem.eql(u8, method, "to_string")) {
            const fn_name = switch (receiver_tid) {
                .int => "forge_int_to_string",
                .float => "forge_float_to_string",
                .bool => "forge_bool_to_string",
                else => unreachable,
            };
            try self.writeStr(fn_name);
            try self.writeByte('(');
            try self.emitExpr(mc.receiver);
            try self.writeByte(')');
            return;
        }

        // Int.to_float() → (double)(receiver)
        if (receiver_tid == .int and std.mem.eql(u8, method, "to_float")) {
            try self.writeStr("(double)(");
            try self.emitExpr(mc.receiver);
            try self.writeByte(')');
            return;
        }

        // Float.to_int() → (int64_t)(receiver)
        if (receiver_tid == .float and std.mem.eql(u8, method, "to_int")) {
            try self.writeStr("(int64_t)(");
            try self.emitExpr(mc.receiver);
            try self.writeByte(')');
            return;
        }
    }

    /// emit forge_list_push(&list, &(T){value}, sizeof(T))
    fn emitCollectionPush(self: *CEmitter, receiver: *const ast.Expr, value: *const ast.Expr, elem_type: TypeId) EmitError!void {
        const c_type = self.cTypeStringForId(elem_type);
        // for struct types (e.g. forge_string_t), compound literal init
        // (Type){value} doesn't work — use a temp variable instead
        if (elem_type == .string or elem_type.index() >= TypeId.first_user) {
            const n = self.push_counter;
            self.push_counter += 1;
            try self.writeStr("{ ");
            try self.writeStr(c_type);
            try self.writeFmt(" __push_{d} = ", .{n});
            try self.emitExpr(value);
            try self.writeStr("; forge_list_push(&");
            try self.emitExpr(receiver);
            try self.writeFmt(", &__push_{d}, sizeof(", .{n});
            try self.writeStr(c_type);
            try self.writeStr(")); }");
        } else {
            try self.writeStr("forge_list_push(&");
            try self.emitExpr(receiver);
            try self.writeStr(", &(");
            try self.writeStr(c_type);
            try self.writeStr("){");
            try self.emitExpr(value);
            try self.writeStr("}, sizeof(");
            try self.writeStr(c_type);
            try self.writeStr("))");
        }
    }

    /// emit forge_map_set_by_{string,int}(&map, key, &val, sizeof(K), sizeof(V))
    /// uses a temp variable to avoid compound literal issues with struct types
    fn emitMapInsert(self: *CEmitter, receiver: *const ast.Expr, key: *const ast.Expr, value: *const ast.Expr, key_type: TypeId, val_type: TypeId) EmitError!void {
        const key_c = self.cTypeStringForId(key_type);
        const val_c = self.cTypeStringForId(val_type);
        // use a temp variable so &val works for both scalars and structs
        try self.writeStr("{ ");
        try self.writeStr(val_c);
        try self.writeStr(" __mv = ");
        try self.emitExpr(value);
        try self.writeStr("; ");
        if (key_type == .string) {
            try self.writeStr("forge_map_set_by_string(&");
        } else {
            try self.writeStr("forge_map_set_by_int(&");
        }
        try self.emitExpr(receiver);
        try self.writeStr(", ");
        try self.emitExpr(key);
        try self.writeStr(", &__mv, sizeof(");
        try self.writeStr(key_c);
        try self.writeStr("), sizeof(");
        try self.writeStr(val_c);
        try self.writeStr(")); }");
    }

    /// emit forge_set_add(&set, &val, sizeof(T))
    /// uses a temp variable to avoid compound literal issues with struct types
    fn emitSetAdd(self: *CEmitter, receiver: *const ast.Expr, value: *const ast.Expr, elem_type: TypeId) EmitError!void {
        const c_type = self.cTypeStringForId(elem_type);
        try self.writeStr("{ ");
        try self.writeStr(c_type);
        try self.writeStr(" __sv = ");
        try self.emitExpr(value);
        try self.writeStr("; forge_set_add(&");
        try self.emitExpr(receiver);
        try self.writeStr(", &__sv, sizeof(");
        try self.writeStr(c_type);
        try self.writeStr(")); }");
    }

    /// emit a built-in list method call. returns true if handled.
    fn emitListMethod(self: *CEmitter, mc: *const ast.MethodCallExpr, elem_type: TypeId) EmitError!bool {
        const method = mc.method;
        const c_type = self.cTypeStringForId(elem_type);

        if (std.mem.eql(u8, method, "push") and mc.args.len == 1) {
            try self.emitCollectionPush(mc.receiver, mc.args[0].value, elem_type);
            return true;
        }
        if (std.mem.eql(u8, method, "len") or std.mem.eql(u8, method, "is_empty")) {
            if (std.mem.eql(u8, method, "is_empty")) {
                try self.writeByte('(');
                try self.emitExpr(mc.receiver);
                try self.writeStr(".len == 0)");
            } else {
                try self.emitExpr(mc.receiver);
                try self.writeStr(".len");
            }
            return true;
        }
        if (std.mem.eql(u8, method, "remove")) {
            try self.writeStr("forge_list_remove(&");
            try self.emitExpr(mc.receiver);
            try self.writeStr(", ");
            try self.emitExpr(mc.args[0].value);
            try self.writeStr(", sizeof(");
            try self.writeStr(c_type);
            try self.writeStr("))");
            return true;
        }
        if (std.mem.eql(u8, method, "contains")) {
            if (elem_type == .string) {
                try self.writeStr("forge_list_contains_string(");
                try self.emitExpr(mc.receiver);
                try self.writeStr(", ");
                try self.emitExpr(mc.args[0].value);
                try self.writeByte(')');
            } else {
                try self.writeStr("forge_list_contains(");
                try self.emitExpr(mc.receiver);
                try self.writeStr(", &(");
                try self.writeStr(c_type);
                try self.writeStr("){");
                try self.emitExpr(mc.args[0].value);
                try self.writeStr("}, sizeof(");
                try self.writeStr(c_type);
                try self.writeStr("))");
            }
            return true;
        }
        if (std.mem.eql(u8, method, "reverse")) {
            try self.writeStr("forge_list_reverse(&");
            try self.emitExpr(mc.receiver);
            try self.writeStr(", sizeof(");
            try self.writeStr(c_type);
            try self.writeStr("))");
            return true;
        }
        if (std.mem.eql(u8, method, "clear")) {
            try self.writeStr("forge_list_clear(&");
            try self.emitExpr(mc.receiver);
            try self.writeByte(')');
            return true;
        }
        if (std.mem.eql(u8, method, "join") and mc.args.len == 1) {
            try self.writeStr("forge_list_join(");
            try self.emitExpr(mc.receiver);
            try self.writeStr(", ");
            try self.emitExpr(mc.args[0].value);
            try self.writeByte(')');
            return true;
        }
        // index_of(T) -> Int
        if (std.mem.eql(u8, method, "index_of") and mc.args.len == 1) {
            if (elem_type == .string) {
                try self.writeStr("forge_list_index_of_string(");
                try self.emitExpr(mc.receiver);
                try self.writeStr(", ");
                try self.emitExpr(mc.args[0].value);
                try self.writeByte(')');
            } else {
                try self.writeStr("forge_list_index_of(");
                try self.emitExpr(mc.receiver);
                try self.writeStr(", &(");
                try self.writeStr(c_type);
                try self.writeStr("){");
                try self.emitExpr(mc.args[0].value);
                try self.writeStr("}, sizeof(");
                try self.writeStr(c_type);
                try self.writeStr("))");
            }
            return true;
        }
        // slice(Int, Int) -> List[T]
        if (std.mem.eql(u8, method, "slice") and mc.args.len == 2) {
            try self.writeStr("forge_list_slice(");
            try self.emitExpr(mc.receiver);
            try self.writeStr(", ");
            try self.emitExpr(mc.args[0].value);
            try self.writeStr(", ");
            try self.emitExpr(mc.args[1].value);
            try self.writeStr(", sizeof(");
            try self.writeStr(c_type);
            try self.writeStr("))");
            return true;
        }
        // sort() -> List[T]
        if (std.mem.eql(u8, method, "sort")) {
            const tag: []const u8 = if (elem_type == .int) "0" else if (elem_type == .float) "1" else "2";
            try self.writeStr("forge_list_sort(");
            try self.emitExpr(mc.receiver);
            try self.writeStr(", sizeof(");
            try self.writeStr(c_type);
            try self.writeStr("), ");
            try self.writeStr(tag);
            try self.writeByte(')');
            return true;
        }
        return false;
    }

    /// emit a built-in map method call. returns true if handled.
    fn emitMapMethod(self: *CEmitter, mc: *const ast.MethodCallExpr, key_type: TypeId, val_type: TypeId) EmitError!bool {
        const method = mc.method;
        const key_c = self.cTypeStringForId(key_type);
        const val_c = self.cTypeStringForId(val_type);

        if (std.mem.eql(u8, method, "insert") and mc.args.len == 2) {
            try self.emitMapInsert(mc.receiver, mc.args[0].value, mc.args[1].value, key_type, val_type);
            return true;
        }
        if (std.mem.eql(u8, method, "len")) {
            try self.emitExpr(mc.receiver);
            try self.writeStr(".len");
            return true;
        }
        if (std.mem.eql(u8, method, "is_empty")) {
            try self.writeByte('(');
            try self.emitExpr(mc.receiver);
            try self.writeStr(".len == 0)");
            return true;
        }
        if (std.mem.eql(u8, method, "contains_key")) {
            if (key_type == .string) {
                try self.writeStr("forge_map_contains_key_string(");
            } else {
                try self.writeStr("forge_map_contains_key_int(");
            }
            try self.emitExpr(mc.receiver);
            try self.writeStr(", ");
            try self.emitExpr(mc.args[0].value);
            try self.writeByte(')');
            return true;
        }
        if (std.mem.eql(u8, method, "remove")) {
            if (key_type == .string) {
                try self.writeStr("forge_map_remove_by_string(&");
            } else {
                try self.writeStr("forge_map_remove_by_int(&");
            }
            try self.emitExpr(mc.receiver);
            try self.writeStr(", ");
            try self.emitExpr(mc.args[0].value);
            try self.writeStr(", sizeof(");
            try self.writeStr(key_c);
            try self.writeStr("), sizeof(");
            try self.writeStr(val_c);
            try self.writeStr("))");
            return true;
        }
        if (std.mem.eql(u8, method, "keys")) {
            try self.writeStr("forge_map_keys(");
            try self.emitExpr(mc.receiver);
            try self.writeStr(", sizeof(");
            try self.writeStr(key_c);
            try self.writeStr("))");
            return true;
        }
        if (std.mem.eql(u8, method, "values")) {
            try self.writeStr("forge_map_values(");
            try self.emitExpr(mc.receiver);
            try self.writeStr(", sizeof(");
            try self.writeStr(val_c);
            try self.writeStr("))");
            return true;
        }
        if (std.mem.eql(u8, method, "clear")) {
            try self.writeStr("forge_map_clear(&");
            try self.emitExpr(mc.receiver);
            try self.writeByte(')');
            return true;
        }
        return false;
    }

    /// emit a built-in set method call. returns true if handled.
    fn emitSetMethod(self: *CEmitter, mc: *const ast.MethodCallExpr, elem_type: TypeId) EmitError!bool {
        const method = mc.method;
        const c_type = self.cTypeStringForId(elem_type);

        if (std.mem.eql(u8, method, "add") and mc.args.len == 1) {
            try self.emitSetAdd(mc.receiver, mc.args[0].value, elem_type);
            return true;
        }
        if (std.mem.eql(u8, method, "len")) {
            try self.emitExpr(mc.receiver);
            try self.writeStr(".len");
            return true;
        }
        if (std.mem.eql(u8, method, "is_empty")) {
            try self.writeByte('(');
            try self.emitExpr(mc.receiver);
            try self.writeStr(".len == 0)");
            return true;
        }
        if (std.mem.eql(u8, method, "contains")) {
            if (elem_type == .string) {
                try self.writeStr("forge_set_contains_string(");
                try self.emitExpr(mc.receiver);
                try self.writeStr(", ");
                try self.emitExpr(mc.args[0].value);
                try self.writeByte(')');
            } else {
                try self.writeStr("forge_set_contains(");
                try self.emitExpr(mc.receiver);
                try self.writeStr(", &(");
                try self.writeStr(c_type);
                try self.writeStr("){");
                try self.emitExpr(mc.args[0].value);
                try self.writeStr("}, sizeof(");
                try self.writeStr(c_type);
                try self.writeStr("))");
            }
            return true;
        }
        if (std.mem.eql(u8, method, "remove")) {
            if (elem_type == .string) {
                try self.writeStr("forge_set_remove_string(&");
                try self.emitExpr(mc.receiver);
                try self.writeStr(", ");
                try self.emitExpr(mc.args[0].value);
                try self.writeByte(')');
            } else {
                try self.writeStr("forge_set_remove(&");
                try self.emitExpr(mc.receiver);
                try self.writeStr(", &(");
                try self.writeStr(c_type);
                try self.writeStr("){");
                try self.emitExpr(mc.args[0].value);
                try self.writeStr("}, sizeof(");
                try self.writeStr(c_type);
                try self.writeStr("))");
            }
            return true;
        }
        if (std.mem.eql(u8, method, "clear")) {
            try self.writeStr("forge_set_clear(&");
            try self.emitExpr(mc.receiver);
            try self.writeByte(')');
            return true;
        }
        return false;
    }

    /// emit a built-in sync primitive method call. returns true if handled.
    fn emitSyncMethod(self: *CEmitter, mc: *const ast.MethodCallExpr, type_name: []const u8) EmitError!bool {
        const method = mc.method;

        if (std.mem.eql(u8, type_name, "Mutex")) {
            if (std.mem.eql(u8, method, "lock")) {
                try self.writeStr("forge_mutex_lock(&");
                try self.emitExpr(mc.receiver);
                try self.writeByte(')');
                return true;
            }
            if (std.mem.eql(u8, method, "unlock")) {
                try self.writeStr("forge_mutex_unlock(&");
                try self.emitExpr(mc.receiver);
                try self.writeByte(')');
                return true;
            }
        } else if (std.mem.eql(u8, type_name, "WaitGroup")) {
            if (std.mem.eql(u8, method, "add") and mc.args.len == 1) {
                try self.writeStr("forge_waitgroup_add(&");
                try self.emitExpr(mc.receiver);
                try self.writeStr(", ");
                try self.emitExpr(mc.args[0].value);
                try self.writeByte(')');
                return true;
            }
            if (std.mem.eql(u8, method, "done")) {
                try self.writeStr("forge_waitgroup_done(&");
                try self.emitExpr(mc.receiver);
                try self.writeByte(')');
                return true;
            }
            if (std.mem.eql(u8, method, "wait")) {
                try self.writeStr("forge_waitgroup_wait(&");
                try self.emitExpr(mc.receiver);
                try self.writeByte(')');
                return true;
            }
        } else if (std.mem.eql(u8, type_name, "Semaphore")) {
            if (std.mem.eql(u8, method, "acquire")) {
                try self.writeStr("forge_semaphore_acquire(&");
                try self.emitExpr(mc.receiver);
                try self.writeByte(')');
                return true;
            }
            if (std.mem.eql(u8, method, "release")) {
                try self.writeStr("forge_semaphore_release(&");
                try self.emitExpr(mc.receiver);
                try self.writeByte(')');
                return true;
            }
        }
        return false;
    }

    /// return type inference for built-in String methods.
    fn inferStringMethodType(self: *const CEmitter, method: []const u8) TypeId {
        if (std.mem.eql(u8, method, "len")) return .int;
        if (std.mem.eql(u8, method, "contains")) return .bool;
        if (std.mem.eql(u8, method, "starts_with")) return .bool;
        if (std.mem.eql(u8, method, "ends_with")) return .bool;
        if (std.mem.eql(u8, method, "is_empty")) return .bool;
        if (std.mem.eql(u8, method, "index_of")) return .int;
        if (std.mem.eql(u8, method, "last_index_of")) return .int;
        if (std.mem.eql(u8, method, "trim")) return .string;
        if (std.mem.eql(u8, method, "to_upper")) return .string;
        if (std.mem.eql(u8, method, "to_lower")) return .string;
        if (std.mem.eql(u8, method, "substring")) return .string;
        if (std.mem.eql(u8, method, "replace")) return .string;
        if (std.mem.eql(u8, method, "repeat")) return .string;
        if (std.mem.eql(u8, method, "pad_left")) return .string;
        if (std.mem.eql(u8, method, "pad_right")) return .string;
        if (std.mem.eql(u8, method, "split") or std.mem.eql(u8, method, "chars")) {
            return self.type_table.lookup("List[String]") orelse .err;
        }
        return .err;
    }

    /// return type inference for built-in Int methods.
    fn inferIntMethodType(_: *const CEmitter, method: []const u8) TypeId {
        if (std.mem.eql(u8, method, "to_string")) return .string;
        if (std.mem.eql(u8, method, "to_float")) return .float;
        return .err;
    }

    /// return type inference for built-in Float methods.
    fn inferFloatMethodType(_: *const CEmitter, method: []const u8) TypeId {
        if (std.mem.eql(u8, method, "to_string")) return .string;
        if (std.mem.eql(u8, method, "to_int")) return .int;
        return .err;
    }

    /// return type inference for built-in List methods.
    fn inferListMethodType(self: *const CEmitter, method: []const u8, elem_type: TypeId) ?TypeId {
        if (std.mem.eql(u8, method, "len")) return .int;
        if (std.mem.eql(u8, method, "is_empty")) return .bool;
        if (std.mem.eql(u8, method, "contains")) return .bool;
        if (std.mem.eql(u8, method, "index_of")) return .int;
        if (std.mem.eql(u8, method, "push") or
            std.mem.eql(u8, method, "remove") or
            std.mem.eql(u8, method, "reverse") or
            std.mem.eql(u8, method, "clear")) return .void;
        if (std.mem.eql(u8, method, "join")) return .string;
        // slice and sort return the same list type
        if (std.mem.eql(u8, method, "slice") or std.mem.eql(u8, method, "sort")) {
            const elem_name = self.type_table.typeName(elem_type);
            var buf: [128]u8 = undefined;
            const lookup_name = std.fmt.bufPrint(&buf, "List[{s}]", .{elem_name}) catch return null;
            return self.type_table.lookup(lookup_name);
        }
        return null;
    }

    /// return type inference for built-in Map methods.
    fn inferMapMethodType(self: *const CEmitter, method: []const u8, key_type: TypeId, val_type: TypeId) ?TypeId {
        if (std.mem.eql(u8, method, "len")) return .int;
        if (std.mem.eql(u8, method, "is_empty")) return .bool;
        if (std.mem.eql(u8, method, "contains_key")) return .bool;
        if (std.mem.eql(u8, method, "insert") or
            std.mem.eql(u8, method, "remove") or
            std.mem.eql(u8, method, "clear")) return .void;
        if (std.mem.eql(u8, method, "keys")) {
            // look up List[K] type
            const lookup_name = std.fmt.allocPrint(self.allocator, "List[{s}]", .{self.type_table.typeName(key_type)}) catch return null;
            return self.type_table.lookup(lookup_name);
        }
        if (std.mem.eql(u8, method, "values")) {
            const lookup_name = std.fmt.allocPrint(self.allocator, "List[{s}]", .{self.type_table.typeName(val_type)}) catch return null;
            return self.type_table.lookup(lookup_name);
        }
        return null;
    }

    /// return type inference for built-in Set methods.
    fn inferSetMethodType(_: *const CEmitter, method: []const u8) ?TypeId {
        if (std.mem.eql(u8, method, "len")) return .int;
        if (std.mem.eql(u8, method, "is_empty")) return .bool;
        if (std.mem.eql(u8, method, "contains")) return .bool;
        if (std.mem.eql(u8, method, "add") or
            std.mem.eql(u8, method, "remove") or
            std.mem.eql(u8, method, "clear")) return .void;
        return null;
    }

    /// return type inference for sync primitive methods.
    fn inferSyncMethodType(_: *const CEmitter, type_name: []const u8, method: []const u8) ?TypeId {
        if (std.mem.eql(u8, type_name, "Mutex")) {
            if (std.mem.eql(u8, method, "lock") or std.mem.eql(u8, method, "unlock")) return .void;
        } else if (std.mem.eql(u8, type_name, "WaitGroup")) {
            if (std.mem.eql(u8, method, "add") or std.mem.eql(u8, method, "done") or std.mem.eql(u8, method, "wait")) return .void;
        } else if (std.mem.eql(u8, type_name, "Semaphore")) {
            if (std.mem.eql(u8, method, "acquire") or std.mem.eql(u8, method, "release")) return .void;
        }
        return null;
    }

    /// get the type name string for a TypeId by looking it up in the type table.
    fn typeNameFromId(self: *const CEmitter, tid: TypeId) ?[]const u8 {
        if (tid.isErr()) return null;
        const ty = self.type_table.get(tid) orelse return null;
        return switch (ty) {
            .@"struct" => |s| s.name,
            .@"enum" => |e| e.name,
            else => null,
        };
    }

    /// look up a method's function type id from the checker's method_types.
    /// key format is "TypeName.methodName".
    fn lookupMethodKey(self: *const CEmitter, type_name: []const u8, method_name: []const u8) ?TypeId {
        const key = std.fmt.allocPrint(self.allocator, "{s}.{s}", .{ type_name, method_name }) catch return null;
        const entry = self.method_types.get(key) orelse return null;
        return entry.type_id;
    }

    fn emitFieldAccess(self: *CEmitter, fa: *const ast.FieldAccess) EmitError!void {
        try self.emitExpr(fa.object);
        try self.writeByte('.');
        // tuple field access: t.0 → t._0 (fields are named _0, _1, etc in C)
        if (fa.field.len > 0 and fa.field[0] >= '0' and fa.field[0] <= '9') {
            try self.writeByte('_');
        }
        try self.writeStr(fa.field);
    }

    // ---------------------------------------------------------------
    // collection literals
    // ---------------------------------------------------------------

    fn emitListLiteral(self: *CEmitter, elems: []const *const ast.Expr) EmitError!void {
        if (elems.len == 0) {
            try self.writeStr("forge_list_create(0, 0, NULL)");
            return;
        }
        const elem_tid = self.inferExprType(elems[0]);
        const c_type = self.cTypeStringForId(elem_tid);
        try self.writeFmt("forge_list_create({d}, sizeof(", .{@as(i64, @intCast(elems.len))});
        try self.writeStr(c_type);
        try self.writeStr("), (");
        try self.writeStr(c_type);
        try self.writeStr("[]){");
        for (elems, 0..) |elem, i| {
            if (i > 0) try self.writeStr(", ");
            try self.emitExpr(elem);
        }
        try self.writeStr("})");
    }

    fn emitMapLiteral(self: *CEmitter, entries: []const ast.MapEntry) EmitError!void {
        if (entries.len == 0) {
            try self.writeStr("(forge_map_t){.keys = NULL, .values = NULL, .len = 0}");
            return;
        }
        const key_tid = self.inferExprType(entries[0].key);
        const val_tid = self.inferExprType(entries[0].value);
        const key_c = self.cTypeStringForId(key_tid);
        const val_c = self.cTypeStringForId(val_tid);

        try self.writeFmt("forge_map_create({d}, sizeof(", .{@as(i64, @intCast(entries.len))});
        try self.writeStr(key_c);
        try self.writeStr("), sizeof(");
        try self.writeStr(val_c);
        try self.writeStr("), (");
        try self.writeStr(key_c);
        try self.writeStr("[]){");
        for (entries, 0..) |entry, i| {
            if (i > 0) try self.writeStr(", ");
            try self.emitExpr(entry.key);
        }
        try self.writeStr("}, (");
        try self.writeStr(val_c);
        try self.writeStr("[]){");
        for (entries, 0..) |entry, i| {
            if (i > 0) try self.writeStr(", ");
            try self.emitExpr(entry.value);
        }
        try self.writeStr("})");
    }

    fn emitSetLiteral(self: *CEmitter, elems: []const *const ast.Expr) EmitError!void {
        if (elems.len == 0) {
            try self.writeStr("forge_set_create(0, 0, NULL)");
            return;
        }
        const elem_tid = self.inferExprType(elems[0]);
        const c_type = self.cTypeStringForId(elem_tid);
        try self.writeFmt("forge_set_create({d}, sizeof(", .{@as(i64, @intCast(elems.len))});
        try self.writeStr(c_type);
        try self.writeStr("), (");
        try self.writeStr(c_type);
        try self.writeStr("[]){");
        for (elems, 0..) |elem, i| {
            if (i > 0) try self.writeStr(", ");
            try self.emitExpr(elem);
        }
        try self.writeStr("})");
    }

    fn emitIndexExpr(self: *CEmitter, idx: *const ast.IndexExpr) EmitError!void {
        const obj_tid = self.inferExprType(idx.object);

        // string indexing: s[n] → forge_string_char_at(s, n)
        if (obj_tid == .string) {
            try self.writeStr("forge_string_char_at(");
            try self.emitExpr(idx.object);
            try self.writeStr(", ");
            try self.emitExpr(idx.index);
            try self.writeByte(')');
            return;
        }

        if (self.type_table.get(obj_tid)) |ty| {
            switch (ty) {
                .list => |l| {
                    const elem_c = self.cTypeStringForId(l.element);
                    try self.writeStr("FORGE_LIST_GET(");
                    try self.emitExpr(idx.object);
                    try self.writeStr(", ");
                    try self.writeStr(elem_c);
                    try self.writeStr(", ");
                    try self.emitExpr(idx.index);
                    try self.writeByte(')');
                },
                .map => |m| {
                    const val_c = self.cTypeStringForId(m.value);
                    try self.writeStr("*(");
                    try self.writeStr(val_c);
                    try self.writeStr("*)forge_map_get_checked(");
                    // dispatch on key type
                    if (m.key == .string) {
                        try self.writeStr("forge_map_get_by_string(");
                    } else {
                        try self.writeStr("forge_map_get_by_int(");
                    }
                    try self.emitExpr(idx.object);
                    try self.writeStr(", ");
                    try self.emitExpr(idx.index);
                    try self.writeStr(", sizeof(");
                    try self.writeStr(val_c);
                    try self.writeStr(")))");
                },
                else => {
                    try self.writeStr("/* index on unsupported type */");
                },
            }
        } else {
            try self.writeStr("/* index: unknown obj type */");
        }
    }

    fn emitArgList(self: *CEmitter, args: []const ast.Arg) EmitError!void {
        try self.writeByte('(');
        for (args, 0..) |arg, i| {
            if (i > 0) try self.writeStr(", ");
            try self.emitExpr(arg.value);
        }
        try self.writeByte(')');
    }

    fn emitIfExpr(self: *CEmitter, if_e: *const ast.IfExpr) EmitError!void {
        // ternary: condition ? then : else
        // for elif chains, nest them
        try self.writeByte('(');
        try self.emitExpr(if_e.condition);
        try self.writeStr(" ? ");
        try self.emitExpr(if_e.then_expr);
        try self.writeStr(" : ");
        if (if_e.elif_branches.len > 0) {
            // nested ternaries for elif
            for (if_e.elif_branches) |*elif| {
                try self.emitExpr(elif.condition);
                try self.writeStr(" ? ");
                try self.emitExpr(elif.expr);
                try self.writeStr(" : ");
            }
        }
        try self.emitExpr(if_e.else_expr);
        try self.writeByte(')');
    }

    fn emitStringInterp(self: *CEmitter, interp: *const ast.StringInterp) EmitError!void {
        // string interpolation: "hello {name}!" → forge_string_concat chain
        const part_count = interp.parts.len;

        if (part_count == 0) {
            try self.writeStr("forge_string_empty");
            return;
        }
        if (part_count == 1) {
            try self.emitStringPart(&interp.parts[0]);
            return;
        }

        // nested concat calls: concat(concat(a, b), c)
        // for N parts we need N-1 concat calls, so N-2 leading opens
        // (the first concat wraps parts 0 and 1, each subsequent wraps
        // the previous result and the next part)
        var i: usize = 0;
        while (i + 2 < part_count) : (i += 1) {
            try self.writeStr("forge_string_concat(");
        }

        // emit first concat pair
        try self.writeStr("forge_string_concat(");
        try self.emitStringPart(&interp.parts[0]);
        try self.writeStr(", ");
        try self.emitStringPart(&interp.parts[1]);
        try self.writeByte(')');

        // emit remaining parts, each wrapped in a concat with the result so far
        for (interp.parts[2..]) |*part| {
            try self.writeStr(", ");
            try self.emitStringPart(part);
            try self.writeByte(')');
        }
    }

    fn emitStringPart(self: *CEmitter, part: *const ast.StringPart) EmitError!void {
        switch (part.*) {
            .literal => |lit| {
                // interpolation literal parts may include boundary quotes
                // from the lexer (string_start has leading ", string_end
                // has trailing "). strip them.
                var content = lit;
                if (content.len > 0 and content[0] == '"')
                    content = content[1..];
                if (content.len > 0 and content[content.len - 1] == '"')
                    content = content[0 .. content.len - 1];
                try self.writeStr("FORGE_STRING_LIT(\"");
                try self.writeEscapedString(content);
                try self.writeStr("\")");
            },
            .expr => |expr| {
                // need to convert the expression to string
                try self.emitExprAsString(expr);
            },
        }
    }

    /// emit an expression converted to forge_string_t. uses type inference
    /// to pick the right conversion function.
    fn emitExprAsString(self: *CEmitter, expr: *const ast.Expr) EmitError!void {
        const tid = self.inferExprType(expr);
        if (tid == .string) {
            // already a string — emit directly
            try self.emitExpr(expr);
            return;
        }
        if (tid == .int or tid.isInteger()) {
            try self.writeStr("forge_int_to_string(");
            try self.emitExpr(expr);
            try self.writeByte(')');
            return;
        }
        if (tid == .float) {
            try self.writeStr("forge_float_to_string(");
            try self.emitExpr(expr);
            try self.writeByte(')');
            return;
        }
        if (tid == .bool) {
            try self.writeStr("forge_bool_to_string(");
            try self.emitExpr(expr);
            try self.writeByte(')');
            return;
        }
        // fallback: use the expression kind for literals
        switch (expr.kind) {
            .string_lit => |lit| try self.emitStringLit(lit),
            .string_interp => |interp| try self.emitStringInterp(&interp),
            .int_lit => {
                try self.writeStr("forge_int_to_string(");
                try self.emitExpr(expr);
                try self.writeByte(')');
            },
            .float_lit => {
                try self.writeStr("forge_float_to_string(");
                try self.emitExpr(expr);
                try self.writeByte(')');
            },
            .bool_lit => {
                try self.writeStr("forge_bool_to_string(");
                try self.emitExpr(expr);
                try self.writeByte(')');
            },
            else => {
                // last resort — assume it's already a string expression
                try self.emitExpr(expr);
            },
        }
    }

    fn emitMatchAsIfChain(self: *CEmitter, m: *const ast.MatchExpr, is_stmt: bool, needs_return: bool) EmitError!void {
        if (m.arms.len == 0) return;

        if (is_stmt) {
            // open a scoped block
            try self.writeIndent();
            try self.writeStr("{\n");
            self.indent_level += 1;
        }

        for (m.arms, 0..) |*arm, i| {
            const is_last = i == m.arms.len - 1;
            const is_wildcard = arm.pattern.kind == .wildcard or arm.pattern.kind == .binding;

            if (is_stmt) {
                if (i == 0) {
                    if (is_wildcard and is_last) {
                        // single wildcard arm — just emit the body
                    } else if (!is_wildcard) {
                        try self.writeIndent();
                        try self.writeStr("if (");
                        try self.emitPatternCondition(m.subject, &arm.pattern);
                        try self.writeStr(") {\n");
                        self.indent_level += 1;
                    }
                } else if (is_wildcard and is_last) {
                    try self.writeIndent();
                    try self.writeStr("} else {\n");
                    self.indent_level += 1;
                } else {
                    try self.writeIndent();
                    try self.writeStr("} else if (");
                    try self.emitPatternCondition(m.subject, &arm.pattern);
                    try self.writeStr(") {\n");
                    self.indent_level += 1;
                }

                // emit binding if present — use subject's type
                if (arm.pattern.kind == .binding) {
                    const subject_type = self.inferExprType(m.subject);
                    try self.writeIndent();
                    if (!subject_type.isErr()) {
                        try self.emitCType(subject_type);
                    } else {
                        try self.emitTypeForExpr(m.subject);
                    }
                    try self.writeByte(' ');
                    try self.writeStr(arm.pattern.kind.binding);
                    try self.writeStr(" = ");
                    try self.emitExpr(m.subject);
                    try self.writeStr(";\n");
                }

                switch (arm.body) {
                    .expr => |e| {
                        try self.writeIndent();
                        if (needs_return) try self.writeStr("return ");
                        try self.emitExpr(e);
                        try self.writeStr(";\n");
                    },
                    .block => |*blk| try self.emitBlock(blk),
                }

                if (!is_wildcard or !is_last) {
                    self.indent_level -= 1;
                } else if (i > 0) {
                    self.indent_level -= 1;
                    try self.writeIndent();
                    try self.writeStr("}\n");
                }
            } else {
                // match as expression — emit as ternary chain
                if (i > 0) try self.writeStr(" : ");
                if (!is_wildcard or !is_last) {
                    try self.writeStr("(");
                    try self.emitPatternCondition(m.subject, &arm.pattern);
                    try self.writeStr(" ? ");
                }
                switch (arm.body) {
                    .expr => |e| try self.emitExpr(e),
                    .block => |*blk| {
                        // block arm in expression context — emit last statement
                        if (blk.stmts.len > 0) {
                            const last_stmt = blk.stmts[blk.stmts.len - 1];
                            switch (last_stmt.kind) {
                                .expr_stmt => |e| try self.emitExpr(e),
                                else => try self.writeStr("0 /* block */"),
                            }
                        } else {
                            try self.writeStr("0 /* empty block */");
                        }
                    },
                }
                if (!is_wildcard or !is_last) {
                    try self.writeByte(')');
                }
            }
        }

        if (is_stmt) {
            // close the last if block if needed
            if (m.arms.len > 0) {
                const last = m.arms[m.arms.len - 1];
                const last_is_wildcard = last.pattern.kind == .wildcard or last.pattern.kind == .binding;
                if (!last_is_wildcard) {
                    try self.writeIndent();
                    try self.writeStr("}\n");
                }
            }
            self.indent_level -= 1;
            try self.writeIndent();
            try self.writeStr("}\n");
        }
    }

    fn emitPatternCondition(self: *CEmitter, subject: *const ast.Expr, pattern: *const ast.Pattern) EmitError!void {
        switch (pattern.kind) {
            .wildcard, .binding => try self.writeStr("1"),
            .int_lit => |lit| {
                try self.emitExpr(subject);
                try self.writeStr(" == ");
                try self.writeStr(lit);
            },
            .float_lit => |lit| {
                try self.emitExpr(subject);
                try self.writeStr(" == ");
                try self.writeStr(lit);
            },
            .string_lit => |lit| {
                const content = stripQuotes(lit);
                try self.writeStr("forge_string_eq(");
                try self.emitExpr(subject);
                try self.writeStr(", FORGE_STRING_LIT(\"");
                try self.writeEscapedString(content);
                try self.writeStr("\"))");
            },
            .bool_lit => |b| {
                try self.emitExpr(subject);
                try self.writeStr(if (b) " == true" else " == false");
            },
            .none_lit => try self.writeStr("0 /* None pattern */"),
            .variant => |vp| {
                try self.emitExpr(subject);
                try self.writeStr(".tag == ");
                try self.writeStr(vp.type_name);
                try self.writeStr("_TAG_");
                try self.writeStr(vp.variant);
            },
            .tuple => |_| try self.writeStr("1 /* tuple pattern */"),
        }
    }

    // ---------------------------------------------------------------
    // type inference helpers
    // ---------------------------------------------------------------

    /// resolve the TypeId for an AST type expression. used to track
    /// parameter and binding types during emission.
    fn resolveTypeExprToId(self: *const CEmitter, te: *const ast.TypeExpr) TypeId {
        return switch (te.kind) {
            .named => |name| {
                if (std.mem.eql(u8, name, "Int")) return .int;
                if (std.mem.eql(u8, name, "UInt")) return .uint;
                if (std.mem.eql(u8, name, "Float")) return .float;
                if (std.mem.eql(u8, name, "Bool")) return .bool;
                if (std.mem.eql(u8, name, "String")) return .string;
                if (std.mem.eql(u8, name, "Void")) return .void;
                if (std.mem.eql(u8, name, "Bytes")) return .bytes;
                // user-defined — look up in the type table
                return self.type_table.lookup(name) orelse .err;
            },
            .generic => |g| {
                // build the instantiated name: "List[Int]", "Map[String,Int]"
                var parts: std.ArrayList(u8) = .empty;
                parts.appendSlice(self.allocator, g.name) catch return .err;
                parts.append(self.allocator, '[') catch return .err;
                for (g.args, 0..) |arg, i| {
                    if (i > 0) parts.append(self.allocator, ',') catch return .err;
                    const arg_name = switch (arg.kind) {
                        .named => |n| n,
                        else => continue,
                    };
                    parts.appendSlice(self.allocator, arg_name) catch return .err;
                }
                parts.append(self.allocator, ']') catch return .err;
                const lookup = parts.toOwnedSlice(self.allocator) catch return .err;
                return self.type_table.lookup(lookup) orelse .err;
            },
            .result => |r| {
                // resolve the ok type and find the matching result type in the table
                const ok_id = self.resolveTypeExprToId(r.ok_type);
                if (ok_id.isErr()) return .err;
                // scan the type table for a matching result type
                const items = self.type_table.types.items;
                for (items, 0..) |ty, idx| {
                    switch (ty) {
                        .result => |res| {
                            if (res.ok_type == ok_id) {
                                return TypeId.fromIndex(@intCast(idx));
                            }
                        },
                        else => {},
                    }
                }
                return .err;
            },
            .optional => |o| {
                // resolve the inner type and find the matching optional type in the table
                const inner_id = self.resolveTypeExprToId(o);
                if (inner_id.isErr()) return .err;
                const items = self.type_table.types.items;
                for (items, 0..) |ty, idx| {
                    switch (ty) {
                        .optional => |opt| {
                            if (opt.inner == inner_id) {
                                return TypeId.fromIndex(@intCast(idx));
                            }
                        },
                        else => {},
                    }
                }
                return .err;
            },
            .tuple => |elems| {
                // resolve each element type and find the matching tuple type
                var elem_ids: [16]TypeId = undefined;
                if (elems.len > elem_ids.len) return .err;
                for (elems, 0..) |elem, i| {
                    const eid = self.resolveTypeExprToId(elem);
                    if (eid.isErr()) return .err;
                    elem_ids[i] = eid;
                }
                const resolved = elem_ids[0..elems.len];

                const items = self.type_table.types.items;
                for (items, 0..) |ty, idx| {
                    switch (ty) {
                        .tuple => |tup| {
                            if (tup.elements.len == resolved.len) {
                                var match = true;
                                for (tup.elements, resolved) |a, b| {
                                    if (a != b) {
                                        match = false;
                                        break;
                                    }
                                }
                                if (match) return TypeId.fromIndex(@intCast(idx));
                            }
                        },
                        else => {},
                    }
                }
                return .err;
            },
            .fn_type => |ft| {
                // resolve param and return types, find matching function type
                var param_ids: [16]TypeId = undefined;
                if (ft.params.len > param_ids.len) return .err;
                for (ft.params, 0..) |param, i| {
                    const pid = self.resolveTypeExprToId(param);
                    if (pid.isErr()) return .err;
                    param_ids[i] = pid;
                }
                const ret_id = if (ft.return_type) |rt| self.resolveTypeExprToId(rt) else .void;
                if (ret_id.isErr()) return .err;
                const resolved = param_ids[0..ft.params.len];

                const items = self.type_table.types.items;
                for (items, 0..) |ty, idx| {
                    switch (ty) {
                        .function => |func| {
                            if (func.return_type == ret_id and func.param_types.len == resolved.len) {
                                var match = true;
                                for (func.param_types, resolved) |a, b| {
                                    if (a != b) {
                                        match = false;
                                        break;
                                    }
                                }
                                if (match) return TypeId.fromIndex(@intCast(idx));
                            }
                        },
                        else => {},
                    }
                }
                return .err;
            },
        };
    }

    /// infer the TypeId of an expression. uses local variable types,
    /// module scope (for function return types), and expression structure.
    fn inferExprType(self: *const CEmitter, expr: *const ast.Expr) TypeId {
        return switch (expr.kind) {
            .int_lit => .int,
            .float_lit => .float,
            .string_lit, .string_interp => .string,
            .bool_lit => .bool,
            .none_lit => .void,
            .self_expr => {
                // `self` is registered as a local variable in method bodies
                if (self.local_types.get("self")) |tid| return tid;
                return .err;
            },
            .ident => |name| {
                // check local variables first
                if (self.local_types.get(name)) |tid| return tid;
                // then module scope (functions, etc.)
                if (self.module_scope.lookup(name)) |binding| {
                    return binding.type_id;
                }
                return .err;
            },
            .binary => |bin| {
                switch (bin.op) {
                    .eq, .neq, .lt, .gt, .lte, .gte, .@"and", .@"or" => return .bool,
                    .add => {
                        const left_type = self.inferExprType(bin.left);
                        if (left_type == .string) return .string;
                        return left_type;
                    },
                    .pipe => {
                        // x | f → result type is f's return type
                        const fn_name = switch (bin.right.kind) {
                            .ident => |n| n,
                            else => return self.inferExprType(bin.left),
                        };
                        if (self.module_scope.lookup(fn_name)) |binding| {
                            if (self.type_table.get(binding.type_id)) |ty| {
                                switch (ty) {
                                    .function => |f| return f.return_type,
                                    else => {},
                                }
                            }
                        }
                        return self.inferExprType(bin.left);
                    },
                    else => return self.inferExprType(bin.left),
                }
            },
            .unary => |un| {
                return switch (un.op) {
                    .not => .bool,
                    .negate => self.inferExprType(un.operand),
                };
            },
            .call => |call| {
                // look up the function's return type
                const name = switch (call.callee.kind) {
                    .ident => |n| n,
                    else => return .err,
                };
                // check module scope first (functions, builtins)
                if (self.module_scope.lookup(name)) |binding| {
                    if (self.type_table.get(binding.type_id)) |ty| {
                        switch (ty) {
                            .function => |f| return f.return_type,
                            .@"struct" => return binding.type_id,
                            else => {},
                        }
                    }
                }
                // check type table for struct/enum constructors
                if (self.type_table.lookup(name)) |tid| {
                    if (self.type_table.get(tid)) |ty| {
                        switch (ty) {
                            .@"struct", .@"enum" => return tid,
                            else => {},
                        }
                    }
                }
                // generic function/struct — infer from call-site arg types
                if (self.buildGenericInstName(name, call.args)) |inst_name| {
                    if (self.type_table.lookup(inst_name)) |tid| {
                        if (self.type_table.get(tid)) |ty| {
                            switch (ty) {
                                .function => |f| return f.return_type,
                                .@"struct" => return tid,
                                else => {},
                            }
                        }
                    }
                }
                return .err;
            },
            .method_call => |mc| {
                const receiver_tid = self.inferExprType(mc.receiver);

                // built-in methods on primitive types
                if (receiver_tid == .string) return self.inferStringMethodType(mc.method);
                if (receiver_tid == .int) return self.inferIntMethodType(mc.method);
                if (receiver_tid == .float) return self.inferFloatMethodType(mc.method);
                if (receiver_tid == .bool and std.mem.eql(u8, mc.method, "to_string")) return .string;

                // built-in collection methods
                if (!receiver_tid.isErr()) {
                    if (self.type_table.get(receiver_tid)) |ty| {
                        switch (ty) {
                            .list => |l| {
                                if (self.inferListMethodType(mc.method, l.element)) |tid| return tid;
                            },
                            .map => |m| {
                                if (self.inferMapMethodType(mc.method, m.key, m.value)) |tid| return tid;
                            },
                            .set => {
                                if (self.inferSetMethodType(mc.method)) |tid| return tid;
                            },
                            .@"struct" => |st| {
                                if (self.inferSyncMethodType(st.name, mc.method)) |tid| return tid;
                            },
                            else => {},
                        }
                    }
                }

                // user-defined methods
                const type_name = self.typeNameFromId(receiver_tid) orelse return .err;
                const key = self.lookupMethodKey(type_name, mc.method) orelse return .err;
                if (self.type_table.get(key)) |ty| {
                    switch (ty) {
                        .function => |f| return f.return_type,
                        else => {},
                    }
                }
                return .err;
            },
            .field_access => |fa| {
                // resolve the object type and look up the field
                const obj_type = self.inferExprType(fa.object);
                if (self.type_table.get(obj_type)) |ty| {
                    switch (ty) {
                        .@"struct" => |s| {
                            for (s.fields) |field| {
                                if (std.mem.eql(u8, field.name, fa.field))
                                    return field.type_id;
                            }
                        },
                        .tuple => |tup| {
                            // numeric field access: .0, .1, etc.
                            const idx = std.fmt.parseInt(usize, fa.field, 10) catch return .err;
                            if (idx < tup.elements.len) return tup.elements[idx];
                        },
                        else => {},
                    }
                }
                return .err;
            },
            .grouped => |inner| return self.inferExprType(inner),
            .if_expr => |if_e| return self.inferExprType(if_e.then_expr),
            .list => |elems| {
                if (elems.len == 0) return .err;
                const elem_tid = self.inferExprType(elems[0]);
                const elem_name = self.type_table.typeName(elem_tid);
                // look up "List[ElemType]" in the type table
                var buf: [128]u8 = undefined;
                const inst = std.fmt.bufPrint(&buf, "List[{s}]", .{elem_name}) catch return .err;
                return self.type_table.lookup(inst) orelse .err;
            },
            .map => |entries| {
                if (entries.len == 0) return .err;
                const key_tid = self.inferExprType(entries[0].key);
                const val_tid = self.inferExprType(entries[0].value);
                const key_name = self.type_table.typeName(key_tid);
                const val_name = self.type_table.typeName(val_tid);
                var buf: [128]u8 = undefined;
                const inst = std.fmt.bufPrint(&buf, "Map[{s},{s}]", .{ key_name, val_name }) catch return .err;
                return self.type_table.lookup(inst) orelse .err;
            },
            .set => |elems| {
                if (elems.len == 0) return .err;
                const elem_tid = self.inferExprType(elems[0]);
                const elem_name = self.type_table.typeName(elem_tid);
                var buf: [128]u8 = undefined;
                const inst = std.fmt.bufPrint(&buf, "Set[{s}]", .{elem_name}) catch return .err;
                return self.type_table.lookup(inst) orelse .err;
            },
            .index => |idx| {
                const obj_tid = self.inferExprType(idx.object);
                if (obj_tid == .string) return .string;
                if (self.type_table.get(obj_tid)) |ty| {
                    return switch (ty) {
                        .list => |l| l.element,
                        .map => |m| m.value,
                        else => .err,
                    };
                }
                return .err;
            },
            .unwrap => |inner| {
                // expr? on Optional[T] → T
                const inner_tid = self.inferExprType(inner);
                if (self.type_table.get(inner_tid)) |ty| {
                    switch (ty) {
                        .optional => |o| return o.inner,
                        else => {},
                    }
                }
                return .err;
            },
            .try_expr => |inner| {
                // expr! on Result[T, E] → T
                const inner_tid = self.inferExprType(inner);
                if (self.type_table.get(inner_tid)) |ty| {
                    switch (ty) {
                        .result => |r| return r.ok_type,
                        else => {},
                    }
                }
                return .err;
            },
            .spawn_expr => |inner| {
                // spawn wraps the return type in Task[T] — search for matching task type
                const inner_tid = self.inferExprType(inner);
                for (self.type_table.types.items, 0..) |ty, i| {
                    switch (ty) {
                        .task => |t| {
                            if (t.inner == inner_tid) return TypeId.fromIndex(@intCast(i));
                        },
                        else => {},
                    }
                }
                return .err;
            },
            .await_expr => |inner| {
                // await unwraps Task[T] → T
                const inner_tid = self.inferExprType(inner);
                if (self.type_table.get(inner_tid)) |ty| {
                    switch (ty) {
                        .task => |t| return t.inner,
                        else => {},
                    }
                }
                return .err;
            },
            .lambda => |lam| {
                // infer param types and return type, look up in cache
                if (lam.params.len > 16) return .err;
                var key = FnSigKey{
                    .param_count = @intCast(lam.params.len),
                    .return_type = switch (lam.body) {
                        .expr => |body_expr| self.inferExprType(body_expr),
                        .block => .void,
                    },
                };
                for (lam.params, 0..) |param, i| {
                    if (param.type_expr) |te| {
                        key.params[i] = self.resolveTypeExprToId(te);
                    } else {
                        return .err;
                    }
                }
                return self.fn_type_cache.get(key) orelse .err;
            },
            .tuple => |elems| {
                // infer element types and look up in cache
                if (elems.len > 16) return .err;
                var key = TupleSigKey{
                    .elem_count = @intCast(elems.len),
                };
                for (elems, 0..) |elem, i| {
                    key.elements[i] = self.inferExprType(elem);
                }
                return self.tuple_type_cache.get(key) orelse .err;
            },
            else => .err,
        };
    }

    // ---------------------------------------------------------------
    // type emission
    // ---------------------------------------------------------------

    fn emitTypeExpr(self: *CEmitter, te: *const ast.TypeExpr) EmitError!void {
        switch (te.kind) {
            .named => |name| try self.emitNamedType(name),
            .generic => |g| {
                // collection types map to runtime C types
                if (std.mem.eql(u8, g.name, "List")) {
                    try self.writeStr("forge_list_t");
                } else if (std.mem.eql(u8, g.name, "Map")) {
                    try self.writeStr("forge_map_t");
                } else if (std.mem.eql(u8, g.name, "Set")) {
                    try self.writeStr("forge_set_t");
                } else {
                    // user-defined generic — resolve to mangled name via type table
                    const tid = self.resolveTypeExprToId(te);
                    if (!tid.isErr()) {
                        try self.writeStr(self.cTypeStringForId(tid));
                    } else {
                        try self.writeStr(g.name);
                    }
                }
            },
            .optional => {
                const tid = self.resolveTypeExprToId(te);
                if (!tid.isErr()) {
                    try self.writeStr(self.cTypeStringForId(tid));
                } else {
                    try self.writeStr("/* optional */");
                }
            },
            .result => {
                // resolve to the concrete result typedef name
                const tid = self.resolveTypeExprToId(te);
                if (!tid.isErr()) {
                    try self.writeStr(self.cTypeStringForId(tid));
                } else {
                    try self.writeStr("/* result */");
                }
            },
            .tuple => {
                const tid = self.resolveTypeExprToId(te);
                if (!tid.isErr()) {
                    try self.writeStr(self.cTypeStringForId(tid));
                } else {
                    try self.writeStr("/* tuple type */");
                }
            },
            .fn_type => {
                try self.writeStr("forge_closure_t");
            },
        }
    }

    fn emitNamedType(self: *CEmitter, name: []const u8) EmitError!void {
        try self.writeStr(mapType(name));
    }

    /// emit a C type for a TypeId from the type table.
    pub fn emitCType(self: *CEmitter, tid: TypeId) EmitError!void {
        try self.writeStr(mapTypeId(tid));

        // user-defined types — look up the name
        if (tid.index() >= TypeId.first_user) {
            // check mangled names cache (generic instantiations + result types)
            if (self.mangled_names.get(tid)) |mangled| {
                try self.writeStr(mangled);
                return;
            }

            if (self.type_table.get(tid)) |ty| {
                switch (ty) {
                    .@"struct" => |s| {
                        // sync primitive types map to their C runtime types
                        if (std.mem.eql(u8, s.name, "Mutex")) {
                            try self.writeStr("forge_mutex_t");
                        } else if (std.mem.eql(u8, s.name, "WaitGroup")) {
                            try self.writeStr("forge_waitgroup_t");
                        } else if (std.mem.eql(u8, s.name, "Semaphore")) {
                            try self.writeStr("forge_semaphore_t");
                        } else {
                            try self.writeStr(s.name);
                        }
                        return;
                    },
                    .@"enum" => |e| {
                        try self.writeStr(e.name);
                        return;
                    },
                    .list => {
                        try self.writeStr("forge_list_t");
                        return;
                    },
                    .map => {
                        try self.writeStr("forge_map_t");
                        return;
                    },
                    .set => {
                        try self.writeStr("forge_set_t");
                        return;
                    },
                    .function => {
                        try self.writeStr("forge_closure_t");
                        return;
                    },
                    .task => {
                        try self.writeStr("void*");
                        return;
                    },
                    else => {},
                }
            }
        }
    }

    // ---------------------------------------------------------------
    // type mapping
    // ---------------------------------------------------------------

    /// map a forge type name to a C type string.
    pub fn mapType(name: []const u8) []const u8 {
        if (std.mem.eql(u8, name, "Int")) return "int64_t";
        if (std.mem.eql(u8, name, "UInt")) return "uint64_t";
        if (std.mem.eql(u8, name, "Float")) return "double";
        if (std.mem.eql(u8, name, "Bool")) return "bool";
        if (std.mem.eql(u8, name, "String")) return "forge_string_t";
        if (std.mem.eql(u8, name, "Bytes")) return "uint8_t*";
        if (std.mem.eql(u8, name, "Void")) return "void";
        if (std.mem.eql(u8, name, "Int8")) return "int8_t";
        if (std.mem.eql(u8, name, "Int16")) return "int16_t";
        if (std.mem.eql(u8, name, "Int32")) return "int32_t";
        if (std.mem.eql(u8, name, "Int64")) return "int64_t";
        if (std.mem.eql(u8, name, "UInt8")) return "uint8_t";
        if (std.mem.eql(u8, name, "UInt16")) return "uint16_t";
        if (std.mem.eql(u8, name, "UInt32")) return "uint32_t";
        if (std.mem.eql(u8, name, "UInt64")) return "uint64_t";
        // user-defined type — use the name as-is (should be typedef'd)
        return name;
    }

    /// map a TypeId to a C type string for builtin types.
    /// returns empty string for user-defined types (caller handles those).
    pub fn mapTypeId(tid: TypeId) []const u8 {
        return switch (tid) {
            .int => "int64_t",
            .uint => "uint64_t",
            .float => "double",
            .bool => "bool",
            .string => "forge_string_t",
            .bytes => "uint8_t*",
            .void => "void",
            .int8 => "int8_t",
            .int16 => "int16_t",
            .int32 => "int32_t",
            .int64 => "int64_t",
            .uint8 => "uint8_t",
            .uint16 => "uint16_t",
            .uint32 => "uint32_t",
            .uint64 => "uint64_t",
            .err => "/* error */",
            _ => "", // user-defined — caller handles
        };
    }

    /// return the C type string for any TypeId — builtins and user-defined.
    /// for collection types returns the runtime struct name; for structs/enums
    /// returns the forge type name (which is typedef'd in emitted C).
    fn cTypeStringForId(self: *CEmitter, tid: TypeId) []const u8 {
        // builtins have a direct mapping
        const builtin = mapTypeId(tid);
        if (builtin.len > 0) return builtin;

        // check for mangled generic type names first
        if (self.mangled_names.get(tid)) |mangled| return mangled;

        // user-defined — look up in the type table
        if (self.type_table.get(tid)) |ty| {
            return switch (ty) {
                .@"struct" => |s| {
                    // sync primitive types map to their C runtime types
                    if (std.mem.eql(u8, s.name, "Mutex")) return "forge_mutex_t";
                    if (std.mem.eql(u8, s.name, "WaitGroup")) return "forge_waitgroup_t";
                    if (std.mem.eql(u8, s.name, "Semaphore")) return "forge_semaphore_t";
                    return s.name;
                },
                .@"enum" => |e| e.name,
                .list => "forge_list_t",
                .map => "forge_map_t",
                .set => "forge_set_t",
                .function => "forge_closure_t",
                .task => "void*",
                // optional and result types should have been registered in mangled_names
                // during emitOptionalTypedefs/emitResultTypedefs — fall through to unknown
                else => "/* unknown */",
            };
        }
        return "/* unknown */";
    }

    // ---------------------------------------------------------------
    // output helpers
    // ---------------------------------------------------------------

    fn writeStr(self: *CEmitter, s: []const u8) EmitError!void {
        try self.output.appendSlice(self.allocator, s);
    }

    fn writeByte(self: *CEmitter, b: u8) EmitError!void {
        try self.output.append(self.allocator, b);
    }

    fn writeFmt(self: *CEmitter, comptime fmt: []const u8, args: anytype) EmitError!void {
        var buf: [256]u8 = undefined;
        const s = std.fmt.bufPrint(&buf, fmt, args) catch return;
        try self.output.appendSlice(self.allocator, s);
    }

    fn writeIndent(self: *CEmitter) EmitError!void {
        var i: u32 = 0;
        while (i < self.indent_level) : (i += 1) {
            try self.output.appendSlice(self.allocator, "    ");
        }
    }

    fn writeEscapedString(self: *CEmitter, s: []const u8) EmitError!void {
        // the forge lexer preserves escape sequences in their raw form:
        // "\n" in source is stored as bytes '\' 'n', not as byte 0x0A.
        // since C uses the same escape sequences, pass them through as-is.
        var i: usize = 0;
        while (i < s.len) : (i += 1) {
            const c = s[i];
            switch (c) {
                '\\' => {
                    if (i + 1 < s.len) {
                        const next = s[i + 1];
                        switch (next) {
                            'n', 't', 'r', '\\', '"', '0', '\'' => {
                                // recognized escape sequence — pass through to C
                                try self.output.append(self.allocator, '\\');
                                try self.output.append(self.allocator, next);
                                i += 1;
                            },
                            else => {
                                try self.output.appendSlice(self.allocator, "\\\\");
                            },
                        }
                    } else {
                        try self.output.appendSlice(self.allocator, "\\\\");
                    }
                },
                '"' => try self.output.appendSlice(self.allocator, "\\\""),
                '\n' => try self.output.appendSlice(self.allocator, "\\n"),
                '\r' => try self.output.appendSlice(self.allocator, "\\r"),
                '\t' => try self.output.appendSlice(self.allocator, "\\t"),
                else => try self.output.append(self.allocator, c),
            }
        }
    }
};
