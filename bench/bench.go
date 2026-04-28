package main

import (
	"fmt"
	"io"
	"net/http"
	"os"
	"sort"
	"time"
)

func bench(name string, url string, requests int) (p50, p95, p99 float64, rps float64, errs int) {
	client := &http.Client{
		Timeout: 5 * time.Second,
		Transport: &http.Transport{
			DisableKeepAlives: true,
		},
	}
	var lats []float64

	start := time.Now()
	for i := 0; i < requests; i++ {
		t0 := time.Now()
		resp, err := client.Get(url)
		lat := time.Since(t0)
		if err != nil {
			errs++
			continue
		}
		io.Copy(io.Discard, resp.Body)
		resp.Body.Close()
		lats = append(lats, float64(lat.Microseconds()))
	}
	elapsed := time.Since(start)

	if len(lats) == 0 {
		return 0, 0, 0, 0, errs
	}

	sort.Float64s(lats)
	p50 = lats[len(lats)*50/100]
	p95 = lats[len(lats)*95/100]
	p99 = lats[len(lats)*99/100]
	rps = float64(len(lats)) / elapsed.Seconds()
	return
}

func printRow(label string, p50, p95, p99, rps float64, errs int) {
	fmt.Printf("  %-8s p50=%7.0fus  p95=%7.0fus  p99=%7.0fus  rps=%6.0f  err=%d\n",
		label, p50, p95, p99, rps, errs)
}

func waitForServer(url string, timeout time.Duration) bool {
	client := &http.Client{Timeout: 2 * time.Second}
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		resp, err := client.Get(url)
		if err == nil {
			resp.Body.Close()
			return true
		}
		time.Sleep(200 * time.Millisecond)
	}
	return false
}

func main() {
	goPort := "9001"
	pithPort := "9002"

	fmt.Println("Waiting for servers...")
	goOK := waitForServer("http://localhost:"+goPort+"/", 5*time.Second)
	pithOK := waitForServer("http://localhost:"+pithPort+"/", 5*time.Second)
	if !goOK {
		fmt.Println("Go server not responding on :" + goPort)
		os.Exit(1)
	}
	if !pithOK {
		fmt.Println("Pith server not responding on :" + pithPort)
		os.Exit(1)
	}
	fmt.Println("Both servers ready.")
	fmt.Println()

	// warmup
	bench("warmup", "http://localhost:"+goPort+"/", 20)
	bench("warmup", "http://localhost:"+pithPort+"/", 20)

	type scenario struct {
		name string
		path string
		n    int
	}
	scenarios := []scenario{
		{"GET /", "/", 200},
		{"GET /json", "/json", 200},
		{"GET /echo", "/echo?msg=test", 200},
		{"GET /compute", "/compute?n=100", 100},
		{"GET /compute", "/compute?n=1000", 50},
	}

	fmt.Println("Sequential latency benchmark (1 connection at a time)")
	fmt.Println("======================================================")
	fmt.Println()

	for _, s := range scenarios {
		fmt.Printf("[%s] %d requests\n", s.name, s.n)
		gP50, gP95, gP99, gRPS, gErr := bench("go", "http://localhost:"+goPort+s.path, s.n)
		fP50, fP95, fP99, fRPS, fErr := bench("pith", "http://localhost:"+pithPort+s.path, s.n)
		printRow("go", gP50, gP95, gP99, gRPS, gErr)
		printRow("pith", fP50, fP95, fP99, fRPS, fErr)
		if gP50 > 0 {
			fmt.Printf("  ratio    p50=%.1fx     p95=%.1fx     p99=%.1fx\n", fP50/gP50, fP95/gP95, fP99/gP99)
		}
		fmt.Println()
	}
}
