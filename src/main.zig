// main — CLI entry point for the forge compiler

const std = @import("std");
const lexer_mod = @import("lexer.zig");
const Lexer = lexer_mod.Lexer;
const Token = lexer_mod.Token;
const Parser = @import("parser.zig").Parser;
const errors = @import("errors.zig");
const printer = @import("printer.zig");
const checker_mod = @import("checker.zig");
const Checker = checker_mod.Checker;
const CEmitter = @import("codegen.zig").CEmitter;
const formatter = @import("formatter.zig");
const lint = @import("lint.zig");
const ast = @import("ast.zig");
const io = @import("io.zig");

// compiler modules — imported here so zig build sees them
comptime {
    _ = @import("ast.zig");
    _ = @import("parser.zig");
    _ = @import("errors.zig");
    _ = @import("intern.zig");
    _ = @import("printer.zig");
    _ = @import("io.zig");
    _ = @import("types.zig");
    _ = @import("checker.zig");
    _ = @import("checker_test.zig");
    _ = @import("codegen.zig");
    _ = @import("codegen_test.zig");
    _ = @import("formatter.zig");
    _ = @import("lint.zig");
}

const version = "0.1.0";

/// max source file size the compiler will read (10 MiB). prevents
/// accidental reads of large binary files.
const max_source_size = 10 * 1024 * 1024;

fn renderDiagnostics(diags: *const errors.DiagnosticList, json: bool) void {
    if (json) {
        var buf: [io.write_buf_size]u8 = undefined;
        var w = std.fs.File.stdout().writer(&buf);
        diags.renderJson(&w.interface) catch {};
        w.interface.flush() catch {};
    } else {
        var buf: [io.write_buf_size]u8 = undefined;
        var w = std.fs.File.stderr().writer(&buf);
        diags.render(&w.interface) catch {};
        w.interface.flush() catch {};
    }
}

fn readSourceFile(allocator: std.mem.Allocator, path: []const u8) ?[]const u8 {
    const source = std.fs.cwd().readFileAlloc(allocator, path, max_source_size) catch |err| {
        io.writeErr("error: could not read '{s}': {}\n", .{ path, err });
        return null;
    };

    if (!std.unicode.utf8ValidateSlice(source)) {
        io.writeErr("error: '{s}' contains invalid UTF-8\n", .{path});
        allocator.free(source);
        return null;
    }

    return source;
}

/// bundles the outputs of lexing + parsing so callers can clean up
/// with a single deinit call.
const ParseResult = struct {
    module: ast.Module,
    tokens: []const Token,
    arena: std.heap.ArenaAllocator,

    fn deinit(self: *ParseResult, allocator: std.mem.Allocator) void {
        allocator.free(self.tokens);
        self.arena.deinit();
    }
};

/// lex and parse source code. returns null if there are errors
/// (diagnostics are rendered before returning).
fn lexAndParse(allocator: std.mem.Allocator, source: []const u8, json: bool) !?ParseResult {
    // lex
    var lexer = try Lexer.init(source, allocator);
    defer lexer.deinit();
    const tokens = try lexer.tokenize();

    if (lexer.diagnostics.hasErrors()) {
        renderDiagnostics(&lexer.diagnostics, json);
        allocator.free(tokens);
        return null;
    }

    // parse
    var arena = std.heap.ArenaAllocator.init(allocator);
    var parser = Parser.init(tokens, source, arena.allocator());
    defer parser.deinit();

    const module = parser.parseModule() catch {
        io.writeErr("error: parse failed (out of memory)\n", .{});
        arena.deinit();
        allocator.free(tokens);
        return null;
    };

    if (parser.diagnostics.hasErrors()) {
        renderDiagnostics(&parser.diagnostics, json);
        arena.deinit();
        allocator.free(tokens);
        return null;
    }

    return .{ .module = module, .tokens = tokens, .arena = arena };
}

const builtin = @import("builtin");

