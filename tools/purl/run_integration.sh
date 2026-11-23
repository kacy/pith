#!/bin/sh
# End-to-end integration test suite for purl against local fixture_server instances.
# Tests HTTP/1.1, TLS 1.2/1.3, mTLS, HTTP/2 (h2 and h2c), HTTP proxy, and file://.
set -eu

PURL="${1:-./tools/purl/purl}"
SERVER="${2:-./tools/purl/fixture_server}"
FIXTURES="tests/live/fixtures"

TMPDIR=$(mktemp -d /tmp/purl-int-XXXXXX)
PIDS=""
cleanup() {
    for p in $PIDS; do
        kill "$p" 2>/dev/null || true
    done
    rm -rf "$TMPDIR"
}
trap cleanup EXIT

PASS=0
FAIL=0

pass() {
    PASS=$((PASS + 1))
    printf 'ok   %s\n' "$1"
}

fail() {
    FAIL=$((FAIL + 1))
    printf 'FAIL %s: %s\n' "$1" "$2" >&2
}

assert_eq() {
    name="$1"
    got="$2"
    want="$3"
    if [ "$got" = "$want" ]; then
        pass "$name"
    else
        fail "$name" "expected '$want', got '$got'"
    fi
}

assert_contains() {
    name="$1"
    haystack="$2"
    needle="$3"
    case "$haystack" in
        *"$needle"*) pass "$name" ;;
        *) fail "$name" "expected output to contain '$needle', got '$haystack'" ;;
    esac
}

start_server() {
    "$SERVER" "$@" &
    PIDS="$PIDS $!"
}

# Use fixed ports well outside Linux's ephemeral range (32768-60999).
HTTP_PORT=18901
TLS_PORT=18902
MTLS_PORT=18903
H2C_PORT=18904
H2_PORT=18905
PROXY_PORT=18906
SOCKS_PORT=18907
SOCKS_AUTH_PORT=18908

start_server http "$HTTP_PORT" 200
start_server tls "$TLS_PORT" 30 "$FIXTURES/localhost.crt" "$FIXTURES/localhost.key"
start_server tls "$MTLS_PORT" 20 "$FIXTURES/localhost.crt" "$FIXTURES/localhost.key" "$FIXTURES/clientcert-ca.crt"
start_server h2c "$H2C_PORT" 10
start_server h2 "$H2_PORT" 20 "$FIXTURES/localhost.crt" "$FIXTURES/localhost.key"
start_server proxy "$PROXY_PORT" 10
start_server socks5 "$SOCKS_PORT" 10
start_server socks5 "$SOCKS_AUTH_PORT" 10 alice secret
sleep 0.5

BASE="http://127.0.0.1:$HTTP_PORT"

# 1. Basic GET without trailing newline
out=$("$PURL" -s "$BASE/hello")
assert_eq "basic GET" "$out" "hello from purl"

# 2. Schemeless URL (defaults to http://)
out=$("$PURL" -s "127.0.0.1:$HTTP_PORT/hello")
assert_eq "schemeless URL" "$out" "hello from purl"

# 3. Raw binary body with NUL bytes preserved intact
"$PURL" -s "$BASE/binary" > "$TMPDIR/bin.out"
hex=$(od -An -tx1 "$TMPDIR/bin.out" | tr -d ' \n')
assert_eq "binary NUL bytes stdout" "$hex" "000102ff"

# 4. POST with -d, -H, -A, -u, -e
out=$("$PURL" -s -d "a=1" -d "b=2" -H "X-Custom: sparkle" -A "my-agent/2" -u "alice:secret" -e "https://ref.test/" "$BASE/echo")
assert_contains "POST method" "$out" '"method":"POST"'
assert_contains "POST joined data" "$out" '"body":"a=1&b=2"'
assert_contains "POST default form content-type" "$out" '"content_type":"application/x-www-form-urlencoded"'
assert_contains "POST custom header" "$out" '"x_custom":"sparkle"'
assert_contains "POST user-agent" "$out" '"user_agent":"my-agent/2"'
assert_contains "POST basic auth" "$out" '"authorization":"Basic YWxpY2U6c2VjcmV0"'
assert_contains "POST referer" "$out" '"referer":"https://ref.test/"'

