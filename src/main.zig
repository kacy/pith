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
const ModuleExports = checker_mod.ModuleExports;
const Export = checker_mod.Export;
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
}

const version = "0.1.0";

/// max source file size the compiler will read (10 MiB). prevents
/// accidental reads of large binary files.
const max_source_size = 10 * 1024 * 1024;

fn renderDiagnostics(diags: *const errors.DiagnosticList) void {
    var buf: [io.write_buf_size]u8 = undefined;
    var w = std.fs.File.stderr().writer(&buf);
    const out = &w.interface;
    diags.render(out) catch {};
    out.flush() catch {};
}

fn renderDiagnosticsForPath(path: []const u8, diags: *const errors.DiagnosticList) void {
    io.writeErr("in {s}:\n", .{path});
    renderDiagnostics(diags);
}

fn readSourceFile(allocator: std.mem.Allocator, path: []const u8) ?[]const u8 {
    return std.fs.cwd().readFileAlloc(allocator, path, max_source_size) catch |err| {
        io.writeErr("error: could not read '{s}': {}\n", .{ path, err });
        return null;
    };
}

const ModuleCacheEntry = struct {
    path: []const u8,
    source: []const u8,
    parse: ParseResult,
    exports: ModuleExports,
    had_errors: bool,
};

const ModuleLoader = struct {
    allocator: std.mem.Allocator,
    entries: std.ArrayList(ModuleCacheEntry),
    index_by_path: std.StringHashMap(usize),
    states: std.StringHashMap(State),
    had_errors: bool,

    const State = enum {
        visiting,
        done,
    };

    const ModuleError = error{
        ImportCycle,
        ParseFailed,
        ReadFailed,
        OutOfMemory,
    };

    fn init(allocator: std.mem.Allocator) ModuleLoader {
        return .{
            .allocator = allocator,
            .entries = .empty,
            .index_by_path = std.StringHashMap(usize).init(allocator),
            .states = std.StringHashMap(State).init(allocator),
            .had_errors = false,
        };
    }

    fn deinit(self: *ModuleLoader) void {
        for (self.entries.items) |*entry| {
            entry.exports.deinit();
            entry.parse.deinit(self.allocator);
            self.allocator.free(entry.source);
            self.allocator.free(entry.path);
        }
        self.entries.deinit(self.allocator);
        self.index_by_path.deinit();
        self.states.deinit();
    }

    fn checkRoot(self: *ModuleLoader, root_path: []const u8) bool {
        const canonical = self.canonicalPath(root_path) orelse return false;
        defer self.allocator.free(canonical);

        _ = self.loadModule(canonical) catch {
            self.had_errors = true;
            return false;
        };

        return !self.had_errors;
    }

    fn loadModule(self: *ModuleLoader, canonical_path: []const u8) ModuleError!*const ModuleCacheEntry {
        if (self.states.get(canonical_path)) |state| {
            return switch (state) {
                .done => &self.entries.items[self.index_by_path.get(canonical_path).?],
                .visiting => error.ImportCycle,
            };
        }

        try self.states.put(canonical_path, .visiting);
        errdefer _ = self.states.remove(canonical_path);

        const owned_path = try self.allocator.dupe(u8, canonical_path);
        errdefer self.allocator.free(owned_path);

        const source = readSourceFile(self.allocator, canonical_path) orelse return ModuleError.ReadFailed;
        errdefer self.allocator.free(source);

        var parse = try lexAndParse(self.allocator, source) orelse {
            self.had_errors = true;
            return ModuleError.ParseFailed;
        };
        errdefer parse.deinit(self.allocator);

        var checker = Checker.init(self.allocator, source) catch return ModuleError.OutOfMemory;
        defer checker.deinit();

        for (parse.module.imports) |imp| {
            self.applyImport(&checker, canonical_path, imp) catch |err| switch (err) {
                error.ImportCycle => {
                    checker.diagnostics.addError(imp.location, "import cycle detected") catch {};
                    self.had_errors = true;
                },
                error.ReadFailed, error.ParseFailed => {
                    checker.diagnostics.addError(imp.location, "failed to load imported module") catch {};
                    self.had_errors = true;
                },
                else => return err,
            };
        }

        checker.check(&parse.module);
        const module_exports = try Checker.collectExports(&parse.module, self.allocator);

        const entry_index = self.entries.items.len;
        try self.entries.append(self.allocator, .{
            .path = owned_path,
            .source = source,
            .parse = parse,
            .exports = module_exports,
            .had_errors = checker.diagnostics.hasErrors(),
        });
        try self.index_by_path.put(self.entries.items[entry_index].path, entry_index);
        try self.states.put(self.entries.items[entry_index].path, .done);

        if (checker.diagnostics.hasErrors()) {
            renderDiagnosticsForPath(canonical_path, &checker.diagnostics);
            self.had_errors = true;
        }

        return &self.entries.items[entry_index];
    }

    fn applyImport(self: *ModuleLoader, checker: *Checker, importer_path: []const u8, imp: ast.ImportDecl) ModuleError!void {
        const resolved_path = try self.resolveImportPath(importer_path, imp);
        defer self.allocator.free(resolved_path);

        const imported = try self.loadModule(resolved_path);

        switch (imp.kind) {
            .simple => |simple| {
                const alias = simple.alias orelse simple.path[simple.path.len - 1];
                checker.importNamespace(alias, &imported.exports, imp.location);
            },
            .from => |from| {
                for (from.names) |name| {
                    const exported_symbol = imported.exports.get(name.name) orelse {
                        const message = std.fmt.allocPrint(checker.arena.allocator(), "module '{s}' has no exported symbol '{s}'", .{
                            imported.path,
                            name.name,
                        }) catch "module has no exported symbol";
                        checker.diagnostics.addError(name.location, message) catch {};
                        continue;
                    };
                    checker.importSymbol(name.alias orelse name.name, exported_symbol, name.location);
                }
            },
        }
    }

    fn resolveImportPath(self: *ModuleLoader, importer_path: []const u8, imp: ast.ImportDecl) ModuleError![]const u8 {
        const path_parts = switch (imp.kind) {
            .simple => |simple| simple.path,
            .from => |from| from.path,
        };

        var rel_path = std.ArrayList(u8).empty;
        defer rel_path.deinit(self.allocator);
        for (path_parts, 0..) |part, i| {
            if (i > 0) try rel_path.append(self.allocator, std.fs.path.sep);
            try rel_path.appendSlice(self.allocator, part);
        }
        try rel_path.appendSlice(self.allocator, ".fg");

        const importer_dir = std.fs.path.dirname(importer_path) orelse ".";
        const joined = try std.fs.path.join(self.allocator, &.{ importer_dir, rel_path.items });
        defer self.allocator.free(joined);

        return self.canonicalPath(joined) orelse ModuleError.ReadFailed;
    }

    fn canonicalPath(self: *ModuleLoader, path: []const u8) ?[]const u8 {
        return std.fs.cwd().realpathAlloc(self.allocator, path) catch |err| {
            io.writeErr("error: could not resolve '{s}': {}\n", .{ path, err });
            return null;
        };
    }
};

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
/// (diagnostics are rendered to stderr before returning).
fn lexAndParse(allocator: std.mem.Allocator, source: []const u8) !?ParseResult {
    // lex
    var lexer = try Lexer.init(source, allocator);
    defer lexer.deinit();
    const tokens = try lexer.tokenize();

    if (lexer.diagnostics.hasErrors()) {
        renderDiagnostics(&lexer.diagnostics);
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
        renderDiagnostics(&parser.diagnostics);
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
        try runCheck(allocator, file_path);
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
        renderDiagnostics(&lexer.diagnostics);
    }
}

