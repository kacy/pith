// task_pingpong_converge - Go counterpart of bench/task_pingpong_converge.pith.
//
// one ping-pong pair runs `windows` batches of `per` round trips over the
// same two capacity-1 channels and prints the time of each window, so the
// handoff cost is sampled across the lifetime of one pair.
// GOMAXPROCS plays the role of PITH_GREEN_WORKERS.
//
// build: go build -o bench/task_pingpong_converge_go bench/task_pingpong_converge.go
// run:   ./bench/task_pingpong_converge_go 10 100000
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

func pinger(req chan<- int64, rsp <-chan int64, windows, per int64) int64 {
	for w := int64(0); w < windows; w++ {
		start := time.Now()
		for i := int64(0); i < per; {
			req <- i
			if _, ok := <-rsp; !ok {
				break
			}
			i++
		}
		fmt.Printf("window %d %dms\n", w, time.Since(start).Milliseconds())
	}
	req <- -1
	return windows
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
	windows := positiveArg(1, 10)
	per := positiveArg(2, 20000)
	req := make(chan int64, 1)
	rsp := make(chan int64, 1)
	pong := make(chan int64, 1)
	ping := make(chan int64, 1)
	go func() { pong <- ponger(req, rsp) }()
	go func() { ping <- pinger(req, rsp, windows, per) }()
	done := <-ping
	seen := <-pong
	fmt.Printf("done windows=%d seen=%d\n", done, seen)
}
