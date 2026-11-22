# purl

A `curl`-compatible HTTP client written in Pith on top of Pith's standard library (`std.net.http`, `std.net.http2`, `std.net.tls`, `std.net.url`, `std.compress.{gzip,zlib,zstd}`, `std.json`, `std.fs`, `std.os`, `std.time`).

The goal is a practical drop-in for everyday `curl` workflows — including aliasing `curl` to `purl` — while dogfooding as much of Pith's standard library as possible.

## Build & install

From the repository root:

```sh
pith build tools/purl/purl.pith
./tools/purl/purl --version
```

### Aliasing your local `curl` to `purl`

Copy (or symlink) the binary somewhere on your `PATH`:

```sh
mkdir -p ~/.local/bin
cp tools/purl/purl ~/.local/bin/purl
```

Then add an alias to your shell rc file:

- **bash** (`~/.bashrc`) or **zsh** (`~/.zshrc`):
  ```sh
  alias curl=purl
  ```
- **fish** (`~/.config/fish/config.fish`):
  ```fish
  alias curl purl
  ```

Whenever you hit a protocol or flag `purl` doesn't support yet (e.g. SFTP, SOCKS5, HTTP/3), bypass the alias for one command with `\curl ...` or `command curl ...`.

Default flags can live in `~/.purlrc` (using standard curl config syntax); pass `-q` / `--disable` as the first argument to skip it.

## Quick examples

```sh
# Fetch a script or web page (combined short flags + standard exit codes)
purl -fsSL https://example.com

# POST JSON from stdin
echo '{"name":"pith"}' | purl --json @- http://localhost:8080/users

# Send custom headers, remove a default header, Basic or Bearer auth
purl -H "X-Env: dev" -H "User-Agent:" -u alice:secret https://api.example.com/me
purl --oauth2-bearer "$TOKEN" https://api.example.com/me

# Multipart file upload
purl -F "title=avatar" -F "file=@photo.png;type=image/png" https://example.com/upload

# Follow redirects, persist cookies across hops, save jar
purl -L -c cookies.txt https://example.com/login
purl -b cookies.txt https://example.com/dashboard

# Download with Content-Disposition filename, or resume an interrupted download
purl -O -J --output-dir ./downloads https://example.com/latest
purl -C - -o archive.tar.gz https://example.com/archive.tar.gz

# Inspect headers, TLS handshake, and write-out metrics
purl -v --compressed https://example.com
purl -s -o /dev/null -w '%{http_code} %{time_total}s %{size_download}B\n' https://example.com
purl -s -o /dev/null -w '%{json}\n' https://example.com

# HTTP/2 (over TLS with ALPN fallback, or plaintext h2c)
purl --http2 https://example.com
purl --http2-prior-knowledge http://localhost:8080/hello

# Multiple independent requests in one invocation
purl -o a.json https://api.example.com/a --next -X POST -d "k=v" https://api.example.com/b
```

## Feature parity with `curl`

### Supported

