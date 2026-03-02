// types — type representation for the forge type system
//
// index-based approach: TypeId is a u32 index into a flat array of types.
// equality is integer comparison. builtins are pre-registered at known
// indices so TypeId.int is a compile-time constant.

const std = @import("std");

// ---------------------------------------------------------------
// type identifiers
// ---------------------------------------------------------------

/// a handle to a type in the type table. builtins have known indices,
/// user-defined types are allocated starting at `first_user`.
pub const TypeId = enum(u32) {
    // primitive types
    int = 0,
    uint = 1,
    float = 2,
    bool = 3,
    string = 4,
    bytes = 5,
    void = 6,

    // sized integers
    int8 = 7,
    int16 = 8,
    int32 = 9,
    int64 = 10,
    uint8 = 11,
    uint16 = 12,
    uint32 = 13,
    uint64 = 14,

    // error sentinel — suppresses cascading diagnostics
    err = 15,

    // user-defined types start here
    _,

    pub const first_user: u32 = 16;

    pub fn index(self: TypeId) u32 {
        return @intFromEnum(self);
    }

    pub fn fromIndex(i: u32) TypeId {
        return @enumFromInt(i);
    }

    /// true if this is the error sentinel (used to suppress cascading errors)
    pub fn isErr(self: TypeId) bool {
        return self == .err;
    }

    /// true if this is a numeric type (any int, uint, float, or sized variant)
    pub fn isNumeric(self: TypeId) bool {
        return switch (self) {
            .int, .uint, .float, .int8, .int16, .int32, .int64, .uint8, .uint16, .uint32, .uint64 => true,
            else => false,
        };
    }

    /// true if this is an integer type (not float)
    pub fn isInteger(self: TypeId) bool {
        return switch (self) {
            .int, .uint, .int8, .int16, .int32, .int64, .uint8, .uint16, .uint32, .uint64 => true,
            else => false,
        };
    }
};

// ---------------------------------------------------------------
// type representations
// ---------------------------------------------------------------

pub const Field = struct {
    name: []const u8,
    type_id: TypeId,
    is_pub: bool,
    is_mut: bool,
};

pub const Variant = struct {
    name: []const u8,
    fields: []const TypeId,
};

/// a type in the forge type system. stored in the TypeTable as a flat array.
pub const Type = union(enum) {
    /// built-in scalar type (Int, Float, Bool, String, etc.)
    primitive: struct {
        name: []const u8,
    },
    /// user-defined struct with named, typed fields.
    @"struct": struct {
        name: []const u8,
        fields: []const Field,
    },
    /// algebraic data type — a set of named variants, each with optional fields.
    @"enum": struct {
        name: []const u8,
        variants: []const Variant,
    },
    /// callable — parameter types and a return type.
    function: struct {
        param_types: []const TypeId,
        return_type: TypeId,
    },
    /// T? — a value that may be absent (sugar for Option[T]).
    optional: struct {
        inner: TypeId,
    },
    /// T! or T!E — a value-or-error (sugar for Result[T, Error]).
    result: struct {
        ok_type: TypeId,
        err_type: TypeId,
    },
    /// (A, B, C) — fixed-size, heterogeneous collection.
    tuple: struct {
        elements: []const TypeId,
    },
    /// Task[T] — a spawned concurrent computation that yields T.
    task: struct {
        inner: TypeId,
    },
    /// Channel[T] — a typed message-passing channel.
    channel: struct {
        inner: TypeId,
    },
};

// ---------------------------------------------------------------
// type table
// ---------------------------------------------------------------

/// the central type registry. all types in a program live here as a flat
/// array indexed by TypeId. builtins are pre-registered at init.
pub const TypeTable = struct {
    types: std.ArrayList(Type),
    name_map: std.StringHashMap(TypeId),
    allocator: std.mem.Allocator,

    pub fn init(allocator: std.mem.Allocator) !TypeTable {
        var table = TypeTable{
            .types = .empty,
            .name_map = std.StringHashMap(TypeId).init(allocator),
            .allocator = allocator,
        };
        try table.registerBuiltins();
        return table;
    }

    pub fn deinit(self: *TypeTable) void {
        self.types.deinit(self.allocator);
        self.name_map.deinit();
    }

    /// add a type to the table and return its id.
    pub fn addType(self: *TypeTable, ty: Type) !TypeId {
        const id = TypeId.fromIndex(@intCast(self.types.items.len));
        try self.types.append(self.allocator, ty);
        return id;
    }

    /// register a name → type mapping.
    pub fn register(self: *TypeTable, name: []const u8, id: TypeId) !void {
        try self.name_map.put(name, id);
    }

    /// look up a type by name.
    pub fn lookup(self: *const TypeTable, name: []const u8) ?TypeId {
        return self.name_map.get(name);
    }

    /// get the Type data for a given id.
    pub fn get(self: *const TypeTable, id: TypeId) ?Type {
        const idx = id.index();
        if (idx >= self.types.items.len) return null;
        return self.types.items[idx];
    }

    /// get a human-readable name for a type (used in error messages).
    pub fn typeName(self: *const TypeTable, id: TypeId) []const u8 {
        // builtins have well-known names
        return switch (id) {
            .int => "Int",
            .uint => "UInt",
            .float => "Float",
            .bool => "Bool",
            .string => "String",
            .bytes => "Bytes",
            .void => "Void",
            .int8 => "Int8",
            .int16 => "Int16",
            .int32 => "Int32",
            .int64 => "Int64",
            .uint8 => "UInt8",
            .uint16 => "UInt16",
            .uint32 => "UInt32",
            .uint64 => "UInt64",
            .err => "<error>",
            _ => {
                // user-defined type — look up in the table
                if (self.get(id)) |ty| {
                    return switch (ty) {
                        .primitive => |p| p.name,
                        .@"struct" => |s| s.name,
                        .@"enum" => |e| e.name,
                        .function => "fn",
                        .optional => "optional",
                        .result => "result",
                        .tuple => "tuple",
                        .task => "Task",
                        .channel => "Channel",
                    };
                }
                return "<unknown>";
            },
        };
    }

    // -- builtin registration --

    fn registerBuiltins(self: *TypeTable) !void {
        // the order here must match the TypeId enum values exactly
        const builtins = [_]struct { name: []const u8 }{
            .{ .name = "Int" },
            .{ .name = "UInt" },
            .{ .name = "Float" },
            .{ .name = "Bool" },
            .{ .name = "String" },
            .{ .name = "Bytes" },
            .{ .name = "Void" },
            .{ .name = "Int8" },
            .{ .name = "Int16" },
            .{ .name = "Int32" },
            .{ .name = "Int64" },
            .{ .name = "UInt8" },
            .{ .name = "UInt16" },
            .{ .name = "UInt32" },
            .{ .name = "UInt64" },
            .{ .name = "<error>" },
        };

        for (builtins) |b| {
            const id = try self.addType(.{ .primitive = .{ .name = b.name } });
            try self.name_map.put(b.name, id);
        }

        // sanity check: the first user slot is right after the builtins
        std.debug.assert(self.types.items.len == TypeId.first_user);
    }
};

