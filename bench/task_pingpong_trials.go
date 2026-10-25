// task_pingpong_trials - Go counterpart of bench/task_pingpong_trials.pith.
//
// runs many ping-pong trials inside one process, each with fresh channels
// and a fresh pair of goroutines, and prints "<index> <ms>" per trial. in
// pith each trial is a new placement decision for the pair; the go
// scheduler makes its own, so the per-trial spread is what to compare.
// GOMAXPROCS plays the role of PITH_GREEN_WORKERS.
//
// build: go build -o bench/task_pingpong_trials_go bench/task_pingpong_trials.go
// run:   ./bench/task_pingpong_trials_go 200 20000
package main

import (
	"fmt"
	"os"
	"strconv"
	"time"
)

func ponger(req <-chan int64, rsp chan<- int64) int64 {
	var seen int64
	for value := range req {
		if value < 0 {
			break
		}
		rsp <- value + 1
		seen++
	}
	return seen
}

func pinger(req chan<- int64, rsp <-chan int64, rounds int64) int64 {
	var sum int64
	for i := int64(0); i < rounds; i++ {
		req <- i
		if back, ok := <-rsp; ok {
			sum += back
		}
	}
	req <- -1
	return sum
}

// one trial: fresh channels, fresh pair, timed round trip.
func trial(rounds int64) int64 {
	req := make(chan int64, 1)
	rsp := make(chan int64, 1)
	start := time.Now()
	pong := make(chan int64, 1)
	ping := make(chan int64, 1)
	go func() { pong <- ponger(req, rsp) }()
	go func() { ping <- pinger(req, rsp, rounds) }()
	<-ping
	seen := <-pong
	elapsed := time.Since(start).Milliseconds()
	if seen != rounds {
		fmt.Printf("MISMATCH seen=%d rounds=%d\n", seen, rounds)
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
	trials := positiveArg(1, 100)
	rounds := positiveArg(2, 20000)
	for i := int64(0); i < trials; i++ {
		fmt.Printf("%d %d\n", i, trial(rounds))
	}
}
