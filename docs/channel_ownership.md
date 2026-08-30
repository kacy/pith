# channel ownership

a `Channel[T]` is allocated once and never freed. `pith_channel_new`
(`cranelift/runtime/src/concurrency/channel.rs`) hands the language a raw
pointer built with `Box::into_raw`, and there is no `Box::from_raw` anywhere in
the runtime — no `pith_channel_free`, no `pith_channel_release`, nothing in the
ABI table. `close()` marks the channel closed and wakes whoever is parked on it;
the memory stays. nothing in the language owns one either: `ir_rc_kind` answers
`""` for a channel, so a channel in a local, a struct field, a container or a
closure capture is a bare integer with no retain and no release.

this page is the design work behind issue #960: what a channel actually costs,
who can be holding one when it would be freed, why the obvious free path is a
crash rather than a fix, the options with their costs, and a staged plan. it is
a plan, not a description of what ships today. only stage 1 is implemented; a
prototype of the rest exists unmerged, and what it settled is recorded under
"what a prototype established" below.

## what a channel costs

measured with a probe that creates and closes channels in a loop and reads
`VmHWM`, 20,000 rounds per capacity, plus the `PITH_PERF_STATS` accounting
stage 1 adds:

| capacity | ring slots | bytes requested | bytes resident |
|---|---|---|---|
| 0 (unbuffered) | 0 | 416 | 512 |
| 1 | 2 | 448 | 544 |
| 64 | 64 | 1,440 | 1,536 |
| 256 | 256 | 4,512 | 4,608 |

the fixed part is 416 bytes: the tagged stub, the `Arc` header, and a
`ChannelInner` whose two `#[repr(align(64))]` ring positions round it up. the
variable part is exactly 16 bytes per ring slot, and the ring is eager —
`Ring::new` allocates `capacity.next_power_of_two().max(2)` slots at
construction whether or not a value is ever sent. the gap between the two
columns is allocator rounding, so the requested figure under-reports.

20,000 channels of capacity 256 grow the process by about 88 MiB, and none of it
comes back.

what that looks like in a real program: `tests/cases/test_grpc_interactive_stream`
runs two rpcs over one connection and reports

```
  channels: new=15 closed=14 retained_bytes=29024
```

fourteen of the fifteen were closed. none were freed.

the streaming rpc path is where this shows up as a per-request cost.
`Client.open_stream_with_body` (`std/net/http2/connection.pith:1459`) creates a
`Channel[StreamEvent](STREAM_INBOX_CAP)` — 256 on the client, 4,512 bytes — and
at `:1465` a `Channel[Int](CREDIT_POKE_CAP)`, another 448. that is 4,960 bytes
per stream that the process can never reclaim, which is the bulk of the
~5.8 KB/rpc separating a streaming call from a unary one. sequential unary
requests ride the inline path and create no channels at all, which is why the
two arms diverge.

## the lifetime facts

### how a handle is represented

`Channel[T]` is checker kind `"channel"` (`self-host/checker_type_intern.pith`),
emission kind `"channel"` (`self-host/ir_type_helpers.pith:51`), and at the IR
and machine level a bare `i64`. `grep -i channel cranelift/codegen/src/` returns
nothing: codegen has no notion of a channel, it moves an integer. copying a
handle is an integer copy, and there is no point in the pipeline where a copy is
observable.

### who can hold one at once

a survey of every `Channel[...]` construction in the tree — 130 live sites
across 46 files — puts them in these buckets:

| shape | sites |
|---|---|
| scope-local, never escapes the creating function | 37 |
| handed to one or more `spawn`ed tasks | 51 |
| stored in a struct field | 26 |
| stored in a list or map | 15 |
| captured by a closure | 0 |
| returned bare from a function | 1 (12 more escape inside a struct) |
| module-level | 0 direct, 1 reachable through a global map |

multi-owner is the normal case, not the exception. three shapes make that
concrete:

