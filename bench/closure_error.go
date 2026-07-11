package main

import (
	"fmt"
	"os"
	"strconv"
	"time"
)

func makeAdder(base int) func(int) int {
	return func(x int) int { return base + x }
}

func applyTwice(f func(int) int, x int) int {
	return f(f(x))
}

func benchClosures(iterations int) int {
	total := 0
	for i := 0; i < iterations; i++ {
		add := makeAdder(i)
		step := i%7 + 1
		mul := func(x int) int { return x * step }
		for j := 0; j < 8; j++ {
			total += add(j) - mul(j) + applyTwice(add, j)
		}
	}
	return total
}

func checked(n int) (int, error) {
	label := "value-" + strconv.Itoa(n)
	tags := []string{"a", "b", label}
	if n%5 == 0 {
		return 0, fmt.Errorf("rejected: %s", label)
	}
	return n*2 + len(tags), nil
}

func twoLayers(n int) (int, error) {
	a, err := checked(n)
	if err != nil {
		return 0, err
	}
	b, err := checked(n + 1)
	if err != nil {
		return 0, err
	}
	return a + b, nil
}

func benchErrors(iterations int) int {
	total := 0
	for i := 0; i < iterations; i++ {
		if v, err := twoLayers(i); err == nil {
			total += v
		}
		if v, err := checked(i); err == nil {
			total += v
		} else {
			total += -1
		}
	}
	return total
}

func nowMillis() int64 {
	return time.Now().UnixMilli()
}

func main() {
	iterations := 200000
	if len(os.Args) > 1 {
		if v, err := strconv.Atoi(os.Args[1]); err == nil && v > 0 {
			iterations = v
		}
	}
	fmt.Println("closure/error benchmark")
	fmt.Println("iterations=" + strconv.Itoa(iterations))

	totalStart := nowMillis()

	t0 := nowMillis()
	closureTotal := benchClosures(iterations)
	closureMS := nowMillis() - t0

	t1 := nowMillis()
	errorTotal := benchErrors(iterations)
	errorMS := nowMillis() - t1

	totalMS := nowMillis() - totalStart
	checksum := closureTotal + errorTotal

	fmt.Printf("closure_ms=%d\n", closureMS)
	fmt.Printf("error_ms=%d\n", errorMS)
	fmt.Printf("total_ms=%d\n", totalMS)
	fmt.Printf("checksum=%d\n", checksum)
}
