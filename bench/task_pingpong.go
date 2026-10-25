// task_pingpong - Go counterpart of bench/task_pingpong.pith.
//
// two goroutines pass a token back and forth over a pair of capacity-1
// channels, so the time is the cost of one goroutine-to-goroutine handoff
// and its reply. both sides are spawned, as in the pith program; main only
// waits. GOMAXPROCS plays the role of PITH_GREEN_WORKERS.
//
// build: go build -o bench/task_pingpong_go bench/task_pingpong.go
// run:   ./bench/task_pingpong_go 1000000
package main

import (
	"fmt"
	"os"
	"strconv"
	"time"
)

const roundsDefault = 200000

func roundsFromArgs() int64 {
	if len(os.Args) > 1 {
		if n, err := strconv.ParseInt(os.Args[1], 10, 64); err == nil && n > 0 {
			return n
		}
	}
	return roundsDefault
}

// echoes value+1 until it is told to stop with a negative token.
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

func main() {
	rounds := roundsFromArgs()
	req := make(chan int64, 1)
	rsp := make(chan int64, 1)

	start := time.Now()
	pong := make(chan int64, 1)
	ping := make(chan int64, 1)
	go func() { pong <- ponger(req, rsp) }()
	go func() { ping <- pinger(req, rsp, rounds) }()
	sum := <-ping
	seen := <-pong
	elapsed := time.Since(start).Milliseconds()

	expected := rounds * (rounds + 1) / 2
	if sum != expected || seen != rounds {
		fmt.Printf("MISMATCH sum=%d expected=%d seen=%d\n", sum, expected, seen)
		os.Exit(1)
	}
	fmt.Printf("rounds=%d elapsed_ms=%d\n", rounds, elapsed)
}
