# resilience

`std.resilience` is the layer a service composes around anything that can
fail or flood: a retry policy with exponential backoff, a token-bucket rate
limiter, and a circuit breaker. it carries no protocol knowledge — the same
delay curve backs off otlp exports, tcp accept failures, and grpc unary
retries, and `std.web` wraps the limiter and breaker into middleware — so
"how do we behave under failure" is decided once and read everywhere.
`examples/resilience.pith` runs everything below.

## retry with backoff

a policy is attempts plus a curve: a base delay, doubling each retry, capped.

```pith
import std.resilience as resilience

policy := resilience.retries(5).base(100).cap(2_000)
report := resilience.attempt(policy, fetch_report)!
```

`attempt` returns the first success, sleeps the backoff between failures,
and propagates the last error once attempts are exhausted. when some errors
are not worth retrying, `attempt_if` takes a classifier and refuses to
retry a permanent failure:

```pith
fn is_transient(err: String) -> Bool:
    return not err.starts_with("invalid")

report := resilience.attempt_if(policy, fetch_report, is_transient)!
```

the curve is deliberately deterministic — no jitter — so tests can assert
the schedule; spread callers out with the rate limiter instead. protocols
with their own loops reuse the curve without the runner: otlp classifies a
typed http status and honors Retry-After, grpc retries on its own status
codes, and both sleep `policy.delay_ms(attempt)`.

```pith
# a grpc unary call retried on UNAVAILABLE / RESOURCE_EXHAUSTED / ABORTED
reply := conn.unary_retry("/shop.Catalog/Get", request, resilience.retries(3))!
```

## rate limiting

a token bucket: `per_second` tokens flow in continuously, up to a standing
reserve of `burst`. one bucket is shared by every task that holds it, which
is the point — the cap holds for the whole app, not per connection.

```pith
limiter := resilience.rate_limiter(100, 20)   # 100/s, bursts to 20
if not limiter.allow():
    # a denied caller decides what denial means: 429, drop, fallback
```

`allow` never waits. in a served app, wrap it as middleware:

```pith
app := web.new().use_mw(web.rate_limit(limiter)).get("/", home)
# or scope it to one expensive surface:
app.group("/search", web.rate_limit(limiter))
```

## circuit breaking

a breaker is closed while things work, open after `failures` consecutive
failures — refusing instantly instead of piling onto a drowning dependency —
and half-open once `reset_ms` passes: exactly one probe goes through, and
its outcome decides whether the circuit closes.

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
the web middleware counts a 5xx from the handler as a failure:

```pith
app.group("/reports", web.circuit(breaker))
```

## what is deliberately not here

no jitter (determinism is worth more at this scale; the limiter is the
spreading tool), no waiting `allow` (backpressure decisions belong to the
caller), and no timeout combinator — deadlines are per-protocol (`ctx` on
tcp reads, `with_timeout` on http requests, grpc deadlines) because a
timeout that cannot cancel the underlying work is a lie.
