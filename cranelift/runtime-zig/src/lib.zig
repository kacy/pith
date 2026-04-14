const std = @import("std");
const abi = @import("runtime_abi");

const c = @cImport({
    @cInclude("stdio.h");
    @cInclude("stdlib.h");
    @cInclude("string.h");
});

const allocator = std.heap.c_allocator;

const ForgeBytes = struct {
    data: []u8,
};

const ForgeByteBuffer = struct {
    data: std.ArrayListUnmanaged(u8) = .{},
};

const ListImpl = extern struct {
    magic: u32,
    _pad0: u32,
    elem_size: usize,
    type_tag: i32,
    _pad1: u32,
    elements_ptr: ?*anyopaque,
    elements_len: usize,
    elements_cap: usize,
    values8_unused_ptr: ?*anyopaque,
    values8_unused_len: usize,
    values8_unused_cap: usize,
    values8_ptr: ?[*]i64,
    values8_len: usize,
    values8_cap: usize,
};

comptime {
    if (@offsetOf(ListImpl, "elem_size") != abi.elem_size_offset) {
        @compileError("zig list layout elem_size offset mismatch");
    }
    if (@offsetOf(ListImpl, "values8_ptr") != abi.values8_ptr_offset) {
        @compileError("zig list layout values8_ptr offset mismatch");
    }
    if (@offsetOf(ListImpl, "values8_len") != abi.values8_len_offset) {
        @compileError("zig list layout values8_len offset mismatch");
    }
}

const StringEntry = struct {
    key: []u8,
    value: i64,
};

const IntEntry = struct {
    key: i64,
    value: i64,
};

const MapImpl = struct {
    kind: i32,
    val_size: usize,
    val_is_heap: bool,
    string_entries: std.ArrayListUnmanaged(StringEntry) = .{},
    int_entries: std.ArrayListUnmanaged(IntEntry) = .{},
};

const SetImpl = struct {
    items: std.ArrayListUnmanaged(i64) = .{},
};

fn unsupported(message: []const u8) noreturn {
    _ = c.fprintf(c.stderr, "forge zig runtime: %s\n", message.ptr);
    std.process.exit(1);
}

fn strlen(ptr: [*c]const u8) usize {
    if (ptr == null) return 0;
    return @intCast(c.strlen(@ptrCast(ptr)));
}

fn span(ptr: [*c]const u8) []const u8 {
    return ptr[0..strlen(ptr)];
}

fn allocCString(bytes: []const u8) [*c]u8 {
    const raw = allocator.alloc(u8, bytes.len + 1) catch unsupported("out of memory");
    @memcpy(raw[0..bytes.len], bytes);
    raw[bytes.len] = 0;
    return raw.ptr;
}

fn cmpCStrings(a: [*c]const u8, b: [*c]const u8) i64 {
    const a_bytes = span(a);
    const b_bytes = span(b);
    const order = std.mem.order(u8, a_bytes, b_bytes);
    return switch (order) {
        .lt => -1,
        .eq => 0,
        .gt => 1,
    };
}

fn listFromHandle(handle: i64) ?*ListImpl {
    if (handle == 0) return null;
    return @ptrFromInt(@as(usize, @intCast(handle)));
}

fn mapFromHandle(handle: i64) ?*MapImpl {
    if (handle == 0) return null;
    return @ptrFromInt(@as(usize, @intCast(handle)));
}

fn setFromHandle(handle: i64) ?*SetImpl {
    if (handle == 0) return null;
    return @ptrFromInt(@as(usize, @intCast(handle)));
}

fn bytesFromHandle(handle: i64) ?*ForgeBytes {
    if (handle == 0) return null;
    return @ptrFromInt(@as(usize, @intCast(handle)));
}

fn byteBufferFromHandle(handle: i64) ?*ForgeByteBuffer {
    if (handle == 0) return null;
    return @ptrFromInt(@as(usize, @intCast(handle)));
}

fn listSlice(list: *ListImpl) []i64 {
    if (list.values8_ptr == null or list.values8_cap == 0) return &.{};
    const ptr = list.values8_ptr.?;
    return ptr[0..list.values8_cap];
}

fn ensureListCapacity(list: *ListImpl, needed: usize) void {
    if (list.values8_cap >= needed) return;
    var new_cap = if (list.values8_cap == 0) @as(usize, 4) else list.values8_cap * 2;
    while (new_cap < needed) : (new_cap *= 2) {}

    const old_cap = list.values8_cap;
    const old_ptr = list.values8_ptr;
    const new_mem = allocator.alloc(i64, new_cap) catch unsupported("out of memory");
    if (list.values8_len > 0 and old_ptr != null) {
        @memcpy(new_mem[0..list.values8_len], old_ptr.?[0..list.values8_len]);
    }
    if (old_cap > 0 and old_ptr != null) {
        allocator.free(old_ptr.?[0..old_cap]);
    }
    list.values8_ptr = new_mem.ptr;
    list.values8_cap = new_cap;
}

