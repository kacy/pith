# purl

A `curl`-compatible HTTP client written in Pith on top of Pith's standard library (`std.net.http`, `std.net.http2`, `std.net.socks5`, `std.net.tls`, `std.net.dns`, `std.net.url`, `std.compress.{gzip,zlib,inflate,zstd}`, `std.hash`, `std.crypto.{hmac,random}`, `std.datetime`, `std.strings`, `std.json`, `std.io`, `std.fs`, `std.os`, `std.time`) and Pith's green-thread concurrency (`spawn`, `Semaphore`).

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

Whenever you hit a protocol or flag `purl` doesn't support yet (e.g. SFTP, HTTP/3, `--unix-socket`), bypass the alias for one command with `\curl ...` or `command curl ...`.

Default flags can live in `~/.purlrc` (using standard curl config syntax); pass `-q` / `--disable` as the first argument to skip it.

## Quick examples

```sh
# Fetch a script or web page (combined short flags + standard exit codes)
purl -fsSL https://example.com

# POST JSON from stdin
echo '{"name":"pith"}' | purl --json @- http://localhost:8080/users

# Send custom headers, remove a default header, Basic, Bearer, Digest, .netrc, or AWS SigV4 auth
purl -H "X-Env: dev" -H "User-Agent:" -u alice:secret https://api.example.com/me
purl --oauth2-bearer "$TOKEN" https://api.example.com/me
purl --digest -u alice:secret https://api.example.com/protected
purl -n https://api.example.com/me
purl --aws-sigv4 "aws:amz:us-east-1:s3" -u "$AWS_ACCESS_KEY_ID:$AWS_SECRET_ACCESS_KEY" https://mybucket.s3.us-east-1.amazonaws.com/key

# Multipart file upload
purl -F "title=avatar" -F "file=@photo.png;type=image/png" https://example.com/upload

# Follow redirects, persist cookies across hops (including Expires= & Max-Age), save jar
purl -L -c cookies.txt https://example.com/login
purl -b cookies.txt https://example.com/dashboard

# Stream downloads straight to disk, cap max size, resume, or use Content-Disposition filename
purl --max-filesize 10485760 -o archive.tar.gz https://example.com/archive.tar.gz
purl -O -J --output-dir ./downloads https://example.com/latest
purl -C - -o archive.tar.gz https://example.com/archive.tar.gz

# URL globbing + #1 output capture + parallel transfers (-Z)
purl -Z --parallel-max 8 'https://example.com/chunks/[01-16].bin' -o 'chunk_#1.bin'
purl 'https://api.example.com/{users,teams,projects}' -o '#1.json'

# Override DNS / routing while preserving Host and TLS SNI
purl --resolve api.example.com:443:127.0.0.1 https://api.example.com/health
purl --connect-to api.example.com:443:127.0.0.1:8443 https://api.example.com/health

# Conditional requests & ETag caching
purl --etag-save etag.txt --etag-compare etag.txt https://example.com/data.json
purl -z cached.json -o cached.json https://example.com/data.json

# Inspect headers, TLS handshake, DNS IP, and write-out metrics
purl -v --compressed https://example.com
purl -s -o /dev/null -w '%{http_code} %{remote_ip} dns=%{time_namelookup}s total=%{time_total}s\n' https://example.com
purl -s -o /dev/null -w '%{json}\n' https://example.com

# HTTP/2 (single-handshake TLS ALPN fallback, or plaintext h2c)
purl --http2 https://example.com
purl --http2-prior-knowledge http://localhost:8080/hello

# SOCKS5 proxies (local or remote DNS, optional username/password auth)
purl --socks5-hostname 127.0.0.1:1080 https://example.com
purl -x socks5h://alice:secret@127.0.0.1:1080 https://example.com

# Multiple independent requests in one invocation
purl -o a.json https://api.example.com/a --next -X POST -d "k=v" https://api.example.com/b
```

## Feature parity with `curl`

### Supported

