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
    _ = @import("codegen.zig");
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

pub fn main() !void {
    var gpa: std.heap.GeneralPurposeAllocator(.{}) = .init;
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

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
        try runBuild(allocator, file_path, false, json);
    } else if (std.mem.eql(u8, cmd, "run")) {
        const file_path = args.next() orelse {
            io.writeErr("error: forge run requires a file path\n", .{});
            return;
        };
        const json = hasFlag(&args, "--json");
        try runBuild(allocator, file_path, true, json);
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

    checker.check(&result.module);

    if (json) {
        renderDiagnostics(&checker.diagnostics, true);
    } else if (checker.diagnostics.hasErrors()) {
        renderDiagnostics(&checker.diagnostics, false);
    } else {
        io.write("ok\n", .{});
    }
}

/// the forge runtime header, embedded at compile time. written to the
/// build directory so the C compiler can find it via #include.
const runtime_header = @embedFile("forge_runtime.h");

/// lex, parse, type-check, generate C, and compile a forge source file.
/// if `run_after` is true, also executes the resulting binary.
fn runBuild(allocator: std.mem.Allocator, path: []const u8, run_after: bool, json: bool) !void {
    const source = readSourceFile(allocator, path) orelse return;
    defer allocator.free(source);

    var result = try lexAndParse(allocator, source, json) orelse return;
    defer result.deinit(allocator);

    var checker = Checker.init(allocator, source) catch {
        io.writeErr("error: checker init failed (out of memory)\n", .{});
        return;
    };
    defer checker.deinit();

    checker.check(&result.module);

    if (checker.diagnostics.hasErrors()) {
        renderDiagnostics(&checker.diagnostics, json);
        return;
    }

    // generate C
    var emitter = CEmitter.init(allocator, &checker.type_table, &checker.module_scope);
    defer emitter.deinit();

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
        .argv = &.{ "zig", "cc", "-o", out_path, "-I", build_dir, c_path },
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

    // run the binary
    const run_result = std.process.Child.run(.{
        .allocator = allocator,
        .argv = &.{out_path},
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
        \\  check <file>          type check a source file
        \\  check <file> --json   type check with JSON output
        \\  lex <file>            tokenize a source file
        \\  parse <file>          parse and print AST
        \\  version               print version
        \\  help                  show this message
        \\
    , .{version});
}
