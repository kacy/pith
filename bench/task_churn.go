// task_churn - Go counterpart of bench/task_churn.pith.
//
// one short goroutine after another, each allocating a batch of structs
// into a slice and dropping them at return; the caller waits for each one
// before starting the next. the structs are stored by pointer so every row
// is its own heap allocation, the work the pith program puts on its struct
// pool. go has no way to run each task on a fresh os thread, so this only
// mirrors the green arm. GOMAXPROCS plays the role of PITH_GREEN_WORKERS.
//
// build: go build -o bench/task_churn_go bench/task_churn.go
// run:   ./bench/task_churn_go 200000
package main

import (
	"fmt"
	"os"
	"strconv"
	"time"
)

type blob struct {
	a int64
	b int64
}

func worker(seed, perTask int64) int64 {
	var rows []*blob
	for i := int64(0); i < perTask; i++ {
		rows = append(rows, &blob{a: seed, b: i})
	}
	var acc int64
	for _, r := range rows {
		acc += r.b
	}
	return acc
}

func main() {
	tasks := int64(2000)
	const perTask = 400
	if len(os.Args) > 1 {
		if n, err := strconv.ParseInt(os.Args[1], 10, 64); err == nil {
			tasks = n
		}
	}
	start := time.Now()
	var acc int64
	for i := int64(0); i < tasks; i++ {
		done := make(chan int64, 1)
		go func(seed int64) { done <- worker(seed, perTask) }(i)
		acc += <-done
	}
	elapsed := time.Since(start).Milliseconds()
	fmt.Println("task churn benchmark")
	fmt.Printf("tasks=%d\n", tasks)
	fmt.Printf("structs_per_task=%d\n", perTask)
	fmt.Printf("elapsed_ms=%d\n", elapsed)
	fmt.Printf("checksum=%d\n", acc)
}
