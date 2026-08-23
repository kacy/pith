#!/usr/bin/env python3
# sequential single-connection http latency probe.
#
#   bench/http_seq_latency.py <port> [requests] [path]
#
# opens ONE keepalive connection and times each request round trip, reporting
# p50/p90/p99/max. this is the primary http metric on a small shared box:
# wrk throughput there is launch-cadence noise (repeated runs read progressively
# lower as the host's cpu burst decays), while this probe is deterministic to a
# few microseconds and pins down per-request cost directly. it is also immune
# to the serving model — it measured the serial-accept-loop bug (#902) as
# "throughput equals 1/p50" and settled the 2026-08 false throughput regression
# in two runs where a bisect was being planned.
#
# python on purpose: the probe must be neutral instrumentation, not a pith
# client benchmarking a pith server.
import socket, sys, time, statistics

def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    port = int(sys.argv[1])
    n = int(sys.argv[2]) if len(sys.argv) > 2 else 3000
    path = sys.argv[3] if len(sys.argv) > 3 else "/item?id=12345"
    s = socket.create_connection(("127.0.0.1", port))
    s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    req = ("GET %s HTTP/1.1\r\nHost: x\r\nConnection: keep-alive\r\n\r\n" % path).encode()
    lat, buf = [], b""
    for i in range(n):
        t0 = time.perf_counter_ns()
        s.sendall(req)
        while True:
            if b"\r\n\r\n" in buf:
                head, rest = buf.split(b"\r\n\r\n", 1)
                cl = 0
                for line in head.split(b"\r\n"):
                    if line.lower().startswith(b"content-length:"):
                        cl = int(line.split(b":")[1])
                if len(rest) >= cl:
                    buf = rest[cl:]
                    break
            chunk = s.recv(65536)
            if not chunk:
                print("connection closed early at request %d" % i)
                return 1
            buf += chunk
        lat.append((time.perf_counter_ns() - t0) / 1000)
    lat.sort()
    print("n=%d p50=%.0fus p90=%.0fus p99=%.0fus max=%.0fus"
          % (n, statistics.median(lat), lat[int(n * 0.9)], lat[int(n * 0.99)], lat[-1]))
    return 0

if __name__ == "__main__":
    sys.exit(main())
