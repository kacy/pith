// build — zig build configuration for the forge compiler

const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    const exe = b.addExecutable(.{
        .name = "forge",
        .root_module = b.createModule(.{
            .root_source_file = b.path("bootstrap/main.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });

    b.installArtifact(exe);

    // zig build release — optimized binary for day-to-day use
    const release_exe = b.addExecutable(.{
        .name = "forge",
        .root_module = b.createModule(.{
            .root_source_file = b.path("bootstrap/main.zig"),
            .target = target,
            .optimize = .ReleaseFast,
        }),
    });
    const release_step = b.step("release", "build optimized forge binary");
    release_step.dependOn(&b.addInstallArtifact(release_exe, .{}).step);

    // zig build run -- <args>
    const run_cmd = b.addRunArtifact(exe);
    run_cmd.step.dependOn(b.getInstallStep());
    if (b.args) |args| {
        run_cmd.addArgs(args);
    }
    const run_step = b.step("run", "run the forge compiler");
    run_step.dependOn(&run_cmd.step);

    // zig build test
    const exe_tests = b.addTest(.{
        .root_module = b.createModule(.{
            .root_source_file = b.path("bootstrap/main.zig"),
            .target = target,
            .optimize = optimize,
        }),
    });
    const run_tests = b.addRunArtifact(exe_tests);
    const test_step = b.step("test", "run unit tests");
    test_step.dependOn(&run_tests.step);
}
