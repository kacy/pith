const std = @import("std");
const build_support = @import("../build_support.zig");
const CEmitter = @import("../codegen.zig").CEmitter;
const io = @import("../io.zig");
const pipeline = @import("../pipeline.zig");

pub fn run(
    allocator: std.mem.Allocator,
    path: []const u8,
    run_after: bool,
    json: bool,
    extra_args: []const []const u8,
) !void {
    var checked = try pipeline.checkFile(allocator, path, json) orelse return;
    defer checked.deinit();

    if (checked.checker.diagnostics.hasErrors()) {
        pipeline.renderDiagnostics(&checked.checker.diagnostics, json);
        return;
    }

    var emitter = CEmitter.init(
        allocator,
        &checked.checker.type_table,
        &checked.checker.module_scope,
        &checked.checker.method_types,
        &checked.checker.generic_decls,
    );
    defer emitter.deinit();
    emitter.imported_modules = checked.checker.imported_modules.items;

    emitter.emitModule(&checked.parsed.parse_result.module) catch {
        io.writeErr("error: code generation failed (out of memory)\n", .{});
        return;
    };

    var paths = build_support.BuildPaths.init(allocator, path, .build) catch {
        io.writeErr("error: out of memory\n", .{});
        return;
    };
    defer paths.deinit();

    build_support.ensureBuildDir(paths.build_dir) catch |err| {
        io.writeErr("error: could not create build directory: {}\n", .{err});
        return;
    };
    build_support.writeRuntimeHeader(paths.header_path) catch |err| {
        io.writeErr("error: could not write runtime header: {}\n", .{err});
        return;
    };
    build_support.writeGeneratedC(paths.c_path, emitter.getOutput()) catch |err| {
        io.writeErr("error: could not write generated C: {}\n", .{err});
        return;
    };

    const cc_result = build_support.compileGeneratedC(allocator, &paths) catch |err| {
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
        io.write("built {s}\n", .{paths.out_path});
        return;
    }

    var argv = std.ArrayList([]const u8).empty;
    defer argv.deinit(allocator);

    argv.append(allocator, paths.out_path) catch {
        io.writeErr("error: out of memory\n", .{});
        return;
    };
    for (extra_args) |arg| {
        argv.append(allocator, arg) catch {
            io.writeErr("error: out of memory\n", .{});
            return;
        };
    }

    const run_result = build_support.runBinary(allocator, argv.items) catch |err| {
        io.writeErr("error: could not run binary: {}\n", .{err});
        return;
    };
    defer allocator.free(run_result.stdout);
    defer allocator.free(run_result.stderr);

    build_support.printCapturedOutput(run_result.stdout, run_result.stderr);
}