# 5. Header removal (-H "User-Agent:") and --oauth2-bearer
out=$("$PURL" -s -H "User-Agent:" --oauth2-bearer "tok123" "$BASE/echo")
assert_contains "header removal User-Agent" "$out" '"user_agent":""'
assert_contains "oauth2-bearer" "$out" '"authorization":"Bearer tok123"'

# 6. --json convenience flag + stdin (@-)
out=$(printf '{"hi":42}' | "$PURL" -s --json @- "$BASE/echo")
assert_contains "json content-type" "$out" '"content_type":"application/json"'
assert_contains "json stdin body" "$out" '"body":"{\"hi\":42}"'

# 7. -G / --get moves -d / --data-urlencode into query string
out=$("$PURL" -s -G --data-urlencode "q=hello world" -d "page=2" "$BASE/echo")
assert_contains "-G uses GET" "$out" '"method":"GET"'
assert_contains "-G query string" "$out" '"query":"q=hello%20world&page=2"'

# 8. -T / --upload-file (PUT with basename appended to trailing slash)
printf 'uploaded payload' > "$TMPDIR/note.txt"
out=$("$PURL" -s -T "$TMPDIR/note.txt" "$BASE/upload/")
assert_contains "-T uses PUT" "$out" '"method":"PUT"'
assert_contains "-T appends basename" "$out" '"path":"/upload/note.txt"'
assert_contains "-T body" "$out" '"body":"uploaded payload"'

# 9. -F / --form multipart upload
printf 'file-contents-here' > "$TMPDIR/up.txt"
out=$("$PURL" -s -F "kind=avatar" -F "file=@$TMPDIR/up.txt;filename=pic.txt;type=text/plain" "$BASE/multipart")
assert_contains "-F text field" "$out" '"name":"kind"'
assert_contains "-F text value" "$out" '"body":"avatar"'
assert_contains "-F file name" "$out" '"filename":"pic.txt"'
assert_contains "-F file body" "$out" '"body":"file-contents-here"'

# 10. Redirects (-L), cookies across hops, auto-referer (-e ";auto"), -i includes intermediate headers
out=$("$PURL" -s -i -L -e ";auto" "$BASE/redirect/1")
assert_contains "-iL shows 302" "$out" "HTTP/1.1 302 Found"
assert_contains "-iL shows 301" "$out" "HTTP/1.1 301 Moved Permanently"
assert_contains "-iL shows 200" "$out" "HTTP/1.1 200 OK"
assert_contains "-L cookies propagated" "$out" "hop1=alpha"
assert_contains "-L second cookie propagated" "$out" "hop2=beta"
assert_contains "-e ;auto sets referer" "$out" "/redirect/2"

# 11. Redirect method preservation (307 preserves POST; 302 switches to GET unless --post302)
out=$("$PURL" -s -L -d "stay=yes" "$BASE/redirect-307")
assert_contains "307 preserves POST" "$out" '"method":"POST"'
assert_contains "307 preserves body" "$out" '"body":"stay=yes"'

out=$("$PURL" -s -L -d "drop=yes" "$BASE/redirect-302")
assert_contains "302 switches to GET" "$out" '"method":"GET"'

out=$("$PURL" -s -L --post302 -d "keep=yes" "$BASE/redirect-302")
assert_contains "--post302 keeps POST" "$out" '"method":"POST"'
assert_contains "--post302 keeps body" "$out" '"body":"keep=yes"'

# 12. --max-redirs triggers curl exit code 47
set +e
"$PURL" -s -L --max-redirs 2 "$BASE/redirect-loop" > /dev/null 2>&1
rc=$?
set -e
assert_eq "--max-redirs exit 47" "$rc" "47"

# 13. -I HEAD request and -D header dump
out=$("$PURL" -s -I "$BASE/hello")
assert_contains "-I status line" "$out" "HTTP/1.1 200 OK"
"$PURL" -s -D "$TMPDIR/dump.hdr" -o /dev/null "$BASE/hello"
dump_content=$(cat "$TMPDIR/dump.hdr")
assert_contains "-D dumped headers" "$dump_content" "HTTP/1.1 200 OK"

