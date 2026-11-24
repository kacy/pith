# security

the pith compiler is written in pith (`self-host/`). it emits code
through a cranelift backend, and every program links a rust runtime
(`cranelift/`). this page lists what the language guarantees, what ci
checks, the known limits, and how to report a problem.

## memory safety

the compiler inserts every retain and release itself. a program that
compiles never reads freed memory, frees a value twice, or writes past
a buffer, including when several tasks run at once. three cases fall
outside that guarantee:

- a strong reference cycle with no `weak` edge leaks.
- some runtime errors stop the process, such as an index out of range
  outside a `T!` function or a stack overflow.
- a channel send that races a close can lose its value.

the compiler refuses programs that would share mutable state between
tasks, or reach runtime memory they do not own:

| code | what it refuses |
|---|---|
| E248 | handing a task (by spawn, closure capture or channel send) a value whose buffer the sender still holds |
| E251 | naming a private field outside its module, or a raw-handle `byte_buffer_*` builtin outside `std.bytes` |
| E271 | touching a guarded field without holding its lock |
| E276 | a guarded buffer that is still reachable after the lock is released |
| E309 | a module global that every task can reach and that is changed after init |
| E318 | a `drop` body that lets `self` outlive the call |

[docs/ownership.md](docs/ownership.md) describes the counting model
and [docs/concurrency.md](docs/concurrency.md) lists the sharing rules.

## what ci checks

every pull request and every push to main runs:

- the count checker (`make count-marks-check-only`). it fails the build if
  the retains and releases the compiler placed do not balance.
- valgrind memcheck over a curated set of programs, and leak-growth
  checks that fail if memory use grows over repeated runs.
- `make check-no-panics`, which scans the rust runtime and backend for `panic!`,
  `.unwrap()`, `expect(`, `transmute` and similar calls (plus `process::exit`
  in the runtime and codegen crates). each one must carry a comment that
  explains why it cannot be reached or why stopping is correct.
- the bootstrap fixed point: the compiler builds itself and the output
  must match.

the address and thread sanitizers run nightly over the regression
corpus, the examples and the std tests (`.github/workflows/sanitizers.yml`).

to find a use after free by hand, build and run with
`PITH_QUARANTINE=1`. freed blocks are poisoned and never reused, so a
later use prints a report instead of reading stale memory. the
variable must be set at build time and at run time; `pith run` and
`pith test` see it in both steps.

## compiler input limits

| limit | value | error |
|---|---|---|
| source file size | 16 MiB | E243 (`self-host/lexer.pith`) |
| expression, type and pattern nesting | 256 levels | E242 (`self-host/parser.pith`) |
| expression depth | 25,000 levels | E242 (`self-host/parser.pith`) |
| block nesting | 1,000 levels | E242 (`self-host/parser.pith`) |
| type nesting | 128 levels | E233 (`self-host/checker.pith`) |
| diagnostics printed | 100 per run | a closing note gives the number left out |

nesting counts brackets, prefix operators such as `-` and `not`, and
strings nested in interpolations. expression depth counts the levels of
the tree the parser builds; a call, method call, field access or index
counts as three levels. that allows a chain of 25,000 binary operators
or about 8,000 calls. every pass after the parser walks the tree
recursively, and this limit keeps those passes inside an 8 MiB stack.
the size limit bounds memory: the front end holds a few hundred bytes
per token, so a 16 MiB file dense with tokens needs about 3.5 GB.

after a nesting error the parser skips to the end of the line and
reports nothing more until the next statement. a file that is not valid
utf-8 fails with E243 at the line, column and byte offset of its first
invalid byte. `make input-limits-check-only` generates inputs at and
past each limit and checks, under an 8 MiB stack, that each one compiles
or fails with its diagnostic.

## runtime failures in servers

a runtime error inside an http or grpc handler fails that one request
and the server keeps running. http returns 500 and grpc returns
`INTERNAL`. this covers an index out of range, a missing map key,
division by zero, integer overflow and `.value()` on `none`. two cases
still end the process: a stack overflow, and a runtime error inside a
task the handler spawned. see
[docs/failure_isolation.md](docs/failure_isolation.md).

## standard library defaults

- tls: the client verifies the certificate chain and the hostname
  (subject alternative names only) by default. turning that off
  requires `dangerously_skip_verification()`. `require_tls13()` refuses
  tls 1.2. when 1.2 is allowed, only ecdhe key exchange with aead
  ciphers is offered. every handshake has a deadline.
  see [docs/tls.md](docs/tls.md).
- passwords: argon2id with m=19456 KiB, t=2, p=1. costs parsed from
  a stored hash are capped, so a crafted hash cannot force a huge
  allocation.
- jwt: `alg: none` is refused, and the token's `alg` must match the
  one the caller names. signature and token comparisons are constant
  time through `std.crypto.subtle`. see [docs/auth.md](docs/auth.md).
- sessions and csrf: session cookies are `HttpOnly`, `Secure` and
  `SameSite=Lax`. the csrf middleware requires a token on unsafe
  methods.
- http/1.1: a request with both `Content-Length` and
  `Transfer-Encoding`, conflicting `Content-Length`, or
  obsolete header folding is refused. oversized bodies, oversized
  headers and slow headers get 413, 431 and 408. in `std.web`, a
  request body has 60 s in total after its head, and a slower one gets
  408.
- http/2: at most 128 concurrent streams per connection, capped
  header lists, and a stream past the cap is refused, so rapid stream
  resets cannot pile up handlers. see [docs/web.md](docs/web.md).
- templates: a `<%= %>` value is escaped for where it lands. a url
  attribute value goes through `html.safe_url`, an unquoted attribute
  value has every delimiter encoded, and compile refuses a value inside
  `<script>`, `<style>`, an event handler, `style` or `srcdoc`.
  see [docs/html.md](docs/html.md).
- yaml: no anchors, aliases or tags, with caps on depth and input
  size.
- databases: the postgres and mysql drivers send parameters
  separately from the query text, so values are never spliced into
  sql.

two defaults need care:

- `html.safe_url` stops script urls, not off-site ones:
  `https://evil.example` passes. a redirect to a destination the
  request chose should use `web.local_redirect`, which accepts only a
  path on this site. `<%raw %>` in a template writes a value
  unchecked, wherever it sits. see [docs/html.md](docs/html.md).
- the plain `std.net.http` entry points (`http.server_config()`) have
  every timeout off, as net/http does. `std.web` turns on the head,
  idle and body deadlines. a server built on `std.net.http` directly
  should set them. see [docs/http_apps.md](docs/http_apps.md).

## container

the final stage of the `Dockerfile` runs as the non-root user `pith`
(uid 1000).

## reporting a problem

open an issue on this repository. if the report has details that
should not be public before a fix, open an issue that asks for a
private contact and leave the details out. a compiler or runtime crash
on any input is a bug, so please report those too.
