# tls

pith's native tls stack lives in `std.net.tls` and `std.net.tls13`.

the current shape is:
- tls 1.3, with a tls 1.2 fallback (ecdhe + aead only)
- client and server handshakes in pith, in tls 1.3 middlebox-compatibility mode
  (rfc 8446 appendix d.4: both ends emit the dummy change_cipher_spec)
- alpn
- strict and optional verified client auth
- session tickets and client-side resumption
- post-verification hooks
- server-side config selection from parsed client hello data

## basic config

client configs come from system roots or a pem ca bundle:

```pith
client_cfg := tls.client_config()!
custom_cfg := tls.client_config_with_ca_file("certs/root-ca.pem")!
```

both are yours to close — see [who closes a config](#who-closes-a-config).

server configs come from a certificate chain and a pkcs#8 private key:

```pith
server_cfg := tls.server_config("certs/server.crt", "certs/server.key")!
```

## who closes a config

a config is a handle into a registry that `std.net.tls` keeps for the life of
the process. **whoever builds a config closes it**, and nothing closes one for
you. a config that is never closed keeps its registry slot until the program
exits, so an https client that builds one per request and drops it leaks once
per request.

```pith
cfg := tls.client_config()!
defer cfg.close()
conn := tls.dial_with_config(host, 443, host, cfg)!
```

closing after the connection is up is deliberate, not a trick. a client config
is read only while the handshake runs — the roots to verify the peer chain, the
alpn list to offer, the client certificate to present. once the handshake
returns, the connection holds its own keys and never looks the config up again,
so the config has no job left.

a server config is the same rule with the timing spelled out. `tls.listen`
records the config against the listening socket, but that is a borrow, not a
transfer: `Listener.close()` gives the borrow back, and whoever called
`server_config()` is still the one who closes the config.

the timing is what makes a server different in practice. an accept loop hands
each socket to its own task, and that task reads the certificate and the private
key out of the registry when its own handshake reaches them — which can be a
whole handshake timeout after the accept that spawned it returned, and which for
a connection accepted just before the shutdown may not have happened at all yet.
so a server gives the borrow back and closes its config once it has drained,
never before:

```pith
cfg := tls.server_config("certs/server.crt", "certs/server.key")!
listener := tls.listen(host, 443, cfg)!
shutdown.register_listener(listener.handle)
# ... accept, spawning a task per connection, until a shutdown is requested ...
shutdown.close_listener(listener.handle)   # free the port, and only the port
drained := shutdown.drain_default()
tls.release_listener_config(listener)      # give the borrow back
cfg.close()
```

the two halves of `Listener.close()` are split on purpose, and the split is the
point. the socket goes first, because the replacement instance of a rolling
deploy is waiting to bind that port and the drain can take the whole grace
period. the listener's *binding* to the config goes last, because that binding
is how a handshake finds the certificate — dropping it while a connection is
still in the drain count is the same mistake as closing the config early, one
step removed.

either mistake corrupts nothing — a handle is never reissued, so a handshake
that loses the race fails cleanly rather than reaching for another config's key
— but both turn working connections into failed ones on the way out, which is a
worse shutdown than the leak they were fixing.

the std servers do this for you: `App.listen_tls`, `http2.listen_h2_tls` and its
streaming twin each build a config, serve on it, and close it after their drain,
so a program that calls one of those has nothing to close.
`tls.open_server_configs()` counts the server configs still holding a
certificate, and on a healthy process the number is flat.

`close()` is idempotent and safe on a config that holds nothing, so a failure
path can close unconditionally on its way out — as long as it is a path where
nothing is still handshaking on it. that caveat is the whole reason a server
closes after its drain rather than in a `defer`, which would fire on the paths
that never drain too.

the functions that build a config for you close it for you — `tls.dial`,
`http.get` and friends, `http2.connect`. the `_with_config` variants never
close, because there the config came from the caller and may well be reused for
the next connection.

what a config does **not** own is its trust anchors. root bundles are parsed
once per distinct bundle and shared by every config that trusts them, and the
system bundle is read from disk at most once per process, so building a config
per request costs a few small map entries rather than a copy and a re-parse of
a couple of hundred kilobytes of pem. that sharing is why `client_config()` is
cheap enough to call on a hot path; closing is what keeps it that way.

the trade is that a process does not notice the system trust store changing
underneath it: `client_config()` reads it once and keeps those roots until the
process restarts. to pick up a rotation without restarting, read the bundle
yourself and build configs with `client_config_with_ca_file`.

## common options

these helpers can be combined on the same config:
- `with_alpn(...)`
- `with_client_certificate(...)`
- `require_client_ca_file(...)`
- `request_client_ca_file(...)`
- `enable_session_resumption()`
- `with_verify_connection(...)`

`enable_session_resumption()` turns on the in-process session stores: the
server's ticket store and the client's session cache. both are guarded by a
lock inside `std.net.tls`, so it is safe to run handshakes concurrently from
many tasks. a server can handshake each connection in its own task, and a
client can dial from a pool of tasks, with no locking of its own.

for servers that need to choose policy before the handshake finishes, use:
- `tls.with_config_selector(config, chooser)`

the selector receives `ClientHelloInfo` with:
- `server_name`
- `alpn_protocols`

and returns the server config that should handle that connection.

that means one selector can choose between different certificate chains,
different alpn lists, and different client-auth policies just by returning a
different `server_config(...)` value.

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

## accepting connections

`tls.listen(host, port, config)` returns a listener. `tls.accept(listener)`
takes the next connection off it and hands back a handshaked `Conn`. that is
the one-shot form, and it is fine for a test or a tool that serves a single
client.

a server with an accept loop wants the two halves separately, because the
handshake reads from the network:

```pith
listener := tls.listen(host, port, config)!
while true:
    fd := tls.accept_socket(listener)!
    spawn serve(tls.listener_from_handle(listener.handle), fd)
```

`accept_socket` returns the raw socket without handshaking it, and
`tls.handshake(listener, fd)` runs the handshake on a socket that came from it.
splitting them keeps two problems off the loop. a client that dribbles its
handshake records now holds only its own task, instead of every other client
waiting to be accepted. and a client whose handshake fails fails on its own task,
instead of propagating an error out of the loop and ending it. that second one
matters more than it sounds: the things that open a connection and never finish a
handshake are ordinary traffic, like port scanners, load balancer probes, and
browsers racing two connections and dropping one.

the socket belongs to `handshake` once you pass it in. on success the returned
`Conn` owns it and `Conn.close()` releases it; on failure `handshake` closes it
before the error propagates, so a spawned task never leaks the socket of a
client that never got past the handshake.

a spawned task takes its arguments by value while the loop keeps using the
listener, which is why the shape above passes `listener.handle` and rebuilds the
listener with `tls.listener_from_handle` inside the task.

a failed handshake is dropped without a log line, because a log line per failure
is a way for anyone with a socket to fill your disk. what it leaves behind is a
counter:

```pith
failures := tls.server_handshake_failures()
```

that counts every server handshake that has failed since the process started:
peers that hang up, peers that send something that is not tls, peers that offer
no acceptable cipher or alpn protocol, and peers that fail client-certificate
verification. a low steady rate is background noise on any public port. a spike
is worth an alert. `std.web`, `std.net.http2.server`, and `std.net.grpc` already
accept this way, so their servers count failures with no extra wiring.

## connection state

every native tls connection exposes a `ConnectionState`:

```pith
state := conn.state()

print(state.version.to_string())
print(state.negotiated_protocol)
print(state.peer_common_name)
```

the fields are:
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
- `client_auth_requested` — whether this endpoint, acting as a server, asked the
  peer for a client certificate; always false on a client connection
- `client_auth_verified` — whether this endpoint, acting as a server, verified
  the peer's client certificate; always false on a client connection

there are also small wrappers on `Conn` for the common cases:
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
- `TLS_CHACHA20_POLY1305_SHA256` (1.3)
- `TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256` (1.2)
- `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256` (1.2)
- `TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256` (1.2)
- `TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256` (1.2)

ecdhe key exchange uses x25519 or nist p-256 (secp256r1) on both 1.3 and the
1.2 fallback. the client offers both and prefers x25519; the server keys with
whichever group the client sent a share for, so a client restricted to p-256
(as some fips deployments are) still negotiates.

## verification hooks

`with_verify_connection(...)` runs after normal certificate verification succeeds.

return `""` to accept the connection. return any non-empty string to reject it.

```pith
fn verify_internal(state: tls.ConnectionState) -> String:
    if state.negotiated_protocol != "pith.rpc":
        return "unexpected protocol"
    return ""

cfg := tls.client_config_with_ca_file("certs/root-ca.pem")!
cfg = cfg.with_alpn(["pith.rpc"]).with_verify_connection(verify_internal)
```

this runs on both client and server configs.

## client auth modes

there are three current server modes:
- no client auth
- optional verified client auth with `request_client_ca_file(...)`
- required verified client auth with `require_client_ca_file(...)`

in optional mode, a client may omit its certificate.
if it does send one, pith verifies it against the configured ca bundle.

## the tls 1.2 fallback

both the client and server negotiate tls 1.3 and, when a peer cannot speak 1.3,
fall back to tls 1.2 — the highest version a peer supports wins, and anything
below 1.2 is refused. this matches go's crypto/tls and rustls. the fallback is
deliberately narrow and forward-secret: only the four ecdhe-rsa/ecdhe-ecdsa
aes-128-gcm and chacha20-poly1305 suites are offered or accepted, so static-rsa
key exchange, cbc suites, rc4, 3des, dhe, and psk are refused by construction.

downgrade protection is built in: the client offers `supported_versions`
{1.3, 1.2} plus extended master secret (rfc 7627) and renegotiation_info
(rfc 5746); a 1.3-capable server that negotiates 1.2 marks its random with the
rfc 8446 downgrade sentinel, and a 1.3-capable client that lands on 1.2 aborts
if it sees that sentinel (an active attacker stripped the 1.3 offer). extended
master secret is mandatory on both sides: the client requires the server to
negotiate it, and the server refuses a 1.2 client that did not offer it rather
than fall back to the weaker rfc 5246 master secret. the server echoes an
extension only when the client asked for it — extended master secret when the
client offered it, and renegotiation_info only when the client advertised
secure renegotiation (the rfc 5746 extension or the SCSV cipher value).

a refused handshake is answered with a fatal alert before the connection
closes, so the peer learns why instead of seeing a bare tcp reset:
`protocol_version` when no version this stack speaks was offered (the client
hello's legacy_version must also be at least 0x0303, the value rfc 8446 froze
it at), `handshake_failure` when the version was fine but no common cipher
suite, signature algorithm, or required extension could be agreed,
`no_application_protocol` for an alpn mismatch, `illegal_parameter` for an
unusable key share, `decrypt_error` for a psk binder that does not validate
against a recognized ticket (rfc 8446 §4.2.11.2 requires the abort), and
`decode_error` / `record_overflow` / `unexpected_message` for malformed input.
the 1.2 fallback engages only when the client actually offered tls 1.2 —
through supported_versions when present, or legacy_version otherwise — never
as an answer to a version the client did not offer. its ServerHello carries an
empty session id (echoing the client's would falsely announce a resumed
session under rfc 5246 §7.4.1.3), and its ServerKeyExchange signature uses a
scheme from the client's signature_algorithms extension, preferring rsa-pss.

`require_tls13()` locks a config to tls 1.3, refusing the fallback — the
equivalent of a minimum version of 1.3:

```pith
cfg := tls.client_config_with_ca_file("certs/root-ca.pem")!.require_tls13()
```

what the 1.2 fallback does not do (v1): session resumption, renegotiation
(refused), client-certificate auth (the server refuses a 1.2 CertificateRequest
path), or aes-256 suites. sni-based server config selection works on 1.2. rsa
(≥2048-bit) and ecdsa (p-256) server certificates are both supported.

## current limits

- the 1.2 fallback is ecdhe + aead only, with the caveats above
- config selection is the dynamic handshake hook today
- the connection state exposes peer identity summaries, not full verified chains yet
