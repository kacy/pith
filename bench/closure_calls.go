// closure_calls: go counterpart of bench/closure_calls.pith.
//
// splits the cost of a closure into three phases, each folded into one
// checksum:
//
//	build   n closures, each built, called once, and dropped
//	call    one closure called n times
//	direct  a plain function called n times, the floor
//
// go's escape analysis keeps a closure that does not leave its function on
// the stack, and the compiler may inline the call, so these phases measure
// what go does with the idiomatic form rather than a forced heap closure.
//
// build: go build -o bench/closure_calls_go bench/closure_calls.go
// run:   ./bench/closure_calls_go 50000000
package main

import (
	"fmt"
	"os"
	"strconv"
	"time"
)

func directAdd(base, x int64) int64 {
	return base + x
}

func benchBuild(n int64) int64 {
	var total int64
	for i := int64(0); i < n; i++ {
		base := i % 13
		f := func(x int64) int64 { return base + x }
		total += f(i)
	}
	return total
}

func benchCall(n int64) int64 {
	base := int64(7)
	f := func(x int64) int64 { return base + x }
	var total int64
	for i := int64(0); i < n; i++ {
		total += f(i)
	}
	return total
}

func benchDirect(n int64) int64 {
	var total int64
	for i := int64(0); i < n; i++ {
		total += directAdd(7, i)
	}
	return total
}

func main() {
	n := int64(2000000)
	if len(os.Args) > 1 {
		if v, err := strconv.ParseInt(os.Args[1], 10, 64); err == nil {
			n = v
		}
	}

	t0 := time.Now()
	build := benchBuild(n)
	t1 := time.Now()
	call := benchCall(n)
	t2 := time.Now()
	direct := benchDirect(n)
	t3 := time.Now()

	fmt.Println("closure calls benchmark")
	fmt.Printf("iterations=%d\n", n)
	fmt.Printf("build_ms=%d\n", t1.Sub(t0).Milliseconds())
	fmt.Printf("call_ms=%d\n", t2.Sub(t1).Milliseconds())
	fmt.Printf("direct_ms=%d\n", t3.Sub(t2).Milliseconds())
	fmt.Printf("checksum=%d\n", build+call+direct)
}