| Area | Options |
|---|---|
| **Protocols** | `http://`, `https://`, `file://`, schemeless URLs (`--proto-default`, default `http`), embedded `user:pass@host`, bracketed IPv6 (`[::1]:8080`) |
| **HTTP versions** | `--http1.1` (default), `--http2` (ALPN `h2` over TLS with HTTP/1.1 fallback), `--http2-prior-knowledge` (`h2c`) |
| **Methods & bodies** | `-X`/`--request`, `-I`/`--head`, `-G`/`--get`, `-d`/`--data`/`--data-ascii` (`@file`, `@-`), `--data-raw`, `--data-binary`, `--data-urlencode` (all 5 forms), `--json`, `-F`/`--form` (`@file`, `<file`, `;filename=`, `;type=`), `--form-string`, `-T`/`--upload-file` (`-` for stdin, trailing-slash basename append) |
| **Headers & auth** | `-H`/`--header` (`Name: Value`, `Name:` removal, `Name;` empty value, `@file`), `-A`/`--user-agent`, `-e`/`--referer` (`;auto`), `-u`/`--user`, `--oauth2-bearer`, `-U`/`--proxy-user` (plain HTTP proxy) |
| **Cookies** | `-b`/`--cookie` (inline `k=v` or Netscape jar file), `-c`/`--cookie-jar` (including `-` for stdout), in-memory `Set-Cookie` handling (`Domain`, `Path`, `Max-Age`, `Secure`, `HttpOnly`) across redirects |
| **Redirects** | `-L`/`--location`, `--location-trusted`, `--max-redirs` (default 50, exit 47), `--post301`, `--post302`, `--post303`, cross-origin `Authorization` stripping |
| **Output & files** | Raw binary stdout (no stray newlines), `-o`/`--output` (`-` for stdout), `-O`/`--remote-name`, `--remote-name-all`, `-J`/`--remote-header-name` (path-traversal sanitized), `--output-dir`, `--create-dirs`, `-i`/`--include`, `-D`/`--dump-header`, `-r`/`--range`, `-C`/`--continue-at` (`-` auto-resume or offset) |
| **Compression** | `--compressed` (`gzip`, `deflate` zlib + raw RFC 1951, `zstd`), `--raw` |
| **TLS** | TLS 1.3 and TLS 1.2 via `std.net.tls`: `-k`/`--insecure`, `--cacert`, `--cert` + `--key` (mTLS, PKCS#8 PEM), `--tlsv1.2`, `--tlsv1.3`, `--tls-max` |
| **Timeouts & retries** | `-m`/`--max-time`, `--connect-timeout`, `--expect100-timeout`, `--retry`, `--retry-delay`, `--retry-max-time`, `--retry-all-errors`, `--retry-connrefused`, numeric `Retry-After` support |
| **Proxies** | `-x`/`--proxy`, `--noproxy`, `HTTP_PROXY` / `HTTPS_PROXY` / `NO_PROXY` (and lowercase variants), HTTPS `CONNECT` tunnels via `std.net.http.Client` |
| **Diagnostics & scripting** | `-v`/`--verbose` (including TLS version, cipher, ALPN, and peer cert info), `-s`/`--silent`, `-S`/`--show-error`, `-f`/`--fail`, `--fail-with-body`, `-w`/`--write-out` (`@file`, `\n`, `\r`, `\t`, `%%`, `%{http_code}`, `%{response_code}`, `%{content_type}`, `%{size_download}`, `%{size_upload}`, `%{size_header}`, `%{size_request}`, `%{time_total}`, `%{time_starttransfer}`, `%{speed_download}`, `%{url_effective}`, `%{redirect_url}`, `%{num_redirects}`, `%{http_version}`, `%{scheme}`, `%{remote_port}`, `%{filename_effective}`, `%{exitcode}`, `%{errormsg}`, `%{onerror}`, `%{json}`, `%header{name}`) |
| **CLI ergonomics** | Combined short flags (`-fsSL`, `-iL`, `-svk`), attached short values (`-XPOST`, `-oout`), `--opt=val`, `--no-<flag>` negations, `--` separator, `-K`/`--config` (and `-K -`), `~/.purlrc` + `-q`/`--disable`, `--next`/`-:`, `--url`, `--request-target`, `--no-keepalive`, `-h`/`--help`, `-V`/`--version` |

### Partial support / caveats

- **HTTP/2 default & proxies:** Unlike modern `curl`, `purl` defaults to HTTP/1.1 unless `--http2` or `--http2-prior-knowledge` is passed. When an HTTP proxy (`-x` or env proxy) is active, `--http2` routes over HTTP/1.1 so `std.net.http.Client`'s proxy / `CONNECT` path is used.
- **Duplicate headers (HTTP/1.1):** `std.net.http` stores request and response headers (other than `Set-Cookie`) in a `Map[String, String]`, so sending or receiving multiple headers with the same name collapses to one value on HTTP/1.1.
- **Client certificates (`--cert` / `--key`):** `std.net.tls` accepts PKCS#8 PEM private keys (`-----BEGIN PRIVATE KEY-----`) without passphrases. PKCS#1 (`RSA PRIVATE KEY`), encrypted keys, and PKCS#12/DER bundles are not supported.
- **Proxy authentication (`-U` / `--proxy-user`):** Sends `Proxy-Authorization` on plain HTTP proxy requests; HTTPS `CONNECT` tunnel authentication is not yet supported by `std.net.http`.
- **Cookie expiration:** `Max-Age` is honored; HTTP-date `Expires=` attributes are currently stored as session cookies (`expires = 0`).
- **Progress meter:** Interactive progress bars are omitted (`-#` / `--progress-bar` and `--no-progress-meter` are accepted as no-ops so scripts work unchanged).
- **Script-compat no-op flags:** `-N`/`--no-buffer`, `-g`/`--globoff`, `-4`/`--ipv4`, `--tcp-nodelay`, `-0`/`--http1.0`, `--anyauth`, `--basic` are accepted without failing.

### Not yet supported

- **Other protocols:** FTP, FTPS, SFTP, SCP, SMTP, IMAP, POP3, LDAP, TFTP, Telnet, Gopher, Dict, SMB, MQTT. (Passing an unsupported scheme exits with curl code `1`.)
- **HTTP/3 (QUIC):** `--http3` / `--http3-only`.
- **SOCKS proxies:** `--socks4`, `--socks5`, `--socks5-hostname`, and `socks5://` proxy URLs.
- **Non-Basic HTTP auth:** `--digest`, `--ntlm`, `--negotiate`, `--aws-sigv4`, `.netrc` (`-n`).
- **DNS / routing overrides:** `--resolve`, `--connect-to`, `--interface`, `--unix-socket`, `--dns-servers`, `--doh-url`, `-6`/`--ipv6` forcing.
- **URL globbing & parallel transfers:** `[1-10]` / `{a,b}` URL expansion, `-Z`/`--parallel`.
- **Brotli (`br`) compression:** `--compressed` supports `gzip`, `deflate`, and `zstd`; `Content-Encoding: br` fails with exit code `61`.
- **Legacy TLS / advanced TLS:** TLS 1.0 / 1.1 (`-1`, `--tlsv1.0`, `--tlsv1.1`), `--capath`, `--pinnedpubkey`, `--ciphers`, OCSP stapling, TLS session ticket cache across invocations.
- **Streaming very large bodies:** `purl` currently buffers response bodies in memory before writing (to support decompression and `-f` suppression cleanly); `--limit-rate`, `--max-filesize`, and unbounded streaming downloads are not yet wired up.
- **Conditional time headers & fine-grained timing:** `-z`/`--time-cond`, `-R`/`--remote-time`, `--etag-compare`/`--etag-save`, and per-phase `-w` timings (`time_namelookup`, `time_connect`, `time_appconnect`, `time_pretransfer`).

### Exit codes

`purl` maps common failures to standard `curl` exit codes so shell scripts (`purl -fsSL ... || ...`) behave identically:

| Code | Meaning |
|---:|---|
| `0` | Success |
| `1` | Unsupported protocol |
| `2` | Failed to initialize / bad CLI option or missing URL |
| `3` | Malformed URL |
| `6` | Could not resolve host |
| `7` | Failed to connect to host |
| `22` | HTTP error (`-f` / `--fail` / `--fail-with-body` on status >= 400) |
| `23` | Write error (output file, header dump, or cookie jar) |
| `26` | Read error (missing `@file` or `-T` upload file) |
| `28` | Operation timeout (`-m` / `--connect-timeout`) |
| `35` | TLS handshake / connect error |
| `37` | Failed to open `file://` URL |
| `47` | Too many redirects (`--max-redirs`) |
| `52` | Empty reply from server |
| `56` | Failure receiving network data |
| `60` | TLS certificate verification failure |
| `61` | Unrecognized `Content-Encoding` |

## Testing

```sh
# Run unit tests + multi-protocol integration suite (HTTP/1.1, TLS 1.2/1.3, mTLS, HTTP/2 h2 & h2c, proxy, file://)
make purl-check

# Or directly:
pith test tools/purl/purl_test.pith
pith build tools/purl/purl.pith
pith build tools/purl/fixture_server.pith
sh tools/purl/run_integration.sh
```

## Pith language & stdlib findings uncovered while building `purl`

Building a real CLI HTTP client surfaced several concrete compiler and standard-library gaps worth tracking:

1. **`fn main() -> Int` return values are ignored (process always exits 0).**
   The entry glue in `cranelift/runtime/src/runtime_core.rs` pins normal returns from `main` to exit status `0`, so `return 1` in `main() -> Int` does not produce a non-zero exit code. CLI tools must explicitly call the builtin `exit(code)` to return non-zero exit codes.
2. **`std.net.url.parse` gaps (`std/net/url.pith`):**
   - Embedded `userinfo@` (`http://user:pass@host:8080/path`) is not handled: scanning for the first `:` inside the authority splits between `user` and `pass`, misparsing `host` as `"user"` and scanning `pass@host:8080` as a port.
   - Bracketed IPv6 literals (`http://[::1]:8080/`) split on the first `:` inside `[::1]`.
   - Non-digit characters in the port segment (e.g. `:80abc`) are silently skipped instead of rejecting the port.
   (`purl_url.pith` works around all three before delegating path/query/fragment parsing and percent-encoding to `std.net.url`.)
3. **Optional widening fails inside a Result-wrapped tuple return.**
   In a function returning `(Bytes, Bytes?)!`, `return (loaded, loaded)` where `loaded: Bytes` fails type checking (`E200: return type mismatch: expected result, got tuple`), whereas binding `next_cache: Bytes? := loaded` and returning `(loaded, next_cache)` succeeds.
4. **Module import-order sensitivity between `std.net.http2.*` and `std.net.tls` / `std.net.http`.**
   As noted in `std/net/http2/connection.pith`, `std.net.http2.{frames,hpack,connection}` must be imported before `std.net.http` / `std.net.tls` so `std.binary.Reader` keeps a single type identity in the self-hosted checker.
5. **No `std.io` byte streams for process stdin/stdout/stderr.**
   `print()` always appends a newline and only takes UTF-8 `String`, so raw binary stdout, exact stderr output, and piped/redirected stdin rely on the lower-level `term_write(1/2, Bytes)` and `term_read(0, max, -1)` builtins from `std.term.tty`.
6. **`std.net.http` / `std.net.http2` / `std.net.tls` client-side gaps for full curl parity:**
   - `http.ClientRequest.headers` and `http.HttpResponse.headers` are `Map[String, String]`, collapsing duplicate header names (except `Set-Cookie`).
   - `http.Client` has no separate dial-host vs TLS SNI `server_name` override (blocking `--resolve` / `--connect-to` without bypassing `http.Client`), even though `ClientRequest.send_tls_with_config` accepts a custom `server_name`.
   - `http.Client`'s `open_tunnel` does not emit `Proxy-Authorization` on HTTPS `CONNECT` tunnels.
   - `std.net.http2.connection.connect_with_config` closes the TLS session if the peer negotiates `http/1.1` instead of `h2`, so `--http2` fallback currently requires a second handshake over `http.Client`.
   - `std.net.tls` only parses unencrypted PKCS#8 PEM private keys (`PRIVATE KEY`).
7. **`std.args` and `std.cli` don't cover POSIX/curl CLI conventions.**
   Neither supports combined short flags (`-fsSL`), attached short values (`-XPOST`), repeatable options (`-H`), `--no-*` negations, or `--next` scopes, so `purl` includes its own parser (`purl_args.pith`).