pub fn main() !void {
    // in debug builds, use GPA for leak detection and memory safety checks.
    // in release builds, use smp_allocator — no wrapper overhead.
    var gpa: std.heap.GeneralPurposeAllocator(.{}) = .init;
    defer if (builtin.mode == .Debug) {
        _ = gpa.deinit();
    };
    const allocator = if (builtin.mode == .Debug)
        gpa.allocator()
    else
        std.heap.smp_allocator;

    var args = std.process.argsWithAllocator(allocator) catch {
        printUsage();
        return;
    };
    defer args.deinit();

    // skip the program name
    _ = args.next();

    const cmd = args.next() orelse {
        printUsage();
        return;
    };

    if (std.mem.eql(u8, cmd, "version") or std.mem.eql(u8, cmd, "--version")) {
        printVersion();
    } else if (std.mem.eql(u8, cmd, "help") or std.mem.eql(u8, cmd, "--help")) {
        printUsage();
    } else if (std.mem.eql(u8, cmd, "lex")) {
        const file_path = args.next() orelse {
            io.writeErr("error: forge lex requires a file path\n", .{});
            return;
        };
        try runLex(allocator, file_path);
    } else if (std.mem.eql(u8, cmd, "parse")) {
        const file_path = args.next() orelse {
            io.writeErr("error: forge parse requires a file path\n", .{});
            return;
        };
        try runParse(allocator, file_path);
    } else if (std.mem.eql(u8, cmd, "check")) {
        const file_path = args.next() orelse {
            io.writeErr("error: forge check requires a file path\n", .{});
            return;
        };
        const json = hasFlag(&args, "--json");
        try runCheck(allocator, file_path, json);
    } else if (std.mem.eql(u8, cmd, "build")) {
        const file_path = args.next() orelse {
            io.writeErr("error: forge build requires a file path\n", .{});
            return;
        };
        const json = hasFlag(&args, "--json");
        try runBuild(allocator, file_path, false, json, &.{});
    } else if (std.mem.eql(u8, cmd, "run")) {
        const file_path = args.next() orelse {
            io.writeErr("error: forge run requires a file path\n", .{});
            return;
        };
        // collect remaining args to forward to the compiled program
        var run_args: std.ArrayList([]const u8) = .empty;
        defer run_args.deinit(allocator);
        while (args.next()) |a| {
            run_args.append(allocator, a) catch {};
        }
        try runBuild(allocator, file_path, true, false, run_args.items);
    } else if (std.mem.eql(u8, cmd, "test")) {
        const file_path = args.next() orelse {
            io.writeErr("error: forge test requires a file path\n", .{});
            return;
        };
        const json = hasFlag(&args, "--json");
        try runTest(allocator, file_path, json);
    } else if (std.mem.eql(u8, cmd, "fmt")) {
        var check_only = false;
        var file_path: ?[]const u8 = null;
        while (args.next()) |arg| {
            if (std.mem.eql(u8, arg, "--check")) {
                check_only = true;
            } else if (file_path == null) {
                file_path = arg;
            }
        }
        const path = file_path orelse {
            io.writeErr("error: forge fmt requires a file path\n", .{});
            return;
        };
        try runFmt(allocator, path, check_only);
    } else if (std.mem.eql(u8, cmd, "lint")) {
        const file_path = args.next() orelse {
            io.writeErr("error: forge lint requires a file path\n", .{});
            return;
        };
        const json = hasFlag(&args, "--json");
        try runLint(allocator, file_path, json);
    } else {
        io.writeErr("error: unknown command '{s}'\n", .{cmd});
        printUsage();
    }
}

/// lex a source file and print each token.
fn runLex(allocator: std.mem.Allocator, path: []const u8) !void {
    const source = readSourceFile(allocator, path) orelse return;
    defer allocator.free(source);

    var lexer = try Lexer.init(source, allocator);
    defer lexer.deinit();

    while (true) {
        const tok = try lexer.nextToken();

        switch (tok.kind) {
            .newline => io.write("{s:<16}  \\n\n", .{@tagName(tok.kind)}),
            .indent => io.write("{s:<16}  >>>\n", .{@tagName(tok.kind)}),
            .dedent => io.write("{s:<16}  <<<\n", .{@tagName(tok.kind)}),
            .eof => {
                io.write("{s:<16}  <eof>\n", .{@tagName(tok.kind)});
                break;
            },
            else => {
                if (tok.lexeme.len > 0) {
                    io.write("{s:<16}  {s}\n", .{ @tagName(tok.kind), tok.lexeme });
                } else {
                    io.write("{s:<16}\n", .{@tagName(tok.kind)});
                }
            },
        }
    }

    if (lexer.diagnostics.hasErrors()) {
        renderDiagnostics(&lexer.diagnostics, false);
    }
}

