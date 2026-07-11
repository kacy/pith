// event_ledger — Zig counterpart of bench/event_ledger.pith.
//
// Zig's standard library has JSON (std.json) and crypto
// (std.crypto.auth.hmac), so like Go and Pith this needs no third-party
// dependencies. The deterministic event stream, aggregation, and
// HMAC-signed summary match the other three versions byte for byte.
//
// build: zig build-exe -O ReleaseFast -femit-bin=bench/event_ledger_zig bench/event_ledger.zig
// run:   ./bench/event_ledger_zig 200000
//
// Written for Zig 0.16 (main takes std.process.Init; the Io-based clock
// and stdout writer; unmanaged ArrayList).

const std = @import("std");

fn lcgNext(state: i64) i64 {
    return @mod(state * 1103515245 + 12345, 2147483648);
}

fn actionName(k: i64) []const u8 {
    return switch (k) {
        0 => "view",
        1 => "click",
        2 => "buy",
        else => "refund",
    };
}

fn regionName(k: i64) []const u8 {
    return switch (k) {
        0 => "north",
        1 => "south",
        2 => "east",
        else => "west",
    };
}

fn generateEvents(gpa: std.mem.Allocator, count: usize) ![]u8 {
    var buf: std.ArrayList(u8) = .empty;
    var state: i64 = 20260711;
    var i: usize = 0;
    while (i < count) : (i += 1) {
        state = lcgNext(state);
        const user = @mod(@divTrunc(state, 256), 1000);
        state = lcgNext(state);
        const action = actionName(@mod(@divTrunc(state, 256), 4));
        state = lcgNext(state);
        const amount = @mod(@divTrunc(state, 256), 500);
        state = lcgNext(state);
        const region = regionName(@mod(@divTrunc(state, 256), 4));
        if (i > 0) try buf.append(gpa, '\n');
        try buf.print(gpa, "{{\"id\":{d},\"user\":{d},\"action\":\"{s}\",\"amount\":{d},\"region\":\"{s}\"}}", .{ i, user, action, amount, region });
    }
    return buf.toOwnedSlice(gpa);
}

const Event = struct {
    user: i64,
    action: []const u8,
    amount: i64,
    region: []const u8,
};

// parse the stream into an in-memory list of events with std.json. the
// string fields point into `stream` (no escapes to unwind), so they stay
// valid for the whole run.
fn parseEvents(gpa: std.mem.Allocator, arena: std.mem.Allocator, stream: []const u8) !std.ArrayList(Event) {
    var events: std.ArrayList(Event) = .empty;
    var it = std.mem.splitScalar(u8, stream, '\n');
    while (it.next()) |line| {
        if (line.len == 0) continue;
        const e = std.json.parseFromSliceLeaky(Event, arena, line, .{ .ignore_unknown_fields = true }) catch continue;
        try events.append(gpa, e);
    }
    return events;
}

const Analysis = struct {
    region_amount: std.StringHashMap(i64),
    action_count: std.StringHashMap(i64),
    unique_users: std.AutoHashMap(i64, void),
    high_value: i64,
    top_user: i64,
    top_user_total: i64,
    total_amount: i64,
    record_count: i64,
};

fn bump(m: *std.StringHashMap(i64), key: []const u8, delta: i64) !i64 {
    const gop = try m.getOrPut(key);
    if (!gop.found_existing) gop.value_ptr.* = 0;
    gop.value_ptr.* += delta;
    return gop.value_ptr.*;
}

// the analyze phase: several maps, a set, and a per-user rollup with a
// top-spender scan tracked inline as the totals grow.
fn analyze(gpa: std.mem.Allocator, events: []const Event) !Analysis {
    var a = Analysis{
        .region_amount = std.StringHashMap(i64).init(gpa),
        .action_count = std.StringHashMap(i64).init(gpa),
        .unique_users = std.AutoHashMap(i64, void).init(gpa),
        .high_value = 0,
        .top_user = -1,
        .top_user_total = -1,
        .total_amount = 0,
        .record_count = @intCast(events.len),
    };
    var user_total = std.AutoHashMap(i64, i64).init(gpa);
    for (events) |e| {
        _ = try bump(&a.region_amount, e.region, e.amount);
        _ = try bump(&a.action_count, e.action, 1);
        const gop = try user_total.getOrPut(e.user);
        if (!gop.found_existing) gop.value_ptr.* = 0;
        gop.value_ptr.* += e.amount;
        const running = gop.value_ptr.*;
        try a.unique_users.put(e.user, {});
        if (e.amount >= 400) a.high_value += 1;
        a.total_amount += e.amount;
        if (running > a.top_user_total or (running == a.top_user_total and e.user < a.top_user)) {
            a.top_user = e.user;
            a.top_user_total = running;
        }
    }
    return a;
}