- `std/net/http2/connection.pith:1459` binds one inbox handle to a local,
  inserts it into `Client.streams`, and returns it inside a `Stream` struct at
  `:1493`. a third holder is the connection's reader task, which copies the
  handle back out of the map in `deliver` (`:1004`) and sends to it. that
  channel has three live references from the moment the stream opens.
- `std/concurrent.pith:73` stores one `done_ch` in a `Context` *and* a
  `CancelToken`, returns both as a tuple, and hands the same handle to
  `spawn watch_parent`. the aliasing is the feature: cancelling the token is
  how the context learns it was cancelled.
- `tests/green/waitgroup.pith:38` creates a channel in `main`, passes it to
  `spawn coordinator(...)`, and the coordinator fans it out to `n` more spawned
  tasks. two levels of hand-off, and the creating frame returns first.

three further shapes constrain any design:

- constructors appear inline in struct-literal argument lists with no local
  binding at all — `connection.pith:717` builds four channels that way, and
  `examples/concurrency.pith:65` passes `Channel[Int]()` straight into a call as
  an anonymous temporary.
- the copy-under-mutex idiom is repeated at least six times
  (`connection.pith:1004`, `:1236`, `:1409`; `server.pith:776` and mirrors): the
  handle is read out of a map under `self.mu` and used after the unlock,
  sometimes accumulated into a `List[Channel[Int]]` first. this has to stay a
  borrow — a refcount bump inside the lock would put an atomic on a contended
  path that deliberately has none.
- `spawn` does not marshal arguments as call arguments. `ir_emit_spawn_wrapper`
  (`self-host/ir_emitter_core.pith:6794`) reifies the spawned call into a
  synthetic zero-argument function and passes everything it needs as *closure
  captures*, through the same `ir_emit_lambda_capture_setup` a lambda uses. a
  captured channel takes the untagged `__closure_set_env` branch at `:3310` —
  no count, no element tag — because its `ir_rc_kind` is `""`. a spawn handle
  can also be discarded at statement position (`tests/cases/test_discarded_spawn_runs.pith:13`,
  `std/net/http2/server.pith:1114`), so there is not even a join point to hang a
  drop on.

### what the runtime does on close

`pith_channel_close` (`channel.rs:805`) sets `state.closed`, publishes
`closed_flag`, clears any rendezvous `pending_value`, then `notify_all`s both
condvars and drains both green waiter lists. a woken caller re-checks and
returns a failure or a `none`:

- a parked buffered receiver takes the `closed_flag` branch in `buffered_recv`,
  drains anything still in the ring, and returns `optional_tuple(false, 0)`.
- a parked buffered sender takes the `closed_flag` branch in `buffered_send` and
  returns 0.
- a parked unbuffered sender compares `state.deliveries` against the count it
  recorded before parking, so a value a receiver already took is reported
  delivered even though the channel closed underneath it.

close is not free, and the codebase depends on that. `connection.pith:686`
relies on a closed inbox still yielding its buffered events, and
`tests/cases/test_channel_runtime.pith:29-30` pins double close as a no-op that
returns `false`. any free would have to be a third operation, not a change to
this one.

### every way a handle outlives its creating scope

1. **a spawned task** — captured untagged into the spawn closure's env
   (`ir_emitter_core.pith:3310`), which the closure's own free never touches.
   51 sites.
2. **a struct field** — `ir_rc_field_kind` resolves a `Channel[T]` field to
   `""`, so `ir_struct_needs_dtor` (`self-host/ir_struct_registry.pith:161`)
   does not count it, and a struct whose only heap-ish field is a channel is
   recorded as needing no destructor at all. 26 sites.
3. **a container** — `Map[Int, Channel[StreamEvent]]` and
   `Map[Int, Channel[Int]]` are untagged primitive maps: `ir_element_tag_code`
   (`ir_emitter_core.pith:4930`) has no channel value, so the map neither
   retains on insert nor releases on eviction. 15 sites.
4. **a return value** — three signatures return a bare channel
   (`std/concurrent.pith:148`, `:246`, `:320`); the last two are accessors that
   hand out an alias of a stored field, so `ticker.channel().recv()` creates a
   second reference for the duration of one expression.
