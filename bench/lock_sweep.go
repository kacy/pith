// lock_sweep - Go counterpart of bench/lock_sweep.pith.
//
// every run does the same total number of operations, split evenly across
// `tasks` goroutines, and prints the elapsed milliseconds in the pith
// format. each mode maps a pith std facility to the closest go idiom:
//
//	log        slog.Debug with the default logger's handler at info level,
//	           so every call is filtered out. close: both check a level and
//	           return.
//	spawn      each op starts a goroutine and waits for its result on a
//	           channel. close.
//	labels     a counter series looked up by name and label set on every
//	           write, in an RWMutex-guarded map keyed by the rendered series
//	           key, then a compare-and-swap float add. this is the shape of
//	           prometheus' CounterVec.WithLabelValues(...).Inc() written with
//	           the standard library. pith also caches resolved series per
//	           task, which this does not.
//	breaker    a small circuit breaker, one per goroutine, with its own
//	           mutex, mirroring std.resilience's allow() and success(). go
//	           has no standard breaker, so this is a hand port.
//	readfile   os.ReadFile of bench/lock_sweep.pith. close: go blocks the
//	           goroutine's thread in the syscall, pith hands the read to a
//	           file pool thread.
//	trace      skipped. pith's span start and end allocate ids, stamp
//	           times and push into a sharded span buffer; the go equivalent
//	           is the opentelemetry sdk, which does a different amount of
//	           work, and a hand-written stand-in would not mean much.
//
// GOMAXPROCS plays the role of PITH_GREEN_WORKERS. run from the repo root
// so the readfile path resolves.
//
// build: go build -o bench/lock_sweep_go bench/lock_sweep.go
// run:   ./bench/lock_sweep_go <log|spawn|labels|breaker|readfile> <tasks> <ops>
package main

import (
	"fmt"
	"log/slog"
	"math"
	"os"
	"sort"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

func trivial(n int64) int64 {
	return n + 1
}

func logWorker(n int64) int64 {
	for i := int64(0); i < n; i++ {
		slog.Debug("filtered out")
	}
	return n
}

func spawnWorker(n int64) int64 {
	var sum int64
	for i := int64(0); i < n; i++ {
		done := make(chan int64, 1)
		go func(v int64) { done <- trivial(v) }(i)
		sum += <-done
	}
	_ = sum
	return n
}

// --- labels: a series registry keyed by the rendered label set ---

type counterCell struct {
	bits atomic.Uint64
}

func (c *counterCell) add(delta float64) {
	for {
		old := c.bits.Load()
		if c.bits.CompareAndSwap(old, math.Float64bits(math.Float64frombits(old)+delta)) {
			return
		}
	}
}

var (
	seriesMu sync.RWMutex
	series   = map[string]*counterCell{}
)

// name{k="v",...} with keys sorted, so a label set has one key whatever
// order the pairs come in.
func seriesKey(name string, pairs []string) string {
	kv := make(map[string]string, len(pairs)/2)
	keys := make([]string, 0, len(pairs)/2)
	for i := 0; i+1 < len(pairs); i += 2 {
		if _, seen := kv[pairs[i]]; !seen {
			keys = append(keys, pairs[i])
		}
		kv[pairs[i]] = pairs[i+1]
	}
	sort.Strings(keys)
	var b strings.Builder
	b.WriteString(name)
	b.WriteByte('{')
	for i, k := range keys {
		if i > 0 {
			b.WriteByte(',')
		}
		b.WriteString(k)
		b.WriteString(`="`)
		b.WriteString(kv[k])
		b.WriteByte('"')
	}
	b.WriteByte('}')
	return b.String()
}

func counterWithLabels(name string, pairs []string) *counterCell {
	key := seriesKey(name, pairs)
	seriesMu.RLock()
	c := series[key]
	seriesMu.RUnlock()
	if c != nil {
		return c
	}
	seriesMu.Lock()
	defer seriesMu.Unlock()
	if c = series[key]; c == nil {
		c = &counterCell{}
		series[key] = c
	}
	return c
}

func labelsWorker(n int64) int64 {
	for i := int64(0); i < n; i++ {
		counterWithLabels("lock_sweep_requests", []string{"route", "/api", "method", "GET"}).add(1)
	}
	return n
}

// --- breaker: closed, open after `threshold` failures, one probe after reset ---

type breaker struct {
	mu        sync.Mutex
	threshold int64
	reset     time.Duration
	failures  int64
	open      bool
	openedAt  time.Time
	probing   bool
}

func newBreaker(failures int64, resetMs int64) *breaker {
	return &breaker{threshold: failures, reset: time.Duration(resetMs) * time.Millisecond}
}

func (b *breaker) allow() bool {
	b.mu.Lock()
	defer b.mu.Unlock()
	if !b.open {
		return true
	}
	if time.Since(b.openedAt) >= b.reset && !b.probing {
		b.probing = true
		return true
	}
	return false
}

func (b *breaker) success() {
	b.mu.Lock()
	b.failures = 0
	b.open = false
	b.probing = false
	b.mu.Unlock()
}

func (b *breaker) failure() {
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.open {
		b.openedAt = time.Now()
		b.probing = false
		return
	}
	b.failures++
	if b.failures >= b.threshold {
		b.open = true
		b.openedAt = time.Now()
		b.probing = false
	}
}

func breakerWorker(n int64) int64 {
	br := newBreaker(5, 1000)
	for i := int64(0); i < n; i++ {
		if br.allow() {
			br.success()
		}
	}
	return n
}

func readfileWorker(n int64) int64 {
	for i := int64(0); i < n; i++ {
		_, _ = os.ReadFile("bench/lock_sweep.pith")
	}
	return n
}

var workers = map[string]func(int64) int64{
	"log":      logWorker,
	"spawn":    spawnWorker,
	"labels":   labelsWorker,
	"breaker":  breakerWorker,
	"readfile": readfileWorker,
}

func main() {
	if len(os.Args) < 4 {
		fmt.Println("usage: lock_sweep <log|spawn|labels|breaker|readfile> <tasks> <ops>")
		os.Exit(1)
	}
	mode := os.Args[1]
	run, ok := workers[mode]
	if !ok {
		fmt.Printf("mode %s has no go counterpart\n", mode)
		os.Exit(1)
	}
	tasks, err1 := strconv.ParseInt(os.Args[2], 10, 64)
	ops, err2 := strconv.ParseInt(os.Args[3], 10, 64)
	if err1 != nil || err2 != nil || tasks <= 0 {
		fmt.Println("tasks and ops must be integers, tasks above zero")
		os.Exit(1)
	}
	slog.SetDefault(slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelInfo})))
	per := ops / tasks
	start := time.Now()
	results := make([]chan int64, tasks)
	for t := range results {
		results[t] = make(chan int64, 1)
		go func(out chan<- int64) { out <- run(per) }(results[t])
	}
	var done int64
	for _, r := range results {
		done += <-r
	}
	fmt.Printf("mode=%s tasks=%d ops=%d elapsed_ms=%d\n", mode, tasks, done, time.Since(start).Milliseconds())
}
