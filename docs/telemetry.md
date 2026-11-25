# Telemetry

Pith ships its own observability stack, written in Pith like everything else:
Prometheus metrics you scrape, and OpenTelemetry traces and metrics you push to
a collector. The std HTTP and gRPC clients instrument themselves, so a service
that talks over them gets request traces and RED metrics without you writing any
of it.

There are three parts, and you can adopt them in any order:

- `std.metrics`: counters, gauges and histograms, with labels and units.
- `std.prometheus`: serves those metrics at `/metrics` for Prometheus to scrape.
- `std.trace` + `std.otlp` + `std.obs`: distributed tracing and OTLP push export.

## The zero-code path

The whole setup is two environment variables and one call:

```pith
import std.obs as obs

fn main() -> Int!:
    obs.init()          # reads OTEL_* from the environment
    run_server()!
    obs.shutdown()      # optional: flush one last time on the way out
    return 0
```

```
OTEL_SERVICE_NAME=checkout
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
```

With the endpoint set, `init()` turns tracing on, tells `std.metrics` to start
collecting, and spawns a background task that ships finished spans and the
current metric snapshot to the collector. Every call your service makes over the
std gRPC or HTTP client now produces a client span and RED metrics, and any
inbound `traceparent` is joined to the right trace.

With no endpoint set, `init()` returns immediately and the program runs
uninstrumented. Leaving the call in costs nothing in production, so you don't
need to guard it behind a build flag.

## Metrics

`std.metrics` is a small registry of three instrument types. You name an
instrument, optionally give it labels, a unit and help text, and record into it.
Values are floats, so seconds and ratios are as natural as counts.

```pith
import std.metrics as metrics

metrics.counter("orders_total").inc()
metrics.counter("orders_total").labels(["region", "eu"]).inc()
metrics.gauge("cpu_load").set(0.7)

latency := metrics.histogram("request_seconds").buckets([0.005, 0.01, 0.05, 0.1, 0.5, 1.0])
latency.describe("request latency", "s")
latency.observe(0.234)
```

`labels(["k1", "v1", "k2", "v2"])` takes a flat key/value list and returns an
instrument bound to that label set. Each distinct set accumulates on its own, so
the two `orders_total` lines above are separate series that render under one
`# TYPE` line. `buckets([...])` sets a histogram's boundaries (choose them before
the first observation; the default is a 1..5000 ladder) and hands back the
instrument to record into, since the boundaries decide how many bucket cells
the series has. `describe(help, unit)` attaches a `# HELP` line and feeds the
OTLP description and unit. Whole-number values still render cleanly: a count
reads `5`, not `5.0`.

Recording is lock-free. An instrument is a handle onto the atomic cells of one
series, resolved when you create it, so `inc`, `add`, `set` and `observe`
compare-and-set those cells and take no lock at all. The name-to-cells registry
behind them is sharded across eight mutexes and is touched only when you look a
metric up by name. An instrument is therefore safe to keep in a global and write
from every task, and it is equally fine to resolve one per request the way the
std clients do.

A scrape is coherent per series, not process-wide. Within one histogram the
buckets never outrun the count and the `+Inf` bucket always equals it, which is
the property Prometheus needs. Two different series may have been read
microseconds apart and need not describe the same instant.

`reset()` is for tests and short-lived tools. It clears the registry and leaves
behind the cells that instruments already hold, so an instrument taken before a
reset goes on writing to a series nothing can scrape. Take a fresh one after.

The std clients register these on your behalf:

| Metric | Labels | Source |
|--------|--------|--------|
| `http_client_requests_total` | `method`, `status` | `http` client calls |
| `http_client_duration_ms` | `method` | `http` client calls |
| `http_server_requests_total` | `method`, `route`, `status` | `begin/end_server_span` |
| `http_server_duration_ms` | `method`, `route` | `begin/end_server_span` |
| `rpc_client_requests_total` | `rpc_method`, `grpc_status` | gRPC client calls |
| `rpc_client_duration_ms` | `rpc_method` | gRPC client calls |
| `db_queries_total` | `driver`, `op`, `status` | Postgres and MySQL calls |
| `db_query_duration_ms` | `driver`, `op` | Postgres and MySQL calls |

## Exposing /metrics for Prometheus

`std.prometheus` renders the registry in Prometheus text format and serves it.
Run it in the background next to your app:

```pith
import std.metrics as metrics
import std.prometheus as prometheus

fn serve_metrics() -> Int!:
    return prometheus.serve("0.0.0.0", 9464)!

fn main() -> Int!:
    spawn serve_metrics()
    run_server()!
    return 0
```

`serve` answers `GET /metrics` with the current snapshot (including any `# HELP`
lines) and 404s everything else. It's pull-based, so there's no interval to
configure: Prometheus scrapes on its own schedule. Prometheus metrics and OTLP
push are independent; use either, both, or neither.