5. **a global** — `std/net/grpc.pith:1800`'s `live_streams` map reaches
   `ServerStream.inbox` and is drained by a detached watchdog task spawned at
   `:1889`, which can outlive every handler.
6. **an anonymous temporary** — `examples/concurrency.pith:65` constructs a
   channel directly in an argument list. nothing ever names it.

no closure captures a channel today, and no channel is ever a map key or set
element — `Set[Channel[Int]]` is rejected by E254
(`tests/invalid/unhashable_set_element.pith:52`), so a handle is never hashed or
compared. that is one fewer constraint on any encoding change.

### close is also not reliably reached

for completeness, because it bears on option d: `std/net/http2/connection.pith:817`
holds the only `defer` on a channel in the tree, and there is no `errdefer` on
one anywhere. `Client.handoff` has no `close()` call site at all. `Client.ready`
is closed only if the client ever promotes out of inline mode.
`mysql.Pool.slots` and `postgres.Pool.slots` survive `Pool.close()`.
`concurrent.Ticker`'s two channels survive `Ticker.stop()`. a `background()`
context that is never cancelled never closes its `done_ch`, and there are 46
`background()` call sites. and in all four programs that drive a gRPC stream,
`stream.close()` is never called — only `conn.close()`, which does not touch the
per-stream channels.

## the hazard

### what the tag buys today

a channel handle is validated by alignment plus a magic word rather than through
the global handle registry:

```rust
const CHANNEL_MAGIC: u32 = 0x50434841;

unsafe fn channel_ref<'a>(handle: i64) -> Option<&'a PithChannelHandle> {
    let ptr = handle as *const TaggedChannel;
    if !handle_registry::plausibly_aligned::<TaggedChannel>(ptr as *const ()) { return None; }
    if (*ptr).magic != CHANNEL_MAGIC { return None; }
    Some(&(*ptr).channel)
}
```

the registry it replaced cost two global mutex round trips per message, send
plus recv, which was the hottest shared line left once the ring made the channel
body lock-free. the tag is two loads on a line the operation is about to touch
anyway.

the check is sound *only because channels are never freed*. an address that once
named a channel names that same channel forever, so a stale handle either finds
its own channel or fails the magic compare. the comment on `CHANNEL_MAGIC` says
as much, and the free path is what would take it away.

### what freeing would cost

there are two distinct failures, and they need separating.

**a recycled tagged address is worse than either alternative.** if the
`TaggedChannel` stub is freed, the allocator will hand that same address back
for the next channel of the same size class — they are all the same size class.
a stale handle then passes the alignment check, passes the magic compare, and
operates on a *different, live* channel: sends land in the wrong queue, a `recv`
takes a value out from under its real receiver, a `close` shuts down a stream
that is still running. that is a silent wrong answer on the concurrency
primitive, which is strictly worse than the current leak and worse than a
crash. any design that frees a channel must therefore either keep the stub
address permanently reserved or replace the tag with a check that survives
address reuse.

**a freed body under a parked caller is a use-after-free.** every channel
operation holds `inner: &ChannelInner` — a reference derived from a raw pointer
with an unbounded lifetime — across the point at which it can block. the
sequence in `buffered_recv` is:

```rust
state = block_on_channel(inner, state, green_task, Role::Receiver);
drop(state);
inner.parked_receivers.fetch_sub(1, Ordering::SeqCst);
```

`block_on_channel`'s green arm registers the task, drops the channel lock,
`green::park_current`s, and on resume does `lock_state(&inner.state)` — it locks
a mutex living in the freed allocation. `buffered_send` has the mirror shape.
the os-thread arm is no better: `cvar.wait(state)` returns a guard onto the same
memory.

and a free cannot simply wait for its waiters, because `wake_green` does not
resume anyone. it drains `state.green_receivers` under the channel lock and
calls `green::wake(id)`, which flips a scheduling word and enqueues the task on
a worker. the task dereferences `inner` some time later, on another thread. by
then the closing frame has long returned.

