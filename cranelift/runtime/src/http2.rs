// http2.rs — a synchronous facade over the async `h2` client.
//
// pith is synchronous and handle-based; `h2` is async (tokio/futures). one
// shared multi-thread tokio runtime drives h2, and each pith call blocks on it.
// a response is a heap object behind an i64 handle with a magic word, like the
// other runtime handle types.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

const H2_RESP_MAGIC: u32 = 0x48325250; // "H2RP"

struct PithH2Response {
    status: u16,
    body: Vec<u8>,
    magic: u32,
}

static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

fn rt() -> &'static tokio::runtime::Runtime {
    RT.get_or_init(|| {
        // install the ring crypto provider once for rustls 0.23.
        let _ = rustls::crypto::ring::default_provider().install_default();
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("h2 tokio runtime")
    })
}

// the last error, for debugging from the test harness.
static LAST_ERR: OnceLock<std::sync::Mutex<String>> = OnceLock::new();
fn set_err(e: String) {
    let m = LAST_ERR.get_or_init(|| std::sync::Mutex::new(String::new()));
    if let Ok(mut g) = m.lock() {
        *g = e;
    }
}

async fn h2_get(url: &str) -> Result<(u16, Vec<u8>), String> {
    use tokio::net::TcpStream;
    use tokio_rustls::TlsConnector;

    let rest = url.strip_prefix("https://").ok_or("only https:// is supported")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().map_err(|_| "bad port")?),
        None => (authority.to_string(), 443u16),
    };

    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec()];
    let connector = TlsConnector::from(Arc::new(config));

    let tcp = TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|e| e.to_string())?;
    let server_name = rustls::pki_types::ServerName::try_from(host.clone()).map_err(|e| e.to_string())?;
    let tls = connector.connect(server_name, tcp).await.map_err(|e| e.to_string())?;

    if tls.get_ref().1.alpn_protocol() != Some(b"h2") {
        return Err("server did not negotiate h2 over alpn".into());
    }

    let (mut client, connection) = h2::client::handshake(tls).await.map_err(|e| e.to_string())?;
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let req = http::Request::builder()
        .method("GET")
        .uri(format!("https://{}{}", authority, path))
        .body(())
        .map_err(|e| e.to_string())?;
    let (response, _send) = client.send_request(req, true).map_err(|e| e.to_string())?;
    let response = response.await.map_err(|e| e.to_string())?;
    let status = response.status().as_u16();

    let mut body = response.into_body();
    let mut buf = Vec::new();
    while let Some(chunk) = body.data().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        let n = chunk.len();
        buf.extend_from_slice(&chunk);
        let _ = body.flow_control().release_capacity(n);
    }
    Ok((status, buf))
}

/// GET an https url over http/2. Returns a response handle, or 0 on error.
///
/// # Safety
/// `url_ptr` must be a valid NUL-terminated string for this call.
#[no_mangle]
pub unsafe extern "C" fn pith_h2_get(url_ptr: *const i8) -> i64 {
    let len = crate::string::pith_cstring_len(url_ptr) as usize;
    let url = String::from_utf8_lossy(std::slice::from_raw_parts(url_ptr as *const u8, len)).into_owned();
    match rt().block_on(h2_get(&url)) {
        Ok((status, body)) => {
            let boxed = Box::new(PithH2Response {
                status,
                body,
                magic: H2_RESP_MAGIC,
            });
            Box::into_raw(boxed) as i64
        }
        Err(e) => {
            set_err(e);
            0
        }
    }
}

unsafe fn resp_ref<'a>(handle: i64) -> Option<&'a PithH2Response> {
    if handle == 0 {
        return None;
    }
    let r = &*(handle as *const PithH2Response);
    if r.magic != H2_RESP_MAGIC {
        return None;
    }
    Some(r)
}

/// The http status of a response handle (0 if invalid).
#[no_mangle]
pub unsafe extern "C" fn pith_h2_response_status(handle: i64) -> i64 {
    resp_ref(handle).map(|r| r.status as i64).unwrap_or(0)
}

/// The response body as a pith bytes handle (empty bytes if invalid).
#[no_mangle]
pub unsafe extern "C" fn pith_h2_response_body(handle: i64) -> i64 {
    match resp_ref(handle) {
        Some(r) => crate::bytes::pith_bytes_from_vec(r.body.clone()),
        None => crate::bytes::pith_bytes_from_vec(Vec::new()),
    }
}

/// Free a response handle.
///
/// # Safety
/// `handle` must be a valid response handle from `pith_h2_get`, or 0.
#[no_mangle]
pub unsafe extern "C" fn pith_h2_response_free(handle: i64) {
    if resp_ref(handle).is_some() {
        drop(Box::from_raw(handle as *mut PithH2Response));
    }
}

// keep the import used even if the atomic helper is trimmed later
#[allow(dead_code)]
fn _touch(a: &AtomicU32) -> u32 {
    a.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "hits the network; run manually with --ignored"]
    fn h2_get_cloudflare_returns_a_status_and_body() {
        let out = rt().block_on(h2_get("https://www.cloudflare.com/"));
        match out {
            Ok((status, body)) => {
                eprintln!("h2 GET cloudflare: status={} body={} bytes", status, body.len());
                assert!(status > 0);
                assert!(!body.is_empty());
            }
            Err(e) => panic!("h2 GET failed: {}", e),
        }
    }
}
