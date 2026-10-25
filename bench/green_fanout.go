// green_fanout - Go counterpart of bench/green_fanout.pith.
//
// spawn a batch of 64 short goroutines, wait for the batch, repeat. each
// goroutine writes its result into its own slot and a WaitGroup stands in
// for awaiting each task. it reports throughput and peak rss from
// /proc/self/status in the same line format as the pith program.
// GOMAXPROCS plays the role of PITH_GREEN_WORKERS.
//
// build: go build -o bench/green_fanout_go bench/green_fanout.go
// run:   ./bench/green_fanout_go 2000000
package main

import (
	"fmt"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"
)

const batch = 64

func tasksFromArgs() int64 {
	if len(os.Args) > 1 {
		if n, err := strconv.ParseInt(os.Args[1], 10, 64); err == nil && n > 0 {
			return n
		}
	}
	return 500000
}

func worker(id int64) int64 {
	return id*2 + 1
}

// spawn a batch of workers, then fan in by waiting for all of them.
func runBatch() int64 {
	var results [batch]int64
	var wg sync.WaitGroup
	for i := int64(0); i < batch; i++ {
		wg.Add(1)
		go func(id int64) {
			defer wg.Done()
			results[id] = worker(id)
		}(i)
	}
	wg.Wait()
	var total int64
	for _, r := range results {
		total += r
	}
	return total
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
	totalTasks := tasksFromArgs()
	rounds := totalTasks / batch

	start := time.Now()
	var checksum int64
	for r := int64(0); r < rounds; r++ {
		checksum += runBatch()
	}
	elapsed := time.Since(start).Milliseconds()

	tasksRun := rounds * batch
	var rate int64
	if elapsed > 0 {
		rate = tasksRun * 1000 / elapsed
	}
	fmt.Printf("green_fanout: tasks %d batch %d time %dms rate %d/s peak_rss %dkb checksum %d\n",
		tasksRun, batch, elapsed, rate, peakRSSKB(), checksum)
}