/// lex and parse a source file, then print the AST.
fn runParse(allocator: std.mem.Allocator, path: []const u8) !void {
    const source = readSourceFile(allocator, path) orelse return;
    defer allocator.free(source);

    var result = try lexAndParse(allocator, source) orelse return;
    defer result.deinit(allocator);

    printer.printModule(result.module);
}

/// lex, parse, and type-check a source file. prints "ok" on success.
fn runCheck(allocator: std.mem.Allocator, path: []const u8) !void {
    var loader = ModuleLoader.init(allocator);
    defer loader.deinit();

    if (loader.checkRoot(path)) {
        io.write("ok\n", .{});
    }
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
        \\  check <file>   type check a source file
        \\  lex <file>     tokenize a source file
        \\  parse <file>   parse and print AST
        \\  version        print version
        \\  help           show this message
        \\
    , .{version});
}

const TestFile = struct {
    path: []const u8,
    contents: []const u8,
};

fn writeTestFile(dir: std.fs.Dir, path: []const u8, contents: []const u8) !void {
    if (std.fs.path.dirname(path)) |parent| {
        try dir.makePath(parent);
    }
    try dir.writeFile(.{
        .sub_path = path,
        .data = contents,
    });
}

fn checkTempModuleGraph(files: []const TestFile, root: []const u8) !bool {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    for (files) |file| {
        try writeTestFile(tmp.dir, file.path, file.contents);
    }

    const root_path = try tmp.dir.realpathAlloc(std.testing.allocator, root);
    defer std.testing.allocator.free(root_path);

    var loader = ModuleLoader.init(std.testing.allocator);
    defer loader.deinit();

    return loader.checkRoot(root_path);
}