worse, the parked counters do not even cover everyone. `parked_senders` and
`parked_receivers` count callers that decided to block. a caller between
`channel_ref` returning and its own return — the whole lock-free ring fast path
— is counted nowhere at all. so "no waiters" is not "no references", and a free
gated on the parked counts would still be a race.

### the `Arc` is not currently doing anything

`ChannelInner` sits behind an `Arc`, and `Arc::clone` is never called on it
anywhere in the file: `channel_ref` returns a *borrow* of the handle, not a
clone. the strong count is permanently 1. as it stands the `Arc` is 16 bytes of
header and an indirection that buys no lifetime guarantee at all.

it is also the obvious lever. a channel operation that cloned the `Arc` before
it could block, and dropped it after, would make the body outlive every parked
caller by construction — the last clone to drop runs the destructor. the cost is
two atomic read-modify-writes on one shared cache line, which is exactly the
traffic the ring design removed from the fast path. so the clone can only be
afforded on the *slow* path, and the fast path needs something else.

### do generation counters transfer from task handles

task handles solve the same problem, and the answer is: the mechanism transfers,
the encoding does not, and the reason is where the cost lands.

`cranelift/runtime/src/concurrency/task.rs` runs a generational slotmap. a
handle packs `index + 1` in the low 32 bits and the slot's generation in bits
32..62 (`make_handle`, `:122`). reclaiming a slot bumps its generation, so a
stale handle fails `get_checked` (`:183`) and reads as "no such task" instead of
aliasing whatever took the slot. `reclaim` (`:210`) uses the generation to
settle a race between several callers trying to reclaim the same slot.

a generation would fix the *first* failure above and only that one: it makes a
recycled slot detectable, so a stale handle gets the safe default instead of
someone else's channel. it does nothing about the second — a parked receiver
already holds a direct `&ChannelInner`, not a handle it re-resolves, so no
amount of handle validation reaches it.

and the encoding cannot be copied as-is. a task slot is an index into a
`Vec` behind a global mutex, and every task operation — spawn, await, detach —
already pays that mutex, a handful of times per task. a channel operation is the
hot path; the tag scheme exists precisely because the equivalent global lock was
measured as the dominant cost. a generation for channels would have to live
*in* the allocation, next to the magic word, and be compared without a lock:

```rust
struct TaggedChannel { magic: u32, generation: u32, channel: ... }
```

with the generation also packed into the handle's high bits. that is one extra
load and compare off a line already being read, which is affordable. what it
does *not* give is a safe free — only a safe detection of a stale handle to a
reused one.

### the payload problem

there is a second obligation that any free path inherits, and it is easy to miss.

values sent down a channel are already counted asymmetrically. a *borrowed* rc
value passed to `send` gets a retain the sender adds and nobody drops —
`ir_channel_send_needs_retain` (`ir_emitter_core.pith:5007`), documented in
`docs/ownership.md` as a deliberate leak in the safe direction, because a
channel holds a raw handle between the send and the receive and neither side can
take the count. the receive side compensates where it can:
`ir_runtime_opt_producer_payload_kind` (`ir_emitter_core.pith:1229`) attaches an
`__opt_dtor_<kind>` to the shell `recv()` returns, so a value that *is* received
balances out.

a value still sitting in the ring when the channel dies is never received, so
its count is never balanced. freeing the channel therefore has to drain the ring
and release each remaining element — and to do that it must know the element's
kind, which the channel does not have. `Ring` stores bare `i64` cells. a
`List[T]` learns its element tag at the store (`ir_element_tag_code`); a channel
learns nothing.

this is why the free path and the element tag are one piece of work rather than
two. it also explains a known hole in the cycle collector: `docs/ownership.md`
lists a cycle through a channel's buffer as uncollectable, because the tracers
cannot see edges the channel does not know it has.

## the options

### a. give channels a reference-counted rc kind

teach `ir_rc_kind` to answer `"channel"` and let the existing ARC machinery own
the handle.