fn appendListValue(list: *ListImpl, value: i64) void {
    if (list.elem_size != 8) {
        unsupported("only 8-byte list values are supported in the zig runtime experiment");
    }
    ensureListCapacity(list, list.values8_len + 1);
    list.values8_ptr.?[list.values8_len] = value;
    list.values8_len += 1;
}

fn mapLen(map: *MapImpl) usize {
    return if (map.kind == 1) map.string_entries.items.len else map.int_entries.items.len;
}

fn allocBytesFromSlice(bytes: []const u8) i64 {
    const duped = allocator.dupe(u8, bytes) catch unsupported("out of memory");
    const handle = allocator.create(ForgeBytes) catch unsupported("out of memory");
    handle.* = .{ .data = duped };
    return @intCast(@intFromPtr(handle));
}

pub export fn forge_print_cstr(ptr: [*c]const u8) void {
    if (ptr == null) return;
    _ = c.puts(@ptrCast(ptr));
}

pub export fn forge_print_err(ptr: [*c]const u8) void {
    if (ptr == null) return;
    _ = c.fprintf(c.stderr, "%s\n", ptr);
}

pub export fn forge_concat_cstr(a: [*c]const u8, b: [*c]const u8) [*c]u8 {
    const a_bytes = span(a);
    const b_bytes = span(b);
    const raw = allocator.alloc(u8, a_bytes.len + b_bytes.len + 1) catch unsupported("out of memory");
    @memcpy(raw[0..a_bytes.len], a_bytes);
    @memcpy(raw[a_bytes.len .. a_bytes.len + b_bytes.len], b_bytes);
    raw[a_bytes.len + b_bytes.len] = 0;
    return raw.ptr;
}

pub export fn forge_cstring_eq(a: [*c]const u8, b: [*c]const u8) i64 {
    return if (cmpCStrings(a, b) == 0) 1 else 0;
}

pub export fn forge_cstring_compare(a: [*c]const u8, b: [*c]const u8) i64 {
    return cmpCStrings(a, b);
}

pub export fn forge_cstring_contains(haystack: [*c]const u8, needle: [*c]const u8) i64 {
    const haystack_bytes = span(haystack);
    const needle_bytes = span(needle);
    if (needle_bytes.len == 0) return 1;
    return if (std.mem.indexOf(u8, haystack_bytes, needle_bytes) != null) 1 else 0;
}

pub export fn forge_int_to_cstr(n: i64) [*c]u8 {
    var buf: [64]u8 = undefined;
    const text = std.fmt.bufPrint(&buf, "{}", .{n}) catch unreachable;
    return allocCString(text);
}

pub export fn forge_float_to_cstr(n: f64) [*c]u8 {
    var buf: [128]u8 = undefined;
    const text = std.fmt.bufPrint(&buf, "{d}", .{n}) catch unreachable;
    return allocCString(text);
}

pub export fn forge_bool_to_cstr(value: i64) [*c]u8 {
    return allocCString(if (value != 0) "true" else "false");
}

pub export fn forge_chr_cstr(n: i64) [*c]u8 {
    if (n < 0 or n > 0x10FFFF) return allocCString("");
    var buf: [5]u8 = [_]u8{0} ** 5;
    const len = std.unicode.utf8Encode(@intCast(n), buf[0..4]) catch return allocCString("");
    return allocCString(buf[0..len]);
}

pub export fn forge_list_new_default() i64 {
    return forge_list_new(8, 0);
}

pub export fn forge_list_new(elem_size: i64, type_tag: i32) i64 {
    const list = allocator.create(ListImpl) catch unsupported("out of memory");
    list.* = .{
        .magic = abi.list_magic,
        ._pad0 = 0,
        .elem_size = @intCast(elem_size),
        .type_tag = type_tag,
        ._pad1 = 0,
        .elements_ptr = null,
        .elements_len = 0,
        .elements_cap = 0,
        .values8_unused_ptr = null,
        .values8_unused_len = 0,
        .values8_unused_cap = 0,
        .values8_ptr = null,
        .values8_len = 0,
        .values8_cap = 0,
    };
    return @intCast(@intFromPtr(list));
}

pub export fn forge_list_push(list_handle: i64, elem: [*c]const u8, elem_size: i64) void {
    const list = listFromHandle(list_handle) orelse return;
    if (elem == null or elem_size != 8) unsupported("forge_list_push only supports 8-byte values");
    const bytes = @as(*const [8]u8, @ptrCast(elem));
    appendListValue(list, std.mem.readInt(i64, bytes, .little));
}