/// lex and parse a source file, then print the AST.
fn runParse(allocator: std.mem.Allocator, path: []const u8) !void {
    const source = readSourceFile(allocator, path) orelse return;
    defer allocator.free(source);

    var result = try lexAndParse(allocator, source, false) orelse return;
    defer result.deinit(allocator);

    printer.printModule(result.module);
}

/// lex, parse, and type-check a source file. prints "ok" on success.
/// with --json, outputs diagnostics as a JSON array to stdout.
fn runCheck(allocator: std.mem.Allocator, path: []const u8, json: bool) !void {
    const source = readSourceFile(allocator, path) orelse return;
    defer allocator.free(source);

    var result = try lexAndParse(allocator, source, json) orelse return;
    defer result.deinit(allocator);

    var checker = Checker.init(allocator, source) catch {
        io.writeErr("error: checker init failed (out of memory)\n", .{});
        return;
    };
    defer checker.deinit();
    checker.source_path = path;
    const stdlib_root = findStdlibRoot(allocator, path);
    defer if (stdlib_root) |r| allocator.free(r);
    checker.stdlib_root = stdlib_root;

    checker.check(&result.module);

    if (json) {
        renderDiagnostics(&checker.diagnostics, true);
    } else if (checker.diagnostics.hasErrors()) {
        renderDiagnostics(&checker.diagnostics, false);
    } else {
        io.write("ok\n", .{});
    }
}

/// format a source file. with --check, just reports whether the file
/// would change (exit 1) without writing. otherwise writes back.
fn runFmt(allocator: std.mem.Allocator, path: []const u8, check_only: bool) !void {
    const source = readSourceFile(allocator, path) orelse return;
    defer allocator.free(source);

    const formatted = formatter.format(allocator, source) catch {
        io.writeErr("error: formatting failed (out of memory)\n", .{});
        return;
    };
    defer allocator.free(formatted);

    if (check_only) {
        if (!std.mem.eql(u8, source, formatted)) {
            io.write("{s}\n", .{path});
            std.process.exit(1);
        }
        return;
    }

    // only write if changed
    if (!std.mem.eql(u8, source, formatted)) {
        writeFile(path, formatted) catch |err| {
            io.writeErr("error: could not write '{s}': {}\n", .{ path, err });
            return;
        };
    }
}

/// lex, parse, type-check, then run lint rules. reports naming violations,
/// unused variables, missing doc comments, and deep nesting.
fn runLint(allocator: std.mem.Allocator, path: []const u8, json: bool) !void {
    const source = readSourceFile(allocator, path) orelse return;
    defer allocator.free(source);

    var result = try lexAndParse(allocator, source, json) orelse return;
    defer result.deinit(allocator);

    var checker = Checker.init(allocator, source) catch {
        io.writeErr("error: checker init failed (out of memory)\n", .{});
        return;
    };
    defer checker.deinit();
    checker.source_path = path;
    const lint_stdlib_root = findStdlibRoot(allocator, path);
    defer if (lint_stdlib_root) |r| allocator.free(r);
    checker.stdlib_root = lint_stdlib_root;

    checker.check(&result.module);

    if (checker.diagnostics.hasErrors()) {
        renderDiagnostics(&checker.diagnostics, json);
        return;
    }

    // run lint pass on the checked module.
    // use an arena for diagnostic messages so they're freed together.
    var arena: std.heap.ArenaAllocator = .init(allocator);
    defer arena.deinit();
    const lint_alloc = arena.allocator();

    var lint_diags = errors.DiagnosticList.init(lint_alloc, source);
    defer lint_diags.deinit();

    lint.lint(&result.module, &lint_diags, source);

    if (lint_diags.diagnostics.items.len > 0) {
        renderDiagnostics(&lint_diags, json);
        // exit 1 if any errors (warnings don't cause failure)
        if (lint_diags.hasErrors()) {
            std.process.exit(1);
        }
    }
}