what it fixes: every escape shape at once, because every escape shape already
goes through the kind dispatch. a bind retains, a scope exit releases, a struct
field carries the kind into the generated destructor, a container store carries
an element tag, a spawn capture takes the tagged branch and the closure's free
releases it. multi-owner aliasing — the `Context`/`CancelToken` pair, the inbox
in a map and a struct and a reader task — is exactly what a refcount is for. it
also gives the channel a place to release its undrained payloads, since the same
change is what supplies an element tag.

what it cannot fix on its own: a cycle through a channel still leaks, the same
as any other cycle. and the count says nothing about a *parked* caller — a
receiver blocked inside `buffered_recv` holds a `&ChannelInner`, not a count, so
the last release could still fire under it. the runtime side has to hold a count
for the duration of a blocking operation, which brings back the `Arc` clone and
its cost.

crash risk: high if landed as one change, and the failure mode is the bad one —
a use-after-free on a concurrency primitive, which reproduces under load and not
under a unit test.

blast radius: the emitter side is a long but mechanical list —
`ir_rc_kind` (`ir_struct_registry.pith:33`), `ir_rc_retain_reg` and
`ir_rc_release_reg` (`ir_ownership.pith:58`, `:119`), `ir_cc_visit_code`,
`ir_element_tag_code`, `ir_closure_capture_tag`, `ir_tuple_elem_code` and its
inverse, `ir_list_ctor_for_elem_kind`, `ir_store_learns_kind`,
`ir_container_store_takes_count`, `ir_owned_arg_kind_releases`. the runtime side
adds a tag to `element_tag_from_code` (`collections/list.rs:275`) and a
`CLOSURE_TAG_CHANNEL = 9` (`runtime_core.rs:338`), plus two new ABI entries in
`runtime-abi/runtime_functions.txt`. every one of the numeric tag tables is a
cross-language contract that must not be renumbered. an emitter change of this
size means a regenerated bootstrap seed and a full byte-identical goldens pass.

cost: the largest of these, and the only one that ends with channels behaving
like every other heap value.

### b. an owning registry with a generation-tagged handle

keep the allocation in a runtime-owned table, hand out `(slot, generation)`
handles the way task handles work, and free on an explicit `pith_channel_free`.

what it fixes: the stale-handle failure, cleanly and detectably. a handle to a
freed slot reads as invalid and gets the safe default.

what it cannot fix: who calls `free`. the language has no owner to call it, so
this option only relocates the question — either std calls it by hand, which is
`docs/destructors_roadmap.md`'s problem all over again on a type where a missed
call is now a dangling handle rather than a leak, or the emitter calls it, which
needs option a anyway. it also does nothing for the parked-caller race.

crash risk: moderate for stale handles (they are caught), high for parked
callers (they are not).

blast radius: runtime only, but it puts a lookup back on the hot path. a
lock-free slot table would avoid the mutex the tag scheme was introduced to
escape; a `Vec` behind a mutex would reintroduce it. the handle encoding change
is invisible to pith source because a handle is never hashed or compared.

cost: moderate, and it does not stand alone.

### c. scope-bound channels the checker forbids from escaping

restrict a channel to its creating function so a scope-exit free is trivially
safe.

what it fixes: the entire problem, for 37 of 130 sites.

what it cannot fix: the other 93. it breaks `spawn` hand-off outright, which is
51 sites and the single most common shape in the tree — and `spawn` is the one
thing channels exist for. `spawn_arg_shares_collection`
(`self-host/checker.pith:6698`) currently flags a list, map or set shared into a
`spawn` and deliberately exempts channels, because "send it through a channel"
is the escape hatch the diagnostic recommends. this option would close the
escape hatch. it also breaks every struct that holds one (`Context`, `Ticker`,
`Client`, `Server`, `Stream`, `Pool`, `Terminal`) and both http/2 stream tables.

crash risk: none. it is a checker restriction.

blast radius: catastrophic in source terms. std would not compile.

cost: low to implement, unacceptable in what it costs the language. not viable.

### d. leave the allocation permanent, shrink the quantum

