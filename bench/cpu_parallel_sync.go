// cpu_parallel_sync - Go counterpart of bench/cpu_parallel_sync.pith.
//
// eight goroutines each do a fixed amount of arithmetic in chunks and,
// between chunks, send their partial to a collector goroutine and wait for
// an acknowledgement on a shared ack channel. both channels hold 8, as in
// the pith program, so any worker may take any ack. the checksum is the
// sum of the workers' partials and must equal the collector's total.
// GOMAXPROCS plays the role of PITH_GREEN_WORKERS.
//
// build: go build -o bench/cpu_parallel_sync_go bench/cpu_parallel_sync.go
// run:   ./bench/cpu_parallel_sync_go 200 1000000
package main

import (
	"fmt"
	"os"
	"strconv"
	"time"
)

const mod = 1000000007

func collector(req <-chan int64, ack chan<- int64, expected int64) int64 {
	var total, seen int64
	for seen < expected {
		v, ok := <-req
		if !ok {
			break
		}
		total = (total + v) % mod
		seen++
		ack <- 1
	}
	return total
}

func mix(value int64) int64 {
	x := (value*1103515245 + 12345) % 2147483648
	x = (x*1103515245 + 12345) % 2147483648
	return x
}

func worker(id int64, req chan<- int64, ack <-chan int64, chunks, chunkSize int64) int64 {
	var partial int64
	for c := int64(0); c < chunks; c++ {
		x := id*7919 + c
		for i := int64(0); i < chunkSize; i++ {
			x = mix(x)
		}
		partial = (partial + x) % mod
		req <- x
		<-ack
	}
	return partial
}

func arg(i int, fallback int64) int64 {
	if len(os.Args) > i {
		if n, err := strconv.ParseInt(os.Args[i], 10, 64); err == nil && n > 0 {
			return n
		}
	}
	return fallback
}

func main() {
	chunks := arg(1, 200)
	chunkSize := arg(2, 20000)
	req := make(chan int64, 8)
	ack := make(chan int64, 8)
	start := time.Now()
	coll := make(chan int64, 1)
	go func() { coll <- collector(req, ack, chunks*8) }()
	results := make([]chan int64, 8)
	for id := int64(0); id < 8; id++ {
		results[id] = make(chan int64, 1)
		go func(id int64) { results[id] <- worker(id, req, ack, chunks, chunkSize) }(id)
	}
	var checksum int64
	for _, r := range results {
		checksum = (checksum + <-r) % mod
	}
	total := <-coll
	elapsed := time.Since(start).Milliseconds()
	fmt.Printf("cpu_parallel_sync chunks=%d chunk_size=%d elapsed_ms=%d checksum=%d total=%d\n",
		chunks, chunkSize, elapsed, checksum, total)
}