# 14. -f (--fail) vs --fail-with-body (exit code 22)
set +e
out=$("$PURL" -fsS "$BASE/status/404" 2>"$TMPDIR/fail.err")
rc=$?
set -e
assert_eq "-f exit 22" "$rc" "22"
assert_eq "-f suppresses body" "$out" ""
assert_contains "-fsS shows error on stderr" "$(cat "$TMPDIR/fail.err")" "purl: (22)"

set +e
out=$("$PURL" -s --fail-with-body "$BASE/status/404" 2>/dev/null)
rc=$?
set -e
assert_eq "--fail-with-body exit 22" "$rc" "22"
assert_eq "--fail-with-body keeps body" "$out" "missing page body"

# 15. Compression: gzip, deflate, zstd via --compressed
assert_eq "--compressed gzip" "$("$PURL" -s --compressed "$BASE/gzip")" "compressed via gzip payload"
assert_eq "--compressed deflate" "$("$PURL" -s --compressed "$BASE/deflate")" "compressed via deflate payload"
assert_eq "--compressed zstd" "$("$PURL" -s --compressed "$BASE/zstd")" "compressed via zstd payload"

# 16. Cookie jar round-trip (-c then -b)
"$PURL" -s -c "$TMPDIR/jar.txt" "$BASE/cookie-set" > /dev/null
out=$("$PURL" -s -b "$TMPDIR/jar.txt" "$BASE/echo")
assert_contains "cookie jar session" "$out" "session=xyz99"
assert_contains "cookie jar prefs" "$out" "prefs=compact"

# 17. Output files: -o with --create-dirs, -O -J --output-dir, -r range, -C - resume
"$PURL" -s --create-dirs -o "$TMPDIR/nested/sub/hello.txt" "$BASE/hello"
assert_eq "-o --create-dirs" "$(cat "$TMPDIR/nested/sub/hello.txt")" "hello from purl"

mkdir -p "$TMPDIR/dl"
"$PURL" -s -O -J --output-dir "$TMPDIR/dl" "$BASE/download"
assert_eq "-O -J Content-Disposition filename" "$(cat "$TMPDIR/dl/report.txt")" "0123456789abcdef"

assert_eq "-r range request" "$("$PURL" -s -r 4-7 "$BASE/download")" "4567"

printf '012345' > "$TMPDIR/partial.txt"
"$PURL" -s -C - -o "$TMPDIR/partial.txt" "$BASE/download"
assert_eq "-C - resume download" "$(cat "$TMPDIR/partial.txt")" "0123456789abcdef"

# 18. -w write-out & -v verbose
out=$("$PURL" -s -o /dev/null -w '%{http_code}:%{size_download}:%{num_redirects}:%{http_version}' -L "$BASE/redirect/1")
assert_contains "-w format" "$out" "200:"
assert_contains "-w redirect count" "$out" ":2:1.1"

"$PURL" -v "$BASE/hello" > /dev/null 2>"$TMPDIR/verb.err"
verb=$(cat "$TMPDIR/verb.err")
assert_contains "-v request line" "$verb" "> GET /hello HTTP/1.1"
assert_contains "-v response line" "$verb" "< HTTP/1.1 200 OK"

# 19. --retry on 503 flaky endpoint
out=$("$PURL" -s --retry 3 --retry-delay 0.05 "$BASE/flaky")
assert_eq "--retry recovers from 503" "$out" "recovered"

# 20. -m timeout triggers exit code 28
set +e
"$PURL" -s -m 0.1 "$BASE/slow" > /dev/null 2>&1
rc=$?
set -e
assert_eq "-m timeout exit 28" "$rc" "28"

# 21. -K config file and --next multiple independent requests
cat > "$TMPDIR/req.cfg" <<EOF
# purl config test
header = "X-Custom: from-config"
url = "$BASE/echo"
EOF
out=$("$PURL" -s -K "$TMPDIR/req.cfg")
assert_contains "-K config header" "$out" '"x_custom":"from-config"'

"$PURL" -s -o "$TMPDIR/n1.txt" "$BASE/hello" --next -s -d "x=9" -o "$TMPDIR/n2.txt" "$BASE/echo"
assert_eq "--next first request" "$(cat "$TMPDIR/n1.txt")" "hello from purl"
assert_contains "--next second request" "$(cat "$TMPDIR/n2.txt")" '"body":"x=9"'