accept the leak and reduce it: a smaller default ring, or a pool of channels
reused across streams.

what it fixes: the magnitude, not the shape. dropping `STREAM_INBOX_CAP` on the
client from 256 to the 64 the server already uses takes the per-stream cost from
4,960 bytes to 1,888 — a 62% cut on the streaming path for a one-constant
change, with no runtime change, no crash risk, and no new machinery.

what it cannot fix: anything about lifetime. a long-running server still grows
without bound, just more slowly. and it is not free of behaviour: the client's
reader task is one task multiplexing every stream on the connection, and
`deliver` (`connection.pith:1006`) does a *blocking* `inbox.send` outside the
mutex. a smaller inbox means the reader stalls sooner when one consumer falls
behind, and a stalled reader is head-of-line blocking across every stream on
that connection. the server runs at 64 with the same structure, which is
evidence that 64 is tolerable, not proof — the server's reader serves one
connection's streams too, but its handlers are spawned per stream and its
events per stream are fewer. the constant's comment says 256 was chosen so "the
reader rarely blocks handing an event to a stream whose consumer is momentarily
behind", and nothing in the tree measures where that boundary is.

pooling is worse than it looks. returning a channel to a pool and handing it to
a new stream *is* address recycling, with the additional problem that a stale
handle to the pooled channel passes every check and the channel is genuinely
live. a generation (option b) is the minimum entry price, and even then a task
still parked on the recycled channel would corrupt the new stream's state rather
than fault.

crash risk: none for the constant, moderate for pooling.

blast radius: one line in std for the constant; a tuning change with an
unmeasured failure mode under load.

cost: lowest, and it buys time rather than a fix.

### e. deferred reclamation for the fast path

not an alternative to a — a component it needs. hold a count only where blocking
is possible (clone the `Arc` before parking, drop it after), and cover the
lock-free fast path with a grace period: the freed body is retired to a pending
list and its memory returned only once every worker has passed a quiescent
point, which under the green backend is a natural scheduling boundary. the fast
path then costs nothing, because it cannot span a quiescent point.

what it fixes: the parked-caller and mid-operation races, which are the part
option a cannot express in the emitter.

what it cannot fix: the stale-handle failure. it must be paired with a
permanently reserved stub or a generation.

crash risk: this is the subtle machinery, and the place to be most careful. it
needs the quiescent point defined against the os-thread backend as well as the
green one, where a caller blocked in `cvar.wait` is not passing any scheduling
boundary at all.

blast radius: runtime only, concentrated in `channel.rs` and the green
scheduler.

cost: moderate, and unavoidable if channels are ever to be freed.

## the recommendation

**option a, staged, with option e as its runtime half — and with the stub
address never recycled.**

the others lose for specific reasons. **c** is not viable: it forbids the
pattern channels exist for, and std would not compile. **b** relocates the
question rather than answering it, and answering it needs a anyway; its
generation idea is worth keeping as a defence, not as the design. **d** is a
real 62% cut on the streaming path and it is tempting, but it is a tuning change
with an unmeasured failure mode — a stalled connection reader — and it leaves a
long-running server growing without bound. it belongs in the plan as a
measurement, not as the answer.

three constraints shape how a is staged.

**the stub stays permanent.** split the allocation: the `TaggedChannel` stub is
never freed, so a validated address never names a different channel and the
magic check keeps the invariant it depends on; the `ChannelInner` and its ring —
400 bytes plus 16 per slot — are what a free reclaims. a stub per channel ever
created is a leak the process can carry; 4,512 bytes is not. the stub is bigger
than the 16 bytes this section first assumed, and the prototype measured it: see
below.

**the runtime holds the count across a block, not the emitter.** the emitter
cannot see that a receiver is parked. the runtime can, and the `Arc` is already
there for it to use.

**the element tag comes with the free, not after it.** a freed channel must
release what is still in its ring, and it cannot do that without knowing the
element kind. the same emitter change supplies both.

## staged plan

### stage 1 — make the cost measurable — done

`PITH_PERF_STATS` now reports

