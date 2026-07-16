// the rust (tonic) grpc benchmark client: warm up, then time many unary echo
// calls, sequentially or across N concurrent workers over one connection.
// prints a one-line result the runner collects.

use std::time::{Duration, Instant};

use tonic::transport::{Certificate, Channel, ClientTlsConfig};

pub mod echobench {
    tonic::include_proto!("echobench");
}
use echobench::echo_client::EchoClient;
use echobench::EchoRequest;

#[derive(Clone)]
struct Args {
    addr: String,
    ca: String,
    authority: String,
    size: usize,
    calls: usize,
    warmup: usize,
    conc: usize,
}

fn parse_args() -> Args {
    let mut a = Args {
        addr: "https://127.0.0.1:50051".into(),
        ca: "certs/localhost-ca.crt".into(),
        authority: "localhost".into(),
        size: 16,
        calls: 20000,
        warmup: 2000,
        conc: 1,
    };
    let argv: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i + 1 < argv.len() {
        let v = argv[i + 1].clone();
        match argv[i].as_str() {
            "-addr" => a.addr = v,
            "-ca" => a.ca = v,
            "-authority" => a.authority = v,
            "-size" => a.size = v.parse().unwrap(),
            "-calls" => a.calls = v.parse().unwrap(),
            "-warmup" => a.warmup = v.parse().unwrap(),
            "-concurrency" => a.conc = v.parse().unwrap(),
            _ => {}
        }
        i += 2;
    }
    a
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a = parse_args();

    let ca = std::fs::read(&a.ca)?;
    let tls = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(ca))
        .domain_name(a.authority.clone());
    let channel = Channel::from_shared(a.addr.clone())?
        .tls_config(tls)?
        .connect()
        .await?;
    let client = EchoClient::new(channel);

    let payload = vec![0u8; a.size];

    // warmup
    {
        let mut c = client.clone();
        for _ in 0..a.warmup {
            c.unary(EchoRequest { payload: payload.clone() }).await?;
        }
    }

    let start = Instant::now();
    let mut latencies: Vec<Duration> = Vec::with_capacity(a.calls);

    if a.conc <= 1 {
        let mut c = client.clone();
        for _ in 0..a.calls {
            let t0 = Instant::now();
            c.unary(EchoRequest { payload: payload.clone() }).await?;
            latencies.push(t0.elapsed());
        }
    } else {
        let per = a.calls / a.conc;
        let mut handles = Vec::new();
        for _ in 0..a.conc {
            let mut c = client.clone();
            let p = payload.clone();
            handles.push(tokio::spawn(async move {
                let mut lat = Vec::with_capacity(per);
                for _ in 0..per {
                    let t0 = Instant::now();
                    c.unary(EchoRequest { payload: p.clone() }).await.unwrap();
                    lat.push(t0.elapsed());
                }
                lat
            }));
        }
        for h in handles {
            latencies.extend(h.await?);
        }
    }

    report("rust", a.size, a.conc, latencies.len(), start.elapsed(), &mut latencies);
    Ok(())
}

fn report(name: &str, size: usize, conc: usize, calls: usize, elapsed: Duration, lat: &mut [Duration]) {
    lat.sort();
    let median = lat[lat.len() / 2];
    let p99 = lat[(lat.len() * 99) / 100];
    let throughput = calls as f64 / elapsed.as_secs_f64();
    println!(
        "{:<6} size={:<6} conc={:<3} calls={}  median={:<8} p99={:<8}  {:.0} calls/sec",
        name,
        size,
        conc,
        calls,
        format!("{:?}", median),
        format!("{:?}", p99),
        throughput
    );
}
