// chan_fanout — Go counterpart of bench/chan_fanout.pith.
//
// Four producer goroutines push messages into one bounded channel and
// four consumer goroutines drain it. The per-message work is two LCG
// rounds and the aggregate is a sum modulo a prime, so the checksum is
// order-independent and matches the Pith, Rust, and Zig versions.
//
// build: go build -o bench/chan_fanout_go bench/chan_fanout.go
// run:   ./bench/chan_fanout_go 1000000
package main

import (
	"bufio"
	"fmt"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"
)

const (
	producers = 4
	consumers = 4
	capacity  = 256
	mod       = 1000000007
)

func messagesFromArgs() int64 {
	if len(os.Args) > 1 {
		if n, err := strconv.ParseInt(os.Args[1], 10, 64); err == nil && n > 0 {
			return n
		}
	}
	return 1000000
}

// two rounds of a 31-bit LCG (POSIX constants), masked so it never
// overflows and every language reproduces it exactly.
func mix(value int64) int64 {
	x := (value*1103515245 + 12345) % 2147483648
	x = (x*1103515245 + 12345) % 2147483648
	return x
}

type partial struct {
	sum  int64
	seen int64
}

func peakRSSKB() int64 {
	data, err := os.ReadFile("/proc/self/status")
	if err != nil {
		return 0
	}
	for _, line := range strings.Split(string(data), "\n") {
		if strings.HasPrefix(line, "VmHWM:") {
			digits := strings.TrimSpace(strings.ReplaceAll(strings.TrimPrefix(line, "VmHWM:"), "kB", ""))
			if n, err := strconv.ParseInt(digits, 10, 64); err == nil {
				return n
			}
		}
	}
	return 0
}

func main() {
	requested := messagesFromArgs()
	per := requested / producers
	messages := per * producers

	jobs := make(chan int64, capacity)
	results := make(chan partial, consumers)

	start := time.Now()

	// consumers start first and block on recv until work shows up.
	var consumerWg sync.WaitGroup
	for c := 0; c < consumers; c++ {
		consumerWg.Add(1)
		go func() {
			defer consumerWg.Done()
			var sum, seen int64
			for value := range jobs {
				sum = (sum + mix(value)) % mod
				seen++
			}
			results <- partial{sum, seen}
		}()
	}

	// each producer owns a disjoint slice of the id space, and reports
	// how many messages it pushed into its own slot.
	var sentCounts [producers]int64
	var producerWg sync.WaitGroup
	for p := int64(0); p < producers; p++ {
		producerWg.Add(1)
		go func(id int64) {
			defer producerWg.Done()
			for i := int64(0); i < per; i++ {
				jobs <- id*per + i
			}
			sentCounts[id] = per
		}(p)
	}

	producerWg.Wait()
	close(jobs)
	consumerWg.Wait()
	close(results)

	var checksum, received, sent int64
	for r := range results {
		checksum = (checksum + r.sum) % mod
		received += r.seen
	}
	for _, n := range sentCounts {
		sent += n
	}

	elapsed := time.Since(start).Milliseconds()
	var rate int64
	if elapsed > 0 {
		rate = messages * 1000 / elapsed
	}

	w := bufio.NewWriter(os.Stdout)
	defer w.Flush()
	fmt.Fprintln(w, "chan fanout benchmark")
	fmt.Fprintf(w, "messages=%d\n", messages)
	fmt.Fprintf(w, "producers=%d\n", producers)
	fmt.Fprintf(w, "consumers=%d\n", consumers)
	fmt.Fprintf(w, "sent=%d\n", sent)
	fmt.Fprintf(w, "received=%d\n", received)
	fmt.Fprintf(w, "elapsed_ms=%d\n", elapsed)
	fmt.Fprintf(w, "rate_per_sec=%d\n", rate)
	fmt.Fprintf(w, "peak_rss_kb=%d\n", peakRSSKB())
	fmt.Fprintf(w, "checksum=%d\n", checksum)
}