```
  channels: new=N closed=C retained_bytes=B
```

`retained_bytes` is the exact allocation request per channel — stub, `Arc`
header, `ChannelInner`, ring slots — summed over every channel the process ever
created, which is also every channel it still holds. `closed` counts channels
taken out of service, not memory handed back, and the gap between the two is the
point of the line.

this is a counter and nothing else: no free path, no behaviour change, gated by
`perf_count` so it costs a predicted branch when the flag is off. it is here
first because every later stage is judged against this number, and because
until now the only way to see the leak at all was to watch rss and infer.

## what a prototype established

a working prototype exists on the local branch `channel-reclaim-probe`. it is
not merged, and the numbers below come from it. what it settles answers four
questions this page left open, and removes one stage from the plan.

**the free path works, and its trigger is simpler than the rc kind.** a channel
that is closed and drained is inert: every operation on one already returns a
fixed answer (send 0, recv none, close 0, len 0, is_closed 1). reclaiming the
body at that moment needs no emitter change, no rc kind and no ownership
analysis, because nothing is left for an owner to decide. twenty thousand
create-and-close cycles of a 256-slot channel measure 92,240 kb of peak rss on
main against 6,320 kb on the prototype, reporting `freed=20000` and
`freed_bytes=89600000`.

what remains is the permanent stub, and it is larger than this page assumed.
counted over a create-and-close loop, 128 bytes per channel is never freed, and
peak rss grows about 192 bytes per channel once allocator rounding is included.
the size is not an oversight: the stub carries the park-path reference count on
a cache line of its own, which is what keeps the fast path off a shared line and
buys the 2% figure below. the two are the same decision, so the cost per channel
and the cost per operation trade against each other directly.

so this is a 35-fold reduction rather than an elimination: a process that
creates a channel per request still grows, 4,512 bytes at a time before and 128
after. a channel leak-gate case cannot assert flatness on channel *creation* for
that reason, and the one added here churns messages through a fixed set of
channels instead, which is the part that must be flat and is.

**stage 2 is unnecessary.** generations were kept in the plan as a defence, but
the permanent stub already supplies what they were defending: a validated
address can never name a different channel, because the address is never reused.
a generation would detect a hazard the split allocation prevents outright.

**stage 3's flatness requirement is the real constraint, and it decides the
design.** the obvious reference discipline, a count on the stub held for the
span of every operation, fails it: two shared read-modify-writes on the hot path
cost about 25% of `chan_fanout` throughput on os threads and about 47% under
green. per-thread hazard slots and a limbo list bring that to roughly 2%
**on the green backend**, because an operation then publishes the body pointer
to a line no other thread touches, and the count that remains covers only the
paths that can block.

the os-thread backend is a different story, and it was measured only after the
change had landed. against the commit before it, spaced and order-rotated over
21 and then 15 rounds, `PITH_GREEN=0` reads 430-492 ms before and 504-652 after
on the first run, and a median of 477 against 581 on the second: somewhere
between 15% and 35% slower. green over the same runs is within a few percent,
which is what the 2% figure above describes.

the likely reason is that a hazard slot is per os thread and the two backends
have very different numbers of them. under green a fixed pool of workers carries
any number of tasks, so retirement scans a handful of slots; under the
os-thread backend every spawned task is its own thread, so eight producer and
consumer tasks mean eight slots to register through a mutex and eight to scan.
that is a hypothesis rather than a profile, and #984 carries it. the
guard is load-bearing rather than defensive: an arm that freed at retirement
without it hung `chan_fanout`, with a consumer parked on a mutex freed
underneath it.

measuring this needs care. `chan_fanout` is bimodal on a two-core machine,
clustering near 130 ms and near 195 ms, which is the task placement race rather
than the change under test. medians over all runs are meaningless — a nine-round
sample reported the prototype 25% *faster* than main, which cannot be true of
code that adds bookkeeping. the figures above come from spaced, order-rotated
launches restricted to well-placed runs, over 21 rounds for os threads and 31
for green.