pub export fn forge_list_push_value(list_handle: i64, value: i64) void {
    const list = listFromHandle(list_handle) orelse return;
    appendListValue(list, value);
}

pub export fn forge_list_get_value(list_handle: i64, index: i64) i64 {
    const list = listFromHandle(list_handle) orelse return 0;
    if (index < 0) return 0;
    const idx: usize = @intCast(index);
    if (idx >= list.values8_len or list.values8_ptr == null) return 0;
    return list.values8_ptr.?[idx];
}

pub export fn forge_list_get_value_unchecked(list_handle: i64, index: i64) i64 {
    const list = listFromHandle(list_handle) orelse return 0;
    if (list.values8_ptr == null) return 0;
    return list.values8_ptr.?[@intCast(index)];
}

pub export fn forge_list_len(list_handle: i64) i64 {
    const list = listFromHandle(list_handle) orelse return 0;
    return @intCast(list.values8_len);
}

pub export fn forge_auto_len(ptr: i64) i64 {
    if (ptr == 0) return 0;
    const raw: *const u32 = @ptrFromInt(@as(usize, @intCast(ptr)));
    if (raw.* == abi.list_magic) {
        return forge_list_len(ptr);
    }
    return @intCast(strlen(@ptrFromInt(@as(usize, @intCast(ptr)))));
}

pub export fn forge_map_new_default() i64 {
    return forge_map_new(1, 8, 0);
}

pub export fn forge_map_new_int() i64 {
    return forge_map_new(0, 8, 0);
}

pub export fn forge_map_new(key_type: i32, val_size: i64, val_is_heap: i64) i64 {
    const map = allocator.create(MapImpl) catch unsupported("out of memory");
    map.* = .{
        .kind = key_type,
        .val_size = @intCast(val_size),
        .val_is_heap = val_is_heap != 0,
    };
    return @intCast(@intFromPtr(map));
}

pub export fn forge_map_len_handle(map_handle: i64) i64 {
    const map = mapFromHandle(map_handle) orelse return 0;
    return @intCast(mapLen(map));
}

pub export fn forge_map_insert_cstr(map_handle: i64, key: [*c]const u8, value: i64) void {
    const map = mapFromHandle(map_handle) orelse return;
    const key_bytes = span(key);
    for (map.string_entries.items) |*entry| {
        if (std.mem.eql(u8, entry.key, key_bytes)) {
            entry.value = value;
            return;
        }
    }
    const duped = allocator.dupe(u8, key_bytes) catch unsupported("out of memory");
    map.string_entries.append(allocator, .{ .key = duped, .value = value }) catch unsupported("out of memory");
}

pub export fn forge_map_get_cstr(map_handle: i64, key: [*c]const u8) i64 {
    const map = mapFromHandle(map_handle) orelse return 0;
    const key_bytes = span(key);
    for (map.string_entries.items) |entry| {
        if (std.mem.eql(u8, entry.key, key_bytes)) return entry.value;
    }
    return 0;
}

pub export fn forge_map_insert_ikey(map_handle: i64, key: i64, value: i64) void {
    const map = mapFromHandle(map_handle) orelse return;
    for (map.int_entries.items) |*entry| {
        if (entry.key == key) {
            entry.value = value;
            return;
        }
    }
    map.int_entries.append(allocator, .{ .key = key, .value = value }) catch unsupported("out of memory");
}

pub export fn forge_map_get_ikey(map_handle: i64, key: i64) i64 {
    const map = mapFromHandle(map_handle) orelse return 0;
    for (map.int_entries.items) |entry| {
        if (entry.key == key) return entry.value;
    }
    return 0;
}

pub export fn forge_set_new_default() i64 {
    return forge_set_new_handle(0);
}

pub export fn forge_set_new_int() i64 {
    return forge_set_new_handle(0);
}

pub export fn forge_set_new_handle(_: i32) i64 {
    const set = allocator.create(SetImpl) catch unsupported("out of memory");
    set.* = .{};
    return @intCast(@intFromPtr(set));
}

pub export fn forge_set_len_handle(set_handle: i64) i64 {
    const set = setFromHandle(set_handle) orelse return 0;
    return @intCast(set.items.items.len);
}

pub export fn forge_set_add_int_handle(set_handle: i64, elem: i64) i64 {
    const set = setFromHandle(set_handle) orelse return 0;
    for (set.items.items) |item| {
        if (item == elem) return 0;
    }
    set.items.append(allocator, elem) catch unsupported("out of memory");
    return 1;
}

pub export fn forge_set_contains_int_handle(set_handle: i64, elem: i64) i64 {
    const set = setFromHandle(set_handle) orelse return 0;
    for (set.items.items) |item| {
        if (item == elem) return 1;
    }
    return 0;
}

