const std = @import("std");

fn generateRuntimeAbi(allocator: std.mem.Allocator) ![]const u8 {
    const manifest = try std.fs.cwd().readFileAlloc(allocator, "../runtime-abi/list_layout.json", 4096);
    defer allocator.free(manifest);

    const parsed = try std.json.parseFromSlice(std.json.Value, allocator, manifest, .{});
    defer parsed.deinit();
    const obj = parsed.value.object;

    const list_magic = obj.get("list_magic") orelse return error.MissingAbiKey;
    const elem_size_offset = obj.get("elem_size_offset") orelse return error.MissingAbiKey;
    const values8_ptr_offset = obj.get("values8_ptr_offset") orelse return error.MissingAbiKey;
    const values8_len_offset = obj.get("values8_len_offset") orelse return error.MissingAbiKey;

    return std.fmt.allocPrint(
        allocator,
        \\pub const list_magic: u32 = {s};
        \\pub const elem_size_offset: usize = {};
        \\pub const values8_ptr_offset: usize = {};
        \\pub const values8_len_offset: usize = {};
        \\
    ,
        .{
            list_magic.string,
            elem_size_offset.integer,
            values8_ptr_offset.integer,
            values8_len_offset.integer,
        },
    );
}

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});
    const write_files = b.addWriteFiles();
    const abi_source = generateRuntimeAbi(b.allocator) catch @panic("failed to generate runtime abi");
    const abi_file = write_files.add("runtime_abi.zig", abi_source);
    const root_module = b.createModule(.{
        .root_source_file = b.path("src/lib.zig"),
        .target = target,
        .optimize = optimize,
    });
    root_module.addImport("runtime_abi", b.createModule(.{
        .root_source_file = abi_file,
        .target = target,
        .optimize = optimize,
    }));

    const lib = b.addLibrary(.{
        .linkage = .static,
        .name = "forge_runtime_zig",
        .root_module = root_module,
    });
    lib.linkLibC();
    b.installArtifact(lib);
}
