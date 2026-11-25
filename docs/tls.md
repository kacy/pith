# TLS

Pith's native TLS stack lives in `std.net.tls`, `std.net.tls13`, and `std.net.tls12`.

The current shape is:
- TLS 1.3, with a TLS 1.2 fallback (ECDHE + AEAD only)
- Client and server handshakes in Pith, in TLS 1.3 middlebox-compatibility mode
  (RFC 8446 appendix D.4: both ends emit the dummy change_cipher_spec)
- ALPN
- Strict and optional verified client auth
- Session tickets and client-side resumption
- Post-verification hooks
- Server-side config selection from parsed client hello data

## Basic config

Client configs come from system roots or a PEM CA bundle:

```pith
client_cfg := tls.client_config()!
custom_cfg := tls.client_config_with_ca_file("certs/root-ca.pem")!
```

Both give their registry slot back when the last value naming them goes away,
and both can be closed earlier by hand. See [who closes a
config](#who-closes-a-config).

Server configs come from a certificate chain and an unencrypted PEM private key
(PKCS#8 `PRIVATE KEY`, PKCS#1 `RSA PRIVATE KEY`, or SEC1 `EC PRIVATE KEY`):

```pith
server_cfg := tls.server_config("certs/server.crt", "certs/server.key")!
```

## Who closes a config

A config is a handle into a registry that `std.net.tls` keeps for the life of
the process. `Config` implements `Drop` (docs/ownership.md, "destructors"), so
the slot goes back on its own when the last value naming the config goes away:
a config bound in a request handler and forgotten is closed when the handler
returns, one held in a struct field when the struct dies, one in a list when
the list does. An HTTPS client that builds a config per request and drops it
no longer leaks anything.

**Whoever builds a config still decides when it closes.** `close()` is the
explicit form, it is idempotent, and it is still the right call wherever the
timing matters, because a destructor runs when the last holder lets go and
says nothing about when that is:

```pith
cfg := tls.client_config()!
defer cfg.close()
conn := tls.dial_with_config(host, 443, host, cfg)!
```

Closing after the connection is up is deliberate. A client config is read only
while the handshake runs: the roots to verify the peer chain, the ALPN list to
offer, the client certificate to present. Once the handshake returns, the
connection holds its own keys and never looks the config up again, so the
config has no job left. Without the `defer` the same config closes at the end
of the enclosing function instead, which for a per-request handler is the same
moment.

A server config follows the same rule, with the timing spelled out.
`tls.listen` holds the config against the listening socket with a count of its
own, so the config stays open while the listener does even if the caller stops
naming it. Each handshake takes a count for as long as it is reading the
certificate and key. `Listener.close()` and `release_listener_config` give the
listener's count back; the config closes at the later of that and the caller's
last reference, or when the caller closes it by hand.

The timing is what makes a server different in practice. An accept loop hands
each socket to its own task, and that task reads the certificate and the private
key out of the registry when its own handshake reaches them. That can be a
whole handshake timeout after the accept that spawned it returned, and for a
connection accepted just before the shutdown it may not have happened at all
yet. So a server gives the listener's count back and closes its config once it
has drained, never before:

```pith
cfg := tls.server_config("certs/server.crt", "certs/server.key")!
listener := tls.listen(host, 443, cfg)!
shutdown.register_listener(listener.handle)
# ... accept, spawning a task per connection, until a shutdown is requested ...
shutdown.close_listener(listener.handle)   # free the port, and only the port
drained := shutdown.drain_default()
tls.release_listener_config(listener)      # give the listener's count back
cfg.close()
```

The two halves of `Listener.close()` are split on purpose. The socket goes
first, because the replacement instance of a rolling deploy is waiting to bind
that port and the drain can take the whole grace period. The listener's
*binding* to the config goes last, because that binding is how a handshake finds
the certificate. Dropping it while a connection is still in the drain count is
the same mistake as closing the config early, one step removed. A handshake that
has already read the config out of the binding holds its own count and finishes
either way; one that has not yet fails.

Neither mistake corrupts anything. A handle is never reissued, so a handshake
that loses the race fails cleanly instead of reaching for another config's key.
Both mistakes still turn working connections into failed ones on the way out,
which is a worse shutdown than the leak they were fixing. The last line of the
example is optional now (`cfg` closes itself when the function returns) and is
kept because it says where the slot goes back.

The std servers do this for you: `App.listen_tls`, `std.net.http2.server.listen_h2_tls` and its
streaming twin each build a config, serve on it, and close it after their drain,
so a program that calls one of those has nothing to close.
`tls.open_server_configs()` counts the server configs still holding a
certificate, and on a healthy process the number is flat.

`close()` is idempotent and safe on a config that holds nothing, so a failure
path can close unconditionally on its way out, as long as it is a path where
nothing is still handshaking on it. That caveat is the whole reason a server
closes after its drain and not in a `defer`, which would fire on the paths
that never drain too. The destructor has no such caveat: it cannot run while a
listener or a handshake still holds the config, because each of those holds a
count.

The functions that build a config for you close it for you: `tls.dial`,
`http.get_https` and friends, `std.net.http2.connection.connect`. The `_with_config` variants never
close, because there the config came from the caller and may well be reused for
the next connection.

A closed config stays closed. A handshake on it fails, and a `with_*` or
`require_*` call on it changes nothing, so a stale reference cannot rebuild part
of a config after its owner closed it.

A config does **not** own its trust anchors. Root bundles are parsed once per
distinct bundle and shared by every config that trusts them, and the system
bundle is read from disk at most once per process, so building a config per
request costs one small registry record instead of a copy and a re-parse of a
couple of hundred kilobytes of PEM. That sharing is why `client_config()` is
cheap enough to call on a hot path; closing is what keeps it that way.

The trade is that a process does not notice the system trust store changing
underneath it: `client_config()` reads it once and keeps those roots until the
process restarts. To pick up a rotation without restarting, read the bundle
yourself and build configs with `client_config_with_ca_file`.

## Common options

These helpers can be combined on the same config:
- `with_alpn(...)`
- `with_client_certificate(...)`
- `require_client_ca_file(...)`
- `request_client_ca_file(...)`
- `enable_session_resumption()`
- `with_verify_connection(...)`

`enable_session_resumption()` turns on the in-process session stores: the
server's ticket store and the client's session cache. Both are guarded by a
lock inside `std.net.tls`, so it is safe to run handshakes concurrently from
many tasks. A server can handshake each connection in its own task, and a
client can dial from a pool of tasks, with no locking of its own.

For servers that need to choose policy before the handshake finishes, use:
- `tls.with_config_selector(config, chooser)`

The selector receives `ClientHelloInfo` with:
- `server_name`
- `alpn_protocols`

It returns the server config that should handle that connection.

So one selector can choose between different certificate chains, different
ALPN lists, and different client-auth policies just by returning a different
`server_config(...)` value.

```pith
api_cfg := tls.server_config("certs/api.crt", "certs/api.key")!.with_alpn(["pith.rpc"])
web_cfg := tls.server_config("certs/web.crt", "certs/web.key")!.with_alpn(["http/1.1"])

listener_cfg := tls.with_config_selector(
    tls.server_config("certs/default.crt", "certs/default.key")!,
    fn(info: tls.ClientHelloInfo) =>
        if info.alpn_protocols.contains("pith.rpc"):
            api_cfg
        else:
            web_cfg
)
```

## Accepting connections

`tls.listen(host, port, config)` returns a listener. `tls.accept(listener)`
takes the next connection off it and hands back a handshaked `Conn`. That is
the one-shot form, and it is fine for a test or a tool that serves a single
client.

A server with an accept loop wants the two halves separately, because the
handshake reads from the network:

```pith
listener := tls.listen(host, port, config)!
while true:
    fd := tls.accept_socket(listener)!
    spawn serve(listener, fd)
```

`accept_socket` returns the raw socket without handshaking it, and
`tls.handshake(listener, fd)` runs the handshake on a socket that came from it.
Splitting them keeps two problems off the loop. A client that dribbles its
handshake records now holds only its own task, instead of every other client
waiting to be accepted. And a client whose handshake fails fails on its own
task, instead of propagating an error out of the loop and ending it. The second
matters because the things that open a connection and never finish a handshake
are ordinary traffic, like port scanners, load balancer probes, and browsers
racing two connections and dropping one.

The socket belongs to `handshake` once you pass it in. On success the returned
`Conn` owns it and `Conn.close()` releases it; on failure `handshake` closes it
before the error propagates, so a spawned task never leaks the socket of a
client that never got past the handshake.

A handshake has one deadline for the whole exchange, reads and writes alike: 10
seconds from the accept on a server, and 30 seconds from the connect for
`tls.dial` and `tls.dial_with_config`. A timeout per read would let a peer that
sends a byte just inside it keep the handshake going forever. The deadline is
cleared once the handshake finishes, so it never caps the connection's own
reads. `config.with_handshake_timeout(ms)` changes the budget for handshakes
made with that config. `tls.client(stream, ...)` wraps a stream the caller
already holds and leaves its deadlines to the caller.

The shape above passes the listener itself to the task. A spawn shares the
value with the loop, so the task and the loop name one listener, and it stays
open until both are done with it.

## Who closes a connection

A `Conn` owns its session and its socket, and a `Listener` owns its socket and
its binding to the config. `close()` is still the way to say when they go:
`Conn.close()` sends close_notify before it closes the socket, so the peer can
tell a clean end from a cut connection. A second `close()` does nothing.

A value nobody closes is released when the last value naming it goes away,
because both types implement `Drop`. A dropped `Conn` closes its socket and
drops its session state without sending close_notify, since a destructor can
run on any thread and must not wait on the network. A peer reading to the end
of a stream therefore sees a truncation instead of a clean close. Call
`close()` when that matters.

So that one value owns one session, std no longer builds a `Conn` or a
`Listener` from a bare handle (`conn_from_handle` and `listener_from_handle`
are gone). A struct that keeps a connection holds the `Conn` itself. One that
has a connection only on some paths holds `tls.no_conn()` on the others, a
`Conn` that names no session: its reads fail and its close does nothing. Do not
write `tls.Conn(handle)` either, because a second `Conn` over a live handle
closes the session when it dies while the first still uses it.

A failed handshake is dropped without a log line, because a log line per failure
is a way for anyone with a socket to fill your disk. What it leaves behind is a
counter:

```pith
failures := tls.server_handshake_failures()
```

That counts every server handshake that has failed since the process started:
peers that hang up, peers that send something that is not TLS, peers that offer
no acceptable cipher or ALPN protocol, and peers that fail client-certificate
verification. A low steady rate is background noise on any public port. A spike
is worth an alert. `std.web`, `std.net.http2.server`, and `std.net.grpc` already
accept this way, so their servers count failures with no extra wiring.

## Connection state

Every native TLS connection exposes a `ConnectionState`:

```pith
state := conn.state()

print(state.version.to_string())
print(state.negotiated_protocol)
print(state.peer_common_name)
```

The fields are:
- `version`
- `cipher_suite`
- `negotiated_protocol`
- `did_resume`
- `peer_common_name`
- `peer_issuer_common_name`
- `peer_serial_hex`
- `peer_not_before`
- `peer_not_after`
- `peer_dns_names`
- `peer_ip_addresses`
- `peer_certificate_count`
- `peer_certificates`
- `peer_chain_present`
- `client_auth_requested`: whether this endpoint, acting as a server, asked the
  peer for a client certificate; always false on a client connection
- `client_auth_verified`: whether this endpoint, acting as a server, verified
  the peer's client certificate; always false on a client connection

There are also small wrappers on `Conn` for the common cases:
- `version()`
- `version_name()`
- `cipher_suite()`
- `cipher_suite_name()`
- `negotiated_protocol()`
- `did_resume()`
- `peer_common_name()`
- `peer_issuer_common_name()`
- `peer_serial_hex()`
- `peer_not_before()`
- `peer_not_after()`
- `peer_dns_names()`
- `peer_ip_addresses()`
- `peer_certificate_count()`
- `peer_certificates()`
- `peer_chain_present()`
- `client_auth_requested()`
- `client_auth_verified()`

`peer_certificates` and `peer_certificates()` expose a summary list with:
- `subject_common_name`
- `issuer_common_name`
- `serial_hex`
- `not_before`
- `not_after`
- `dns_names`
- `ip_addresses`
- `is_ca`

`version_name()` returns `tls1.3` or `tls1.2` for native connections.
`cipher_suite_name()` returns one of:
- `TLS_AES_128_GCM_SHA256` (1.3)
- `TLS_AES_256_GCM_SHA384` (1.3)
- `TLS_CHACHA20_POLY1305_SHA256` (1.3)
- `TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256` (1.2)
- `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256` (1.2)
- `TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384` (1.2)
- `TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384` (1.2)
- `TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256` (1.2)
- `TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256` (1.2)

Handshake messages fragment across records in both directions. Flights larger
than one record's plaintext ceiling (a certificate chain past 2^14 bytes, say)
split on the wire, and messages a peer fragments (to any size, down to one byte
per record) reassemble on read, during the handshake and after it.

HelloRetryRequest (RFC 8446 §4.1.4) works on both sides. The client honors a
cookie retry, echoing the cookie in a second hello and rebuilding the
transcript over the synthetic message_hash. It refuses a retry that changes
nothing, duplicates an extension, or arrives twice. The server sends a retry
when a client advertises a group it can serve but offers no usable key share,
and verifies a resumption binder in the second hello over the retry
transcript.

ECDHE key exchange uses X25519, NIST P-256 (secp256r1), or NIST P-384
(secp384r1) on both 1.3 and the 1.2 fallback. The client sends shares for
X25519 and P-256 and prefers X25519; secp384r1 is advertised without a share,
and a server that insists on it asks via HelloRetryRequest. The server keys with
whichever group the client sent a share for, so a client restricted to P-256
(as some FIPS deployments are) still negotiates.

## Verification hooks

`with_verify_connection(...)` runs after normal certificate verification succeeds.

Return `""` to accept the connection. Return any non-empty string to reject it.

```pith
fn verify_internal(state: tls.ConnectionState) -> String:
    if state.negotiated_protocol != "pith.rpc":
        return "unexpected protocol"
    return ""

mut cfg := tls.client_config_with_ca_file("certs/root-ca.pem")!
cfg = cfg.with_alpn(["pith.rpc"]).with_verify_connection(verify_internal)
```

The hook runs on both client and server configs.

## Client auth modes

There are three current server modes:
- No client auth
- Optional verified client auth with `request_client_ca_file(...)`
- Required verified client auth with `require_client_ca_file(...)`

In optional mode, a client may omit its certificate.
If it does send one, Pith verifies it against the configured CA bundle.

A client presents one with `with_client_certificate(cert_path, key_path)`: a
PEM chain, leaf first, and an unencrypted PEM private key (PKCS#8, PKCS#1 RSA,
or SEC1 EC), the same pair a server config takes.

```pith
cfg := tls.client_config_with_ca_file("certs/root-ca.pem")!.with_client_certificate("certs/client.crt", "certs/client.key")!
conn := tls.dial_with_config(host, 443, host, cfg)!
```

A client sends its certificate only when the server asks, and only when it can
answer the request. The CertificateRequest names the key types and signature
algorithms the server will take, and a client whose key fits neither declines
with an empty certificate instead of aborting or signing with an algorithm the
server did not offer. A server in optional mode serves that client
unauthenticated; a server in required mode refuses it.

Both versions do this, with different wire shapes. TLS 1.3 carries the
client's Certificate and CertificateVerify inside the encrypted flight and signs
a hashed transcript under a context string; the 1.2 fallback sends them in the
clear before the cipher change and signs the raw transcript through
ClientKeyExchange. An application sees the same thing either way.

The connection reports client auth to the application on `ConnectionState`:
`client_auth_requested` says this server asked, `client_auth_verified` says a
certificate came back and verified, `peer_common_name` and
`peer_issuer_common_name` are the subject and issuer of the leaf, and
`peer_certificates` is the chain the client sent, summarized. A handler
authorizes on the subject name: the client proved it holds that certificate's
key by signing the handshake transcript with it.

```pith
state := conn.state()
if not state.client_auth_verified:
    return unauthorized()
if not allowed(state.peer_common_name):
    return forbidden()
```

`examples/mutual_tls.pith` runs a required, an optional and a refused exchange
against the repository's fixture certificates.

A client certificate is refused for the same reasons a server certificate is,
and the alert says which: `unknown_ca` when the chain does not reach the
configured bundle, `certificate_expired` when a certificate on it is outside
its validity window, `bad_certificate` for anything else the chain check
rejects, `illegal_parameter` when the signature algorithm does not match the
key in the certificate or was not one the request advertised, and
`decrypt_error` when the CertificateVerify signature does not check out. A
client that declines a certificate the server requires gets
`certificate_required` on 1.3 and `handshake_failure` on 1.2, the code RFC 5246
names; 1.3 introduced the more specific one.

A certificate that is presented is always verified, in optional mode too, and a
presented certificate without a CertificateVerify is refused. A certificate is
public; only the signature proves the client holds its key.

## The TLS 1.2 fallback

Both the client and server negotiate TLS 1.3 and, when a peer cannot speak 1.3,
fall back to TLS 1.2. The highest version a peer supports wins, and anything
below 1.2 is refused. This matches Go's crypto/tls and rustls. The fallback is
deliberately narrow and forward-secret: only the six ECDHE-RSA/ECDHE-ECDSA
AES-128-GCM, AES-256-GCM and ChaCha20-Poly1305 suites are offered or accepted,
so static-RSA key exchange, CBC suites, RC4, 3DES, DHE, and PSK are refused by
construction.

Downgrade protection is built in. The client offers `supported_versions`
{1.3, 1.2} plus extended master secret (RFC 7627) and renegotiation_info
(RFC 5746). A 1.3-capable server that negotiates 1.2 marks its random with the
RFC 8446 downgrade sentinel, and a 1.3-capable client that lands on 1.2 aborts
if it sees that sentinel (an active attacker stripped the 1.3 offer). Extended
master secret is mandatory on both sides: the client requires the server to
negotiate it, and the server refuses a 1.2 client that did not offer it instead
of falling back to the weaker RFC 5246 master secret. The server echoes an
extension only when the client asked for it: extended master secret when the
client offered it, and renegotiation_info only when the client advertised
secure renegotiation (the RFC 5746 extension or the SCSV cipher value).

A refused handshake is answered with a fatal alert before the connection
closes, so the peer learns why instead of seeing a bare TCP reset. Rejections
before any key switch send the alert in plaintext. Once a side's records have
entered an encrypted epoch, its alerts are sealed with the current keys: the
1.3 client uses its handshake keys while reading the server's flight, the 1.3
server answers a bad client reply under the application epoch its delivered
Finished established, and the 1.2 client rides the epoch its own Finished
opened. The codes are
`protocol_version` when no version this stack speaks was offered (the client
hello's legacy_version must also be at least 0x0303, the value RFC 8446 froze
it at), `handshake_failure` when the version was fine but no common cipher
suite, signature algorithm, or required extension could be agreed,
`no_application_protocol` for an ALPN mismatch, `illegal_parameter` for an
unusable key share or a client signature algorithm that does not match its
certificate, `unknown_ca` and `certificate_expired` for a client chain that
does not verify, `decrypt_error` for a PSK binder that does not validate
against a recognized ticket (RFC 8446 §4.2.11.2 requires the abort), and
`decode_error` / `record_overflow` / `unexpected_message` for malformed input.

The 1.2 fallback engages only when the client actually offered TLS 1.2,
through supported_versions when present or legacy_version otherwise. It is
never an answer to a version the client did not offer. Its ServerHello carries
an empty session id (echoing the client's would falsely announce a resumed
session under RFC 5246 §7.4.1.3), and its ServerKeyExchange signature uses a
scheme from the client's signature_algorithms extension, preferring RSA-PSS.

`dangerously_skip_verification()` disables certificate-chain and hostname
verification on a client config. The connection is still encrypted, but the
peer is unauthenticated: an active attacker can impersonate the server
freely. It exists for test harnesses (conformance runners present freshly
generated certificates) and must never appear in production configuration;
prefer `client_config_with_ca_file` with the peer's real root.

`require_tls13()` locks a config to TLS 1.3 and refuses the fallback, the
equivalent of a minimum version of 1.3:

```pith
cfg := tls.client_config_with_ca_file("certs/root-ca.pem")!.require_tls13()
```

`require_tls12()` is the inverse: it caps a config at TLS 1.2, so the client
offers only the fallback and never negotiates 1.3. The two are mutually
exclusive; if both are set on one config, `require_tls12()` wins.

The 1.2 fallback (v1) refuses session resumption and renegotiation.
Client-certificate auth, SNI-based server config selection, and ALPN all work
on 1.2. RSA (≥2048-bit) and ECDSA (P-256) server certificates are both
supported, for the server's own certificate and for a client's.

## Conformance harnesses

Three external suites drive this stack beyond the Pith-to-Pith and
OpenSSL/Go/rustls interop gates:

- `make tls-live-interop`, `make tls-go-interop`, and `make tls-rustls-interop`
  run in CI on every build.
- `make tls-bogo BOGO=/path/to/boringssl` runs the BoringSSL test runner
  against a Pith shim (`tests/interop/bogo/`). Unknown runner flags make the
  shim exit 89, which the runner counts as unimplemented and skips;
  `tests/interop/bogo/config.json` disables the cases this stack refuses by
  design (static RSA, TLS 1.2 resumption) with a reason for each.
- `make tls-bogo-gate BOGO=...` turns the same run into a gate: any failure
  not in `tests/interop/bogo/known_failures.txt` fails it, and baseline
  entries that now pass are listed for pruning. CI runs the gate nightly
  against a pinned BoringSSL checkout (`.github/workflows/bogo.yml`); the
  per-build pipeline keeps the lighter interop and live-TLS gates.
- Ad hoc tlsfuzzer runs against a live Pith server pass the
  version-negotiation, legacy-version, conversation, and record-padding suites
  with flags matching this stack's documented cipher policy.

## Current limits

- The 1.2 fallback is ECDHE + AEAD only, with the caveats above
- A server's CertificateRequest sends an empty certificate_authorities list on
  both versions: it does not tell a client which issuers it trusts, so a client
  holding several certificates has nothing to choose by. A client here presents
  the one its config holds
- Config selection is the dynamic handshake hook today
- The connection state exposes peer identity summaries, not full verified chains yet
