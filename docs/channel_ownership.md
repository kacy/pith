# Channel ownership

A `Channel[T]` is a reference-counted value: `ir_rc_kind` answers `"channel"`,
so a handle in a local, a struct field, a container element, or a closure
capture is retained on copy and released at scope exit
(`pith_channel_retain` / `pith_channel_release`), and the channel's body is
freed at the last owner, with any values still queued released by the payload
tag the constructor recorded. `close()` marks the channel closed and wakes
whoever is parked on it. It does not free the channel, and it does not need
to.

This page records how that model works, what a channel costs, and how it
replaced the earlier design, in which a channel was never freed (issues #960
and #984).

## What a channel costs

A channel is one allocation: a fixed part of 448 bytes and a ring of 16 bytes
per slot. The ring is eager: `Ring::new` allocates
`capacity.next_power_of_two().max(2)` slots at construction, whether or not a
value is ever sent. An unbuffered channel has no ring.

| Capacity | Ring slots | Bytes requested |
|---|---|---|
| 0 (unbuffered) | 0 | 448 |
| 1 | 2 | 480 |
| 64 | 64 | 1,472 |
| 256 | 256 | 4,544 |

All of it returns at the last release. `PITH_PERF_STATS=1` reports

```
  channels: new=N closed=C freed=F retained_bytes=B freed_bytes=D
```

`retained_bytes` sums the allocation request of every channel the process
created, and `freed_bytes` the part handed back. A loop that creates and
closes 20,000 channels of capacity 256 reports `freed=20000` and equal
`retained_bytes` and `freed_bytes`.

The HTTP/2 client allocates a `Channel[StreamEvent](STREAM_INBOX_CAP)` inbox
(256 on the client, 64 on the server) and a `Channel[Int](CREDIT_POKE_CAP)` per
stream (`std/net/http2/connection.pith`), so a streaming RPC holds about 5 KB
of channels for as long as the stream lives. Unary requests on the inline path
create no channels.

## How the count works

`"channel"` has a row in every kind table at its own code: element tag 8,
closure capture tag 9 (`CLOSURE_TAG_CHANNEL` in `runtime_core.rs`), tuple
element letter `h`. `pith_channel_new` takes the payload's element tag as a
second argument and records it on the channel, because the binding keeps only
`"channel"` and the constructor site is the one place that knows `T`. The last
release drains values never received and releases each by that tag after it
has dropped its locks. A payload that is itself a channel releases through the
same path.

A value sent is a store into a counted container: a borrowed value takes a
count at the send (`ir_channel_send_needs_retain`), and the receive hands its
optional shell a destructor for the payload. See
[ownership.md](ownership.md).

The runtime takes a count of its own only around a park. A blocking entry point
declares a `ParkCount` first, so it is released last, after every read of the
channel that follows the wake. The fast path takes no count: every operation
validates its handle by alignment and a magic word (`CHANNEL_MAGIC`) and pays
nothing else. A freed channel has its magic word scrubbed, so a stale handle
fails that check and gets the closed-and-drained default, the same way a
garbage handle does.

## What close does

`pith_channel_close` (`cranelift/runtime/src/concurrency/channel.rs`) sets
`state.closed`, publishes `closed_flag`, clears any rendezvous
`pending_value`, then `notify_all`s both condvars and drains both green waiter
lists. A woken caller re-checks and returns a failure or a `none`:

- A parked buffered receiver takes the `closed_flag` branch in `buffered_recv`,
  drains anything still in the ring, and returns `none`.
- A parked buffered sender takes the `closed_flag` branch in `buffered_send` and
  returns 0.
- A parked unbuffered sender compares `state.deliveries` against the count it
  recorded before parking, so a value a receiver already took is reported
  delivered even though the channel closed underneath it.

A closed channel still yields its buffered values, and a second close is a
no-op that returns `false` (`tests/cases/test_channel_runtime.pith`).

A send that passes its closed check just as a concurrent close drains the
channel can enqueue a value nobody will read. The send reports success and the
value is released with the channel. This is a property of close and does not
depend on reclamation.

## Limits

- A cycle through a channel's buffer is not visible to the cycle collector, so
  it leaks like an unmarked cycle (see [ownership.md](ownership.md)).
- The OS-thread backend pays part of the reclamation cost in channel
  throughput. After the change, `chan_fanout` on OS threads measured about 12%
  behind the build that never freed a channel, against a 4 to 5% noise floor.
  The green backend, the default, is within noise. The remaining candidate is
  the two atomics on the park path.
- Whether 256 is the right client inbox capacity has not been measured. A
  smaller inbox costs less per stream, but the client's reader task does a
  blocking `inbox.send` for every stream on the connection, so a smaller inbox
  makes it stall sooner when one consumer falls behind.

## How it got here

A survey of the tree found channels shared between several owners as the
normal case: handed to spawned tasks, stored in struct fields and maps, aliased
across a `Context` and its `CancelToken`, and copied out of a map under a
mutex. Five designs were weighed: an RC kind for channels, an owning registry
with generation-tagged handles, scope-bound channels the checker keeps from
escaping, a permanent allocation with a smaller ring, and deferred
reclamation for the lock-free fast path. Scope-bound channels would have broken
`spawn` hand-off and most of std, and a registry would only have moved the
question of who calls `free`.

The first version that shipped freed a closed and drained channel's body
through per-thread hazard slots and kept a small stub forever, so an address
could never name a different channel. It cost the OS-thread backend about 14%
of `chan_fanout` throughput. It was then replaced by the RC kind described
above (#1020, #1021): the stub, the hazard guard and the limbo list are gone,
and a channel is one allocation owned by its count.