/// lex, parse, type-check, generate C in test mode, compile, and run tests.
fn runTest(allocator: std.mem.Allocator, path: []const u8, json: bool) !void {
    const source = readSourceFile(allocator, path) orelse return;
    defer allocator.free(source);

    var result = try lexAndParse(allocator, source, json) orelse return;
    defer result.deinit(allocator);

    var checker = Checker.init(allocator, source) catch {
        io.writeErr("error: checker init failed (out of memory)\n", .{});
        return;
    };
    defer checker.deinit();
    checker.source_path = path;
    const test_stdlib_root = findStdlibRoot(allocator, path);
    defer if (test_stdlib_root) |r| allocator.free(r);
    checker.stdlib_root = test_stdlib_root;

    checker.check(&result.module);

    if (checker.diagnostics.hasErrors()) {
        renderDiagnostics(&checker.diagnostics, json);
        return;
    }

    // generate C in test mode
    var emitter = CEmitter.init(allocator, &checker.type_table, &checker.module_scope, &checker.method_types, &checker.generic_decls);
    defer emitter.deinit();
    emitter.test_mode = true;
    emitter.imported_modules = checker.imported_modules.items;

    emitter.emitModule(&result.module) catch {
        io.writeErr("error: code generation failed (out of memory)\n", .{});
        return;
    };

    // determine output paths
    const stem = stripExtension(std.fs.path.basename(path));
    const dir = std.fs.path.dirname(path) orelse ".";
    const build_dir = std.fs.path.join(allocator, &.{ dir, ".forge-build" }) catch {
        io.writeErr("error: out of memory\n", .{});
        return;
    };
    defer allocator.free(build_dir);

    std.fs.cwd().makePath(build_dir) catch |err| {
        io.writeErr("error: could not create build directory: {}\n", .{err});
        return;
    };

    // write the runtime header
    const header_path = std.fs.path.join(allocator, &.{ build_dir, "forge_runtime.h" }) catch {
        io.writeErr("error: out of memory\n", .{});
        return;
    };
    defer allocator.free(header_path);
    writeFile(header_path, runtime_header) catch |err| {
        io.writeErr("error: could not write runtime header: {}\n", .{err});
        return;
    };

    // write the generated C source
    const c_filename = std.fmt.allocPrint(allocator, "{s}_test.c", .{stem}) catch {
        io.writeErr("error: out of memory\n", .{});
        return;
    };
    defer allocator.free(c_filename);
    const c_path = std.fs.path.join(allocator, &.{ build_dir, c_filename }) catch {
        io.writeErr("error: out of memory\n", .{});
        return;
    };
    defer allocator.free(c_path);
    writeFile(c_path, emitter.getOutput()) catch |err| {
        io.writeErr("error: could not write generated C: {}\n", .{err});
        return;
    };

    // compile with zig cc
    const out_name = std.fmt.allocPrint(allocator, "{s}_test", .{stem}) catch {
        io.writeErr("error: out of memory\n", .{});
        return;
    };
    defer allocator.free(out_name);
    const out_path = std.fs.path.join(allocator, &.{ dir, out_name }) catch {
        io.writeErr("error: out of memory\n", .{});
        return;
    };
    defer allocator.free(out_path);

    const cc_result = std.process.Child.run(.{
        .allocator = allocator,
        .argv = &.{ "zig", "cc", "-w", "-o", out_path, "-I", build_dir, c_path, "-lpthread", "-lm" },
    }) catch |err| {
        io.writeErr("error: could not run zig cc: {}\n", .{err});
        return;
    };
    defer allocator.free(cc_result.stdout);
    defer allocator.free(cc_result.stderr);

    if (cc_result.term.Exited != 0) {
        io.writeErr("error: C compilation failed:\n{s}", .{cc_result.stderr});
        return;
    }

    // run the test binary
    const run_result = std.process.Child.run(.{
        .allocator = allocator,
        .argv = &.{out_path},
    }) catch |err| {
        io.writeErr("error: could not run test binary: {}\n", .{err});
        return;
    };
    defer allocator.free(run_result.stdout);
    defer allocator.free(run_result.stderr);

    // print output
    if (run_result.stdout.len > 0) {
        var buf: [io.write_buf_size]u8 = undefined;
        var w = std.fs.File.stdout().writer(&buf);
        w.interface.writeAll(run_result.stdout) catch {};
        w.interface.flush() catch {};
    }
    if (run_result.stderr.len > 0) {
        var buf: [io.write_buf_size]u8 = undefined;
        var w = std.fs.File.stderr().writer(&buf);
        w.interface.writeAll(run_result.stderr) catch {};
        w.interface.flush() catch {};
    }

    // exit with the test binary's exit code
    if (run_result.term.Exited != 0) {
        std.process.exit(1);
    }
}