# 22. file:// protocol
printf 'local file bytes' > "$TMPDIR/local.txt"
assert_eq "file:// URL" "$("$PURL" -s "file://$TMPDIR/local.txt")" "local file bytes"

# 23. HTTP proxy (-x)
out=$("$PURL" -s -x "http://127.0.0.1:$PROXY_PORT" "http://example.invalid/via-proxy")
assert_contains "-x HTTP proxy" "$out" '"proxied":true'
assert_contains "-x proxy host" "$out" '"host":"example.invalid"'

# 24. TLS 1.3 & TLS 1.2 (--cacert, -k/--insecure, verbose TLS info, cert failure exit 60)
out=$("$PURL" -s --cacert "$FIXTURES/localhost-ca.crt" --tlsv1.3 "https://localhost:$TLS_PORT/hello")
assert_eq "HTTPS TLS 1.3 with --cacert" "$out" "hello from purl"

out=$("$PURL" -s --cacert "$FIXTURES/localhost-ca.crt" --tlsv1.2 "https://localhost:$TLS_PORT/hello")
assert_eq "HTTPS TLS 1.2 with --cacert" "$out" "hello from purl"

out=$("$PURL" -s -k "https://localhost:$TLS_PORT/hello")
assert_eq "HTTPS -k insecure" "$out" "hello from purl"

"$PURL" -v --cacert "$FIXTURES/localhost-ca.crt" "https://localhost:$TLS_PORT/hello" > /dev/null 2>"$TMPDIR/tls_verb.err"
assert_contains "HTTPS -v TLS handshake info" "$(cat "$TMPDIR/tls_verb.err")" "TLS connection using"

set +e
"$PURL" -s "https://localhost:$TLS_PORT/hello" > /dev/null 2>&1
rc=$?
set -e
assert_eq "HTTPS untrusted cert exit 60" "$rc" "60"

# 25. mTLS client certificate (--cert and --key)
out=$("$PURL" -s --cacert "$FIXTURES/localhost-ca.crt" --cert "$FIXTURES/clientcert-ecdsa.crt" --key "$FIXTURES/clientcert-ecdsa.key" "https://localhost:$MTLS_PORT/hello")
assert_eq "mTLS --cert/--key" "$out" "hello from purl"

# 26. HTTP/2 plaintext prior-knowledge (h2c)
out=$("$PURL" -s --http2-prior-knowledge -w ' [v=%{http_version}]' "http://127.0.0.1:$H2C_PORT/hello")
assert_eq "HTTP/2 h2c GET" "$out" "hello from purl [v=2]"

out=$("$PURL" -s --http2-prior-knowledge -d "h2=yes" -H "X-Custom: frame" "http://127.0.0.1:$H2C_PORT/echo")
assert_contains "HTTP/2 h2c POST body" "$out" '"body":"h2=yes"'
assert_contains "HTTP/2 h2c custom header" "$out" '"x_custom":"frame"'

# 27. HTTP/2 over TLS (ALPN h2) + graceful fallback to HTTP/1.1
out=$("$PURL" -s --http2 --cacert "$FIXTURES/localhost-ca.crt" -w ' [v=%{http_version}]' "https://localhost:$H2_PORT/hello")
assert_eq "HTTP/2 over TLS (ALPN h2)" "$out" "hello from purl [v=2]"

out=$("$PURL" -s --http2 --cacert "$FIXTURES/localhost-ca.crt" -w ' [v=%{http_version}]' "https://localhost:$TLS_PORT/hello")
assert_eq "HTTP/2 ALPN fallback to HTTP/1.1" "$out" "hello from purl [v=1.1]"

# 28. Cross-origin redirect strips Authorization on -L, preserves on --location-trusted
out=$("$PURL" -s -L -u "alice:secret" "$BASE/redirect-cross?port=$HTTP_PORT")
assert_contains "-L strips Authorization cross-origin" "$out" '"authorization":""'

