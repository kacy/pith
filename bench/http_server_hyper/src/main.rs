// hyper counterpart to bench/http_server.go: the same routes and body,
// served by hyper 1.x (http/1.1 only, as net/http is over plain tcp) on a
// tokio multi-thread runtime, one task per connection. like net/http it
// parses the whole query string, sends date, content-type and
// content-length, and answers an unknown path with net/http's 404.
//
// worker threads come from TOKIO_WORKERS (the tokio default, one per cpu,
// when it is unset).
//
// build with: (cd bench/http_server_hyper && cargo build --release)
// run with:   TOKIO_WORKERS=16 bench/http_server_hyper/target/release/http_server_hyper 8080
use std::convert::Infallible;
use std::env;

use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::header::{HeaderValue, CONTENT_TYPE, X_CONTENT_TYPE_OPTIONS};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

const KINDS: [&str; 5] = ["widget", "gadget", "gizmo", "doodad", "contraption"];

// item_name and item_json follow itemName and itemJSON in
// bench/http_server.go; keep them in step.
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

fn json(body: String) -> Response<Full<Bytes>> {
    let mut resp = Response::new(Full::new(Bytes::from(body)));
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    resp
}

// net/http's http.NotFound
fn not_found() -> Response<Full<Bytes>> {
    let mut resp = Response::new(Full::new(Bytes::from_static(b"404 page not found\n")));
    *resp.status_mut() = StatusCode::NOT_FOUND;
    let h = resp.headers_mut();
    h.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    h.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    resp
}

async fn handle(req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(match req.uri().path() {
        "/item" => {
            let query = req.uri().query().unwrap_or("").as_bytes();
            let id = form_urlencoded::parse(query)
                .find(|(k, _)| k == "id")
                .and_then(|(_, v)| v.parse().ok())
                .unwrap_or(0);
            json(item_json(id))
        }
        "/health" => json(r#"{"ok":true}"#.to_string()),
        _ => not_found(),
    })
}

fn main() {
    let port = env::args().nth(1).unwrap_or_else(|| "8080".to_string());
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    if let Some(n) = env::var("TOKIO_WORKERS").ok().and_then(|v| v.parse().ok()) {
        builder.worker_threads(n);
    }
    let rt = builder.enable_all().build().expect("tokio runtime");
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
            tokio::spawn(async move {
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(conn), service_fn(handle))
                    .await;
            });
        }
    });
}
