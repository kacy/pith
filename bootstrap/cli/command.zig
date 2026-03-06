const std = @import("std");

pub const Command = enum {
    build,
    check,
    fmt,
    help,
    lex,
    lint,
    parse,
    run,
    @"test",
    version,

    const lookup = std.StaticStringMap(Command).initComptime(.{
        .{ "build", .build },
        .{ "check", .check },
        .{ "fmt", .fmt },
        .{ "help", .help },
        .{ "--help", .help },
        .{ "lex", .lex },
        .{ "lint", .lint },
        .{ "parse", .parse },
        .{ "run", .run },
        .{ "test", .@"test" },
        .{ "version", .version },
        .{ "--version", .version },
    });

    pub fn fromString(raw: []const u8) ?Command {
        return lookup.get(raw);
    }
};
