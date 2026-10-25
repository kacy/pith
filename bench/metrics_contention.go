// metrics_contention - Go counterpart of bench/metrics_contention.pith.
//
// every trial does the same total number of writes, split evenly across
// `tasks` goroutines, all writing the same series, and prints
// "<trial> <ms> <ops>". go's standard library has no metrics registry, so
// this carries a small one shaped like std.metrics: a name-to-series map
// behind an RWMutex, float cells updated by compare-and-swap, and
// histograms with fixed per-boundary buckets plus sum, min and max.
//
// modes:
//
//	counter    inc() on a counter resolved once per goroutine
//	gauge      set() on a gauge resolved once per goroutine
//	histogram  observe() on a histogram resolved once per goroutine
//	labels     the series looked up by name and label set on every write,
//	           the shape of prometheus' CounterVec.WithLabelValues(...).Inc().
//	           pith also caches resolved series per task; this does not.
//
// GOMAXPROCS plays the role of PITH_GREEN_WORKERS.
//
// build: go build -o bench/metrics_contention_go bench/metrics_contention.go
// run:   ./bench/metrics_contention_go 5 4000000 32 labels
package main

import (
	"fmt"
	"math"
	"os"
	"sort"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

const (
	counterName   = "bench_hits_total"
	gaugeName     = "bench_level"
	histogramName = "bench_latency_ms"
	labeledName   = "bench_labeled_total"
)

// a float64 held as its bit pattern so it can be updated atomically.
type floatCell struct {
	bits atomic.Uint64
}

func newFloatCell(v float64) *floatCell {
	c := &floatCell{}
	c.bits.Store(math.Float64bits(v))
	return c
}

func (c *floatCell) set(v float64) {
	c.bits.Store(math.Float64bits(v))
}

func (c *floatCell) add(delta float64) {
	for {
		old := c.bits.Load()
		if c.bits.CompareAndSwap(old, math.Float64bits(math.Float64frombits(old)+delta)) {
			return
		}
	}
}

// store v if keep(v, current) holds, retrying on a lost race.
func (c *floatCell) update(v float64, keep func(v, current float64) bool) {
	for {
		old := c.bits.Load()
		if !keep(v, math.Float64frombits(old)) {
			return
		}
		if c.bits.CompareAndSwap(old, math.Float64bits(v)) {
			return
		}
	}
}

type histogram struct {
	sum      *floatCell
	lo       *floatCell
	hi       *floatCell
	bounds   []float64
	buckets  []atomic.Int64
	overflow atomic.Int64
}

var defaultBuckets = []float64{1, 5, 10, 25, 50, 100, 250, 500, 1000, 5000}

func newHistogram() *histogram {
	return &histogram{
		sum:     newFloatCell(0),
		lo:      newFloatCell(math.Inf(1)),
		hi:      newFloatCell(math.Inf(-1)),
		bounds:  defaultBuckets,
		buckets: make([]atomic.Int64, len(defaultBuckets)),
	}
}

// sum, min and max first, then the one bucket the value lands in.
func (h *histogram) observe(v float64) {
	h.sum.add(v)
	h.lo.update(v, func(v, cur float64) bool { return v < cur })
	h.hi.update(v, func(v, cur float64) bool { return v > cur })
	for i, b := range h.bounds {
		if v <= b {
			h.buckets[i].Add(1)
			return
		}
	}
	h.overflow.Add(1)
}

// a find-or-create map from series key to instrument.
type registry[T any] struct {
	mu     sync.RWMutex
	series map[string]T
	make   func() T
}

func (r *registry[T]) get(key string) T {
	r.mu.RLock()
	v, ok := r.series[key]
	r.mu.RUnlock()
	if ok {
		return v
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if v, ok = r.series[key]; !ok {
		v = r.make()
		r.series[key] = v
	}
	return v
}

var (
	counters   = &registry[*floatCell]{series: map[string]*floatCell{}, make: func() *floatCell { return newFloatCell(0) }}
	gauges     = &registry[*floatCell]{series: map[string]*floatCell{}, make: func() *floatCell { return newFloatCell(0) }}
	histograms = &registry[*histogram]{series: map[string]*histogram{}, make: newHistogram}
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

func countWorker(n int64) int64 {
	c := counters.get(counterName)
	for i := int64(0); i < n; i++ {
		c.add(1)
	}
	return n
}

func gaugeWorker(n int64) int64 {
	g := gauges.get(gaugeName)
	for i := int64(0); i < n; i++ {
		g.set(float64(i))
	}
	return n
}

func histogramWorker(n int64) int64 {
	h := histograms.get(histogramName)
	for i := int64(0); i < n; i++ {
		h.observe(7.0)
	}
	return n
}

func labeledWorker(n int64) int64 {
	for i := int64(0); i < n; i++ {
		counters.get(seriesKey(labeledName, []string{"route", "/api", "method", "GET"})).add(1)
	}
	return n
}

func workerFor(mode string) func(int64) int64 {
	switch mode {
	case "gauge":
		return gaugeWorker
	case "histogram":
		return histogramWorker
	case "labels":
		return labeledWorker
	}
	return countWorker
}

// one trial: `tasks` writers share `ops` writes to one series, timed end to end.
func trial(mode string, ops, tasks int64) int64 {
	per := ops / tasks
	run := workerFor(mode)
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
	elapsed := time.Since(start).Milliseconds()
	if done != per*tasks {
		fmt.Printf("MISMATCH done=%d expected=%d\n", done, per*tasks)
	}
	return elapsed
}

func positiveArg(i int, fallback int64) int64 {
	if len(os.Args) > i {
		if n, err := strconv.ParseInt(os.Args[i], 10, 64); err == nil && n > 0 {
			return n
		}
	}
	return fallback
}

func main() {
	trials := positiveArg(1, 9)
	ops := positiveArg(2, 400000)
	tasks := positiveArg(3, 1)
	mode := "counter"
	if len(os.Args) > 4 {
		mode = os.Args[4]
	}
	per := ops / tasks
	for i := int64(0); i < trials; i++ {
		ms := trial(mode, ops, tasks)
		fmt.Printf("%d %d %d\n", i, ms, per*tasks)
	}
}
