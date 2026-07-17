# telemetry

pith ships its own observability stack, written in pith like everything else:
prometheus metrics you scrape, and opentelemetry traces and metrics you push to
a collector. the std http and grpc clients instrument themselves, so a service
that talks over them gets request traces and RED metrics without you writing any
of it.

there are three moving parts, and you can adopt them in any order:

- `std.metrics` — counters, gauges, and histograms, with labels.
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
collecting, and spawns a background thread that ships finished spans and the
current metric snapshot to the collector. every call your service makes over the
std grpc or http client now produces a client span and RED metrics, and any
inbound `traceparent` is joined to the right trace.

with no endpoint set, `init()` returns immediately and the program runs
uninstrumented. leaving the call in costs nothing in production, so you don't
need to guard it behind a build flag.

## metrics

`std.metrics` is a small registry of three instrument types. you name an
instrument, optionally attach labels, and record into it.

```pith
import std.metrics as metrics

metrics.counter("orders_total").inc()
metrics.counter("orders_total").labels(["region", "eu"]).inc()
metrics.gauge("queue_depth").set(work.len())
metrics.histogram("job_duration_ms").observe(elapsed)
```

`labels(["k1", "v1", "k2", "v2"])` takes a flat key/value list and returns an
instrument bound to that label set. each distinct set accumulates on its own, so
the two `orders_total` lines above are separate series that render under one
`# TYPE` line. values are integers today, and histogram buckets are fixed.

the std clients already register these on your behalf:

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

fn main() -> Int!:
    spawn prometheus.serve("0.0.0.0", 9464)
    run_server()!
    return 0
```

`serve` answers `GET /metrics` with the current snapshot and 404s everything
else. it's pull-based, so there's no interval to configure here — prometheus
scrapes on its own schedule and each request renders whatever the registry holds
at that moment. prometheus metrics and OTLP push are independent; use either,
both, or neither.

## tracing

a span is one timed unit of work: a name, a start and end, some attributes, a
status, and its place in a trace. you start a span, do the work, and end it.

```pith
import std.trace as trace

fn handle_request():
    span := trace.start("handle_request")
    span.set_attr("http.route", "/api").set_attr("user.tier", "pro")
    ... work ...
    span.set_status(trace.STATUS_OK, "")
    span.end()
```

`set_attr` returns the span so calls chain. `start_kind(name, kind)` picks the
span kind (`SERVER`, `CLIENT`, `PRODUCER`, `CONSUMER`, or the default
`INTERNAL`); the auto-instrumentation uses `CLIENT` and `SERVER`.

### spans nest on their own

the current span is tracked per os thread, so a span started inside another is
parented under it automatically — you don't thread a context object through every
call.

```pith
outer := trace.start("outer")
inner := trace.start("inner")   # parented under outer, same trace id
inner.end()                     # current span is outer again
outer.end()
```

when tracing is off, `start` returns a non-recording span that touches no
per-thread state, and `end` on it does nothing. the check is a single bool, so
instrumented code you leave in an uninstrumented build stays cheap.

### crossing a spawn

a spawned task runs on a fresh thread with no current span, so the one place
propagation is explicit is a `spawn`. capture the parent context before, restore
it inside:

```pith
ctx := trace.current_context()
spawn work_item(ctx)

fn work_item(ctx: trace.SpanContext):
    trace.with_context(ctx)     # re-establish the parent on this thread
    span := trace.start("work_item")
    ... work ...
    span.end()
```

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

## OTLP export

`std.obs.init()` reads the environment and wires everything up. to configure the
exporter from code instead, call `obs.start(...)` directly. the environment
variables follow the opentelemetry sdk names:

| variable | meaning | default |
|----------|---------|---------|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | collector base url; unset means off | — |
| `OTEL_SERVICE_NAME` | `service.name` on exported data | `pith-service` |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `http/protobuf` or `grpc` | `http/protobuf` |
| `OTEL_METRIC_EXPORT_INTERVAL` | ms between metric flushes | `60000` |
| `OTEL_BSP_SCHEDULE_DELAY` | ms between span flushes | `5000` |

the exporter batches in the background: it drains finished spans on the span
interval, POSTs a metric snapshot on the metric interval, and does one final
flush on `shutdown()`. a failed POST is swallowed so a flaky collector never
wedges your app. spans that finish faster than the collector drains them are
capped, so a burst can't grow the buffer without bound.

export goes over OTLP/HTTP with protobuf bodies (to `/v1/traces` and
`/v1/metrics`), encoded by hand in `std.otlp` on top of `std.protobuf`. that's
the default and the recommended transport.

## what isn't here yet

- **OTLP/grpc transport** — `OTEL_EXPORTER_OTLP_PROTOCOL=grpc` is recognized but
  not implemented; export fails with a clear message. use `http/protobuf`.
- **OTLP logs** — logs aren't exported over OTLP. `std.log` records already carry
  the current trace and span ids, so they correlate in a backend that ingests
  logs separately.
- **histogram shape** — buckets are fixed and metric values are integers; there's
  no per-metric bucket configuration or exemplars.
- **sampling** — every started span is recorded when tracing is on; there's no
  head or tail sampler yet.