/// the forge runtime header, embedded at compile time. written to the
/// build directory so the C compiler can find it via #include.
const runtime_header = @embedFile("forge_runtime.h");

/// find the stdlib root directory. checks:
///   1. FORGE_ROOT environment variable
///   2. walk up from the source file's directory looking for a `std/` dir
///   3. relative to the executable's directory (exe_dir/../std/ or exe_dir/../../std/)
/// returns the parent directory containing `std/`, or null if not found.
fn findStdlibRoot(allocator: std.mem.Allocator, source_path: []const u8) ?[]const u8 {
    // 1. check FORGE_ROOT env var
    if (std.process.getEnvVarOwned(allocator, "FORGE_ROOT")) |root| {
        // verify std/ exists under it
        const std_dir = std.fs.path.join(allocator, &.{ root, "std" }) catch {
            allocator.free(root);
            return null;
        };
        std.fs.cwd().access(std_dir, .{}) catch {
            allocator.free(std_dir);
            allocator.free(root);
            // fall through to other methods
            return findStdlibRootFromPath(allocator, source_path);
        };
        allocator.free(std_dir);
        return root;
    } else |_| {}

    return findStdlibRootFromPath(allocator, source_path);
}

/// walk up from the source file looking for a directory containing `std/`.
fn findStdlibRootFromPath(allocator: std.mem.Allocator, source_path: []const u8) ?[]const u8 {
    // resolve to an absolute path so we can walk up reliably
    const abs_path = std.fs.cwd().realpathAlloc(allocator, source_path) catch return null;
    defer allocator.free(abs_path);

    var dir: []const u8 = std.fs.path.dirname(abs_path) orelse return null;

    // walk up the directory tree (max 20 levels to avoid infinite loops)
    var depth: u32 = 0;
    while (depth < 20) : (depth += 1) {
        const std_dir = std.fs.path.join(allocator, &.{ dir, "std" }) catch return null;
        std.fs.cwd().access(std_dir, .{}) catch {
            allocator.free(std_dir);
            // go up one level
            const parent = std.fs.path.dirname(dir) orelse break;
            if (std.mem.eql(u8, parent, dir)) break; // at filesystem root
            dir = parent;
            continue;
        };
        allocator.free(std_dir);
        // found it — return an owned copy
        return allocator.dupe(u8, dir) catch return null;
    }

    return null;
}

