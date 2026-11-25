# Security

The Pith compiler is written in Pith (`self-host/`). It emits code
through a Cranelift backend, and every program links a Rust runtime
(`cranelift/`). This page lists what the language guarantees, what CI
checks, the known limits, and how to report a problem.

## Memory safety

The compiler inserts every retain and release itself. A program that
compiles never reads freed memory, frees a value twice, or writes past
a buffer, including when several tasks run at once. Three cases fall
outside that guarantee:

- A strong reference cycle with no `weak` edge leaks.
- Some runtime errors stop the process, such as an index out of range
  outside a `T!` function or a stack overflow.
- A channel send that races a close can lose its value.

The compiler refuses programs that would share mutable state between
tasks, or reach runtime memory they do not own:

| Code | What it refuses |
|---|---|
| E248 | Handing a task (by spawn, closure capture or channel send) a value whose buffer the sender still holds |
| E251 | Naming a private field outside its module, or a raw-handle `byte_buffer_*` builtin outside `std.bytes` |
| E271 | Touching a guarded field without holding its lock |
| E276 | A guarded buffer that is still reachable after the lock is released |
| E309 | A module global that every task can reach and that is changed after init |
| E318 | A `drop` body that lets `self` outlive the call |

[docs/ownership.md](docs/ownership.md) describes the counting model
and [docs/concurrency.md](docs/concurrency.md) lists the sharing rules.

## What CI checks

Every pull request and every push to main runs:

- The count checker (`make count-marks-check-only`). It fails the build if
  the retains and releases the compiler placed do not balance.
- Valgrind memcheck over a curated set of programs, and leak-growth
  checks that fail if memory use grows over repeated runs.
- `make check-no-panics`, which scans the Rust runtime and backend for `panic!`,
  `.unwrap()`, `expect(`, `transmute` and similar calls (plus `process::exit`
  in the runtime and codegen crates). Each one must carry a comment that
  explains why it cannot be reached or why stopping is correct.
- The bootstrap fixed point: the compiler builds itself and the output
  must match.

The address and thread sanitizers run nightly over the regression
corpus, the examples and the std tests (`.github/workflows/sanitizers.yml`).

To find a use after free by hand, build and run with
`PITH_QUARANTINE=1`. Freed blocks are poisoned and never reused, so a
later use prints a report instead of reading stale memory. The
variable must be set at build time and at run time. `pith run` and
`pith test` see it in both steps.

## Compiler input limits

| Limit | Value | Error |
|---|---|---|
| Source file size | 16 MiB | E243 (`self-host/lexer.pith`) |
| Expression, type and pattern nesting | 256 levels | E242 (`self-host/parser.pith`) |
| Expression depth | 25,000 levels | E242 (`self-host/parser.pith`) |
| Block nesting | 1,000 levels | E242 (`self-host/parser.pith`) |
| Type nesting | 128 levels | E233 (`self-host/checker.pith`) |
| Diagnostics printed | 100 per run | A closing note gives the number left out |

Nesting counts brackets, prefix operators such as `-` and `not`, and
strings nested in interpolations. Expression depth counts the levels of
the tree the parser builds. A call, method call, field access or index
counts as three levels, which allows a chain of 25,000 binary operators
or about 8,000 calls. Every pass after the parser walks the tree
recursively, and this limit keeps those passes inside an 8 MiB stack.
The size limit bounds memory: the front end holds a few hundred bytes
per token, so a 16 MiB file dense with tokens needs about 3.5 GB.

After a nesting error, the parser skips to the end of the line and
reports nothing more until the next statement. A file that is not valid
UTF-8 fails with E243 at the line, column and byte offset of its first
invalid byte. `make input-limits-check-only` generates inputs at and
past each limit and checks, under an 8 MiB stack, that each one compiles
or fails with its diagnostic.

## Runtime failures in servers

A runtime error inside an HTTP or gRPC handler fails that one request,
and the server keeps running. HTTP returns 500 and gRPC returns
`INTERNAL`. This covers an index out of range, a missing map key,
division by zero, integer overflow and `.value()` on `none`. Two cases
still end the process: a stack overflow, and a runtime error inside a
task the handler spawned. See
[docs/failure_isolation.md](docs/failure_isolation.md).

## Standard library defaults

- TLS: the client verifies the certificate chain and the hostname
  (subject alternative names only) by default. Turning that off
  requires `dangerously_skip_verification()`. `require_tls13()` refuses
  TLS 1.2. When 1.2 is allowed, only ECDHE key exchange with AEAD
  ciphers is offered. Every handshake has a deadline.
  See [docs/tls.md](docs/tls.md).
- Passwords: Argon2id with m=19456 KiB, t=2, p=1. Costs parsed from
  a stored hash are capped, so a crafted hash cannot force a huge
  allocation.
- JWT: `alg: none` is refused, and the token's `alg` must match the
  one the caller names. Signature and token comparisons are constant
  time through `std.crypto.subtle`. See [docs/auth.md](docs/auth.md).
- Sessions and CSRF: session cookies are `HttpOnly`, `Secure` and
  `SameSite=Lax`. The CSRF middleware requires a token on unsafe
  methods.
- HTTP/1.1: a request with both `Content-Length` and
  `Transfer-Encoding`, conflicting `Content-Length`, or
  obsolete header folding is refused. Oversized bodies, oversized
  headers and slow headers get 413, 431 and 408. In `std.web`, a
  request body has 60 s in total after its head, and a slower one gets
  408. A chunk size is checked against the body limit before it is read,
  and a size near the largest Int gets 413 instead of overflowing. A
  response with such a size fails the client's request the same way.
- HTTP/2: at most 128 concurrent streams per connection, capped
  header lists, and a stream past the cap is refused, so rapid stream
  resets cannot pile up handlers. See [docs/web.md](docs/web.md).
- Templates: a `<%= %>` value is escaped for where it lands. A URL
  attribute value goes through `html.safe_url`, an unquoted attribute
  value has every delimiter encoded, and compile refuses a value inside
  `<script>`, `<style>`, an event handler, `style` or `srcdoc`.
  See [docs/html.md](docs/html.md).
- Markdown: `std.markdown` shows raw HTML as text and checks link and
  image URLs after decoding them, unless the options turn that off. A
  template's `<%md %>` always uses those defaults and compiles only in
  element text. `std.web.pages` opens only `.md` files under its
  directory, capped at 1 MiB. See [docs/markdown.md](docs/markdown.md).
- YAML: no anchors, aliases or tags, with caps on depth and input
  size.
- Databases: the Postgres and MySQL drivers send parameters
  separately from the query text, so values are never spliced into
  SQL.

Two defaults need care:

- `html.safe_url` stops script URLs. It does not stop off-site ones:
  `https://evil.example` passes. A redirect to a destination the
  request chose should use `web.local_redirect`, which accepts only a
  path on this site. `<%raw %>` in a template writes a value
  unchecked, wherever it sits. See [docs/html.md](docs/html.md).
- The plain `std.net.http` entry points (`http.server_config()`) have
  every timeout off, as net/http does. `std.web` turns on the head,
  idle and body deadlines. A server built on `std.net.http` directly
  should set them. See [docs/http_apps.md](docs/http_apps.md).

## Container

The final stage of the `Dockerfile` runs as the non-root user `pith`
(uid 1000).

## Reporting a problem

Open an issue on this repository. If the report has details that
should not be public before a fix, open an issue that asks for a
private contact and leave the details out. A compiler or runtime crash
on any input is a bug, so please report those too.
