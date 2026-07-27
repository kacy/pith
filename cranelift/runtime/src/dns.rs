//! hostname resolution that does not park a green worker.
//!
//! `getaddrinfo` is synchronous and there is no portable way to poll it, so a
//! lookup made from a green task holds the *worker* OS thread for its whole
//! duration — every other task pinned to that worker stalls with it. sockets do
//! not have this problem (a would-block yields to the epoll reactor in
//! `netpoll`), which leaves dns as one of the two blocking calls on the common
//! client path: every grpc, http, and database dial starts with one.
//!
//! the fix is the usual one for a syscall that cannot be polled: keep the
//! blocking call, but make some other thread do it. `blocking` owns that
//! machinery — the pool, the park, the wake — and this module supplies the
//! resolver pool and the work to run on it.
//!
//! dns gets its own pool rather than sharing one with file i/o so a slow
//! multi-megabyte write cannot queue in front of a dial.
//!
//! under the os-thread backend none of this applies — blocking is what a thread
//! is for, and the pool would only add a handoff — so `blocking::run` calls
//! `to_socket_addrs` inline there, exactly as the callers used to.

use crate::blocking::{self, Pool};
use std::net::{SocketAddr, ToSocketAddrs};

/// the resolver's own threads. four is enough that a burst of dials overlaps
/// without letting a hostile one spawn threads without bound.
static POOL: Pool = Pool::new("pith-dns", 4);

/// the addresses one lookup produced, in resolution order.
///
/// an empty list means "no address", and covers both a resolver error and a
/// lookup that succeeded with nothing in it. collapsing the two loses nothing:
/// every caller in this runtime already treats them the same (a failed connect,
/// a null resolve result), and it keeps the value that crosses between threads a
/// plain `Vec`.
type Answer = Vec<SocketAddr>;

/// resolve `host:port` to its addresses, returning an empty list on failure.
///
/// this is the only entry point; it picks the offloaded or the inline path
/// itself so callers do not have to care which backend they are on.
pub(crate) fn resolve(host: &str, port: i64) -> Answer {
    let target = format!("{}:{}", host, port);
    // a numeric address never reaches a resolver: `to_socket_addrs` parses it
    // in place, with no syscall to block in, so offloading would buy nothing
    // and cost a thread handoff. dialing a literal ip is common enough — tests,
    // benchmarks, a sidecar on 127.0.0.1 — to check for first.
    if let Ok(addr) = target.parse::<SocketAddr>() {
        return vec![addr];
    }
    blocking::run(&POOL, move || resolve_inline(&target))
}

/// the blocking lookup itself, byte-for-byte what the call sites used to do
/// inline. runs on the calling thread under the os-thread backend and on a pool
/// thread under green.
fn resolve_inline(target: &str) -> Answer {
    match target.to_socket_addrs() {
        Ok(addrs) => addrs.collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a hostname whose lookup fails without touching the network: the port is
    /// out of range, so `to_socket_addrs` rejects the target before it would
    /// query a resolver. keeps the failure tests fast and offline.
    const UNPARSEABLE_PORT: i64 = 99999;

    #[test]
    fn inline_resolve_finds_loopback() {
        let addrs = resolve_inline("localhost:80");
        assert!(!addrs.is_empty());
        assert!(addrs.iter().all(|a| a.ip().is_loopback() && a.port() == 80));
    }

    #[test]
    fn inline_resolve_reports_failure_as_no_addresses() {
        assert!(resolve_inline(&format!("localhost:{}", UNPARSEABLE_PORT)).is_empty());
    }

    #[test]
    fn resolve_outside_a_green_task_stays_inline() {
        // nothing here runs on a green worker, so `resolve` must take the
        // blocking path and still produce the same answer.
        assert!(!blocking::offloads());
        assert!(!resolve("localhost", 443).is_empty());
    }

    #[test]
    fn resolve_reports_failure_as_no_addresses() {
        assert!(resolve("localhost", UNPARSEABLE_PORT).is_empty());
    }

    #[test]
    fn literal_addresses_skip_the_resolver() {
        // both families, and the answer must match what `to_socket_addrs`
        // produces for the same string — the fast path is a shortcut, not a
        // second implementation.
        for (host, port) in [("127.0.0.1", 8080), ("[::1]", 443), ("::1", 443)] {
            let target = format!("{}:{}", host, port);
            assert_eq!(resolve(host, port), resolve_inline(&target), "{}", target);
        }
    }

    #[test]
    fn out_of_range_ports_still_fail() {
        // the literal fast path must not accept what `to_socket_addrs` rejects.
        assert!(resolve("127.0.0.1", UNPARSEABLE_PORT).is_empty());
    }
}