/// lex, parse, type-check, generate C, and compile a forge source file.
/// if `run_after` is true, also executes the resulting binary.
fn runBuild(allocator: std.mem.Allocator, path: []const u8, run_after: bool, json: bool, extra_args: []const []const u8) !void {
    const source = readSourceFile(allocator, path) orelse return;
    defer allocator.free(source);

    var result = try lexAndParse(allocator, source, json) orelse return;
    defer result.deinit(allocator);

    var checker = Checker.init(allocator, source) catch {
        io.writeErr("error: checker init failed (out of memory)\n", .{});
        return;
    };
    defer checker.deinit();
    checker.source_path = path;
    const build_stdlib_root = findStdlibRoot(allocator, path);
    defer if (build_stdlib_root) |r| allocator.free(r);
    checker.stdlib_root = build_stdlib_root;

    checker.check(&result.module);

    if (checker.diagnostics.hasErrors()) {
        renderDiagnostics(&checker.diagnostics, json);
        return;
    }

    // generate C
    var emitter = CEmitter.init(allocator, &checker.type_table, &checker.module_scope, &checker.method_types, &checker.generic_decls);
    defer emitter.deinit();
    emitter.imported_modules = checker.imported_modules.items;

    emitter.emitModule(&result.module) catch {
        io.writeErr("error: code generation failed (out of memory)\n", .{});
        return;
    };

    // determine output paths
    const stem = stripExtension(std.fs.path.basename(path));

    // create a build directory next to the source
    const dir = std.fs.path.dirname(path) orelse ".";
    const build_dir = std.fs.path.join(allocator, &.{ dir, ".forge-build" }) catch {
        io.writeErr("error: out of memory\n", .{});
        return;
    };
    defer allocator.free(build_dir);

    std.fs.cwd().makePath(build_dir) catch |err| {
        io.writeErr("error: could not create build directory: {}\n", .{err});
        return;
    };

    // write the runtime header
    const header_path = std.fs.path.join(allocator, &.{ build_dir, "forge_runtime.h" }) catch {
        io.writeErr("error: out of memory\n", .{});
        return;
    };
    defer allocator.free(header_path);
    writeFile(header_path, runtime_header) catch |err| {
        io.writeErr("error: could not write runtime header: {}\n", .{err});
        return;
    };

    // write the generated C source
    const c_filename = std.fmt.allocPrint(allocator, "{s}.c", .{stem}) catch {
        io.writeErr("error: out of memory\n", .{});
        return;
    };
    defer allocator.free(c_filename);
    const c_path = std.fs.path.join(allocator, &.{ build_dir, c_filename }) catch {
        io.writeErr("error: out of memory\n", .{});
        return;
    };
    defer allocator.free(c_path);
    writeFile(c_path, emitter.getOutput()) catch |err| {
        io.writeErr("error: could not write generated C: {}\n", .{err});
        return;
    };

    // compile with zig cc
    const out_path = std.fs.path.join(allocator, &.{ dir, stem }) catch {
        io.writeErr("error: out of memory\n", .{});
        return;
    };
    defer allocator.free(out_path);

    const cc_result = std.process.Child.run(.{
        .allocator = allocator,
        .argv = &.{ "zig", "cc", "-w", "-o", out_path, "-I", build_dir, c_path, "-lpthread", "-lm" },
    }) catch |err| {
        io.writeErr("error: could not run zig cc: {}\n", .{err});
        return;
    };
    defer allocator.free(cc_result.stdout);
    defer allocator.free(cc_result.stderr);

    if (cc_result.term.Exited != 0) {
        io.writeErr("error: C compilation failed:\n{s}", .{cc_result.stderr});
        return;
    }

    if (!run_after) {
        io.write("built {s}\n", .{out_path});
        return;
    }

    // run the binary with any extra args forwarded
    var run_argv: std.ArrayList([]const u8) = .empty;
    defer run_argv.deinit(allocator);
    run_argv.append(allocator, out_path) catch {};
    for (extra_args) |a| {
        run_argv.append(allocator, a) catch {};
    }
    const run_result = std.process.Child.run(.{
        .allocator = allocator,
        .argv = run_argv.items,
    }) catch |err| {
        io.writeErr("error: could not run binary: {}\n", .{err});
        return;
    };
    defer allocator.free(run_result.stdout);
    defer allocator.free(run_result.stderr);

    // print stdout directly
    if (run_result.stdout.len > 0) {
        var buf: [io.write_buf_size]u8 = undefined;
        var w = std.fs.File.stdout().writer(&buf);
        w.interface.writeAll(run_result.stdout) catch {};
        w.interface.flush() catch {};
    }
    if (run_result.stderr.len > 0) {
        var buf: [io.write_buf_size]u8 = undefined;
        var w = std.fs.File.stderr().writer(&buf);
        w.interface.writeAll(run_result.stderr) catch {};
        w.interface.flush() catch {};
    }
}

fn writeFile(path: []const u8, content: []const u8) !void {
    const file = try std.fs.cwd().createFile(path, .{});
    defer file.close();
    try file.writeAll(content);
}

/// check if a specific flag is present in the remaining arguments.
fn hasFlag(args: anytype, flag: []const u8) bool {
    while (args.next()) |arg| {
        if (std.mem.eql(u8, arg, flag)) return true;
    }
    return false;
}

fn stripExtension(filename: []const u8) []const u8 {
    if (std.mem.lastIndexOf(u8, filename, ".")) |dot| {
        return filename[0..dot];
    }
    return filename;
}

fn printVersion() void {
    io.write("forge {s}\n", .{version});
}

fn printUsage() void {
    io.write(
        \\forge {s}
        \\
        \\usage: forge <command> [options]
        \\
        \\commands:
        \\  build <file>          compile to native binary
        \\  run <file>            compile and run
        \\  test <file>           run tests
        \\  fmt <file>            format source code
        \\  fmt <file> --check    check formatting (exit 1 if unformatted)
        \\  lint <file>           check conventions and best practices
        \\  lint <file> --json    lint with JSON output
        \\  check <file>          type check a source file
        \\  check <file> --json   type check with JSON output
        \\  lex <file>            tokenize a source file
        \\  parse <file>          parse and print AST
        \\  version               print version
        \\  help                  show this message
        \\
    , .{version});
}
