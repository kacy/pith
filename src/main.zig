const std = @import("std");

const version = "0.1.0";

pub fn main() !void {
    var args = std.process.argsWithAllocator(std.heap.page_allocator) catch {
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
    } else {
        printUsage();
    }
}

fn printVersion() void {
    var buf: [4096]u8 = undefined;
    var w = std.fs.File.stdout().writer(&buf);
    const out = &w.interface;
    out.print("forge {s}\n", .{version}) catch {};
    out.flush() catch {};
}

fn printUsage() void {
    var buf: [4096]u8 = undefined;
    var w = std.fs.File.stdout().writer(&buf);
    const out = &w.interface;
    out.print(
        \\forge {s}
        \\
        \\usage: forge <command> [options]
        \\
        \\commands:
        \\  version    print version
        \\  help       show this message
        \\
    , .{version}) catch {};
    out.flush() catch {};
}
