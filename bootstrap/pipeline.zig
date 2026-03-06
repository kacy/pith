const std = @import("std");
const ast = @import("ast.zig");
const checker_mod = @import("checker.zig");
const errors = @import("errors.zig");
const io = @import("io.zig");
const lexer_mod = @import("lexer.zig");
const Parser = @import("parser.zig").Parser;

const Checker = checker_mod.Checker;
const Lexer = lexer_mod.Lexer;
const Token = lexer_mod.Token;

pub const max_source_size = 10 * 1024 * 1024;

pub const ParseResult = struct {
    allocator: std.mem.Allocator,
    module: ast.Module,
    tokens: []const Token,
    arena: std.heap.ArenaAllocator,

    pub fn deinit(self: *ParseResult) void {
        self.allocator.free(self.tokens);
        self.arena.deinit();
    }
};

pub const ParsedModule = struct {
    allocator: std.mem.Allocator,
    source: []const u8,
    parse_result: ParseResult,

    pub fn deinit(self: *ParsedModule) void {
        self.parse_result.deinit();
        self.allocator.free(self.source);
    }
};

pub const CheckedModule = struct {
    allocator: std.mem.Allocator,
    parsed: ParsedModule,
    checker: Checker,
    stdlib_root: ?[]const u8,

    pub fn deinit(self: *CheckedModule) void {
        self.checker.deinit();
        if (self.stdlib_root) |root| self.allocator.free(root);
        self.parsed.deinit();
    }
};

pub fn renderDiagnostics(diags: *const errors.DiagnosticList, json: bool) void {
    if (json) {
        var buf: [io.write_buf_size]u8 = undefined;
        var writer = std.fs.File.stdout().writer(&buf);
        diags.renderJson(&writer.interface) catch {};
        writer.interface.flush() catch {};
        return;
    }

    var buf: [io.write_buf_size]u8 = undefined;
    var writer = std.fs.File.stderr().writer(&buf);
    diags.render(&writer.interface) catch {};
    writer.interface.flush() catch {};
}

pub fn readSourceFile(allocator: std.mem.Allocator, path: []const u8) ?[]const u8 {
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

pub fn parseFile(allocator: std.mem.Allocator, path: []const u8, json: bool) !?ParsedModule {
    const source = readSourceFile(allocator, path) orelse return null;
    errdefer allocator.free(source);

    const parse_result = try lexAndParse(allocator, source, json) orelse return null;
    return .{
        .allocator = allocator,
        .source = source,
        .parse_result = parse_result,
    };
}

pub fn checkFile(allocator: std.mem.Allocator, path: []const u8, json: bool) !?CheckedModule {
    var parsed = try parseFile(allocator, path, json) orelse return null;
    errdefer parsed.deinit();

    var checker = Checker.init(allocator, parsed.source) catch {
        io.writeErr("error: checker init failed (out of memory)\n", .{});
        return null;
    };
    errdefer checker.deinit();

    checker.source_path = path;
    const stdlib_root = findStdlibRoot(allocator, path);
    checker.stdlib_root = stdlib_root;
    checker.check(&parsed.parse_result.module);

    return .{
        .allocator = allocator,
        .parsed = parsed,
        .checker = checker,
        .stdlib_root = stdlib_root,
    };
}

fn lexAndParse(allocator: std.mem.Allocator, source: []const u8, json: bool) !?ParseResult {
    var lexer = try Lexer.init(source, allocator);
    defer lexer.deinit();
    const tokens = try lexer.tokenize();

    if (lexer.diagnostics.hasErrors()) {
        renderDiagnostics(&lexer.diagnostics, json);
        allocator.free(tokens);
        return null;
    }

    var arena = std.heap.ArenaAllocator.init(allocator);
    errdefer arena.deinit();

    var parser = Parser.init(tokens, source, arena.allocator());
    defer parser.deinit();

    const module = parser.parseModule() catch {
        io.writeErr("error: parse failed (out of memory)\n", .{});
        allocator.free(tokens);
        return null;
    };

    if (parser.diagnostics.hasErrors()) {
        renderDiagnostics(&parser.diagnostics, json);
        allocator.free(tokens);
        return null;
    }

    return .{
        .allocator = allocator,
        .module = module,
        .tokens = tokens,
        .arena = arena,
    };
}

pub fn findStdlibRoot(allocator: std.mem.Allocator, source_path: []const u8) ?[]const u8 {
    if (std.process.getEnvVarOwned(allocator, "FORGE_ROOT")) |root| {
        const std_dir = std.fs.path.join(allocator, &.{ root, "std" }) catch {
            allocator.free(root);
            return null;
        };
        defer allocator.free(std_dir);

        std.fs.cwd().access(std_dir, .{}) catch {
            allocator.free(root);
            return findStdlibRootFromPath(allocator, source_path);
        };

        return root;
    } else |_| {}

    return findStdlibRootFromPath(allocator, source_path);
}

fn findStdlibRootFromPath(allocator: std.mem.Allocator, source_path: []const u8) ?[]const u8 {
    const abs_path = std.fs.cwd().realpathAlloc(allocator, source_path) catch return null;
    defer allocator.free(abs_path);

    var dir: []const u8 = std.fs.path.dirname(abs_path) orelse return null;
    var depth: u32 = 0;

    while (depth < 20) : (depth += 1) {
        const std_dir = std.fs.path.join(allocator, &.{ dir, "std" }) catch return null;
        defer allocator.free(std_dir);

        if (std.fs.cwd().access(std_dir, .{})) |_| {
            return allocator.dupe(u8, dir) catch return null;
        } else |_| {
            const parent = std.fs.path.dirname(dir) orelse break;
            if (std.mem.eql(u8, parent, dir)) break;
            dir = parent;
        }
    }

    return null;
}
