// intern — string interning pool
//
// deduplicates strings so that equality checks become
// pointer comparisons. used for identifiers, keywords,
// and type names throughout the compiler.

const std = @import("std");

/// an interned string — a pointer into the intern pool's arena.
/// two InternedStrings with the same content will have the same .ptr,
/// so equality checks are just pointer comparisons.
pub const InternedString = struct {
    bytes: []const u8,

    pub fn eql(a: InternedString, b: InternedString) bool {
        return a.bytes.ptr == b.bytes.ptr;
    }

    pub fn str(self: InternedString) []const u8 {
        return self.bytes;
    }
};

/// string intern pool backed by an arena allocator.
/// all interned strings live as long as the pool does.
pub const InternPool = struct {
    /// maps string content -> interned pointer.
    /// uses the raw bytes as the key and stores the arena-owned slice.
    map: std.StringHashMap([]const u8),

    /// arena that owns all interned string memory.
    arena: std.heap.ArenaAllocator,

    pub fn init(backing_allocator: std.mem.Allocator) InternPool {
        return .{
            .map = std.StringHashMap([]const u8).init(backing_allocator),
            .arena = std.heap.ArenaAllocator.init(backing_allocator),
        };
    }

    pub fn deinit(self: *InternPool) void {
        self.map.deinit();
        self.arena.deinit();
    }

    /// intern a string. if it's already in the pool, return the
    /// existing interned version. otherwise, copy it into the arena.
    pub fn intern(self: *InternPool, bytes: []const u8) !InternedString {
        if (self.map.get(bytes)) |existing| {
            return .{ .bytes = existing };
        }

        // copy the string into our arena so it lives as long as the pool
        const owned = try self.arena.allocator().dupe(u8, bytes);
        try self.map.put(owned, owned);
        return .{ .bytes = owned };
    }
};

// -- tests --

test "intern returns same pointer for same string" {
    var pool = InternPool.init(std.testing.allocator);
    defer pool.deinit();

    const a = try pool.intern("hello");
    const b = try pool.intern("hello");

    try std.testing.expect(a.eql(b));
    try std.testing.expectEqualStrings("hello", a.str());
}

test "intern returns different pointers for different strings" {
    var pool = InternPool.init(std.testing.allocator);
    defer pool.deinit();

    const a = try pool.intern("hello");
    const b = try pool.intern("world");

    try std.testing.expect(!a.eql(b));
}

test "intern handles empty string" {
    var pool = InternPool.init(std.testing.allocator);
    defer pool.deinit();

    const a = try pool.intern("");
    const b = try pool.intern("");

    try std.testing.expect(a.eql(b));
    try std.testing.expectEqual(@as(usize, 0), a.str().len);
}

test "intern preserves string content" {
    var pool = InternPool.init(std.testing.allocator);
    defer pool.deinit();

    const s = try pool.intern("fn_declaration");
    try std.testing.expectEqualStrings("fn_declaration", s.str());
}
