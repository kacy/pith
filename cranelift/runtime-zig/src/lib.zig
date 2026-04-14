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

const Task = struct {
    mutex: std.Thread.Mutex = .{},
    cond: std.Thread.Condition = .{},
    done: bool = false,
    result: i64 = 0,
    thread: ?std.Thread = null,
};

const ChannelState = struct {
    queue: std.ArrayListUnmanaged(i64) = .{},
    capacity: usize = 0,
    closed: bool = false,
    pending_value: ?i64 = null,
    receiver_waiting: usize = 0,
    sender_waiting: usize = 0,
};

const Channel = struct {
    mutex: std.Thread.Mutex = .{},
    cond: std.Thread.Condition = .{},
    state: ChannelState,
};

const MutexHandle = struct {
    mutex: std.Thread.Mutex = .{},
    cond: std.Thread.Condition = .{},
    locked: bool = false,
};

const WaitGroupHandle = struct {
    mutex: std.Thread.Mutex = .{},
    cond: std.Thread.Condition = .{},
    count: usize = 0,
};

const SemaphoreHandle = struct {
    mutex: std.Thread.Mutex = .{},
    cond: std.Thread.Condition = .{},
    count: usize = 0,
    max: usize = 0,
};

const forge_closure_env_slots = 16;

const ForgeClosure = struct {
    func_ptr: i64,
    env: [forge_closure_env_slots]i64 = [_]i64{0} ** forge_closure_env_slots,
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
    string_items: std.ArrayListUnmanaged([]u8) = .{},
    string_mode: bool = false,
};

var select_counter = std.atomic.Value(i64).init(0);

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

fn closureFromHandle(handle: i64) ?*ForgeClosure {
    if (handle == 0) return null;
    return @ptrFromInt(@as(usize, @intCast(handle)));
}

fn taskFromHandle(handle: i64) ?*Task {
    if (handle == 0) return null;
    return @ptrFromInt(@as(usize, @intCast(handle)));
}

fn channelFromHandle(handle: i64) ?*Channel {
    if (handle == 0) return null;
    return @ptrFromInt(@as(usize, @intCast(handle)));
}

fn mutexFromHandle(handle: i64) ?*MutexHandle {
    if (handle == 0) return null;
    return @ptrFromInt(@as(usize, @intCast(handle)));
}

fn waitGroupFromHandle(handle: i64) ?*WaitGroupHandle {
    if (handle == 0) return null;
    return @ptrFromInt(@as(usize, @intCast(handle)));
}

