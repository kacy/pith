// A minimal TLS peer used to interop-test the pith TLS stack against rustls.
// It acts as either a server (accept one connection, echo one message) or a
// client (connect, send "hi", print the negotiated version and cipher, read
// the echo). Versions are pinned so the test can drive 1.2 and 1.3 explicitly.
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::exit;
use std::sync::Arc;

static TLS12_ONLY: &[&rustls::SupportedProtocolVersion] = &[&rustls::version::TLS12];
static TLS13_ONLY: &[&rustls::SupportedProtocolVersion] = &[&rustls::version::TLS13];

fn versions(s: &str) -> &'static [&'static rustls::SupportedProtocolVersion] {
    match s {
        "1.2" => TLS12_ONLY,
        "1.3" => TLS13_ONLY,
        _ => rustls::ALL_VERSIONS,
    }
}

fn ver_name(v: Option<rustls::ProtocolVersion>) -> &'static str {
    match v {
        Some(rustls::ProtocolVersion::TLSv1_2) => "tls1.2",
        Some(rustls::ProtocolVersion::TLSv1_3) => "tls1.3",
        _ => "unknown",
    }
}

fn main() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("provider");
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("usage: rustls_peer server|client ...");
        exit(2);
    }
    match args[1].as_str() {
        "server" => {
            // server <port> <cert> <key> <minver> <maxver> (min==max, pinned)
            let certs: Vec<_> = rustls_pemfile::certs(&mut BufReader::new(
                File::open(&args[3]).expect("cert"),
            ))
            .collect::<Result<_, _>>()
            .expect("cert-parse");
            let key = rustls_pemfile::private_key(&mut BufReader::new(
                File::open(&args[4]).expect("key"),
            ))
            .expect("key-read")
            .expect("key-parse");
            let cfg = rustls::ServerConfig::builder_with_protocol_versions(versions(&args[5]))
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .expect("server-config");
            let ln = TcpListener::bind(("127.0.0.1", args[2].parse::<u16>().expect("port")))
                .expect("bind");
            let (sock, _) = ln.accept().expect("accept");
            let conn = rustls::ServerConnection::new(Arc::new(cfg)).expect("conn");
            let mut tls = rustls::StreamOwned::new(conn, sock);
            let mut buf = [0u8; 64];
            match tls.read(&mut buf) {
                Ok(n) => {
                    let reply = [b"echo:", &buf[..n]].concat();
                    let _ = tls.write_all(&reply);
                }
                Err(e) => {
                    println!("server-read-error: {e}");
                    exit(1);
                }
            }
        }
        "client" => {
            // client <host:port> <cafile> <servername> <minver> <maxver>
            let mut roots = rustls::RootCertStore::empty();
            for cert in
                rustls_pemfile::certs(&mut BufReader::new(File::open(&args[3]).expect("ca")))
            {
                roots.add(cert.expect("ca-parse")).expect("ca-add");
            }
            let cfg = rustls::ClientConfig::builder_with_protocol_versions(versions(&args[5]))
                .with_root_certificates(roots)
                .with_no_client_auth();
            let name = rustls::pki_types::ServerName::try_from(args[4].clone()).expect("name");
            let conn = rustls::ClientConnection::new(Arc::new(cfg), name).expect("conn");
            let sock = TcpStream::connect(&args[2]).expect("connect");
            let mut tls = rustls::StreamOwned::new(conn, sock);
            if let Err(e) = tls.write_all(b"hi") {
                println!("dial-error: {e}");
                exit(1);
            }
            let mut buf = [0u8; 64];
            let n = match tls.read(&mut buf) {
                Ok(n) => n,
                Err(e) => {
                    println!("read-error: {e}");
                    exit(1);
                }
            };
            let suite = tls
                .conn
                .negotiated_cipher_suite()
                .map(|s| format!("{:?}", s.suite()))
                .unwrap_or_else(|| "unknown".into());
            println!(
                "{} {} {}",
                ver_name(tls.conn.protocol_version()),
                suite,
                String::from_utf8_lossy(&buf[..n])
            );
        }
        _ => {
            println!("unknown mode");
            exit(2);
        }
    }
}