| Area | Options |
|---|---|
| **Protocols** | `http://`, `https://`, `file://`, schemeless URLs (`--proto-default`, default `http`), embedded `user:pass@host`, bracketed IPv6 (`[::1]:8080`) |
| **HTTP versions** | `--http1.1` (default), `--http2` (single-handshake ALPN `h2` over TLS with HTTP/1.1 fallback), `--http2-prior-knowledge` (`h2c`) |
| **Methods & bodies** | `-X`/`--request`, `-I`/`--head`, `-G`/`--get`, `-d`/`--data`/`--data-ascii` (`@file`, `@-`), `--data-raw`, `--data-binary`, `--data-urlencode` (all 5 forms), `--json`, `-F`/`--form` (`@file`, `<file`, `;filename=`, `;type=`), `--form-string`, `-T`/`--upload-file` (`-` for stdin, trailing-slash basename append) |
| **Headers & auth** | `-H`/`--header` (`Name: Value`, `Name:` removal, `Name;` empty value, `@file`), `-A`/`--user-agent`, `-e`/`--referer` (`;auto`), `-u`/`--user`, `--oauth2-bearer`, `-n`/`--netrc`, `--netrc-optional`, `--netrc-file`, `--digest` (RFC 7616 MD5 & SHA-256 via `std.hash`), `--aws-sigv4` (HMAC-SHA256 request signing), `-U`/`--proxy-user` (plain HTTP proxy, HTTPS `CONNECT` tunnels, and SOCKS5) |
| **Cookies** | `-b`/`--cookie` (inline `k=v` or Netscape jar file), `-c`/`--cookie-jar` (including `-` for stdout), in-memory `Set-Cookie` handling (`Domain`, `Path`, `Max-Age`, RFC 1123 / RFC 850 `Expires`, `Secure`, `HttpOnly`) across redirects |
| **Redirects** | `-L`/`--location`, `--location-trusted`, `--max-redirs` (default 50, exit 47), `--post301`, `--post302`, `--post303`, cross-origin `Authorization` stripping |
| **Output & files** | Raw binary stdout (no stray newlines), direct disk streaming for `-o`/`-O` via `std.net.http.Client.send_to_file`, `-o`/`--output` (`-` for stdout, `#1`/`#2` glob captures), `-O`/`--remote-name`, `--remote-name-all`, `-J`/`--remote-header-name` (path-traversal sanitized), `--output-dir`, `--create-dirs`, `--max-filesize` (exit 63 + partial file cleanup), `-i`/`--include`, `-D`/`--dump-header`, `-r`/`--range`, `-C`/`--continue-at` (`-` auto-resume or offset) |
| **Conditional & caching** | `--etag-save`, `--etag-compare` (`If-None-Match`), `-z`/`--time-cond` (`file`, `-file`, or HTTP date string via `If-Modified-Since` / `If-Unmodified-Since`) |
| **URL globbing & parallel** | `{a,b,c}` sets, `[1-10]` / `[01-05]` / `[a-z]` / `[1-10:2]` ranges, `#1`/`#2` output substitutions, IPv6 `[::1]` preservation, `-g`/`--globoff`, `-Z`/`--parallel`, `--parallel-max` (default 50), `--parallel-immediate` (using Pith `spawn` + `Semaphore`) |
| **DNS & routing overrides** | `--resolve <host:port:addr>` and `--connect-to <h1:p1:h2:p2>` (preserving `Host` header and TLS SNI), DNS lookup resolution via `std.net.dns` |
| **Compression** | `--compressed` (`gzip`, `deflate` zlib + raw RFC 1951, `zstd`), `--raw` |
| **TLS** | TLS 1.3 and TLS 1.2 via `std.net.tls`: `-k`/`--insecure`, `--cacert`, `--cert` + `--key` (mTLS, PKCS#8 / PKCS#1 RSA / SEC1 EC PEM), `--tlsv1.2`, `--tlsv1.3`, `--tls-max` |
| **Timeouts & retries** | `-m`/`--max-time`, `--connect-timeout`, `--expect100-timeout`, `--retry`, `--retry-delay`, `--retry-max-time`, `--retry-all-errors`, `--retry-connrefused`, numeric `Retry-After` support |
| **Proxies** | `-x`/`--proxy` (`http://`, `socks5://`, `socks5h://`), `--socks5`, `--socks5-hostname`, `-U`/`--proxy-user`, `--noproxy`, `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` / `NO_PROXY`, HTTPS `CONNECT` tunnels via `std.net.http.Client`, SOCKS5 (RFC 1928/1929) via `std.net.socks5` |
| **Diagnostics & scripting** | `-v`/`--verbose` (including resolved IP, TLS version, cipher, ALPN, and peer cert info), `-s`/`--silent`, `-S`/`--show-error`, `-f`/`--fail`, `--fail-with-body`, `-w`/`--write-out` (`@file`, `\n`, `\r`, `\t`, `%%`, `%{http_code}`, `%{response_code}`, `%{content_type}`, `%{size_download}`, `%{size_upload}`, `%{size_header}`, `%{size_request}`, `%{time_namelookup}`, `%{time_total}`, `%{time_starttransfer}`, `%{speed_download}`, `%{url_effective}`, `%{redirect_url}`, `%{num_redirects}`, `%{http_version}`, `%{scheme}`, `%{remote_ip}`, `%{remote_port}`, `%{filename_effective}`, `%{exitcode}`, `%{errormsg}`, `%{onerror}`, `%{json}`, `%header{name}`) |
| **CLI ergonomics** | Combined short flags (`-fsSL`, `-iL`, `-svk`), attached short values (`-XPOST`, `-oout`), `--opt=val`, `--no-<flag>` negations, `--` separator, `-K`/`--config` (and `-K -`), `~/.purlrc` + `-q`/`--disable`, `--next`/`-:`, `--url`, `--request-target`, `--no-keepalive`, `-h`/`--help`, `-V`/`--version` |

### Partial support / caveats

- **HTTP/2 default & proxies:** Unlike modern `curl`, `purl` defaults to HTTP/1.1 unless `--http2` or `--http2-prior-knowledge` is passed. When an HTTP proxy (`-x` or env proxy) is active, `--http2` routes over HTTP/1.1 so `std.net.http.Client`'s proxy / `CONNECT` path is used.
- **Direct disk streaming:** `-o` / `-O` streams response bodies directly to disk without buffering in RAM for direct HTTP/1.1 and HTTPS transfers. Transfers combining `--compressed`, `-L`, `-i`, `-J`, `--digest`, `-C`, or HTTP/2 still buffer in memory before writing.
- **Duplicate headers (HTTP/1.1):** `std.net.http` stores request and response headers (other than `Set-Cookie`) in a `Map[String, String]`, so sending or receiving multiple headers with the same name collapses to one value on HTTP/1.1.
- **Client certificates (`--cert` / `--key`):** `std.net.tls` accepts unencrypted PKCS#8 (`PRIVATE KEY`), PKCS#1 (`RSA PRIVATE KEY`), and SEC1 (`EC PRIVATE KEY`) PEM private keys. Encrypted keys and PKCS#12/DER bundles are not supported.
- **Progress meter:** Interactive progress bars are omitted (`-#` / `--progress-bar` and `--no-progress-meter` are accepted as no-ops so scripts work unchanged).
- **Script-compat no-op flags:** `-N`/`--no-buffer`, `-4`/`--ipv4`, `--tcp-nodelay`, `-0`/`--http1.0`, `--anyauth`, `--basic` are accepted without failing.

### Not yet supported

- **Other protocols:** FTP, FTPS, SFTP, SCP, SMTP, IMAP, POP3, LDAP, TFTP, Telnet, Gopher, Dict, SMB, MQTT. (Passing an unsupported scheme exits with curl code `1`.)
- **HTTP/3 (QUIC):** `--http3` / `--http3-only`.
- **SOCKS4 proxies:** `--socks4` (`--socks5`, `--socks5-hostname`, `socks5://`, and `socks5h://` are supported).
- **Enterprise HTTP auth:** `--ntlm`, `--negotiate` (Kerberos / SPNEGO).
- **Low-level socket / DNS controls:** `--unix-socket` (Pith runtime has no `AF_UNIX` socket primitives yet), `--interface`, `--dns-servers`, `--doh-url`, `-6`/`--ipv6` forcing.
- **Brotli (`br`) compression:** `--compressed` supports `gzip`, `deflate`, and `zstd`; `Content-Encoding: br` fails with exit code `61`.
- **Legacy TLS / advanced TLS:** TLS 1.0 / 1.1 (`-1`, `--tlsv1.0`, `--tlsv1.1`), `--capath`, `--pinnedpubkey`, `--ciphers`, OCSP stapling, TLS session ticket cache across invocations.
- **Bandwidth throttling & remote timestamps:** `--limit-rate`, `-R`/`--remote-time`.
- **Per-phase TCP/TLS `-w` timings:** `time_connect`, `time_appconnect`, `time_pretransfer` (`time_namelookup`, `time_starttransfer`, and `time_total` are supported).

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
| `63` | Maximum file size exceeded (`--max-filesize`) |

## Testing

```sh
# Run unit tests + multi-protocol integration suite (HTTP/1.1, TLS 1.2/1.3, mTLS, HTTP/2 h2 & h2c, HTTP/SOCKS5 proxies, file://)
make purl-check

# Or directly:
pith test tools/purl/purl_test.pith
pith build tools/purl/purl.pith
pith build tools/purl/fixture_server.pith
sh tools/purl/run_integration.sh
```

## Pith language & stdlib findings uncovered while building `purl`

Building a real CLI HTTP client surfaced several concrete compiler and standard-library gaps. Most have since been fixed and dogfooded directly in `purl`:

### Resolved

1. **`fn main() -> Int` / `Int!` exit codes** (fixed in #1449): `main` return values now propagate as process exit codes, and `purl.pith` / `fixture_server.pith` return exit codes directly without calling `exit()`.
2. **Splitting strings while keeping empty segments** (fixed in #1448 & #1449): `std.strings.split_keep_empty` / `lines_keep_empty` and `String.split_exact` preserve empty segments (`purl_auth.pith` uses `strings.lines_keep_empty`).
3. **`std.net.url.parse` userinfo, IPv6, and strict port validation** (fixed in #1446).
4. **Process stdio byte streams** (fixed in #1448): `std.io` now provides `read_stdin_bytes`, `write_stdout_bytes`, `write_stdout`, `write_stderr_bytes`, and `write_stderr`, replacing raw `term_write` / `term_read` calls across `purl`.
5. **RFC 1321 MD5 / HMAC-MD5** (fixed in #1448): `std.hash.md5_string` replaced `purl`'s pure-Pith MD5 implementation for legacy HTTP Digest auth.
6. **`std.net.http` client polish** (fixed in #1446): `ClientRequest.with_server_name` for TLS SNI override with connection pooling, `send_to_file` `defer` cleanup, HTTPS `CONNECT` `Proxy-Authorization`, and 4-digit hyphenated `parse_http_date`.
7. **HTTP/2 single-handshake ALPN fallback & SNI override, SOCKS5 client, and PKCS#1/SEC1 PEM keys** (fixed in #1451): `std.net.http2.connection.{connect_tls,open_tls,connect_tls_sni,open_tls_sni}` keep the TLS session on `http/1.1` ALPN fallback (eliminating the second handshake) and support `--connect-to` over HTTP/2; `std.net.socks5` adds RFC 1928/1929 SOCKS5; `std.net.tls` accepts PKCS#1 `RSA PRIVATE KEY` and SEC1 `EC PRIVATE KEY` PEM files alongside PKCS#8.
8. **Destructors (`Drop`) for std resource types & `AtomicInt.fetch_add`/`fetch_sub`** (fixed in #1447 & #1449): `fixture_server.pith` uses `AtomicInt.fetch_add`.

### Remaining / tracked gaps

1. **Duplicate HTTP/1.1 headers:** `std.net.http` currently collapses duplicate header names (other than `Set-Cookie`) into `Map[String, String]` (tracked for multi-value `HeaderField` support).
2. **Optional widening inside Result-wrapped tuples & `from`-import type identity:** `purl_format.pith` still binds `next_cache: Bytes? := loaded` before returning `(Bytes, Bytes?)!`, and `purl_client.pith` imports `std.net.http2.*` before `std.net.http`/`std.net.tls` (tracked in compiler fixes).
3. **`AF_UNIX` Unix domain sockets (`--unix-socket`):** Runtime/stdlib `std.net.unix` support is tracked to enable `--unix-socket <path>`.
4. **POSIX/curl CLI parser ergonomics in `std.cli`:** Short-flag clustering, attached short values, repeatable options, and `--no-*` negations are tracked in `std.cli`; `purl_args.pith` still implements curl-specific `--next` / `-K` config scoping.
5. **Brotli (`br`) compression & HTTP/3 (QUIC):** Not yet in the standard library.
