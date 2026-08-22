# telemetry

pith ships its own observability stack, written in pith like everything else:
prometheus metrics you scrape, and opentelemetry traces and metrics you push to
a collector. the std http and grpc clients instrument themselves, so a service
that talks over them gets request traces and RED metrics without you writing any
of it.

there are three moving parts, and you can adopt them in any order:

- `std.metrics` — counters, gauges, and histograms, with labels and units.
- `std.prometheus` — serve those metrics at `/metrics` for prometheus to scrape.
- `std.trace` + `std.otlp` + `std.obs` — distributed tracing and OTLP push export.

## the zero-code path

set two environment variables and add one call. that's the whole setup.

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

with the endpoint set, `init()` turns tracing on, tells `std.metrics` to start
collecting, and spawns a background task that ships finished spans and the
current metric snapshot to the collector. every call your service makes over the
std grpc or http client now produces a client span and RED metrics, and any
inbound `traceparent` is joined to the right trace.

with no endpoint set, `init()` returns immediately and the program runs
uninstrumented. leaving the call in costs nothing in production, so you don't
need to guard it behind a build flag.

## metrics

`std.metrics` is a small registry of three instrument types. you name an
instrument, optionally give it labels, a unit, and help text, and record into it.
values are floats, so seconds and ratios are as natural as counts.

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
instrument bound to that label set. each distinct set accumulates on its own, so
the two `orders_total` lines above are separate series that render under one
`# TYPE` line. `buckets([...])` sets a histogram's boundaries (choose them before
the first observation; the default is a 1..5000 ladder). `describe(help, unit)`
attaches a `# HELP` line and feeds the OTLP description and unit. whole-number
values still render cleanly — a count reads `5`, not `5.0`.

the std clients register these on your behalf:

| metric | labels | source |
|--------|--------|--------|
| `http_client_requests_total` | `method`, `status` | `http` client calls |
| `http_client_duration_ms` | `method` | `http` client calls |
| `http_server_requests_total` | `method`, `route`, `status` | `begin/end_server_span` |
| `http_server_duration_ms` | `method`, `route` | `begin/end_server_span` |
| `rpc_client_requests_total` | `rpc_method`, `grpc_status` | grpc client calls |
| `rpc_client_duration_ms` | `rpc_method` | grpc client calls |

## exposing /metrics for prometheus

`std.prometheus` renders the registry in prometheus text format and serves it.
run it in the background next to your app:

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
lines) and 404s everything else. it's pull-based, so there's no interval to
configure — prometheus scrapes on its own schedule. prometheus metrics and OTLP
push are independent; use either, both, or neither.

it drains like every other std listener (see [signals and graceful
shutdown](signals.md)). on a shutdown request the port stops accepting, so a
scrape that arrives after the signal is refused rather than answered by a process
on its way out, and a scrape already in flight is finished inside the grace
period rather than cut mid-response. `serve` then returns how much was still
unfinished when that period expired — 0 for a clean drain.

## tracing

a span is one timed unit of work: a name, a start and end, some attributes, a
status, and its place in a trace. you start a span, do the work, and end it.

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

attributes are typed: `set_attr` (string), `set_attr_int`, `set_attr_bool`, and
`set_attr_float`, each chainable. `add_event(name)` records a timestamped point
on the span (`add_event_attrs` adds attributes to it), and `add_link(ctx)` links
to another span's context. a span caps its attributes, events, and links at 128
each and counts what it drops, so a runaway loop can't grow one without bound.
`start_kind(name, kind)` picks the span kind (`SERVER`, `CLIENT`, `PRODUCER`,
`CONSUMER`, or the default `INTERNAL`); the auto-instrumentation uses `CLIENT`
and `SERVER`.

### spans nest on their own

the current span is held in `threadlocal` state — one copy per green task on
the green backend, one per os thread on the os-thread backend — so a span
started inside another is parented under it automatically — you don't thread a context object through every
call.

