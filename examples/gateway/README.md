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
| routing | path prefixes, with the prefix optionally stripped |
| balancing | round robin across a route's upstreams |
| failure handling | retries for repeatable methods, a circuit breaker per upstream, active health checks, per-route timeouts |
| auth | bearer tokens (rs256, ps256, es256, eddsa) checked against a jwks url, with the subject passed upstream |
| limits | requests per second per route and per token subject, and a body size cap |
| grpc | forwards unary and streaming calls for any service, with no generated code |
| observability | `/metrics`, `/healthz`, and trace context carried upstream |
| shutdown | on sigterm it stops accepting, finishes what is in flight, and exits |

the gateway removes headers a client should not control before forwarding. that
covers hop-by-hop headers, anything the `Connection` header names,
`X-Auth-Subject` and the `X-Forwarded-*` family. a path with an encoded slash or
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
| plain | 64 | pith | 118k | 518 us | 1.67 ms | 0 | 17 MB |
| | | go | 278k | 198 us | 0.90 ms | 0 | 28 MB |
| plain | 256 | pith | 236k | 980 us | 4.33 ms | 0 | 25 MB |
| | | go | 403k | 626 us | 4.07 ms | 0 | 44 MB |
| bearer token | 64 | pith | 128k | 484 us | 0.91 ms | 0 | 18 MB |
| | | go | 100k | 556 us | 3.13 ms | 0 | 29 MB |
| bearer token | 256 | pith | 129k | 1.93 ms | 4.03 ms | 0 | 24 MB |
| | | go | 157k | 1.50 ms | 6.56 ms | 0 | 44 MB |
| backend killed mid-run | 64 | pith | 122k | 503 us | 1.49 ms | 0 | 17 MB |
| | | go | 280k | 191 us | 0.77 ms | 0 | 29 MB |
| backend killed mid-run | 256 | pith | 236k | 990 us | 4.60 ms | 0 | 25 MB |
| | | go | 390k | 621 us | 3.46 ms | 0 | 44 MB |

go moves plain requests faster, by about 1.7x at 256 connections and 2.4x at 64.
pith uses about 40% less memory. with a token to check, where most of the time
goes to es256 verification, the two are close, and pith's p99 is lower. neither
lost a request when a backend died.

a profile of the plain path finds no single cost: a request takes about 200,000
instructions, and nothing is above 5%. allocation and parsing response headers in
the http client are the largest parts.

## what it does not do

- **bodies are buffered in both directions**, up to `max_body_bytes`. that keeps
  retries simple and suits json apis. server-sent events, websockets and large
  uploads will not work through it.
- **upstreams are plain http/1.1, and grpc upstreams are h2c.** there is no tls to
  a backend.
- **it serves at most 512 connections.** `std.web` fixes that number and answers
  503 past it. at 1024 connections about 19% of requests are refused.
- **`X-Forwarded-For` is removed and not set**, and there is no limit per client
  address, because `std.web` does not expose the peer address.
- **a grpc backend's response headers and custom trailers stop at the gateway.**
  only the status crosses back, and grpc upstreams have no breaker or health check.
- **`/metrics` is on the public port.** put it behind something.