pub export fn forge_bytes_from_string_utf8(s: [*c]const u8) i64 {
    if (s == null) return allocBytesFromSlice("");
    return allocBytesFromSlice(span(s));
}

pub export fn forge_bytes_to_string_utf8(handle: i64) [*c]u8 {
    const bytes = bytesFromHandle(handle) orelse return null;
    if (!std.unicode.utf8ValidateSlice(bytes.data)) return null;
    return allocCString(bytes.data);
}

pub export fn forge_bytes_len(handle: i64) i64 {
    const bytes = bytesFromHandle(handle) orelse return 0;
    return @intCast(bytes.data.len);
}

pub export fn forge_bytes_is_empty(handle: i64) i64 {
    const bytes = bytesFromHandle(handle) orelse return 1;
    return if (bytes.data.len == 0) 1 else 0;
}

pub export fn forge_bytes_get(handle: i64, idx: i64) i64 {
    const bytes = bytesFromHandle(handle) orelse return 0;
    if (idx < 0) return 0;
    const index: usize = @intCast(idx);
    if (index >= bytes.data.len) return 0;
    return bytes.data[index];
}

pub export fn forge_bytes_slice(handle: i64, start: i64, end: i64) i64 {
    const bytes = bytesFromHandle(handle) orelse return 0;
    const len: i64 = @intCast(bytes.data.len);
    var start_idx = std.math.clamp(start, 0, len);
    var end_idx = std.math.clamp(end, 0, len);
    if (end_idx < start_idx) std.mem.swap(i64, &start_idx, &end_idx);
    return allocBytesFromSlice(bytes.data[@intCast(start_idx)..@intCast(end_idx)]);
}

pub export fn forge_bytes_concat(a: i64, b: i64) i64 {
    const left = bytesFromHandle(a) orelse return 0;
    const right = bytesFromHandle(b) orelse return 0;
    const out = allocator.alloc(u8, left.data.len + right.data.len) catch unsupported("out of memory");
    @memcpy(out[0..left.data.len], left.data);
    @memcpy(out[left.data.len ..], right.data);
    const handle = allocator.create(ForgeBytes) catch unsupported("out of memory");
    handle.* = .{ .data = out };
    return @intCast(@intFromPtr(handle));
}

pub export fn forge_bytes_eq(a: i64, b: i64) i64 {
    if (a == 0 and b == 0) return 1;
    const left = bytesFromHandle(a) orelse return 0;
    const right = bytesFromHandle(b) orelse return 0;
    return if (std.mem.eql(u8, left.data, right.data)) 1 else 0;
}

pub export fn forge_byte_buffer_new() i64 {
    const handle = allocator.create(ForgeByteBuffer) catch unsupported("out of memory");
    handle.* = .{};
    return @intCast(@intFromPtr(handle));
}

pub export fn forge_byte_buffer_with_capacity(capacity: i64) i64 {
    const handle = allocator.create(ForgeByteBuffer) catch unsupported("out of memory");
    handle.* = .{};
    if (capacity > 0) {
        handle.data.ensureTotalCapacity(allocator, @intCast(capacity)) catch unsupported("out of memory");
    }
    return @intCast(@intFromPtr(handle));
}

pub export fn forge_byte_buffer_write(handle: i64, data: i64) i64 {
    const buffer = byteBufferFromHandle(handle) orelse return 0;
    const bytes = bytesFromHandle(data) orelse return 0;
    buffer.data.appendSlice(allocator, bytes.data) catch unsupported("out of memory");
    return @intCast(bytes.data.len);
}

pub export fn forge_byte_buffer_write_byte(handle: i64, value: i64) i64 {
    const buffer = byteBufferFromHandle(handle) orelse return 0;
    if (value < 0 or value > 255) return 0;
    buffer.data.append(allocator, @intCast(value)) catch unsupported("out of memory");
    return 1;
}

pub export fn forge_byte_buffer_bytes(handle: i64) i64 {
    const buffer = byteBufferFromHandle(handle) orelse return 0;
    return allocBytesFromSlice(buffer.data.items);
}

pub export fn forge_byte_buffer_clear(handle: i64) void {
    const buffer = byteBufferFromHandle(handle) orelse return;
    buffer.data.clearRetainingCapacity();
}

pub export fn forge_struct_alloc(num_fields: i64) i64 {
    if (num_fields <= 0) return 0;
    const size: usize = @intCast(num_fields * 8);
    const raw = allocator.alignedAlloc(u8, .fromByteUnits(8), size) catch return 0;
    @memset(raw, 0);
    return @intCast(@intFromPtr(raw.ptr));
}
