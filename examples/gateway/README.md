# API gateway

An API gateway in about 980 lines of Pith, with no dependencies, that builds to
one static binary. The TLS, HTTP/2, gRPC, JWT and metrics code it runs on is all
standard library code that you can read.

```
pith build examples/gateway/main.pith -o gateway
./gateway examples/gateway/gateway.toml
```

To see it work without setting anything up, run one of the walkthroughs. Each
one starts its own backends and an identity provider, and prints what happens:

```
pith run examples/api_gateway.pith
pith run examples/api_gateway_grpc.pith
```

## What it does

| | |
|---|---|
| Listening | HTTP/1.1, or HTTPS with HTTP/2 negotiated, from one port |
| Upstreams | Plain HTTP/1.1 or HTTPS, and gRPC over h2c or TLS |
| Streaming | Large responses and those with no declared length, such as server-sent events, are relayed as they arrive |
| Routing | Path prefixes, with the prefix optionally stripped |
| Balancing | Round robin across a route's upstreams |
| Failure handling | Retries for repeatable methods, a circuit breaker per upstream, active health checks, per-route timeouts |
| Auth | Bearer tokens (RS256, PS256, ES256, EdDSA) checked against a JWKS URL, with the subject passed upstream |
| Limits | Requests per second per route, per token subject and per client address, a request body cap, and a connection cap |
| gRPC | Forwards unary and streaming calls for any service, with no generated code |
| Observability | `/metrics`, `/healthz`, and trace context carried upstream |
| Shutdown | On SIGTERM it stops accepting, finishes what is in flight, and exits |

The gateway removes headers a client should not control before forwarding. That
covers hop-by-hop headers, anything the `Connection` header names,
`X-Auth-Subject`, `X-Forwarded-For`, `X-Forwarded-Host`, and `X-Forwarded-Proto`.
It then sets `X-Forwarded-For` to the client's address, or, with
`trust_forwarded_for` for a gateway behind a load balancer, adds to the value
already there. A path with an encoded slash or backslash in a segment gets a
400, because a backend that decodes before routing could read it as a different
path than the gateway checked.

## The files

| File | What it holds |
|---|---|
| `config.pith` | The TOML config, validated at startup |
| `upstream.pith` | The pool, the choice of upstream, and health checks |
| `proxy.pith` | Forwarding one HTTP request, with its retries |
| `auth.pith` | Checking bearer tokens |
| `limits.pith` | Per-subject and per-client-address rate limits |
| `tunnel.pith` | WebSocket upgrade tunnelling |
| `grpc_proxy.pith` | Forwarding gRPC calls |
| `server.pith` | Turning a config into listeners |
| `main.pith` | The executable |
| `gateway.toml` | Sample configuration |
| `demo_identity.pith`, `demo_fixture.pith` | An identity provider for the walkthroughs and the benchmark |

## How it compares

`bench/gateway_bench.sh` compares it with `bench/gateway.go`, the same gateway
written with Go's `net/http`. The gateway gets 16 hardware threads, and wrk and
the backends get the rest. These are medians of 3 interleaved 8-second runs,
from the 2026-11-12 suite run, on one machine over loopback.

