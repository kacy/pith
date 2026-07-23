// chan_fanout — Zig counterpart of bench/chan_fanout.pith.
//
// Four producer threads push messages into one bounded queue and four
// consumer threads drain it. The per-message work is two LCG rounds and
// the aggregate is a sum modulo a prime, so the checksum is
// order-independent and matches the Pith, Go, and Rust versions.
//
// Zig's standard library has no channel, so the queue below is the
// usual mutex + two condition variables over a ring buffer. That is
// what a Zig programmer writes for this, and its cost is part of what
// this measures.
//
// build: zig build-exe -O ReleaseFast -femit-bin=bench/chan_fanout_zig bench/chan_fanout.zig
// run:   ./bench/chan_fanout_zig 1000000
//
// Written for Zig 0.16 (main takes std.process.Init, Io-based clock and
// stdout writer).

const std = @import("std");

const producers = 4;
const consumers = 4;
const capacity = 256;
const mod: i64 = 1000000007;

// two rounds of a 31-bit LCG (POSIX constants), masked so it never
// overflows and every language reproduces it exactly.
fn mix(value: i64) i64 {
    var x = @mod(value * 1103515245 + 12345, 2147483648);
    x = @mod(x * 1103515245 + 12345, 2147483648);
    return x;
}

// a bounded blocking queue: the same shape as a buffered channel. in
// zig 0.16 the mutex and condition variables take the io instance, so
// the queue carries one.
const Queue = struct {
    io: std.Io,
    buf: []i64,
    head: usize = 0,
    tail: usize = 0,
    count: usize = 0,
    closed: bool = false,
    mutex: std.Io.Mutex = .init,
    not_full: std.Io.Condition = .init,
    not_empty: std.Io.Condition = .init,

    fn push(self: *Queue, value: i64) void {
        self.mutex.lockUncancelable(self.io);
        defer self.mutex.unlock(self.io);
        while (self.count == self.buf.len) self.not_full.waitUncancelable(self.io, &self.mutex);
        self.buf[self.tail] = value;
        self.tail = (self.tail + 1) % self.buf.len;
        self.count += 1;
        self.not_empty.signal(self.io);
    }

    // null once the queue is closed and drained.
    fn pop(self: *Queue) ?i64 {
        self.mutex.lockUncancelable(self.io);
        defer self.mutex.unlock(self.io);
        while (self.count == 0 and !self.closed) self.not_empty.waitUncancelable(self.io, &self.mutex);
        if (self.count == 0) return null;
        const value = self.buf[self.head];
        self.head = (self.head + 1) % self.buf.len;
        self.count -= 1;
        self.not_full.signal(self.io);
        return value;
    }

    fn close(self: *Queue) void {
        self.mutex.lockUncancelable(self.io);
        defer self.mutex.unlock(self.io);
        self.closed = true;
        self.not_empty.broadcast(self.io);
    }
};

const Partial = struct {
    sum: i64 = 0,
    seen: i64 = 0,
};

// each producer owns a disjoint slice of the id space, and reports how
// many messages it pushed into its own slot.
fn produce(queue: *Queue, id: i64, per: i64, sent: *i64) void {
    var i: i64 = 0;
    while (i < per) : (i += 1) {
        queue.push(id * per + i);
    }
    sent.* = per;
}

fn consume(queue: *Queue, out: *Partial) void {
    var sum: i64 = 0;
    var seen: i64 = 0;
    while (queue.pop()) |value| {
        sum = @mod(sum + mix(value), mod);
        seen += 1;
    }
    out.* = .{ .sum = sum, .seen = seen };
}

// linux only, which is what /proc/self/status implies anyway.
fn peakRssKb(io: std.Io) i64 {
    const fd = std.posix.openat(std.posix.AT.FDCWD, "/proc/self/status", .{ .ACCMODE = .RDONLY }, 0) catch return 0;
    const file: std.Io.File = .{ .handle = fd, .flags = .{ .nonblocking = false } };
    defer file.close(io);
    var buf: [8192]u8 = undefined;
    var len: usize = 0;
    while (len < buf.len) {
        const n = std.posix.read(fd, buf[len..]) catch break;
        if (n == 0) break;
        len += n;
    }
    var lines = std.mem.splitScalar(u8, buf[0..len], '\n');
    while (lines.next()) |line| {
        if (std.mem.startsWith(u8, line, "VmHWM:")) {
            const rest = std.mem.trim(u8, line["VmHWM:".len..], " \tkB");
            return std.fmt.parseInt(i64, rest, 10) catch 0;
        }
    }
    return 0;
}

pub fn main(init: std.process.Init) !void {
    const io = init.io;
    const gpa = init.gpa;

    var requested: i64 = 1000000;
    var args = init.minimal.args.iterate();
    _ = args.skip();
    if (args.next()) |arg| {
        requested = std.fmt.parseInt(i64, arg, 10) catch 1000000;
    }
    const per = @divTrunc(requested, producers);
    const messages = per * producers;

    const buf = try gpa.alloc(i64, capacity);
    defer gpa.free(buf);
    var queue = Queue{ .io = io, .buf = buf };

    const start = std.Io.Timestamp.now(io, .awake);

    // consumers start first and block on pop until work shows up.
    var partials: [consumers]Partial = @splat(.{});
    var consumer_threads: [consumers]std.Thread = undefined;
    for (0..consumers) |c| {
        consumer_threads[c] = try std.Thread.spawn(.{}, consume, .{ &queue, &partials[c] });
    }

    var sent_counts: [producers]i64 = @splat(0);
    var producer_threads: [producers]std.Thread = undefined;
    for (0..producers) |p| {
        producer_threads[p] = try std.Thread.spawn(.{}, produce, .{ &queue, @as(i64, @intCast(p)), per, &sent_counts[p] });
    }

    for (producer_threads) |t| t.join();
    queue.close();
    for (consumer_threads) |t| t.join();

    var checksum: i64 = 0;
    var received: i64 = 0;
    for (partials) |p| {
        checksum = @mod(checksum + p.sum, mod);
        received += p.seen;
    }
    var sent: i64 = 0;
    for (sent_counts) |n| sent += n;

    const elapsed: i64 = @intCast(start.untilNow(io, .awake).toMilliseconds());
    const rate: i64 = if (elapsed > 0) @divTrunc(messages * 1000, elapsed) else 0;

    var out_buf: [4096]u8 = undefined;
    var fw = std.Io.File.stdout().writer(io, &out_buf);
    const w = &fw.interface;
    try w.print("chan fanout benchmark\n", .{});
    try w.print("messages={d}\n", .{messages});
    try w.print("producers={d}\n", .{producers});
    try w.print("consumers={d}\n", .{consumers});
    try w.print("sent={d}\n", .{sent});
    try w.print("received={d}\n", .{received});
    try w.print("elapsed_ms={d}\n", .{elapsed});
    try w.print("rate_per_sec={d}\n", .{rate});
    try w.print("peak_rss_kb={d}\n", .{peakRssKb(io)});
    try w.print("checksum={d}\n", .{checksum});
    try w.flush();
}
