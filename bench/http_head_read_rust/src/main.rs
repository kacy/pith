// rust counterpart to bench/http_head_read.pith: http request-head reading,
// isolated from the socket. the same stream of keepalive GETs sits in memory
// and is read through a 4096-byte buffer (the pith arm chunks its reader the
// same way). httparse parses each head out of that buffer, and the reader
// turns it into an owned request (head bytes, method, target, headers, a
// content-length body), which is what the pith reader hands back too. the
// checksum folds the head length and the method of every request, as the
// pith arm does.
//
// build with: (cd bench/http_head_read_rust && cargo build --release)
// run with:   bench/http_head_read_rust/target/release/http_head_read_rust <requests>
use std::env;
use std::io::{self, Read};
use std::time::Instant;

const BUF_SIZE: usize = 4096;

#[allow(dead_code)]
struct Request {
    head: Vec<u8>,
    method: String,
    target: String,
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
}

struct RequestReader<R> {
    src: R,
    buf: Vec<u8>,
    start: usize,
    end: usize,
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

impl<R: Read> RequestReader<R> {
    fn new(src: R) -> Self {
        RequestReader {
            src,
            buf: vec![0; BUF_SIZE],
            start: 0,
            end: 0,
        }
    }

    // move the unread bytes to the front and read more after them; false at
    // the end of the source
    fn fill(&mut self) -> io::Result<bool> {
        self.buf.copy_within(self.start..self.end, 0);
        self.end -= self.start;
        self.start = 0;
        if self.end == self.buf.len() {
            return Err(invalid("request head too large"));
        }
        let n = self.src.read(&mut self.buf[self.end..])?;
        self.end += n;
        Ok(n > 0)
    }

    fn read_exact(&mut self, len: usize) -> io::Result<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            if self.start == self.end && !self.fill()? {
                return Err(invalid("truncated request body"));
            }
            let take = (len - out.len()).min(self.end - self.start);
            out.extend_from_slice(&self.buf[self.start..self.start + take]);
            self.start += take;
        }
        Ok(out)
    }

    // the next request, or None at a clean end of the stream
    fn read_request(&mut self) -> io::Result<Option<Request>> {
        loop {
            let mut headers = [httparse::EMPTY_HEADER; 32];
            let mut parsed = httparse::Request::new(&mut headers);
            let window = &self.buf[self.start..self.end];
            match parsed.parse(window).map_err(|e| invalid(&e.to_string()))? {
                httparse::Status::Complete(n) => {
                    let mut content_length = 0;
                    let mut owned = Vec::with_capacity(parsed.headers.len());
                    for h in parsed.headers.iter() {
                        if h.name.eq_ignore_ascii_case("content-length") {
                            content_length = std::str::from_utf8(h.value)
                                .ok()
                                .and_then(|v| v.trim().parse().ok())
                                .ok_or_else(|| invalid("bad content-length"))?;
                        }
                        owned.push((h.name.to_string(), h.value.to_vec()));
                    }
                    let mut req = Request {
                        head: window[..n].to_vec(),
                        method: parsed.method.unwrap_or("").to_string(),
                        target: parsed.path.unwrap_or("").to_string(),
                        headers: owned,
                        body: Vec::new(),
                    };
                    self.start += n;
                    if content_length > 0 {
                        req.body = self.read_exact(content_length)?;
                    }
                    return Ok(Some(req));
                }
                httparse::Status::Partial => {
                    if !self.fill()? {
                        if self.start == self.end {
                            return Ok(None);
                        }
                        return Err(invalid("truncated request head"));
                    }
                }
            }
        }
    }
}

fn requests_from_args() -> usize {
    env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(20000)
}

fn build_stream(count: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..count {
        // the id varies so no two heads are byte-identical
        out.extend_from_slice(format!("GET /item?id={} HTTP/1.1\r\n", 12345 + i % 1000).as_bytes());
        out.extend_from_slice(b"Host: x\r\n");
        out.extend_from_slice(b"Connection: keep-alive\r\n");
        out.extend_from_slice(b"\r\n");
    }
    out
}

fn main() -> io::Result<()> {
    let count = requests_from_args();
    let stream = build_stream(count);
    let mut reader = RequestReader::new(&stream[..]);
    let start = Instant::now();
    let mut checksum = 0;
    let mut read = 0;
    while read < count {
        let Some(req) = reader.read_request()? else {
            break;
        };
        checksum += req.head.len() + req.method.len();
        read += 1;
    }
    let elapsed = start.elapsed().as_millis();
    println!("http head read benchmark");
    println!("requests={read}");
    println!("elapsed_ms={elapsed}");
    println!("checksum={checksum}");
    Ok(())
}
