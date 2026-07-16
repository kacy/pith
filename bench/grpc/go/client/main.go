// the go grpc benchmark client: warm up, then time many unary echo calls,
// sequentially or across N concurrent workers over one connection. prints a
// one-line result the runner collects.
package main

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"flag"
	"fmt"
	"log"
	"os"
	"sort"
	"sync"
	"time"

	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"

	"pithgrpcbench/echopb"
)

func main() {
	addr := flag.String("addr", "127.0.0.1:50051", "server address")
	ca := flag.String("ca", "certs/localhost-ca.crt", "ca certificate")
	authority := flag.String("authority", "localhost", "tls server name")
	size := flag.Int("size", 16, "request payload size in bytes")
	calls := flag.Int("calls", 20000, "measured calls")
	warmup := flag.Int("warmup", 2000, "warmup calls")
	conc := flag.Int("concurrency", 1, "concurrent workers over one connection")
	flag.Parse()

	caPem, err := os.ReadFile(*ca)
	if err != nil {
		log.Fatalf("read ca: %v", err)
	}
	pool := x509.NewCertPool()
	if !pool.AppendCertsFromPEM(caPem) {
		log.Fatal("parse ca")
	}
	creds := credentials.NewTLS(&tls.Config{RootCAs: pool, ServerName: *authority})

	conn, err := grpc.NewClient(*addr, grpc.WithTransportCredentials(creds))
	if err != nil {
		log.Fatalf("dial: %v", err)
	}
	defer conn.Close()
	client := echopb.NewEchoClient(conn)

	req := &echopb.EchoRequest{Payload: make([]byte, *size)}
	for i := 0; i < *warmup; i++ {
		if _, err := client.Unary(context.Background(), req); err != nil {
			log.Fatalf("warmup: %v", err)
		}
	}

	latencies := make([]time.Duration, *calls)
	start := time.Now()
	if *conc <= 1 {
		for i := 0; i < *calls; i++ {
			t0 := time.Now()
			if _, err := client.Unary(context.Background(), req); err != nil {
				log.Fatalf("call: %v", err)
			}
			latencies[i] = time.Since(t0)
		}
	} else {
		work := make(chan int, *calls)
		for i := 0; i < *calls; i++ {
			work <- i
		}
		close(work)
		var wg sync.WaitGroup
		for w := 0; w < *conc; w++ {
			wg.Add(1)
			go func() {
				defer wg.Done()
				for i := range work {
					t0 := time.Now()
					if _, err := client.Unary(context.Background(), req); err != nil {
						log.Fatalf("call: %v", err)
					}
					latencies[i] = time.Since(t0)
				}
			}()
		}
		wg.Wait()
	}
	report("go", *size, *conc, *calls, time.Since(start), latencies)
}

func report(name string, size, conc, calls int, elapsed time.Duration, lat []time.Duration) {
	sort.Slice(lat, func(i, j int) bool { return lat[i] < lat[j] })
	median := lat[len(lat)/2]
	p99 := lat[(len(lat)*99)/100]
	throughput := float64(calls) / elapsed.Seconds()
	fmt.Printf("%-6s size=%-6d conc=%-3d calls=%d  median=%-8s p99=%-8s  %.0f calls/sec\n",
		name, size, conc, calls, median.Round(time.Microsecond), p99.Round(time.Microsecond), throughput)
}
