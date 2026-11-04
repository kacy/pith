# api gateway

an api gateway in about 700 lines of pith, with no dependencies. it builds to one
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
the backends get the rest. these are medians of 3 interleaved 8-second runs.

| scenario | connections | | ok req/s | p50 | p99 | failed | peak memory |
|---|---:|---|---:|---:|---:|---:|---:|
| plain | 64 | pith | 115k | 525 us | 1.73 ms | 0 | 17 MB |
| | | go | 282k | 195 us | 0.86 ms | 0 | 27 MB |
| plain | 256 | pith | 220k | 1.05 ms | 4.78 ms | 0 | 25 MB |
| | | go | 405k | 609 us | 3.91 ms | 0 | 43 MB |
| plain | 1024 | pith | 355k | 2.68 ms | 11.0 ms | 0 | 44 MB |
| | | go | 351k | 3.39 ms | 20.4 ms | 0 | 106 MB |
| bearer token | 64 | pith | 127k | 490 us | 0.92 ms | 0 | 18 MB |
| | | go | 86k | 638 us | 3.42 ms | 0 | 30 MB |
| bearer token | 256 | pith | 127k | 1.95 ms | 4.06 ms | 0 | 24 MB |
| | | go | 158k | 1.51 ms | 6.36 ms | 0 | 43 MB |
| bearer token | 1024 | pith | 120k | 8.42 ms | 14.6 ms | 0 | 47 MB |
| | | go | 150k | 6.48 ms | 19.2 ms | 0 | 114 MB |
| backend killed mid-run | 64 | pith | 124k | 488 us | 1.65 ms | 0 | 17 MB |
| | | go | 277k | 196 us | 0.76 ms | 0 | 28 MB |
| backend killed mid-run | 256 | pith | 234k | 1.02 ms | 4.08 ms | 0 | 26 MB |
| | | go | 391k | 622 us | 3.45 ms | 0 | 45 MB |
| backend killed mid-run | 1024 | pith | 351k | 2.79 ms | 10.3 ms | 0 | 44 MB |
| | | go | 347k | 3.46 ms | 20.2 ms | 0 | 103 MB |

at 64 and 256 connections go forwards plain requests faster, by 2.4x and 1.8x. at
1024 the two have the same throughput and pith's p99 is about half of go's. pith
uses 40 to 60% less memory throughout. with a token to check, where most of the
time goes to es256 verification, go has 20 to 25% more throughput at 256 and 1024
connections and pith has more at 64, and pith's p99 is lower at all three. neither
lost a request when a backend died.

a profile of the plain path finds no single cost: a request takes about 200,000
instructions, and nothing is above 5%. allocation and parsing response headers in
the http client are the largest parts.

## what it does not do

- **a request body is buffered**, up to `max_body_bytes`, which keeps it in hand
  for a retry. an upload larger than memory should allows will not work through
  it.
- **websockets are not proxied.** an upgrade needs the two connections handed over
  to a raw copy, which `std.web` has no way to do.
- **an endless response holds a shutdown until its next piece.** a stream checks
  for a drain between pieces, so one whose upstream has gone quiet lasts until the
  grace period ends and is counted as unfinished work.
- **a grpc backend's response headers and custom trailers stop at the gateway.**
  only the status crosses back, and grpc upstreams have no breaker or health check.
- **`/metrics` is on the public port.** put it behind something.
