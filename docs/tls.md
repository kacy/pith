# tls

pith's native tls stack lives in `std.net.tls` and `std.net.tls13`.

the current shape is:
- tls 1.3 only
- client and server handshakes in pith
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

server configs come from a certificate chain and a pkcs#8 private key:

```pith
server_cfg := tls.server_config("certs/server.crt", "certs/server.key")!
```

## common options

these helpers can be combined on the same config:
- `with_alpn(...)`
- `with_client_certificate(...)`
- `require_client_ca_file(...)`
- `request_client_ca_file(...)`
- `enable_session_resumption()`
- `with_verify_connection(...)`

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
- `client_auth_requested`
- `client_auth_verified`

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

`version_name()` currently returns `tls1.3` for native connections.
`cipher_suite_name()` returns one of:
- `TLS_AES_128_GCM_SHA256`
- `TLS_CHACHA20_POLY1305_SHA256`

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

## current limits

- tls 1.3 only
- no tls 1.2 compatibility mode
- config selection is the dynamic handshake hook today
- the connection state exposes peer identity summaries, not full verified chains yet