```pith
outer := trace.start("outer")
inner := trace.start("inner")   # parented under outer, same trace id
inner.end()                     # current span is outer again
outer.end()
```

when tracing is off, `start` returns a non-recording span that touches no
per-task state, and `end` on it does nothing. the check is a single bool, so
instrumented code you leave in an uninstrumented build stays cheap.

### crossing a spawn

a spawned task starts with its own copy of that state and so has no current
span, which makes `spawn` the one place propagation is explicit. capture the parent context before, restore
it inside:

```pith
ctx := trace.current_context()
spawn work_item(ctx)

fn work_item(ctx: trace.SpanContext):
    trace.with_context(ctx)     # re-establish the parent in this task
    span := trace.start("work_item")
    ... work ...
    span.end()
```

## sampling

by default every span in an active trace is recorded (the `parentbased_always_on`
sampler). in production you usually sample a fraction to control volume and cost.
`std.obs` reads the standard variables:

```
OTEL_TRACES_SAMPLER=parentbased_traceidratio
OTEL_TRACES_SAMPLER_ARG=0.1        # keep 10%
```

the samplers are `always_on`, `always_off`, `traceidratio`, and the `parentbased_`
variants of each. the ratio decision is deterministic on the trace id, so every
service that sees a trace makes the same call and a trace is kept or dropped as a
whole. the `parentbased_` samplers inherit an incoming decision from the
`traceparent`, which is what keeps a distributed trace intact — a downstream
service records its spans only if the caller's were sampled.

an unsampled span is not a no-op: it still carries a valid context and propagates
(so downstream and child spans agree), it just isn't recorded or exported. the
sampling decision rides out in the `traceparent` flags (`-01` sampled, `-00` not).

## propagation across services

trace context crosses a process boundary as a W3C `traceparent` header
(`00-{trace}-{span}-{flags}`). the std clients handle both ends of it:

- **outbound** — the grpc and http clients inject `traceparent` from the current
  span, so the service you call joins your trace.
- **inbound** — on a server, `begin_server_span(req)` reads any incoming
  `traceparent`, opens a `SERVER` span under it, and makes it current;
  `end_server_span(span, status)` closes it and records the server RED metrics.

```pith
span := http.begin_server_span(req)
resp := route(req)
http.end_server_span(span, resp.status)
```

if you're speaking a protocol the std clients don't cover, `format_traceparent`
and `parse_traceparent` give you the raw header both ways.

an inbound header is untrusted input, so `parse_traceparent` validates it before
it trusts it: the version, trace id, span id, and flags must each be exactly
their spec width in hex, and anything else returns `none` and starts a fresh
trace rather than joining a bogus one. the sampled decision is read as bit 0 of
the flags byte, as the spec defines it, so a peer that sets another flag
alongside it (`-03`) is still understood as sampled.

the spec writes the ids in lowercase, and `format_traceparent` emits them that
way, but an uppercase header is still accepted and joined — an upstream that
gets the case wrong is easier to diagnose from a connected trace than from a
silently broken one. the ids are lowercased as they are parsed, so what the
service propagates and exports is canonical whatever a peer sent.

## OTLP export

`std.obs.init()` reads the environment and wires everything up. the variables
follow the opentelemetry sdk names:

| variable | meaning | default |
|----------|---------|---------|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | collector base url; unset means off | — |
| `OTEL_SERVICE_NAME` | `service.name` on exported data | `pith-service` |
| `OTEL_SERVICE_VERSION` | `service.version` | — |
| `OTEL_SERVICE_INSTANCE_ID` | `service.instance.id` | generated per process |
| `OTEL_RESOURCE_ATTRIBUTES` | extra resource attrs, `k1=v1,k2=v2` | — |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `http/protobuf` or `grpc` | `http/protobuf` |
| `OTEL_EXPORTER_OTLP_HEADERS` | auth/tenant headers, `k1=v1,k2=v2` | — |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | per-request timeout (ms) | `10000` |
| `OTEL_EXPORTER_OTLP_COMPRESSION` | `gzip` or `zstd` to compress request bodies | none |
| `OTEL_TRACES_SAMPLER` / `_ARG` | sampler + argument (see sampling) | `parentbased_always_on` |
| `OTEL_METRIC_EXPORT_INTERVAL` | ms between metric flushes | `60000` |
| `OTEL_BSP_SCHEDULE_DELAY` | ms between span flushes | `5000` |