fn lessThanStr(_: void, a: []const u8, b: []const u8) bool {
    return std.mem.lessThan(u8, a, b);
}

fn sortedKeys(arena: std.mem.Allocator, m: *std.StringHashMap(i64)) ![][]const u8 {
    var keys: std.ArrayList([]const u8) = .empty;
    var it = m.keyIterator();
    while (it.next()) |k| try keys.append(arena, k.*);
    std.mem.sort([]const u8, keys.items, {}, lessThanStr);
    return keys.items;
}

fn buildSummary(arena: std.mem.Allocator, a: *Analysis) ![]u8 {
    var parts: std.ArrayList([]const u8) = .empty;
    for (try sortedKeys(arena, &a.region_amount)) |r| {
        try parts.append(arena, try std.fmt.allocPrint(arena, "region:{s}={d}", .{ r, a.region_amount.get(r).? }));
    }
    for (try sortedKeys(arena, &a.action_count)) |act| {
        try parts.append(arena, try std.fmt.allocPrint(arena, "action:{s}={d}", .{ act, a.action_count.get(act).? }));
    }
    try parts.append(arena, try std.fmt.allocPrint(arena, "users:{d}", .{a.unique_users.count()}));
    try parts.append(arena, try std.fmt.allocPrint(arena, "hivalue:{d}", .{a.high_value}));
    try parts.append(arena, try std.fmt.allocPrint(arena, "topuser:{d}={d}", .{ a.top_user, a.top_user_total }));
    try parts.append(arena, try std.fmt.allocPrint(arena, "total:{d}", .{a.total_amount}));
    try parts.append(arena, try std.fmt.allocPrint(arena, "records:{d}", .{a.record_count}));
    return std.mem.join(arena, ";", parts.items);
}

fn digestScore(digest: []const u8) i64 {
    var score: i64 = 0;
    for (digest) |c| score += @as(i64, c);
    return score;
}

fn elapsedMs(io: std.Io, t0: std.Io.Timestamp) i64 {
    return @intCast(t0.untilNow(io, .awake).toMilliseconds());
}

pub fn main(init: std.process.Init) !void {
    const io = init.io;
    const gpa = init.gpa;
    const arena = init.arena.allocator();

    var events: usize = 200000;
    var args = init.minimal.args.iterate();
    _ = args.skip();
    if (args.next()) |arg| {
        events = std.fmt.parseInt(usize, arg, 10) catch 200000;
    }

    const total_start = std.Io.Timestamp.now(io, .awake);

    var start = std.Io.Timestamp.now(io, .awake);
    const stream = try generateEvents(gpa, events);
    const gen_ms = elapsedMs(io, start);

    start = std.Io.Timestamp.now(io, .awake);
    const parsed = try parseEvents(gpa, arena, stream);
    const parse_ms = elapsedMs(io, start);

    start = std.Io.Timestamp.now(io, .awake);
    var a = try analyze(gpa, parsed.items);
    const analyze_ms = elapsedMs(io, start);

    start = std.Io.Timestamp.now(io, .awake);
    const summary = try buildSummary(arena, &a);
    const Hmac = std.crypto.auth.hmac.sha2.HmacSha256;
    var mac: [Hmac.mac_length]u8 = undefined;
    Hmac.create(&mac, summary, "pith-bench-key");
    const digest = std.fmt.bytesToHex(mac, .lower);
    const sign_ms = elapsedMs(io, start);

    const total_ms = elapsedMs(io, total_start);

    const checksum = a.total_amount + a.record_count +
        @as(i64, @intCast(a.unique_users.count())) * 31 +
        a.high_value + a.top_user_total + digestScore(&digest);

    var out_buf: [8192]u8 = undefined;
    var fw = std.Io.File.stdout().writer(io, &out_buf);
    const w = &fw.interface;
    try w.print("event ledger benchmark\n", .{});
    try w.print("events={d}\n", .{events});
    try w.print("gen_ms={d}\n", .{gen_ms});
    try w.print("parse_ms={d}\n", .{parse_ms});
    try w.print("analyze_ms={d}\n", .{analyze_ms});
    try w.print("sign_ms={d}\n", .{sign_ms});
    try w.print("total_ms={d}\n", .{total_ms});
    try w.print("unique_users={d}\n", .{a.unique_users.count()});
    try w.print("digest={s}\n", .{digest});
    try w.print("checksum={d}\n", .{checksum});
    try w.flush();
}