out=$("$PURL" -s --location-trusted -u "alice:secret" "$BASE/redirect-cross?port=$HTTP_PORT")
assert_contains "--location-trusted preserves Authorization" "$out" '"authorization":"Basic YWxpY2U6c2VjcmV0"'

# 29. Embedded URL userinfo, empty POST, DELETE/PATCH, 204 No Content, -w %header{} & %{json}
out=$("$PURL" -s "http://alice:secret@127.0.0.1:$HTTP_PORT/echo")
assert_contains "URL userinfo basic auth" "$out" '"authorization":"Basic YWxpY2U6c2VjcmV0"'

out=$("$PURL" -s -X DELETE "$BASE/echo")
assert_contains "-X DELETE" "$out" '"method":"DELETE"'

out=$("$PURL" -s -X PATCH -d "p=1" "$BASE/echo")
assert_contains "-X PATCH" "$out" '"method":"PATCH"'

out=$("$PURL" -s -w '%{http_code}' "$BASE/status/204")
assert_eq "204 No Content" "$out" "204"

out=$("$PURL" -s -o "$TMPDIR/wo.txt" -w '%{filename_effective}|%header{content-type}' "$BASE/hello")
assert_eq "-w filename_effective & %header{}" "$out" "$TMPDIR/wo.txt|text/plain"

out=$("$PURL" -s -o /dev/null -w '%{json}' "$BASE/hello")
assert_contains "-w %{json}" "$out" '"http_code":200'

# 30. HTTP/2 h2c HEAD (-I) and -v -k over HTTPS
out=$("$PURL" -s --http2-prior-knowledge -I "http://127.0.0.1:$H2C_PORT/hello")
assert_contains "HTTP/2 h2c HEAD" "$out" "HTTP/2 200"

"$PURL" -v -k "https://localhost:$TLS_PORT/hello" > /dev/null 2>"$TMPDIR/vk.err"
assert_contains "HTTPS -v -k handshake" "$(cat "$TMPDIR/vk.err")" "TLS connection using"

# 31. --noproxy bypasses explicit -x proxy
out=$("$PURL" -s -x "http://127.0.0.1:$PROXY_PORT" --noproxy "127.0.0.1" "$BASE/hello")
assert_eq "--noproxy bypasses -x" "$out" "hello from purl"

# 32. Standard curl error exit codes (1, 3, 7, 23, 26)
set +e
"$PURL" -s "ftp://example.com/file" > /dev/null 2>&1
rc1=$?
"$PURL" -s "http://example.com:99999/x" > /dev/null 2>&1
rc3=$?
"$PURL" -s "http://127.0.0.1:18999/nope" > /dev/null 2>&1
rc7=$?
"$PURL" -s -o "$TMPDIR/no_such_dir/out.txt" "$BASE/hello" > /dev/null 2>&1
rc23=$?
"$PURL" -s -d "@$TMPDIR/does_not_exist.txt" "$BASE/echo" > /dev/null 2>&1
rc26=$?
set -e
assert_eq "unsupported protocol exit 1" "$rc1" "1"
assert_eq "malformed URL exit 3" "$rc3" "3"
assert_eq "connection refused exit 7" "$rc7" "7"
assert_eq "unwritable output exit 23" "$rc23" "23"
assert_eq "missing @file exit 26" "$rc26" "26"

# 33. Streaming -o download (64 KiB) + --max-filesize exit 63
"$PURL" -s -o "$TMPDIR/big.bin" "$BASE/big"
big_sz=$(wc -c < "$TMPDIR/big.bin" | tr -d ' ')
assert_eq "streaming -o 64KiB download" "$big_sz" "65536"

set +e
"$PURL" -s --max-filesize 10k -o "$TMPDIR/toobig.bin" "$BASE/big" 2>/dev/null
rc63=$?
set -e
assert_eq "--max-filesize exit 63" "$rc63" "63"
if [ ! -e "$TMPDIR/toobig.bin" ]; then
    pass "--max-filesize removes partial file"
else
    fail "--max-filesize removes partial file" "partial file was left behind"
fi

