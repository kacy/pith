# Resilience

`std.resilience` is the layer a service composes around anything that can
fail or flood: a retry policy with exponential backoff, a token-bucket rate
limiter, and a circuit breaker. It carries no protocol knowledge. The same
delay curve backs off OTLP exports, TCP accept failures, and gRPC unary
retries, and `std.web` wraps the limiter and breaker into middleware, so
"how do we behave under failure" is decided once and read everywhere.
`examples/resilience.pith` runs everything below.

## Retry with backoff

A policy is attempts plus a curve: a base delay, doubling each retry, capped.

```pith
import std.resilience as resilience

policy := resilience.retries(5).base(100).cap(2_000)
report := resilience.attempt(policy, fetch_report)!
```

`attempt` returns the first success, sleeps the backoff between failures,
and propagates the last error once attempts are exhausted. When some errors
are not worth retrying, `attempt_if` takes a classifier and refuses to
retry a permanent failure:

```pith
fn is_transient(err: String) -> Bool:
    return not err.starts_with("invalid")

report := resilience.attempt_if(policy, fetch_report, is_transient)!
```

The curve is deliberately deterministic, with no jitter, so tests can assert
the schedule; spread callers out with the rate limiter instead. Protocols
with their own loops reuse the curve without the runner: OTLP classifies a
typed HTTP status and honors Retry-After, gRPC retries on its own status
codes, and both sleep `policy.delay_ms(attempt)`.

```pith
# a grpc unary call retried on UNAVAILABLE / RESOURCE_EXHAUSTED / ABORTED
reply := conn.unary_retry("/shop.Catalog/Get", request, resilience.retries(3))!
```

## Rate limiting

A token bucket: `per_second` tokens flow in continuously, up to a standing
reserve of `burst`. One bucket is shared by every task that holds it, so the
cap holds for the whole app and not per connection.

```pith
limiter := resilience.rate_limiter(100, 20)   # 100/s, bursts to 20
if not limiter.allow():
    ...   # a denied caller decides what denial means: 429, drop, fallback
```

`allow` never waits. In a served app, wrap it as middleware:

```pith
app := web.new().use_mw(web.rate_limit(limiter)).get("/", home)
# or scope it to one expensive surface:
app.group("/search", web.rate_limit(limiter))
```

## Circuit breaking

A breaker is closed while things work. After `failures` consecutive failures
it opens and refuses instantly instead of piling onto an overloaded
dependency. Once `reset_ms` passes it is half-open: exactly one probe goes
through, and its outcome decides whether the circuit closes.

```pith
breaker := resilience.circuit_breaker(5, 10_000)
if breaker.allow():
    result := call_downstream()
    if result.is_ok:
        breaker.success()
    else:
        breaker.failure()
```

`state()` answers "closed", "open" or "half_open" for logs and dashboards.
The web middleware counts a 5xx from the handler as a failure:

```pith
app.group("/reports", web.circuit(breaker))
```

## What is deliberately not here

There is no jitter (determinism is worth more at this scale; the limiter is
the spreading tool), no waiting `allow` (backpressure decisions belong to the
caller), and no timeout combinator. Deadlines are per-protocol (`ctx` on
TCP reads, `with_timeout` on HTTP requests, gRPC deadlines) because a
timeout that cannot cancel the underlying work is misleading.