| Scenario | Connections | | OK req/s | p50 | p99 | Failed | Peak memory |
|---|---:|---|---:|---:|---:|---:|---:|
| Plain | 64 | Pith | 473k | 130 us | 0.25 ms | 0 | 13 MB |
| | | Go | 283k | 193 us | 0.90 ms | 0 | 27 MB |
| Plain | 256 | Pith | 490k | 516 us | 0.96 ms | 0 | 17 MB |
| | | Go | 398k | 652 us | 4.25 ms | 0 | 44 MB |
| Plain | 1024 | Pith | 474k | 2.08 ms | 3.69 ms | 0 | 31 MB |
| | | Go | 346k | 3.50 ms | 20.9 ms | 0 | 108 MB |
| Bearer token | 64 | Pith | 154k | 406 us | 0.76 ms | 0 | 18 MB |
| | | Go | 89k | 619 us | 3.33 ms | 0 | 31 MB |
| Bearer token | 256 | Pith | 156k | 1.62 ms | 2.95 ms | 0 | 22 MB |
| | | Go | 157k | 1.52 ms | 6.59 ms | 0 | 44 MB |
| Bearer token | 1024 | Pith | 151k | 6.79 ms | 11.3 ms | 0 | 42 MB |
| | | Go | 148k | 6.59 ms | 19.9 ms | 0 | 111 MB |
| Backend killed mid-run | 64 | Pith | 425k | 150 us | 0.28 ms | 0 | 13 MB |
| | | Go | 273k | 205 us | 0.79 ms | 0 | 27 MB |
| Backend killed mid-run | 256 | Pith | 430k | 605 us | 1.14 ms | 0 | 18 MB |
| | | Go | 393k | 621 us | 3.53 ms | 0 | 44 MB |
| Backend killed mid-run | 1024 | Pith | 407k | 2.63 ms | 3.94 ms | 0 | 36 MB |
| | | Go | 339k | 3.59 ms | 21.5 ms | 0 | 101 MB |

Pith forwards plain requests 1.7x as fast as Go at 64 connections, 1.2x at 256
and 1.4x at 1024, with a p99 3.6 to 5.7x lower. With a token to check, where most
of the time goes to ES256 verification, Pith is 1.7x ahead at 64 connections and
the two are level at 256 and 1024. Go's p50 is a little lower at 256 and 1024,
and Pith's p99 is lower at all three. Pith uses 40 to 70% less memory
throughout. Neither lost a request when a backend died.

The 2026-11-04 run had Pith at 118k, 230k and 352k req/s on plain forwarding, so
Go was 2.4x ahead at 64 connections. These changes closed that gap:

- A mutex unlock leaves the woken task on the worker it was parked on (#1316).
  Before, the unlock moved it to the unlocking worker, the same rule that keeps
  channel fan-out fast. The gateway's pooled upstream connections and shared
  locks hit that path on every request, and the move cost most of its throughput.
- A worker whose own queue never empties now gives tasks that have never run a
  turn (#1315). Before, new connections on a busy server could wait seconds: a
  surge of 700 connections on a 4-worker server got no answers for whole runs,
  and failover runs saw 5-second client timeouts. #1316 alone would have made
  this worse, so #1315 landed first.
- The JWKS cache keeps the parsed key set between verifies (#1317), and a
  token's `kid` is read once (#1318).
- The HTTP client and the proxy path allocate less per request (#1319).

The cost is in #1316. A task that waits on a mutex beside CPU-bound tasks on the
same worker now waits longer for its turn. With 4 workers and 8 CPU-bound tasks,
a mutex-heavy test program went from about 0.8 s to 1.5 s. With no CPU-bound
tasks it went from 77 to 89 ms. With 2 it was faster than before, and with 3
about level. A server whose handlers are CPU-bound and share a lock will see
this; the gateway does not.

## What it does not do

- **A request body is buffered**, up to `max_body_bytes`, which keeps it in hand
  for a retry. An upload larger than memory allows will not work through it.
  The standard library can hand a body to the handler as it arrives
  (`ServerConfig.with_stream_request_bodies`), but a streamed body can't be sent
  twice, so the gateway keeps buffering.
- **WebSockets go to one upstream and are not retried.** `tunnel.pith` sends the
  upgrade to an upstream and copies bytes both ways once the connection is taken
  over. A failure there is the client's to retry, and a TLS upstream gets a
  `501` for an upgrade.
- **An endless response holds a shutdown until its next piece.** A stream checks
  for a drain between pieces, so one whose upstream has gone quiet lasts until
  the grace period ends and is counted as unfinished work.
- **A gRPC backend's response headers and custom trailers stop at the gateway.**
  Only the status crosses back, and gRPC upstreams have no breaker or health
  check.
- **`/metrics` is on the public port.** Put it behind something.