test "check supports import with implicit alias" {
    const files = [_]TestFile{
        .{
            .path = "main.fg",
            .contents =
            \\import lib.math
            \\
            \\fn main():
            \\    answer := math.answer()
            \\    print("{answer}")
            ,
        },
        .{
            .path = "lib/math.fg",
            .contents =
            \\pub fn answer() -> Int:
            \\    return 42
            ,
        },
    };

    try std.testing.expect(try checkTempModuleGraph(&files, "main.fg"));
}

test "check supports import with explicit alias and namespaced constructor" {
    const files = [_]TestFile{
        .{
            .path = "main.fg",
            .contents =
            \\import lib.types as t
            \\
            \\fn main():
            \\    point := t.Point(1, 2)
            \\    print("{point.x}")
            ,
        },
        .{
            .path = "lib/types.fg",
            .contents =
            \\pub struct Point:
            \\    pub x: Int
            \\    pub y: Int
            ,
        },
    };

    try std.testing.expect(try checkTempModuleGraph(&files, "main.fg"));
}

test "check supports from import and alias" {
    const files = [_]TestFile{
        .{
            .path = "main.fg",
            .contents =
            \\from lib.math import answer as get_answer
            \\
            \\fn main():
            \\    value := get_answer()
            \\    print("{value}")
            ,
        },
        .{
            .path = "lib/math.fg",
            .contents =
            \\pub fn answer() -> Int:
            \\    return 42
            ,
        },
    };

    try std.testing.expect(try checkTempModuleGraph(&files, "main.fg"));
}

test "check supports from import of public type" {
    const files = [_]TestFile{
        .{
            .path = "main.fg",
            .contents =
            \\from lib.types import Point
            \\
            \\fn main():
            \\    point := Point(1, 2)
            \\    print("{point.y}")
            ,
        },
        .{
            .path = "lib/types.fg",
            .contents =
            \\pub struct Point:
            \\    pub x: Int
            \\    pub y: Int
            ,
        },
    };

    try std.testing.expect(try checkTempModuleGraph(&files, "main.fg"));
}

test "check rejects non-public import" {
    const files = [_]TestFile{
        .{
            .path = "main.fg",
            .contents =
            \\from lib.math import answer
            ,
        },
        .{
            .path = "lib/math.fg",
            .contents =
            \\fn answer() -> Int:
            \\    return 42
            ,
        },
    };

    try std.testing.expect(!(try checkTempModuleGraph(&files, "main.fg")));
}

test "check rejects duplicate imported names" {
    const files = [_]TestFile{
        .{
            .path = "main.fg",
            .contents =
            \\from lib.math import answer
            \\from lib.other import answer
            ,
        },
        .{
            .path = "lib/math.fg",
            .contents =
            \\pub fn answer() -> Int:
            \\    return 1
            ,
        },
        .{
            .path = "lib/other.fg",
            .contents =
            \\pub fn answer() -> Int:
            \\    return 2
            ,
        },
    };

    try std.testing.expect(!(try checkTempModuleGraph(&files, "main.fg")));
}

test "check rejects missing module file" {
    const files = [_]TestFile{
        .{
            .path = "main.fg",
            .contents =
            \\import lib.missing
            ,
        },
    };

    try std.testing.expect(!(try checkTempModuleGraph(&files, "main.fg")));
}

test "check rejects missing exported symbol" {
    const files = [_]TestFile{
        .{
            .path = "main.fg",
            .contents =
            \\from lib.math import nope
            ,
        },
        .{
            .path = "lib/math.fg",
            .contents =
            \\pub fn answer() -> Int:
            \\    return 42
            ,
        },
    };

    try std.testing.expect(!(try checkTempModuleGraph(&files, "main.fg")));
}

test "check rejects import cycles" {
    const files = [_]TestFile{
        .{
            .path = "main.fg",
            .contents =
            \\import lib.a
            ,
        },
        .{
            .path = "lib/a.fg",
            .contents =
            \\import b
            \\pub fn answer() -> Int:
            \\    return 1
            ,
        },
        .{
            .path = "lib/b.fg",
            .contents =
            \\import a
            \\pub fn answer() -> Int:
            \\    return 2
            ,
        },
    };

    try std.testing.expect(!(try checkTempModuleGraph(&files, "main.fg")));
}

test "check reuses cached imported modules" {
    const files = [_]TestFile{
        .{
            .path = "main.fg",
            .contents =
            \\import lib.math as left
            \\import lib.math as right
            \\
            \\fn main():
            \\    total := left.answer() + right.answer()
            \\    print("{total}")
            ,
        },
        .{
            .path = "lib/math.fg",
            .contents =
            \\pub fn answer() -> Int:
            \\    return 21
            ,
        },
    };

    try std.testing.expect(try checkTempModuleGraph(&files, "main.fg"));
}