**the streaming residual is mostly channels.** issue #960 asked whether the
roughly 8.8 kb per streaming rpc was channel allocation. it is, to about 60-75%.
counters show a streaming rpc creating exactly two channels where a unary rpc
creates none: the 4,512-byte inbox and the 448-byte flow-control channel from
`connection.pith`. on the prototype the streaming-minus-unary rss delta falls
from about 6.9 kb per rpc to about 2.6 kb.

**what remains.** stage 4, the element tag, is untouched and still required: the
prototype retires only a drained ring, so a channel still holding
reference-counted payloads at close strands them exactly as main does. nothing
regresses without the tag, and nothing is finished while it is missing. a send
that passes its closed-check just before a concurrent close and drain can also
enqueue into a body that is then freed, losing the value rather than leaving it
drainable; main has the same window with different behaviour, and the intended
semantics want pinning by a test either way.

### stage 2 — a generation in the tagged stub — not needed

superseded by the prototype, and kept here for the reasoning. the idea was to
add a `generation: u32` beside the magic word and pack it into the handle's high
bits, as task handles do. nothing is freed yet, so no generation ever advances
and every existing handle still validates — the stage is a no-op by
construction, which is what makes it safe to land alone. what it buys is the
defence that has to exist *before* any reclamation does: from here on, a stale
handle to a reclaimed channel would be detectable rather than silently valid.

this is redundant against a permanent stub. the recommendation above already
requires the stub never to be freed, and an address that is never reused cannot
name a different channel, so there is no stale-but-valid handle for a generation
to catch. skip this stage; keep the split allocation it was compensating for.

### stage 3 — a reference discipline in the runtime

make `channel_ref` return an owned `Arc` clone on the paths that can block, and
introduce the retire-and-reclaim machinery of option e for the paths that
cannot. still nothing calls a free, so this stage is measured against the
channel throughput benchmarks and must come out flat: the fast path may not gain
an atomic. `bench/chan_fanout.pith` is the arm to watch, interleaved, and the
green pingpong benches for the handoff shape.

the prototype has now run that measurement, and it rules out the obvious
implementation. a count taken on the stub for the span of every operation is two
shared read-modify-writes on the fast path and costs about 25% on os threads and
about 47% under green. per-thread hazard slots with a limbo list, keeping the
count only on the paths that can block, measure roughly 2%. build this stage in
that shape, and read the note above on how to measure `chan_fanout` without the
placement race answering for you.

### stage 4 — the element tag

give a channel the element tag a container carries: `ir_element_tag_code` gains
a channel entry, `Channel[T]` construction passes its payload kind to
`pith_channel_new`, and send and recv account for the payload the way
`pith_list_push_value_kind` does. this retires
`ir_channel_send_needs_retain` — the last store the emitter counts for — and
gives the cycle collector the edges it currently cannot see. it is a real
ownership change with real goldens exposure, and it lands before any free
because a free that leaves its payloads behind trades one leak for another.

### stage 5 — the rc kind, and the free

add `"channel"` to `ir_rc_kind` and thread it through the kind tables listed
under option a. the release path calls a new `pith_channel_release`, which drops
the stub's `Arc` and, at the last count, drains the ring releasing each element
by its tag and retires the body. the stub is left in place with its generation
bumped.

this is the stage that can crash, so it is the stage with the standard to match:
valgrind clean under `PITH_STRUCT_FREELIST=0` on both backends, on a
parked-receiver-then-close shape and a channel shared across tasks; the leak
gate flat with a create-and-drop-a-channel-per-round case registered; and the
`retained_bytes` line from stage 1 going flat on the gRPC streaming arm, which
is the number issue #960 was opened about.

### the measurement that runs alongside

separately from the stages, and worth doing on its own: measure the client's
peak inbox depth under a realistic streaming load and find out whether 256 is
the right number. if a stream is never more than a few events deep, dropping
`STREAM_INBOX_CAP` cuts the per-stream cost by 62% today and keeps cutting it
after stage 5, since a smaller ring stays smaller once channels are freed. the
number to change it on is the measured depth, not the leak.
