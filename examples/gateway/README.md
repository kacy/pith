# api gateway

an api gateway in about 850 lines of pith, with no dependencies. it builds to one
static binary. the tls, http/2, grpc, jwt and metrics code it runs on is all
standard library that you can read.

```
pith build examples/gateway/main.pith -o gateway
./gateway examples/gateway/gateway.toml
```

to see it work without setting anything up, run one of the walkthroughs. each
starts its own backends and an identity provider, and prints what happens:

```
pith run examples/api_gateway.pith
pith run examples/api_gateway_grpc.pith
```

## what it does

| | |
|---|---|
| listening | http/1.1, or https with http/2 negotiated, from one port |
| upstreams | plain http/1.1 or https, and grpc over h2c or tls |
| streaming | large responses and those with no declared length, such as server-sent events, are relayed as they arrive |
| routing | path prefixes, with the prefix optionally stripped |
| balancing | round robin across a route's upstreams |
| failure handling | retries for repeatable methods, a circuit breaker per upstream, active health checks, per-route timeouts |
| auth | bearer tokens (rs256, ps256, es256, eddsa) checked against a jwks url, with the subject passed upstream |
| limits | requests per second per route, per token subject and per client address, a request body cap, and a connection cap |
| grpc | forwards unary and streaming calls for any service, with no generated code |
| observability | `/metrics`, `/healthz`, and trace context carried upstream |
| shutdown | on sigterm it stops accepting, finishes what is in flight, and exits |

the gateway removes headers a client should not control before forwarding. that
covers hop-by-hop headers, anything the `Connection` header names,
`X-Auth-Subject` and the `X-Forwarded-*` family. it then sets `X-Forwarded-For` to
the client's address, or adds to the value already there with
`trust_forwarded_for`, for a gateway behind a load balancer. a path with an encoded slash or
backslash in a segment gets a 400, because a backend that decodes before routing
could read it as a different path than the gateway checked.

## the files

| file | what it holds |
|---|---|
| `config.pith` | the toml config, validated at startup |
| `upstream.pith` | the pool, the choice of upstream, and health checks |
| `proxy.pith` | forwarding one http request, with its retries |
| `auth.pith` | checking bearer tokens |
| `limits.pith` | the per-subject rate limit |
| `grpc_proxy.pith` | forwarding grpc calls |
| `server.pith` | turning a config into listeners |
| `main.pith` | the executable |
| `demo_identity.pith`, `demo_fixture.pith` | an identity provider for the walkthroughs and the benchmark |

## how it compares

`bench/gateway_bench.sh` compares it with `bench/gateway.go`, the same gateway
written with go's `net/http`. the gateway gets 16 hardware threads, and wrk and
the backends get the rest. these are medians of 3 interleaved 8-second runs, from
the 2026-11-12 suite run, on one machine over loopback.

| scenario | connections | | ok req/s | p50 | p99 | failed | peak memory |
|---|---:|---|---:|---:|---:|---:|---:|
| plain | 64 | pith | 473k | 130 us | 0.25 ms | 0 | 13 MB |
| | | go | 283k | 193 us | 0.90 ms | 0 | 27 MB |
| plain | 256 | pith | 490k | 516 us | 0.96 ms | 0 | 17 MB |
| | | go | 398k | 652 us | 4.25 ms | 0 | 44 MB |
| plain | 1024 | pith | 474k | 2.08 ms | 3.69 ms | 0 | 31 MB |
| | | go | 346k | 3.50 ms | 20.9 ms | 0 | 108 MB |
| bearer token | 64 | pith | 154k | 406 us | 0.76 ms | 0 | 18 MB |
| | | go | 89k | 619 us | 3.33 ms | 0 | 31 MB |
| bearer token | 256 | pith | 156k | 1.62 ms | 2.95 ms | 0 | 22 MB |
| | | go | 157k | 1.52 ms | 6.59 ms | 0 | 44 MB |
| bearer token | 1024 | pith | 151k | 6.79 ms | 11.3 ms | 0 | 42 MB |
| | | go | 148k | 6.59 ms | 19.9 ms | 0 | 111 MB |
| backend killed mid-run | 64 | pith | 425k | 150 us | 0.28 ms | 0 | 13 MB |
| | | go | 273k | 205 us | 0.79 ms | 0 | 27 MB |
| backend killed mid-run | 256 | pith | 430k | 605 us | 1.14 ms | 0 | 18 MB |
| | | go | 393k | 621 us | 3.53 ms | 0 | 44 MB |
| backend killed mid-run | 1024 | pith | 407k | 2.63 ms | 3.94 ms | 0 | 36 MB |
| | | go | 339k | 3.59 ms | 21.5 ms | 0 | 101 MB |

pith forwards plain requests 1.7x as fast as go at 64 connections, 1.2x at 256 and
1.4x at 1024, with a p99 3.6 to 5.7x lower. with a token to check, where most of
the time goes to es256 verification, pith is 1.7x ahead at 64 connections and
the two are level at 256 and 1024. go's p50 is a little lower at 256 and 1024,
and pith's p99 is lower at all three. pith uses 40 to 70% less memory
throughout. neither lost a request when a backend died.

the 2026-11-04 run had pith at 118k, 230k and 352k req/s on plain forwarding, so
go was 2.4x ahead at 64 connections. these changes closed that gap:

- a mutex unlock leaves the woken task on the worker it was parked on (#1316).
  before, the unlock moved it to the unlocking worker, the same rule that keeps
  channel fan-out fast. the gateway's pooled upstream connections and shared
  locks hit that path on every request, and the move cost most of its throughput.
- a worker whose own queue never empties now gives tasks that have never run a
  turn (#1315). before, new connections on a busy server could wait seconds: a
  surge of 700 connections on a 4-worker server got no answers for whole runs, and
  failover runs saw 5-second client timeouts. #1316 alone would have made this
  worse, so it landed first.
- the jwks cache keeps the parsed key set between verifies (#1317), and a token's
  `kid` is read once (#1318).
- the http client and the proxy path allocate less per request (#1319).

the cost is in #1316. a task that waits on a mutex beside cpu-bound tasks on the
same worker now waits longer for its turn. with 4 workers and 8 cpu-bound tasks, a
mutex-heavy test program went from about 0.8 s to 1.5 s. with no cpu-bound tasks
it went from 77 to 89 ms. with 2 it was faster than before, and with 3 about
level. a server whose handlers are cpu-bound and share a lock will see this; the
gateway does not.

## what it does not do

- **a request body is buffered**, up to `max_body_bytes`, which keeps it in hand
  for a retry. an upload larger than memory should allows will not work through
  it. std can hand a body to the handler as it arrives
  (`ServerConfig.with_stream_request_bodies`), but a streamed body can't be sent
  twice, so the gateway keeps buffering.
- **websockets go to one upstream and are not retried.** `tunnel.pith` sends the
  upgrade to an upstream and copies bytes both ways once the connection is taken
  over. a failure there is the client's to retry, and a tls upstream gets a
  `501` for an upgrade.
- **an endless response holds a shutdown until its next piece.** a stream checks
  for a drain between pieces, so one whose upstream has gone quiet lasts until the
  grace period ends and is counted as unfinished work.
- **a grpc backend's response headers and custom trailers stop at the gateway.**
  only the status crosses back, and grpc upstreams have no breaker or health check.
- **`/metrics` is on the public port.** put it behind something.