# 34. URL globbing ({a,b,c}, [1-3]) with #1 output capture substitution + -g globoff
"$PURL" -s -o "$TMPDIR/glob_#1.txt" "$BASE/item/{alpha,beta,gamma}"
assert_eq "glob #1 alpha" "$(cat "$TMPDIR/glob_alpha.txt")" "item=alpha"
assert_eq "glob #1 beta" "$(cat "$TMPDIR/glob_beta.txt")" "item=beta"
assert_eq "glob #1 gamma" "$(cat "$TMPDIR/glob_gamma.txt")" "item=gamma"

out=$("$PURL" -s -g "$BASE/echo?raw={a,b}")
assert_contains "-g globoff keeps braces" "$out" '"query":"raw={a,b}"'

# 35. Parallel transfers (-Z / --parallel) over URL glob range
"$PURL" -s -Z --parallel-max 4 -o "$TMPDIR/par_#1.txt" "$BASE/item/[01-04]"
assert_eq "-Z parallel item 01" "$(cat "$TMPDIR/par_01.txt")" "item=01"
assert_eq "-Z parallel item 02" "$(cat "$TMPDIR/par_02.txt")" "item=02"
assert_eq "-Z parallel item 03" "$(cat "$TMPDIR/par_03.txt")" "item=03"
assert_eq "-Z parallel item 04" "$(cat "$TMPDIR/par_04.txt")" "item=04"

# 36. --resolve (HTTP) and --connect-to (HTTPS with real SNI + cert verification!)
out=$("$PURL" -s --resolve "virtual.local:$HTTP_PORT:127.0.0.1" "http://virtual.local:$HTTP_PORT/echo")
assert_contains "--resolve HTTP preserves Host" "$out" "\"host\":\"virtual.local:$HTTP_PORT\""

out=$("$PURL" -s --connect-to "localhost:443:127.0.0.1:$TLS_PORT" --cacert "$FIXTURES/localhost-ca.crt" "https://localhost/hello")
assert_eq "--connect-to HTTPS with SNI verification" "$out" "hello from purl"

# 37. .netrc (--netrc-file)
cat > "$TMPDIR/test.netrc" <<EOF
machine 127.0.0.1 login alice password secret
EOF
out=$("$PURL" -s --netrc-file "$TMPDIR/test.netrc" "$BASE/echo")
assert_contains "--netrc-file Basic auth" "$out" '"authorization":"Basic YWxpY2U6c2VjcmV0"'

# 38. HTTP Digest Auth (--digest SHA-256 and MD5)
out=$("$PURL" -s --digest -u "alice:secret" "$BASE/digest-sha256")
assert_eq "--digest SHA-256" "$out" "digest-ok-SHA-256"

out=$("$PURL" -s --digest -u "alice:secret" "$BASE/digest-md5")
assert_eq "--digest MD5" "$out" "digest-ok-MD5"

# 39. AWS SigV4 (--aws-sigv4)
out=$("$PURL" -s --aws-sigv4 "aws:amz:us-east-1:s3" -u "AKIAEXAMPLE:secretkey123" "$BASE/echo")
assert_contains "--aws-sigv4 Authorization" "$out" 'AWS4-HMAC-SHA256 Credential=AKIAEXAMPLE/'
assert_contains "--aws-sigv4 scope" "$out" '/us-east-1/s3/aws4_request'
assert_contains "--aws-sigv4 X-Amz-Date" "$out" '"x_amz_date":"20'
assert_contains "--aws-sigv4 X-Amz-Content-Sha256" "$out" 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'

# 40. ETag caching (--etag-save / --etag-compare) and time-cond (-z)
out=$("$PURL" -s --etag-save "$TMPDIR/etag.txt" "$BASE/etag")
assert_eq "--etag-save body" "$out" "etag-body-v1"
assert_eq "--etag-save file" "$(cat "$TMPDIR/etag.txt")" '"v1-etag"'

code=$("$PURL" -s --etag-compare "$TMPDIR/etag.txt" -o /dev/null -w '%{http_code}' "$BASE/etag")
assert_eq "--etag-compare returns 304" "$code" "304"

printf 'ref' > "$TMPDIR/mtime.ref"
out=$("$PURL" -s -z "$TMPDIR/mtime.ref" "$BASE/echo")
assert_contains "-z file sends If-Modified-Since" "$out" '"if_modified_since":"'
assert_contains "-z file uses GMT date" "$out" 'GMT"'

