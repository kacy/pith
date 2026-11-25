# purl

This is a `curl`-compatible HTTP client written in Pith, built on Pith's standard library and its green-thread concurrency.

It uses `std.net.http`, `std.net.http2.{connection,frames,hpack}`, `std.net.socks5`, `std.net.unix`, `std.net.tls`, `std.net.dns`, `std.net.url`, `std.compress.{gzip,zlib,inflate,zstd}`, `std.hash`, `std.crypto.{hmac,random}`, `std.datetime`, `std.bytes`, `std.encoding`, `std.strings`, `std.json`, `std.io`, `std.fs`, `std.os`, `std.os.path`, and `std.time` from the standard library, and `spawn` and `Semaphore` for concurrency.

The goal is a practical drop-in for everyday `curl` workflows, including aliasing `curl` to `purl`, while using as much of Pith's standard library as possible.

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

Whenever you hit a protocol or flag `purl` doesn't support yet (e.g. SFTP, HTTP/3, Brotli), bypass the alias for one command with `\curl ...` or `command curl ...`.

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

# Talk to a local daemon over a Unix domain socket
purl --unix-socket /var/run/docker.sock http://localhost/version

# Send repeated custom headers and inspect repeated response headers
purl -i -H "X-Tag: one" -H "X-Tag: two" https://api.example.com/items

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
| **Headers & auth** | `-H`/`--header` (`Name: Value`, repeated multi-value headers on HTTP/1.1 & HTTP/2, `Name:` removal, `Name;` empty value, `@file`), `-A`/`--user-agent`, `-e`/`--referer` (`;auto`), `-u`/`--user`, `--oauth2-bearer`, `-n`/`--netrc`, `--netrc-optional`, `--netrc-file`, `--digest` (RFC 7616 MD5 & SHA-256 via `std.hash`), `--aws-sigv4` (HMAC-SHA256 request signing), `-U`/`--proxy-user` (plain HTTP proxy, HTTPS `CONNECT` tunnels, and SOCKS5) |
| **Cookies** | `-b`/`--cookie` (inline `k=v` or Netscape jar file), `-c`/`--cookie-jar` (including `-` for stdout), in-memory `Set-Cookie` handling (`Domain`, `Path`, `Max-Age`, RFC 1123 / RFC 850 `Expires`, `Secure`, `HttpOnly`) across redirects |
| **Redirects** | `-L`/`--location`, `--location-trusted`, `--max-redirs` (default 50, exit 47), `--post301`, `--post302`, `--post303`, cross-origin `Authorization` stripping |
| **Output & files** | Raw binary stdout (no stray newlines), direct disk streaming for `-o`/`-O` via `std.net.http.Client.send_to_file`, `-o`/`--output` (`-` for stdout, `#1`/`#2` glob captures), `-O`/`--remote-name`, `--remote-name-all`, `-J`/`--remote-header-name` (path-traversal sanitized), `--output-dir`, `--create-dirs`, `--max-filesize` (exit 63 + partial file cleanup), `-i`/`--include`, `-D`/`--dump-header`, `-r`/`--range`, `-C`/`--continue-at` (`-` auto-resume or offset) |
| **Conditional & caching** | `--etag-save`, `--etag-compare` (`If-None-Match`), `-z`/`--time-cond` (`file`, `-file`, or HTTP date string via `If-Modified-Since` / `If-Unmodified-Since`) |
| **URL globbing & parallel** | `{a,b,c}` sets, `[1-10]` / `[01-05]` / `[a-z]` / `[1-10:2]` ranges, `#1`/`#2` output substitutions, IPv6 `[::1]` preservation, `-g`/`--globoff`, `-Z`/`--parallel`, `--parallel-max` (default 50, using Pith `spawn` + `Semaphore`) |
| **DNS & routing overrides** | `--resolve <host:port:addr>` and `--connect-to <h1:p1:h2:p2>` (preserving `Host` header and TLS SNI), `--unix-socket <path>` (`AF_UNIX` via `std.net.unix`, skipping DNS), DNS lookup resolution via `std.net.dns` |
| **Compression** | `--compressed` (`gzip`, `deflate` zlib + raw RFC 1951, `zstd`), `--raw` |
| **TLS** | TLS 1.3 and TLS 1.2 via `std.net.tls`: `-k`/`--insecure`, `--cacert`, `--cert` + `--key` (mTLS, PKCS#8 / PKCS#1 RSA / SEC1 EC PEM), `--tlsv1.2`, `--tlsv1.3`, `--tls-max` |
| **Timeouts & retries** | `-m`/`--max-time`, `--connect-timeout`, `--expect100-timeout`, `--retry`, `--retry-delay`, `--retry-max-time`, `--retry-all-errors`, `--retry-connrefused`, numeric `Retry-After` support |
| **Proxies** | `-x`/`--proxy` (`http://`, `socks5://`, `socks5h://`), `--socks5`, `--socks5-hostname`, `-U`/`--proxy-user`, `--noproxy`, `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` / `NO_PROXY`, HTTPS `CONNECT` tunnels via `std.net.http.Client`, SOCKS5 (RFC 1928/1929) via `std.net.socks5` |
| **Diagnostics & scripting** | `-v`/`--verbose` (including resolved IP, TLS version, cipher, ALPN, and peer cert info), `-s`/`--silent`, `-S`/`--show-error`, `-f`/`--fail`, `--fail-with-body`, `-w`/`--write-out` (`@file`, `\n`, `\r`, `\t`, `%%`, `%{http_code}`, `%{response_code}`, `%{content_type}`, `%{size_download}`, `%{size_upload}`, `%{size_header}`, `%{size_request}`, `%{time_namelookup}`, `%{time_total}`, `%{time_starttransfer}`, `%{speed_download}`, `%{url_effective}`, `%{redirect_url}`, `%{num_redirects}`, `%{http_version}`, `%{scheme}`, `%{remote_ip}`, `%{remote_port}`, `%{filename_effective}`, `%{exitcode}`, `%{errormsg}`, `%{onerror}`, `%{json}`, `%header{name}`) |
| **CLI ergonomics** | Combined short flags (`-fsSL`, `-iL`, `-svk`), attached short values (`-XPOST`, `-oout`), `--opt=val`, `--no-<flag>` negations, `--` separator, `-K`/`--config` (and `-K -`), `~/.purlrc` + `-q`/`--disable`, `--next`/`-:`, `--url`, `--request-target`, `--no-keepalive`, `-h`/`--help`, `-V`/`--version` |

### Partial support / caveats

- **HTTP/2 default & proxies:** Unlike modern `curl`, `purl` defaults to HTTP/1.1 unless `--http2` or `--http2-prior-knowledge` is passed. When an HTTP proxy (`-x` or env proxy) is active, `--http2` routes over HTTP/1.1 so `std.net.http.Client`'s proxy / `CONNECT` path is used.
- **Direct disk streaming:** `-o` / `-O` streams response bodies directly to disk without buffering in RAM for direct HTTP/1.1, HTTPS, SOCKS5, and `--unix-socket` transfers. Transfers combining `--compressed`, `-L`, `-i`, `-J`, `--digest`, `-C`, or HTTP/2 still buffer in memory before writing.
- **Client certificates (`--cert` / `--key`):** `std.net.tls` accepts unencrypted PKCS#8 (`PRIVATE KEY`), PKCS#1 (`RSA PRIVATE KEY`), and SEC1 (`EC PRIVATE KEY`) PEM private keys. Encrypted keys and PKCS#12/DER bundles are not supported.
- **TLS version cap (`--tls-max`):** `--tls-max 1.2` / `--tls-max 1.3` currently pin that exact TLS version (`require_tls12()` / `require_tls13()`) rather than setting an upper bound over a range.
- **Proxy environment variables (`ALL_PROXY` / `NO_PROXY`):** `ALL_PROXY` currently applies only to `socks5://` / `socks5h://` proxies (HTTP proxies use `HTTP_PROXY` / `HTTPS_PROXY` or `-x`). `NO_PROXY` is checked for HTTP/HTTPS env proxies by `std.net.http.Client`, but does not bypass SOCKS5 env proxies (pass `--noproxy` to bypass SOCKS5).
- **Progress meter:** Interactive progress bars are omitted (`-#` / `--progress-bar` and `--no-progress-meter` are accepted as no-ops so scripts work unchanged).
- **Script-compat no-op flags:** `-N`/`--no-buffer`, `-4`/`--ipv4`, `--tcp-nodelay`, `-0`/`--http1.0`, `--anyauth`, `--basic`, `--parallel-immediate` are accepted without failing.

### Not yet supported

- **Other protocols:** FTP, FTPS, SFTP, SCP, SMTP, IMAP, POP3, LDAP, TFTP, Telnet, Gopher, Dict, SMB, MQTT. (Passing an unsupported scheme exits with curl code `1`.)
- **HTTP/3 (QUIC):** `--http3` / `--http3-only`.
- **SOCKS4 proxies:** `--socks4` (`--socks5`, `--socks5-hostname`, `socks5://`, and `socks5h://` are supported).
- **Enterprise HTTP auth:** `--ntlm`, `--negotiate` (Kerberos / SPNEGO).
- **Low-level socket / DNS controls:** `--interface`, `--dns-servers`, `--doh-url`, `-6`/`--ipv6` forcing.
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
# Run unit tests + multi-protocol integration suite (HTTP/1.1, TLS 1.2/1.3, mTLS, HTTP/2 h2 & h2c, HTTP/SOCKS5 proxies, AF_UNIX, file://)
make purl-check

# Or directly:
pith test tools/purl/purl_test.pith
pith build tools/purl/purl.pith
pith build tools/purl/fixture_server.pith
sh tools/purl/run_integration.sh
```

## Binary size

`pith build tools/purl/purl.pith` produces a single standalone static-stdlib native binary with TLS 1.2/1.3, HTTP/1.1, HTTP/2, SOCKS5, `AF_UNIX`, gzip/deflate/zstd, and crypto built in, requiring only `libc` at runtime. Stripped, it is ~4.6 MiB (4.8 MB) with 1,701 compiled functions, down from 5.9 MB and 3,369 functions before #1454 pruned the dead cycle-collector tracer, globals, and strings.

## Pith language & stdlib findings uncovered while building `purl`

Building a real CLI HTTP client surfaced several concrete compiler, codegen, runtime, and standard-library gaps. All of the initial compiler and stdlib gaps are now resolved, and `purl` uses the fixes directly:

### Resolved

1. **`fn main() -> Int` / `Int!` exit codes** (#1449): `main` return values propagate as process exit codes, and `purl.pith` / `fixture_server.pith` return exit codes directly without calling `exit()`.
2. **Splitting strings while keeping empty segments** (#1448, #1449): `std.strings.split_keep_empty` / `lines_keep_empty` and `String.split_exact` preserve empty segments (`purl_auth.pith` uses `strings.lines_keep_empty`).
3. **`std.net.url.parse` userinfo, IPv6, and strict port validation** (#1446).
4. **Process stdio byte streams** (#1448): `std.io` provides `read_stdin_bytes`, `write_stdout_bytes`, `write_stdout`, `write_stderr_bytes`, and `write_stderr`, replacing raw `term_write` / `term_read` calls across `purl`.
5. **RFC 1321 MD5 / HMAC-MD5** (#1448): `std.hash.md5_string` replaced `purl`'s pure-Pith MD5 implementation for legacy HTTP Digest auth.
6. **`std.net.http` client polish & multi-value headers** (#1446, #1455): `ClientRequest.with_server_name` for TLS SNI override with connection pooling, `send_to_file` `defer` cleanup, HTTPS `CONNECT` `Proxy-Authorization`, 4-digit hyphenated `parse_http_date`, and multi-value `HeaderField` / `add_header` / `raw_headers` / `header_values` across HTTP/1.1 and HTTP/2.
7. **HTTP/2 single-handshake ALPN fallback & SNI override, SOCKS5 client, and PKCS#1/SEC1 PEM keys** (#1451): `std.net.http2.connection.{connect_tls,open_tls,connect_with_config_sni,open_with_config_sni}` keep the TLS session on `http/1.1` ALPN fallback (eliminating the second handshake) and support `--connect-to` over HTTP/2; `std.net.socks5` adds RFC 1928/1929 SOCKS5; `std.net.tls` accepts PKCS#1 `RSA PRIVATE KEY` and SEC1 `EC PRIVATE KEY` PEM files alongside PKCS#8.
8. **Destructors (`Drop`) for std resource types & `AtomicInt.fetch_add`/`fetch_sub`** (#1447, #1449, #1456): `fixture_server.pith` uses `AtomicInt.fetch_add`.
9. **POSIX CLI ergonomics in `std.cli`** (#1452): short-flag clustering (`-fsSL`), attached short values (`-XPOST`), repeatable `multi_option`, `--no-*` negations, and `--` separator (`purl_args.pith` still keeps curl-specific `--next` / `-K` config scoping).
10. **`AF_UNIX` Unix domain sockets, Result-wrapped tuple optional widening, & module-scoped `from`-import type identity** (#1453): `purl` now supports `--unix-socket <path>` via `std.net.unix`, `purl_format.pith` directly returns `(loaded, loaded)` for `(Bytes, Bytes?)!`, and `purl.pith` / `purl_client.pith` / `fixture_server.pith` no longer need the `std.net.http2.*` import-ordering workaround.
11. **Codegen dead tracer/global/string pruning & `.rodata` string deduplication** (#1454): cut `purl`'s compiled functions nearly in half (3,369 → 1,701) and shrank the binary by ~1.1 MB even after adding SOCKS5, `AF_UNIX`, and multi-value headers.

### Longer-term stdlib gaps

1. **Brotli (`br`) decompression (`std.compress.brotli`)**: `--compressed` currently covers `gzip`, `deflate`, and `zstd`.
2. **HTTP/3 (QUIC)**: `--http3` / `--http3-only`.
3. **Encrypted PEM private keys & PKCS#12 bundles** in `std.net.tls`.
4. **Per-phase TCP/TLS connection timings** (`time_connect`, `time_appconnect`, `time_pretransfer`) exposed from `std.net.http` / `std.net.tls`.
