use crate::bytes::{forge_bytes_from_vec, forge_bytes_ref};
use crate::ffi_util::cstr_to_str;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use std::fs::File;
use std::io::{BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

struct ForgeTlsConfig {
    server: Arc<ServerConfig>,
}

enum ForgeTlsConn {
    Server(StreamOwned<ServerConnection, TcpStream>),
}

struct ForgeTlsListener {
    listener: TcpListener,
    config: Arc<ServerConfig>,
}

unsafe fn cstr_to_string(ptr: *const i8, fallback: &str) -> String {
    let value = cstr_to_str(ptr);
    if value.is_empty() {
        return fallback.to_string();
    }
    value.to_string()
}

unsafe fn tls_config_ref<'a>(handle: i64) -> Option<&'a ForgeTlsConfig> {
    if handle <= 0 {
        return None;
    }
    Some(&*(handle as *const ForgeTlsConfig))
}

unsafe fn tls_conn_mut<'a>(handle: i64) -> Option<&'a mut ForgeTlsConn> {
    if handle <= 0 {
        return None;
    }
    Some(&mut *(handle as *mut ForgeTlsConn))
}

unsafe fn tls_listener_mut<'a>(handle: i64) -> Option<&'a mut ForgeTlsListener> {
    if handle <= 0 {
        return None;
    }
    Some(&mut *(handle as *mut ForgeTlsListener))
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn load_cert_chain(path: &str) -> Option<Vec<CertificateDer<'static>>> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .ok()
}

fn load_private_key(path: &str) -> Option<PrivateKeyDer<'static>> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::private_key(&mut reader).ok().flatten()
}

fn server_config_arc(handle: i64) -> Option<Arc<ServerConfig>> {
    Some(unsafe { tls_config_ref(handle)? }.server.clone())
}

fn box_config(config: ForgeTlsConfig) -> i64 {
    Box::into_raw(Box::new(config)) as i64
}

fn box_conn(conn: ForgeTlsConn) -> i64 {
    Box::into_raw(Box::new(conn)) as i64
}

#[no_mangle]
pub unsafe extern "C" fn forge_tls_server_config(cert_path: *const i8, key_path: *const i8) -> i64 {
    let cert_path = cstr_to_string(cert_path, "");
    let key_path = cstr_to_string(key_path, "");
    let Some(certs) = load_cert_chain(&cert_path) else {
        return 0;
    };
    let Some(key) = load_private_key(&key_path) else {
        return 0;
    };
    let Ok(config) = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .and_then(|builder| builder.with_no_client_auth().with_single_cert(certs, key))
    else {
        return 0;
    };
    box_config(ForgeTlsConfig {
        server: Arc::new(config),
    })
}

#[no_mangle]
pub unsafe extern "C" fn forge_tls_listen(host: *const i8, port: i64, config_handle: i64) -> i64 {
    let host = cstr_to_string(host, "0.0.0.0");
    let Some(config) = server_config_arc(config_handle) else {
        return 0;
    };
    let Ok(listener) = TcpListener::bind(format!("{host}:{port}")) else {
        return 0;
    };
    Box::into_raw(Box::new(ForgeTlsListener { listener, config })) as i64
}

#[no_mangle]
pub extern "C" fn forge_tls_accept(listener_handle: i64) -> i64 {
    let Some(listener) = (unsafe { tls_listener_mut(listener_handle) }) else {
        return 0;
    };
    let Ok((stream, _addr)) = listener.listener.accept() else {
        return 0;
    };
    let Ok(conn) = ServerConnection::new(listener.config.clone()) else {
        return 0;
    };
    let mut stream = StreamOwned::new(conn, stream);
    if stream.conn.complete_io(&mut stream.sock).is_err() {
        return 0;
    }
    box_conn(ForgeTlsConn::Server(stream))
}

#[no_mangle]
pub extern "C" fn forge_tls_read_bytes(conn_handle: i64, max_bytes: i64) -> i64 {
    let Some(conn) = (unsafe { tls_conn_mut(conn_handle) }) else {
        return 0;
    };
    let size = if max_bytes > 0 {
        max_bytes as usize
    } else {
        4096
    };
    let mut buf = vec![0_u8; size];
    let read = match conn {
        ForgeTlsConn::Server(stream) => stream.read(&mut buf),
    };
    match read {
        Ok(n) => {
            buf.truncate(n);
            forge_bytes_from_vec(buf)
        }
        Err(err)
            if matches!(
                err.kind(),
                ErrorKind::UnexpectedEof
                    | ErrorKind::ConnectionAborted
                    | ErrorKind::ConnectionReset
            ) =>
        {
            forge_bytes_from_vec(Vec::new())
        }
        Err(_) => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn forge_tls_write_bytes(conn_handle: i64, data: i64) -> i64 {
    let Some(bytes) = forge_bytes_ref(data) else {
        return 0;
    };
    let Some(conn) = tls_conn_mut(conn_handle) else {
        return 0;
    };
    let write = match conn {
        ForgeTlsConn::Server(stream) => stream.write(&bytes.data),
    };
    match write {
        Ok(n) => {
            let _ = match conn {
                ForgeTlsConn::Server(stream) => stream.flush(),
            };
            n as i64
        }
        Err(_) => 0,
    }
}

#[no_mangle]
pub extern "C" fn forge_tls_close(conn_handle: i64) {
    if conn_handle <= 0 {
        return;
    }
    let mut conn = unsafe { Box::from_raw(conn_handle as *mut ForgeTlsConn) };
    match conn.as_mut() {
        ForgeTlsConn::Server(stream) => {
            stream.conn.send_close_notify();
            let _ = stream.flush();
        }
    }
}

#[no_mangle]
pub extern "C" fn forge_tls_listener_close(listener_handle: i64) {
    if listener_handle <= 0 {
        return;
    }
    let _ = unsafe { Box::from_raw(listener_handle as *mut ForgeTlsListener) };
}

#[no_mangle]
pub extern "C" fn forge_tls_config_close(config_handle: i64) {
    if config_handle <= 0 {
        return;
    }
    let _ = unsafe { Box::from_raw(config_handle as *mut ForgeTlsConfig) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_path(name: &str) -> String {
        format!(
            "{}/../../tests/live/fixtures/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        )
    }

    #[test]
    fn tls_fixture_configs_load() {
        let cert_path = fixture_path("localhost.crt");
        let key_path = fixture_path("localhost.key");

        let certs = load_cert_chain(&cert_path).expect("cert fixture should load");
        let key = load_private_key(&key_path).expect("key fixture should load");
        ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .expect("protocol versions should be available")
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .expect("server config should build");
    }
}