It drains like every other std listener (see [signals and graceful
shutdown](signals.md)). On a shutdown request the port stops accepting, so a
scrape that arrives after the signal is refused, and a scrape already in flight
is finished inside the grace period instead of being cut off mid-response.
`serve` then returns how much was still unfinished when that period expired, or
0 for a clean drain.

## Tracing

A span is one timed unit of work: a name, a start and end, some attributes, a
status, and its place in a trace. You start a span, do the work, and end it.

```pith
import std.trace as trace

fn handle_request():
    span := trace.start("handle_request")
    span.set_attr("http.route", "/api").set_attr_int("http.status_code", 200)
    span.add_event("cache.miss")
    ... work ...
    span.set_status(trace.STATUS_OK, "")
    span.end()
```

Attributes are typed: `set_attr` (string), `set_attr_int`, `set_attr_bool`, and
`set_attr_float`, each chainable. `add_event(name)` records a timestamped point
on the span (`add_event_attrs` adds attributes to it), and `add_link(ctx)` links
to another span's context. A span caps its attributes, events and links at 128
each and counts what it drops, so a runaway loop can't grow one without bound.
`start_kind(name, kind)` picks the span kind (`SERVER`, `CLIENT`, `PRODUCER`,
`CONSUMER`, or the default `INTERNAL`); the auto-instrumentation uses `CLIENT`
and `SERVER`.

### Spans nest on their own

The current span is held in `threadlocal` state (one copy per green task on the
green backend, one per OS thread on the OS-thread backend), so a span started
inside another is parented under it automatically. You don't thread a context
object through every call.

```pith
outer := trace.start("outer")
inner := trace.start("inner")   # parented under outer, same trace id
inner.end()                     # current span is outer again
outer.end()
```

When tracing is off, `start` returns a non-recording span that touches no
per-task state, and `end` on it does nothing. The check is a single bool, so
instrumented code you leave in an uninstrumented build stays cheap.

### Crossing a spawn

A spawned task starts with its own copy of that state and so has no current
span, which makes `spawn` the one place where propagation is explicit. Capture
the parent context before the spawn and restore it inside:

```pith
ctx := trace.current_context()
spawn work_item(ctx)

fn work_item(ctx: trace.SpanContext):
    trace.with_context(ctx)     # re-establish the parent in this task
    span := trace.start("work_item")
    ... work ...
    span.end()
```

## Sampling

By default every span in an active trace is recorded (the
`parentbased_always_on` sampler). In production you usually sample a fraction to
control volume and cost. `std.obs` reads the standard variables:

```
OTEL_TRACES_SAMPLER=parentbased_traceidratio
OTEL_TRACES_SAMPLER_ARG=0.1        # keep 10%
```

The samplers are `always_on`, `always_off`, `traceidratio`, and the
`parentbased_` variants of each. The ratio decision is deterministic on the
trace ID, so every service that sees a trace makes the same call and a trace is
kept or dropped as a whole. The `parentbased_` samplers inherit an incoming
decision from the `traceparent`, which keeps a distributed trace intact: a
downstream service records its spans only if the caller's were sampled.

An unsampled span is not a no-op. It still carries a valid context and
propagates (so downstream and child spans agree), but it isn't recorded or
exported. The sampling decision rides out in the `traceparent` flags (`-01`
sampled, `-00` not).

## Propagation across services

Trace context crosses a process boundary as a W3C `traceparent` header
(`00-{trace}-{span}-{flags}`). The std clients handle both ends of it:

- **Outbound**: the gRPC and HTTP clients inject `traceparent` from the current
  span, so the service you call joins your trace.
- **Inbound**: on a server, `begin_server_span(req)` reads any incoming
  `traceparent`, opens a `SERVER` span under it, and makes it current;
  `end_server_span(span, status)` closes it and records the server RED metrics.

```pith
span := http.begin_server_span(req)
resp := route(req)
http.end_server_span(span, resp.status)
```

If you're speaking a protocol the std clients don't cover, `format_traceparent`
and `parse_traceparent` give you the raw header both ways.

An inbound header is untrusted input, so `parse_traceparent` validates it before
it trusts it. The version, trace ID, span ID and flags must each be exactly
their spec width in hex; anything else returns `none` and starts a fresh trace
instead of joining a bogus one. The sampled decision is read as bit 0 of the
flags byte, as the spec defines it, so a peer that sets another flag alongside
it (`-03`) is still understood as sampled.

The spec writes the IDs in lowercase, and `format_traceparent` emits them that
way, but an uppercase header is still accepted and joined, because an upstream
that gets the case wrong is easier to diagnose from a connected trace than from
a silently broken one. The IDs are lowercased as they are parsed, so what the
service propagates and exports is canonical whatever a peer sent.

## OTLP export

`std.obs.init()` reads the environment and wires everything up. The variables
follow the OpenTelemetry SDK names:

| Variable | Meaning | Default |
|----------|---------|---------|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Collector base URL; unset means off | None |
| `OTEL_SERVICE_NAME` | `service.name` on exported data | `pith-service` |
| `OTEL_SERVICE_VERSION` | `service.version` | None |
| `OTEL_SERVICE_INSTANCE_ID` | `service.instance.id` | Generated per process |
| `OTEL_RESOURCE_ATTRIBUTES` | Extra resource attrs, `k1=v1,k2=v2` | None |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `http/protobuf` or `grpc` | `http/protobuf` |
| `OTEL_EXPORTER_OTLP_HEADERS` | Auth/tenant headers, `k1=v1,k2=v2` | None |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | Per-request timeout (ms) | `10000` |
| `OTEL_EXPORTER_OTLP_COMPRESSION` | `gzip` or `zstd` to compress request bodies | None |
| `OTEL_TRACES_SAMPLER` / `_ARG` | Sampler and argument (see sampling) | `parentbased_always_on` |
| `OTEL_METRIC_EXPORT_INTERVAL` | Milliseconds between metric flushes | `60000` |
| `OTEL_BSP_SCHEDULE_DELAY` | Milliseconds between span flushes | `5000` |

The exporter batches in the background: it drains finished spans on the span
interval, POSTs a metric snapshot on the metric interval, and does one final
flush on `shutdown()`. It retries transient failures (a timeout, a dropped
connection, `429`, or any `5xx`) with exponential backoff, honoring a
`Retry-After` header; a permanent `4xx` is dropped without retry. Spans that
finish faster than the collector drains them are capped, so a burst can't grow
the buffer without bound.

The exported data is complete. Spans carry typed attributes, events, links and
a status; histograms carry their explicit bucket bounds, per-bucket counts, and
min/max (so a backend can estimate percentiles); and the resource carries
`service.name`/`version`/`instance.id`, the `telemetry.sdk.*` identity, and any
`OTEL_RESOURCE_ATTRIBUTES`. `OTEL_EXPORTER_OTLP_HEADERS` is what lets you reach
a hosted backend: an API key or tenant header rides on every request over both
transports.

### Transports

`http/protobuf` (the default) POSTs protobuf to `/v1/traces` and `/v1/metrics`,
optionally gzip- or zstd-compressed. `grpc` makes a unary call to the
collector's `TraceService`/`MetricsService` `Export` methods carrying the
identical protobuf, with the auth headers as gRPC metadata. The gRPC transport
is TLS-only: the exporter dials with `grpc.dial` and never with `grpc.dial_h2c`,
so a `grpc` target needs an `https://` endpoint. A plain `http://` gRPC endpoint
fails with a clear message instead of mis-dialing.

## Flushing on exit

The exporter batches, so at any moment there is a span batch in memory that the
collector has not seen. A process killed at that moment loses it, and the spans
worth having are usually the ones from the minute a deploy went wrong.

A graceful shutdown does not lose them. When `obs.start()` spawns its exporter,
it registers with `std.shutdown` as a subsystem with shutdown work of its own,
and answers only once its final export has finished. A server that drains on
`SIGTERM` therefore waits for that export before returning:

```pith
import std.obs as obs
import std.shutdown as shutdown
import std.web as web

fn main() -> Int!:
    obs.init()
    shutdown.on_signals()!
    # returns after the in-flight requests AND the final span batch are done
    web.new().get("/", home).listen("0.0.0.0", 8080)!
    return 0
```

No extra call is needed, because `listen` already waits. The whole drain is
bounded by `shutdown.set_drain_deadline(ms)` (15 seconds by default), so an
unreachable collector delays a shutdown but cannot wedge it. The export is best
effort, and a failed POST is dropped with no retry past the deadline.

The exporter notices a shutdown within about 100ms instead of at the end of its
own interval. The span delay defaults to 5 seconds and the metric interval to a
minute, so a batch that waited a full interval after `SIGTERM` would already
have spent the grace period.

Calling `obs.shutdown()` by hand does the same thing for a program with no
server: it stops the exporter and waits (up to 5 seconds) for the final export
instead of guessing at how long it takes.

See [docs/signals.md](signals.md) for the drain itself.

## What isn't here yet

- **OTLP logs**: logs aren't exported over OTLP, and `std.log` does not read
  the active span. It has no dependency on `std.trace`, so a record emitted
  inside a `trace.start()` span carries no trace or span ID of its own. To
  correlate logs with traces, pass the IDs into the logger yourself with
  `logger.trace_context(trace_id, span_id)`.
- **gRPC without TLS**: the OTLP/gRPC transport requires TLS. The gRPC client
  can speak cleartext h2c (`grpc.dial_h2c`), but the exporter does not use it,
  so a bare local collector on `:4317` is unreachable. Use `http/protobuf` or an
  `https` gRPC endpoint.
- **Exemplars and delta temporality**: metrics export as cumulative, which
  matches how Pith accumulates them. There are no exemplars or delta
  temporality.