out=$("$PURL" -s -z "-$TMPDIR/mtime.ref" "$BASE/echo")
assert_contains "-z -file sends If-Unmodified-Since" "$out" '"if_unmodified_since":"'

# 41. -w %{remote_ip} & %{time_namelookup}
out=$("$PURL" -s -o /dev/null -w '%{remote_ip}|%{time_namelookup}' "$BASE/hello")
assert_contains "-w remote_ip" "$out" "127.0.0.1|"

# 42. Single-handshake HTTP/2 ALPN fallback (only ONE TLS handshake in -v output)
out=$("$PURL" -v --http2 --cacert "$FIXTURES/localhost-ca.crt" "https://localhost:$TLS_PORT/hello" 2>"$TMPDIR/h2_fallback.err")
assert_eq "single-handshake HTTP/2 fallback body" "$out" "hello from purl"
hs_count=$(grep -c "TLS connection using" "$TMPDIR/h2_fallback.err" || true)
assert_eq "single-handshake HTTP/2 fallback count=1" "$hs_count" "1"

# 43. HTTP/2 over HTTPS with --connect-to SNI override
out=$("$PURL" -s --http2 --connect-to "localhost:443:127.0.0.1:$H2_PORT" --cacert "$FIXTURES/localhost-ca.crt" -w ' [v=%{http_version}]' "https://localhost/hello")
assert_eq "HTTP/2 --connect-to SNI override" "$out" "hello from purl [v=2]"

# 44. SOCKS5 proxy (--socks5, --socks5-hostname remote DNS, -x socks5h:// auth + HTTPS, and HTTP/2 over SOCKS5)
out=$("$PURL" -s --socks5 "127.0.0.1:$SOCKS_PORT" "$BASE/hello")
assert_eq "--socks5 HTTP" "$out" "hello from purl"

out=$("$PURL" -s --socks5-hostname "127.0.0.1:$SOCKS_PORT" "http://socks-remote.test:$HTTP_PORT/echo")
assert_contains "--socks5-hostname remote DNS" "$out" "\"host\":\"socks-remote.test:$HTTP_PORT\""

out=$("$PURL" -s -x "socks5h://alice:secret@127.0.0.1:$SOCKS_AUTH_PORT" --cacert "$FIXTURES/localhost-ca.crt" "https://localhost:$TLS_PORT/hello")
assert_eq "-x socks5h:// auth + HTTPS" "$out" "hello from purl"

out=$("$PURL" -s --socks5-hostname "127.0.0.1:$SOCKS_AUTH_PORT" -U "alice:secret" --http2 --cacert "$FIXTURES/localhost-ca.crt" -w ' [v=%{http_version}]' "https://localhost:$H2_PORT/hello")
assert_eq "--socks5-hostname -U auth + HTTP/2" "$out" "hello from purl [v=2]"

# 45. mTLS with PKCS#1 (RSA PRIVATE KEY) and SEC1 (EC PRIVATE KEY) PEM keys
if command -v openssl >/dev/null 2>&1; then
    openssl rsa -in "$FIXTURES/clientcert-rsa.key" -traditional -out "$TMPDIR/client-rsa-pkcs1.key" 2>/dev/null
    openssl ec -in "$FIXTURES/clientcert-ecdsa.key" -out "$TMPDIR/client-ec-sec1.key" 2>/dev/null
    out=$("$PURL" -s --cacert "$FIXTURES/localhost-ca.crt" --cert "$FIXTURES/clientcert-rsa.crt" --key "$TMPDIR/client-rsa-pkcs1.key" "https://localhost:$MTLS_PORT/hello")
    assert_eq "mTLS PKCS#1 RSA PRIVATE KEY" "$out" "hello from purl"
    out=$("$PURL" -s --cacert "$FIXTURES/localhost-ca.crt" --cert "$FIXTURES/clientcert-ecdsa.crt" --key "$TMPDIR/client-ec-sec1.key" "https://localhost:$MTLS_PORT/hello")
    assert_eq "mTLS SEC1 EC PRIVATE KEY" "$out" "hello from purl"
fi

printf '\nIntegration summary: %d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
