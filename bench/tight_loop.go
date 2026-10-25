// tight_loop: go counterpart of bench/tight_loop.pith.
//
// sums i*3 over n iterations in a function of its own and prints the sum as
// the checksum. the loop does no allocation, no call and no channel work, so
// it prices the bare loop. go puts an asynchronous preemption point in by
// signal rather than by a check on the back edge, so this loop carries no
// per-iteration safe-point cost.
//
// build: go build -o bench/tight_loop_go bench/tight_loop.go
// run:   ./bench/tight_loop_go 2000000000
package main

import (
	"fmt"
	"os"
	"strconv"
)

func iterationsFromArgs() int64 {
	if len(os.Args) > 1 {
		if n, err := strconv.ParseInt(os.Args[1], 10, 64); err == nil && n > 0 {
			return n
		}
	}
	return 200000000
}

func run(n int64) int64 {
	var sum int64
	for i := int64(0); i < n; i++ {
		sum += i * 3
	}
	return sum
}

func main() {
	fmt.Printf("checksum=%d\n", run(iterationsFromArgs()))
}