// ---------------------------------------------------------------
// tests
// ---------------------------------------------------------------

test "builtin types are registered at known indices" {
    var table = try TypeTable.init(std.testing.allocator);
    defer table.deinit();

    try std.testing.expectEqual(TypeId.int, table.lookup("Int").?);
    try std.testing.expectEqual(TypeId.uint, table.lookup("UInt").?);
    try std.testing.expectEqual(TypeId.float, table.lookup("Float").?);
    try std.testing.expectEqual(TypeId.bool, table.lookup("Bool").?);
    try std.testing.expectEqual(TypeId.string, table.lookup("String").?);
    try std.testing.expectEqual(TypeId.bytes, table.lookup("Bytes").?);
    try std.testing.expectEqual(TypeId.void, table.lookup("Void").?);
    try std.testing.expectEqual(TypeId.int8, table.lookup("Int8").?);
    try std.testing.expectEqual(TypeId.uint64, table.lookup("UInt64").?);
}

test "typeName returns correct names for builtins" {
    var table = try TypeTable.init(std.testing.allocator);
    defer table.deinit();

    try std.testing.expectEqualStrings("Int", table.typeName(.int));
    try std.testing.expectEqualStrings("String", table.typeName(.string));
    try std.testing.expectEqualStrings("Bool", table.typeName(.bool));
    try std.testing.expectEqualStrings("Void", table.typeName(.void));
    try std.testing.expectEqualStrings("<error>", table.typeName(.err));
}

test "addType creates user-defined types" {
    var table = try TypeTable.init(std.testing.allocator);
    defer table.deinit();

    const id = try table.addType(.{ .@"struct" = .{
        .name = "Point",
        .fields = &.{},
    } });
    try table.register("Point", id);

    try std.testing.expectEqual(id, table.lookup("Point").?);
    try std.testing.expectEqual(TypeId.first_user, id.index());
    try std.testing.expectEqualStrings("Point", table.typeName(id));
}

test "TypeId helper methods" {
    try std.testing.expect(TypeId.int.isNumeric());
    try std.testing.expect(TypeId.float.isNumeric());
    try std.testing.expect(TypeId.uint64.isNumeric());
    try std.testing.expect(!TypeId.string.isNumeric());
    try std.testing.expect(!TypeId.bool.isNumeric());

    try std.testing.expect(TypeId.int.isInteger());
    try std.testing.expect(!TypeId.float.isInteger());

    try std.testing.expect(TypeId.err.isErr());
    try std.testing.expect(!TypeId.int.isErr());
}

test "lookup returns null for unknown types" {
    var table = try TypeTable.init(std.testing.allocator);
    defer table.deinit();

    try std.testing.expect(table.lookup("NonExistent") == null);
}

test "multiple user-defined types get sequential ids" {
    var table = try TypeTable.init(std.testing.allocator);
    defer table.deinit();

    const id1 = try table.addType(.{ .@"struct" = .{ .name = "Foo", .fields = &.{} } });
    const id2 = try table.addType(.{ .@"struct" = .{ .name = "Bar", .fields = &.{} } });

    try std.testing.expectEqual(TypeId.first_user, id1.index());
    try std.testing.expectEqual(TypeId.first_user + 1, id2.index());
}

test "task type stores inner type" {
    var table = try TypeTable.init(std.testing.allocator);
    defer table.deinit();

    const id = try table.addType(.{ .task = .{ .inner = .int } });
    const ty = table.get(id).?;
    try std.testing.expectEqual(TypeId.int, ty.task.inner);
    try std.testing.expectEqualStrings("Task", table.typeName(id));
}

test "channel type stores inner type" {
    var table = try TypeTable.init(std.testing.allocator);
    defer table.deinit();

    const id = try table.addType(.{ .channel = .{ .inner = .string } });
    const ty = table.get(id).?;
    try std.testing.expectEqual(TypeId.string, ty.channel.inner);
    try std.testing.expectEqualStrings("Channel", table.typeName(id));
}