fn semaphoreFromHandle(handle: i64) ?*SemaphoreHandle {
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

fn optionalTuple(is_some: bool, value: i64) i64 {
    const tuple = forge_struct_alloc(2);
    if (tuple == 0) return 0;
    const ptr: [*]i64 = @ptrFromInt(@as(usize, @intCast(tuple)));
    ptr[0] = if (is_some) 1 else 0;
    ptr[1] = value;
    return tuple;
}

fn taskWorker(task: *Task, closure_handle: i64) void {
    const func_ptr = forge_closure_get_fn(closure_handle);
    var result: i64 = 0;
    if (func_ptr != 0) {
        const func: *const fn (i64) callconv(.c) i64 = @ptrFromInt(@as(usize, @intCast(func_ptr)));
        result = func(closure_handle);
    }

    task.mutex.lock();
    defer task.mutex.unlock();
    task.done = true;
    task.result = result;
    task.cond.broadcast();
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

pub export fn forge_cstring_len(s: [*c]const u8) i64 {
    return @intCast(strlen(s));
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

pub export fn forge_ord_cstr(s: [*c]const u8) i64 {
    if (s == null or s[0] == 0) return 0;
    return s[0];
}

pub export fn forge_closure_new(func_ptr: i64) i64 {
    const closure = allocator.create(ForgeClosure) catch unsupported("out of memory");
    closure.* = .{ .func_ptr = func_ptr };
    return @intCast(@intFromPtr(closure));
}

pub export fn forge_closure_get_fn(handle: i64) i64 {
    const closure = closureFromHandle(handle) orelse return 0;
    return closure.func_ptr;
}

pub export fn forge_closure_set_env(handle: i64, slot: i64, value: i64) void {
    if (slot < 0 or slot >= forge_closure_env_slots) return;
    const closure = closureFromHandle(handle) orelse return;
    closure.env[@intCast(slot)] = value;
}

pub export fn forge_closure_get_env(handle: i64, slot: i64) i64 {
    if (slot < 0 or slot >= forge_closure_env_slots) return 0;
    const closure = closureFromHandle(handle) orelse return 0;
    return closure.env[@intCast(slot)];
}

pub export fn forge_sleep(ms: i64) void {
    if (ms <= 0) return;
    std.Thread.sleep(@as(u64, @intCast(ms)) * std.time.ns_per_ms);
}

pub export fn forge_time() i64 {
    return std.time.milliTimestamp();
}

pub export fn forge_format_time_fmt(timestamp_ms: i64, _: [*c]const u8) [*c]u8 {
    return forge_int_to_cstr(@divTrunc(timestamp_ms, 1000));
}

pub export fn forge_spawn(closure_handle: i64) i64 {
    if (closure_handle == 0) return 0;
    const task = allocator.create(Task) catch unsupported("out of memory");
    task.* = .{};
    task.thread = std.Thread.spawn(.{}, taskWorker, .{ task, closure_handle }) catch unsupported("failed to spawn thread");
    return @intCast(@intFromPtr(task));
}

pub export fn forge_await(task_handle: i64) i64 {
    const task = taskFromHandle(task_handle) orelse return 0;
    if (task.thread) |thread| {
        thread.join();
        task.thread = null;
    }
    task.mutex.lock();
    defer task.mutex.unlock();
    while (!task.done) {
        task.cond.wait(&task.mutex);
    }
    return task.result;
}

pub export fn forge_task_is_done(task_handle: i64) i64 {
    const task = taskFromHandle(task_handle) orelse return 0;
    task.mutex.lock();
    defer task.mutex.unlock();
    return if (task.done) 1 else 0;
}

pub export fn forge_task_detach(task_handle: i64) void {
    const task = taskFromHandle(task_handle) orelse return;
    if (task.thread) |thread| {
        thread.detach();
        task.thread = null;
    }
}

pub export fn forge_channel_new(capacity: i64) i64 {
    const channel = allocator.create(Channel) catch unsupported("out of memory");
    channel.* = .{
        .state = .{
            .capacity = @intCast(@max(capacity, 0)),
        },
    };
    return @intCast(@intFromPtr(channel));
}

pub export fn forge_channel_send(handle: i64, value: i64) i64 {
    const channel = channelFromHandle(handle) orelse return 0;
    channel.mutex.lock();
    defer channel.mutex.unlock();

    if (channel.state.closed) return 0;

    if (channel.state.capacity == 0) {
        while (!channel.state.closed) {
            if (channel.state.receiver_waiting > 0 and channel.state.pending_value == null) {
                channel.state.pending_value = value;
                channel.cond.broadcast();
                while (!channel.state.closed and channel.state.pending_value != null) {
                    channel.cond.wait(&channel.mutex);
                }
                return if (channel.state.closed) 0 else 1;
            }
            channel.state.sender_waiting += 1;
            channel.cond.wait(&channel.mutex);
            channel.state.sender_waiting -= 1;
        }
        return 0;
    }

    while (!channel.state.closed and channel.state.queue.items.len >= channel.state.capacity) {
        channel.state.sender_waiting += 1;
        channel.cond.wait(&channel.mutex);
        channel.state.sender_waiting -= 1;
    }

    if (channel.state.closed) return 0;
    channel.state.queue.append(allocator, value) catch unsupported("out of memory");
    channel.cond.broadcast();
    return 1;
}

pub export fn forge_channel_try_send(handle: i64, value: i64) i64 {
    const channel = channelFromHandle(handle) orelse return 0;
    channel.mutex.lock();
    defer channel.mutex.unlock();

    if (channel.state.closed) return 0;

    if (channel.state.capacity == 0) {
        if (channel.state.receiver_waiting == 0 or channel.state.pending_value != null) return 0;
        channel.state.pending_value = value;
        channel.cond.broadcast();
        return 1;
    }

    if (channel.state.queue.items.len >= channel.state.capacity) return 0;
    channel.state.queue.append(allocator, value) catch unsupported("out of memory");
    channel.cond.broadcast();
    return 1;
}

pub export fn forge_channel_recv(handle: i64) i64 {
    const channel = channelFromHandle(handle) orelse return optionalTuple(false, 0);
    channel.mutex.lock();
    defer channel.mutex.unlock();

    while (true) {
        if (channel.state.queue.items.len > 0) {
            const value = channel.state.queue.orderedRemove(0);
            channel.cond.broadcast();
            return optionalTuple(true, value);
        }

        if (channel.state.capacity == 0) {
            if (channel.state.pending_value) |value| {
                channel.state.pending_value = null;
                channel.cond.broadcast();
                return optionalTuple(true, value);
            }
        }

        if (channel.state.closed) return optionalTuple(false, 0);

        channel.state.receiver_waiting += 1;
        channel.cond.broadcast();
        channel.cond.wait(&channel.mutex);
        channel.state.receiver_waiting -= 1;
    }
}

pub export fn forge_channel_try_recv(handle: i64) i64 {
    const channel = channelFromHandle(handle) orelse return optionalTuple(false, 0);
    channel.mutex.lock();
    defer channel.mutex.unlock();

    if (channel.state.queue.items.len > 0) {
        const value = channel.state.queue.orderedRemove(0);
        channel.cond.broadcast();
        return optionalTuple(true, value);
    }
    if (channel.state.capacity == 0) {
        if (channel.state.pending_value) |value| {
            channel.state.pending_value = null;
            channel.cond.broadcast();
            return optionalTuple(true, value);
        }
    }
    return optionalTuple(false, 0);
}

pub export fn forge_channel_close(handle: i64) i64 {
    const channel = channelFromHandle(handle) orelse return 0;
    channel.mutex.lock();
    defer channel.mutex.unlock();
    if (channel.state.closed) return 0;
    channel.state.closed = true;
    channel.state.pending_value = null;
    channel.cond.broadcast();
    return 1;
}

pub export fn forge_channel_len(handle: i64) i64 {
    const channel = channelFromHandle(handle) orelse return 0;
    channel.mutex.lock();
    defer channel.mutex.unlock();
    return @intCast(channel.state.queue.items.len);
}

pub export fn forge_channel_cap(handle: i64) i64 {
    const channel = channelFromHandle(handle) orelse return 0;
    channel.mutex.lock();
    defer channel.mutex.unlock();
    return @intCast(channel.state.capacity);
}

pub export fn forge_channel_is_closed(handle: i64) i64 {
    const channel = channelFromHandle(handle) orelse return 1;
    channel.mutex.lock();
    defer channel.mutex.unlock();
    return if (channel.state.closed) 1 else 0;
}

pub export fn forge_select_next_index(count: i64) i64 {
    if (count <= 1) return 0;
    const next = select_counter.fetchAdd(1, .monotonic);
    return @mod(next, count);
}

pub export fn forge_mutex_new() i64 {
    const handle = allocator.create(MutexHandle) catch unsupported("out of memory");
    handle.* = .{};
    return @intCast(@intFromPtr(handle));
}

pub export fn forge_mutex_lock(handle: i64) void {
    const mutex_handle = mutexFromHandle(handle) orelse return;
    mutex_handle.mutex.lock();
    defer mutex_handle.mutex.unlock();
    while (mutex_handle.locked) {
        mutex_handle.cond.wait(&mutex_handle.mutex);
    }
    mutex_handle.locked = true;
}

pub export fn forge_mutex_unlock(handle: i64) void {
    const mutex_handle = mutexFromHandle(handle) orelse return;
    mutex_handle.mutex.lock();
    defer mutex_handle.mutex.unlock();
    mutex_handle.locked = false;
    mutex_handle.cond.signal();
}

pub export fn forge_waitgroup_new() i64 {
    const handle = allocator.create(WaitGroupHandle) catch unsupported("out of memory");
    handle.* = .{};
    return @intCast(@intFromPtr(handle));
}

pub export fn forge_waitgroup_add(handle: i64, delta: i64) void {
    const wg = waitGroupFromHandle(handle) orelse return;
    wg.mutex.lock();
    defer wg.mutex.unlock();
    const next = @as(i64, @intCast(wg.count)) + delta;
    wg.count = if (next > 0) @intCast(next) else 0;
    if (wg.count == 0) wg.cond.broadcast();
}

pub export fn forge_waitgroup_done(handle: i64) void {
    const wg = waitGroupFromHandle(handle) orelse return;
    wg.mutex.lock();
    defer wg.mutex.unlock();
    if (wg.count > 0) wg.count -= 1;
    if (wg.count == 0) wg.cond.broadcast();
}

pub export fn forge_waitgroup_wait(handle: i64) void {
    const wg = waitGroupFromHandle(handle) orelse return;
    wg.mutex.lock();
    defer wg.mutex.unlock();
    while (wg.count > 0) {
        wg.cond.wait(&wg.mutex);
    }
}

pub export fn forge_semaphore_new(initial: i64) i64 {
    const handle = allocator.create(SemaphoreHandle) catch unsupported("out of memory");
    const count = if (initial > 0) @as(usize, @intCast(initial)) else 0;
    handle.* = .{
        .count = count,
        .max = count,
    };
    return @intCast(@intFromPtr(handle));
}

pub export fn forge_semaphore_acquire(handle: i64) void {
    const sem = semaphoreFromHandle(handle) orelse return;
    sem.mutex.lock();
    defer sem.mutex.unlock();
    while (sem.count == 0) {
        sem.cond.wait(&sem.mutex);
    }
    sem.count -= 1;
}

pub export fn forge_semaphore_release(handle: i64) void {
    const sem = semaphoreFromHandle(handle) orelse return;
    sem.mutex.lock();
    defer sem.mutex.unlock();
    if (sem.count < sem.max) sem.count += 1;
    sem.cond.signal();
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

pub export fn forge_list_is_empty(list_handle: i64) i64 {
    return if (forge_list_len(list_handle) == 0) 1 else 0;
}

pub export fn forge_list_contains_int(list_handle: i64, value: i64) i64 {
    const list = listFromHandle(list_handle) orelse return 0;
    if (list.values8_ptr == null) return 0;
    for (list.values8_ptr.?[0..list.values8_len]) |item| {
        if (item == value) return 1;
    }
    return 0;
}

pub export fn forge_list_join(list_handle: i64, sep: [*c]const u8) [*c]u8 {
    const list = listFromHandle(list_handle) orelse return null;
    if (list.values8_len == 0 or list.values8_ptr == null) return allocCString("");

    const sep_bytes = if (sep == null) "" else span(sep);
    var total_len: usize = 0;
    var idx: usize = 0;
    while (idx < list.values8_len) : (idx += 1) {
        const part_ptr: [*c]const u8 = @ptrFromInt(@as(usize, @intCast(list.values8_ptr.?[idx])));
        total_len += strlen(part_ptr);
        if (idx + 1 < list.values8_len) total_len += sep_bytes.len;
    }

    const out = allocator.alloc(u8, total_len + 1) catch unsupported("out of memory");
    var cursor: usize = 0;
    idx = 0;
    while (idx < list.values8_len) : (idx += 1) {
        const part_ptr: [*c]const u8 = @ptrFromInt(@as(usize, @intCast(list.values8_ptr.?[idx])));
        const part = span(part_ptr);
        @memcpy(out[cursor .. cursor + part.len], part);
        cursor += part.len;
        if (idx + 1 < list.values8_len and sep_bytes.len > 0) {
            @memcpy(out[cursor .. cursor + sep_bytes.len], sep_bytes);
            cursor += sep_bytes.len;
        }
    }
    out[cursor] = 0;
    return out.ptr;
}

pub export fn forge_list_remove_value(list_handle: i64, index: i64) i64 {
    const list = listFromHandle(list_handle) orelse return 0;
    if (index < 0) return 0;
    const idx: usize = @intCast(index);
    if (idx >= list.values8_len or list.values8_ptr == null) return 0;
    const slice = list.values8_ptr.?[0..list.values8_len];
    var i = idx;
    while (i + 1 < list.values8_len) : (i += 1) {
        slice[i] = slice[i + 1];
    }
    list.values8_len -= 1;
    return 1;
}

pub export fn forge_list_reverse_value(list_handle: i64) void {
    const list = listFromHandle(list_handle) orelse return;
    if (list.values8_ptr == null) return;
    std.mem.reverse(i64, list.values8_ptr.?[0..list.values8_len]);
}

pub export fn forge_list_clear_value(list_handle: i64) void {
    const list = listFromHandle(list_handle) orelse return;
    list.values8_len = 0;
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

pub export fn forge_map_contains_cstr(map_handle: i64, key: [*c]const u8) i64 {
    const map = mapFromHandle(map_handle) orelse return 0;
    const key_bytes = span(key);
    for (map.string_entries.items) |entry| {
        if (std.mem.eql(u8, entry.key, key_bytes)) return 1;
    }
    return 0;
}

pub export fn forge_map_get_default_cstr(map_handle: i64, key: [*c]const u8, default: i64) i64 {
    const value = forge_map_get_cstr(map_handle, key);
    return if (value == 0) default else value;
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

pub export fn forge_map_contains_ikey(map_handle: i64, key: i64) i64 {
    const map = mapFromHandle(map_handle) orelse return 0;
    for (map.int_entries.items) |entry| {
        if (entry.key == key) return 1;
    }
    return 0;
}

pub export fn forge_map_get_default_ikey(map_handle: i64, key: i64, default: i64) i64 {
    const map = mapFromHandle(map_handle) orelse return default;
    for (map.int_entries.items) |entry| {
        if (entry.key == key) return entry.value;
    }
    return default;
}

pub export fn forge_map_keys_cstr(map_handle: i64) i64 {
    const map = mapFromHandle(map_handle) orelse return forge_list_new_default();
    const list_handle = forge_list_new_default();
    for (map.string_entries.items) |entry| {
        forge_list_push_value(list_handle, @intCast(@intFromPtr(allocCString(entry.key))));
    }
    return list_handle;
}

pub export fn forge_map_remove_cstr(map_handle: i64, key: [*c]const u8) void {
    const map = mapFromHandle(map_handle) orelse return;
    const key_bytes = span(key);
    var idx: usize = 0;
    while (idx < map.string_entries.items.len) : (idx += 1) {
        if (std.mem.eql(u8, map.string_entries.items[idx].key, key_bytes)) {
            _ = map.string_entries.swapRemove(idx);
            return;
        }
    }
}

pub export fn forge_map_values_handle(map_handle: i64) i64 {
    const map = mapFromHandle(map_handle) orelse return forge_list_new_default();
    const list_handle = forge_list_new_default();
    if (map.kind == 1) {
        for (map.string_entries.items) |entry| {
            forge_list_push_value(list_handle, entry.value);
        }
    } else {
        for (map.int_entries.items) |entry| {
            forge_list_push_value(list_handle, entry.value);
        }
    }
    return list_handle;
}

pub export fn forge_map_clear_handle(map_handle: i64) void {
    const map = mapFromHandle(map_handle) orelse return;
    map.string_entries.clearRetainingCapacity();
    map.int_entries.clearRetainingCapacity();
}

pub export fn forge_map_is_empty_handle(map_handle: i64) i64 {
    return if (forge_map_len_handle(map_handle) == 0) 1 else 0;
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
    return if (set.string_mode) @intCast(set.string_items.items.len) else @intCast(set.items.items.len);
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

pub export fn forge_set_add_cstr(set_handle: i64, elem: [*c]const u8) i64 {
    const set = setFromHandle(set_handle) orelse return 0;
    const elem_bytes = span(elem);
    set.string_mode = true;
    for (set.string_items.items) |item| {
        if (std.mem.eql(u8, item, elem_bytes)) return 0;
    }
    const duped = allocator.dupe(u8, elem_bytes) catch unsupported("out of memory");
    set.string_items.append(allocator, duped) catch unsupported("out of memory");
    return 1;
}

pub export fn forge_set_contains_cstr(set_handle: i64, elem: [*c]const u8) i64 {
    const set = setFromHandle(set_handle) orelse return 0;
    const elem_bytes = span(elem);
    for (set.string_items.items) |item| {
        if (std.mem.eql(u8, item, elem_bytes)) return 1;
    }
    return 0;
}

pub export fn forge_set_remove_cstr(set_handle: i64, elem: [*c]const u8) void {
    const set = setFromHandle(set_handle) orelse return;
    const elem_bytes = span(elem);
    var idx: usize = 0;
    while (idx < set.string_items.items.len) : (idx += 1) {
        if (std.mem.eql(u8, set.string_items.items[idx], elem_bytes)) {
            _ = set.string_items.swapRemove(idx);
            return;
        }
    }
}

pub export fn forge_set_clear_handle(set_handle: i64) void {
    const set = setFromHandle(set_handle) orelse return;
    set.items.clearRetainingCapacity();
    set.string_items.clearRetainingCapacity();
}

pub export fn forge_set_is_empty_handle(set_handle: i64) i64 {
    return if (forge_set_len_handle(set_handle) == 0) 1 else 0;
}

pub export fn forge_cstring_substring(s: [*c]const u8, start: i64, end: i64) [*c]u8 {
    if (s == null) return null;
    const len: i64 = @intCast(strlen(s));
    const start_idx = std.math.clamp(start, 0, len);
    const end_idx = std.math.clamp(end, start_idx, len);
    return allocCString(span(s)[@intCast(start_idx)..@intCast(end_idx)]);
}

pub export fn forge_cstring_trim(s: [*c]const u8) [*c]u8 {
    if (s == null) return null;
    const input = span(s);
    const trimmed = std.mem.trim(u8, input, " \t\n\r");
    return allocCString(trimmed);
}

pub export fn forge_cstring_char_at(s: [*c]const u8, index: i64) [*c]u8 {
    if (s == null) return allocCString("");
    const bytes = span(s);
    if (index < 0) return allocCString("");
    const idx: usize = @intCast(index);
    if (idx >= bytes.len) return allocCString("");
    const one = [_]u8{bytes[idx]};
    return allocCString(one[0..]);
}

pub export fn forge_cstring_to_upper(s: [*c]const u8) [*c]u8 {
    if (s == null) return null;
    const bytes = span(s);
    const out = allocator.alloc(u8, bytes.len + 1) catch unsupported("out of memory");
    for (bytes, 0..) |ch, i| out[i] = std.ascii.toUpper(ch);
    out[bytes.len] = 0;
    return out.ptr;
}

pub export fn forge_cstring_to_lower(s: [*c]const u8) [*c]u8 {
    if (s == null) return null;
    const bytes = span(s);
    const out = allocator.alloc(u8, bytes.len + 1) catch unsupported("out of memory");
    for (bytes, 0..) |ch, i| out[i] = std.ascii.toLower(ch);
    out[bytes.len] = 0;
    return out.ptr;
}

pub export fn forge_cstring_replace(s: [*c]const u8, from: [*c]const u8, to: [*c]const u8) [*c]u8 {
    if (s == null) return null;
    const input = span(s);
    const needle = if (from == null) "" else span(from);
    const replacement = if (to == null) "" else span(to);
    if (needle.len == 0) return allocCString(input);

    var out = std.ArrayListUnmanaged(u8){};
    defer out.deinit(allocator);

    var cursor: usize = 0;
    while (cursor < input.len) {
        if (std.mem.startsWith(u8, input[cursor..], needle)) {
            out.appendSlice(allocator, replacement) catch unsupported("out of memory");
            cursor += needle.len;
        } else {
            out.append(allocator, input[cursor]) catch unsupported("out of memory");
            cursor += 1;
        }
    }
    return allocCString(out.items);
}

pub export fn forge_cstring_index_of(haystack: [*c]const u8, needle: [*c]const u8) i64 {
    if (haystack == null or needle == null) return -1;
    const h = span(haystack);
    const n = span(needle);
    if (n.len == 0) return 0;
    return if (std.mem.indexOf(u8, h, n)) |idx| @intCast(idx) else -1;
}

pub export fn forge_cstring_starts_with(s: [*c]const u8, prefix: [*c]const u8) i64 {
    if (s == null or prefix == null) return 0;
    return if (std.mem.startsWith(u8, span(s), span(prefix))) 1 else 0;
}

pub export fn forge_cstring_ends_with(s: [*c]const u8, suffix: [*c]const u8) i64 {
    if (s == null or suffix == null) return 0;
    return if (std.mem.endsWith(u8, span(s), span(suffix))) 1 else 0;
}

pub export fn forge_cstring_last_index_of(haystack: [*c]const u8, needle: [*c]const u8) i64 {
    if (haystack == null or needle == null) return -1;
    const h = span(haystack);
    const n = span(needle);
    if (n.len == 0) return @intCast(h.len);
    return if (std.mem.lastIndexOf(u8, h, n)) |idx| @intCast(idx) else -1;
}

pub export fn forge_cstring_pad_left(s: [*c]const u8, width: i64, fill: [*c]const u8) [*c]u8 {
    if (s == null) return null;
    const text = span(s);
    const target_width: usize = if (width > 0) @intCast(width) else 0;
    if (text.len >= target_width) return allocCString(text);
    const fill_char: u8 = if (fill != null and fill[0] != 0) fill[0] else ' ';
    const pad = target_width - text.len;
    const out = allocator.alloc(u8, target_width + 1) catch unsupported("out of memory");
    @memset(out[0..pad], fill_char);
    @memcpy(out[pad .. pad + text.len], text);
    out[target_width] = 0;
    return out.ptr;
}

pub export fn forge_string_split_to_list(s: [*c]const u8, delim: [*c]const u8) i64 {
    if (s == null or delim == null) return forge_list_new_default();
    const input = span(s);
    const separator = span(delim);
    const list_handle = forge_list_new_default();
    if (input.len == 0) return list_handle;
    if (separator.len == 0) {
        for (input) |byte| {
            const one = [_]u8{byte};
            forge_list_push_value(list_handle, @intCast(@intFromPtr(allocCString(one[0..]))));
        }
        return list_handle;
    }
    var start: usize = 0;
    while (start <= input.len) {
        const tail = input[start..];
        const rel = std.mem.indexOf(u8, tail, separator);
        const end = if (rel) |idx| start + idx else input.len;
        if (end > start) {
            forge_list_push_value(list_handle, @intCast(@intFromPtr(allocCString(input[start..end]))));
        }
        if (rel == null) break;
        start = end + separator.len;
    }
    return list_handle;
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