the exporter batches in the background: it drains finished spans on the span
interval, POSTs a metric snapshot on the metric interval, and does one final
flush on `shutdown()`. it retries transient failures (a timeout, a dropped
connection, `429`, or any `5xx`) with exponential backoff, honoring a
`Retry-After` header; a permanent `4xx` is dropped without retry. spans that
finish faster than the collector drains them are capped, so a burst can't grow
the buffer without bound.

the exported data is complete: spans carry typed attributes, events, links, and a
status; histograms carry their explicit bucket bounds, per-bucket counts, and
min/max (so a backend can estimate percentiles); and the resource carries
`service.name`/`version`/`instance.id`, the `telemetry.sdk.*` identity, and any
`OTEL_RESOURCE_ATTRIBUTES`. `OTEL_EXPORTER_OTLP_HEADERS` is what lets you reach a
hosted backend — an api key or tenant header rides on every request over both
transports.

### transports

`http/protobuf` (the default) POSTs protobuf to `/v1/traces` and `/v1/metrics`,
optionally gzip-compressed. `grpc` makes a unary call to the collector's
`TraceService`/`MetricsService` `Export` methods carrying the identical protobuf,
with the auth headers as grpc metadata. the grpc transport is tls-only — pith's
grpc client has no cleartext h2c — so a `grpc` target needs an `https://`
endpoint; a plain `http://` grpc endpoint fails with a clear message rather than
mis-dialing.

## flushing on exit

the exporter batches, so at any moment there is a span batch in memory that the
collector has not seen. a process killed at that moment loses it — and the spans
worth having are usually the ones from the minute a deploy went wrong.

a graceful shutdown does not lose them. when `obs.start()` spawns its exporter it
registers with `std.shutdown` as a subsystem with shutdown work of its own, and
answers only once its final export has finished. so a server that drains on
`SIGTERM` waits for that export before returning:

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

no extra call is needed — `listen` already waits. the whole drain is bounded by
`shutdown.set_drain_deadline(ms)` (15 seconds by default), so an unreachable
collector delays a shutdown but cannot wedge it; the export is best effort, and a
failed POST is dropped rather than retried past the deadline.

the exporter notices a shutdown within about 100ms rather than at the end of its
own interval. that matters: the span delay defaults to 5 seconds and the metric
interval to a minute, so a batch that waited a full interval after `SIGTERM` is a
batch the grace period has already spent.

calling `obs.shutdown()` by hand does the same thing for a program with no
server: it stops the exporter and waits (up to 5 seconds) for the final export
rather than guessing at how long it takes.

see [docs/signals.md](signals.md) for the drain itself.

## what isn't here yet

- **OTLP logs** — logs aren't exported over OTLP, and `std.log` does not read
  the active span: it has no dependency on `std.trace`, so a record emitted
  inside a `trace.start()` span carries no trace or span id of its own. to
  correlate logs with traces, pass the ids into the logger yourself with
  `with([log.str("trace_id", ...)])`.
- **grpc without tls** — the OTLP/grpc transport requires tls; there's no
  cleartext h2c for a bare local collector on `:4317`. use `http/protobuf` or an
  `https` grpc endpoint.
- **exemplars and delta temporality** — metrics export as cumulative (which
  matches how pith accumulates them); there are no exemplars or delta temporality.
