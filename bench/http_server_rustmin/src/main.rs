// rust counterpart to bench/http_server_min.go: the same routes, body and
// response headers, served by a hand-rolled http/1.1 keep-alive loop on a
// tokio multi-thread runtime, one task per connection. it does what the go
// min server does and no more: read the request line, skip the headers to
// the blank line (noting content-length and connection), answer with a
// status line, content-type and content-length. no date header, no header
// map, no timeouts. it frames get requests only: a content-length body is
// skipped, a chunked body is not understood.
//
// worker threads come from TOKIO_WORKERS (the tokio default, one per cpu,
// when it is unset).
//
// build with: (cd bench/http_server_rustmin && cargo build --release)
// run with:   TOKIO_WORKERS=16 bench/http_server_rustmin/target/release/http_server_rustmin 8080
use std::env;
use std::io;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::{TcpListener, TcpStream};

const KINDS: [&str; 5] = ["widget", "gadget", "gizmo", "doodad", "contraption"];

// item_name and item_json follow itemName and itemJSON in
// bench/http_server_min.go; keep them in step.
fn item_name(id: i64) -> String {
    format!("{}-{}", KINDS[(id % KINDS.len() as i64) as usize], id)
}

fn item_json(id: i64) -> String {
    let name = item_name(id);
    let price = (id * 37) % 10000;
    let mut b = String::new();
    b.push_str(&format!("{{\"id\":{id}"));
    b.push_str(&format!(",\"name\":\"{name}\""));
    b.push_str(&format!(",\"price\":{price}"));
    b.push_str(",\"tags\":[");
    for (i, tag) in name.split('-').enumerate() {
        if i > 0 {
            b.push(',');
        }
        b.push('"');
        b.push_str(tag);
        b.push('"');
    }
    b.push_str("]}");
    b
}

// query_param returns the value of name in a raw query string, or "".
fn query_param<'a>(query: &'a str, name: &str) -> &'a str {
    for part in query.split('&') {
        if let Some((key, value)) = part.split_once('=') {
            if key == name {
                return value;
            }
        }
    }
    ""
}

// strip_prefix_fold returns the rest of line after prefix, ignoring ascii
// case, or None when line does not start with it.
fn strip_prefix_fold<'a>(line: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    if line.len() >= prefix.len() && line[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&line[prefix.len()..])
    } else {
        None
    }
}

fn trim_end_crlf(mut line: &[u8]) -> &[u8] {
    while let [rest @ .., b'\r' | b'\n'] = line {
        line = rest;
    }
    line
}

async fn write_response<W: AsyncWriteExt + Unpin>(
    w: &mut W,
    status: &str,
    content_type: &str,
    body: &str,
) -> io::Result<()> {
    w.write_all(b"HTTP/1.1 ").await?;
    w.write_all(status.as_bytes()).await?;
    w.write_all(b"\r\nContent-Type: ").await?;
    w.write_all(content_type.as_bytes()).await?;
    w.write_all(b"\r\nContent-Length: ").await?;
    w.write_all(body.len().to_string().as_bytes()).await?;
    w.write_all(b"\r\n\r\n").await?;
    w.write_all(body.as_bytes()).await
}

async fn route<W: AsyncWriteExt + Unpin>(w: &mut W, method: &str, target: &str) -> io::Result<()> {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    match (method, path) {
        ("GET", "/item") => {
            let id = query_param(query, "id").parse().unwrap_or(0);
            write_response(w, "200 OK", "application/json", &item_json(id)).await
        }
        ("GET", "/health") => {
            write_response(w, "200 OK", "application/json", r#"{"ok":true}"#).await
        }
        _ => write_response(w, "404 Not Found", "text/plain", "404 page not found\n").await,
    }
}

async fn serve(conn: TcpStream) -> io::Result<()> {
    let (rd, wr) = conn.into_split();
    let mut r = BufReader::with_capacity(4096, rd);
    let mut w = BufWriter::with_capacity(4096, wr);
    let mut line = Vec::with_capacity(256);
    loop {
        line.clear();
        if r.read_until(b'\n', &mut line).await? == 0 {
            return Ok(());
        }
        // the request line is "method target version"
        let mut fields = line
            .split(|b| b.is_ascii_whitespace())
            .filter(|f| !f.is_empty());
        let (Some(method), Some(target)) = (fields.next(), fields.next()) else {
            return Ok(());
        };
        let method = String::from_utf8_lossy(method).into_owned();
        let target = String::from_utf8_lossy(target).into_owned();
        let mut close_after = fields.next() == Some(b"HTTP/1.0");
        let mut body_len = 0u64;
        loop {
            line.clear();
            if r.read_until(b'\n', &mut line).await? == 0 {
                return Ok(());
            }
            let h = trim_end_crlf(&line);
            if h.is_empty() {
                break;
            }
            if let Some(v) = strip_prefix_fold(h, b"content-length:") {
                body_len = std::str::from_utf8(v.trim_ascii())
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
            } else if let Some(v) = strip_prefix_fold(h, b"connection:") {
                let v = v.trim_ascii();
                close_after = v.eq_ignore_ascii_case(b"close")
                    || (close_after && !v.eq_ignore_ascii_case(b"keep-alive"));
            }
        }
        if body_len > 0 {
            let skipped =
                tokio::io::copy(&mut (&mut r).take(body_len), &mut tokio::io::sink()).await?;
            if skipped < body_len {
                return Ok(());
            }
        }
        route(&mut w, &method, &target).await?;
        w.flush().await?;
        if close_after {
            return Ok(());
        }
    }
}

fn main() {
    let port = env::args().nth(1).unwrap_or_else(|| "8080".to_string());
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    if let Some(n) = env::var("TOKIO_WORKERS").ok().and_then(|v| v.parse().ok()) {
        builder.worker_threads(n);
    }
    let rt = builder.enable_io().build().expect("tokio runtime");
    rt.block_on(async {
        let ln = match TcpListener::bind(format!("0.0.0.0:{port}")).await {
            Ok(ln) => ln,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        };
        println!("listening on {port}");
        loop {
            let Ok((conn, _)) = ln.accept().await else {
                continue;
            };
            // go's net package turns nagle off on every tcp conn; match it
            let _ = conn.set_nodelay(true);
            tokio::spawn(serve(conn));
        }
    });
}
