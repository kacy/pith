package main

import (
	"bytes"
	"fmt"
	"io"
	"net/http"
	"os"
	"sort"
	"time"
)

type scenario struct {
	name    string
	method  string
	path    string
	body    string
	requests int
}

func benchRequest(method, url, body string, requests int) (p50, p95, p99 float64, rps float64, errs int) {
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
		var reqBody io.Reader
		if body != "" {
			reqBody = bytes.NewBufferString(body)
		}
		req, err := http.NewRequest(method, url, reqBody)
		if err != nil {
			errs++
			continue
		}
		if body != "" {
			req.Header.Set("Content-Type", "application/json")
		}
		resp, err := client.Do(req)
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
	goPort := "9101"
	pithPort := "9102"
	if len(os.Args) > 1 && os.Args[1] != "" {
		goPort = os.Args[1]
	}
	if len(os.Args) > 2 && os.Args[2] != "" {
		pithPort = os.Args[2]
	}

	fmt.Println("waiting for catalog servers...")
	goOK := waitForServer("http://localhost:"+goPort+"/health", 5*time.Second)
	pithOK := waitForServer("http://localhost:"+pithPort+"/health", 5*time.Second)
	if !goOK {
		fmt.Println("go catalog server not responding on :" + goPort)
		os.Exit(1)
	}
	if !pithOK {
		fmt.Println("pith catalog server not responding on :" + pithPort)
		os.Exit(1)
	}
	fmt.Println("both catalog servers ready.")
	fmt.Println()

	warmup := scenario{name: "warmup", method: "GET", path: "/health", requests: 20}
	benchRequest(warmup.method, "http://localhost:"+goPort+warmup.path, warmup.body, warmup.requests)
	benchRequest(warmup.method, "http://localhost:"+pithPort+warmup.path, warmup.body, warmup.requests)

	scenarios := []scenario{
		{name: "GET /profile", method: "GET", path: "/profile?id=424", requests: 250},
		{name: "GET /search hot", method: "GET", path: "/search?team=infra&region=us-west&active=1&min_score=400&limit=8", requests: 200},
		{name: "GET /search wide", method: "GET", path: "/search?region=eu-central&limit=24", requests: 150},
		{name: "POST /batch-score", method: "POST", path: "/batch-score", body: `{"team":"payments","region":"us-east","active":"1","min_score":500,"limit":12,"multiplier":4}`, requests: 120},
	}

	fmt.Println("catalog service benchmark (sequential, 1 connection at a time)")
	fmt.Println("============================================================")
	fmt.Println()

	for _, s := range scenarios {
		fmt.Printf("[%s] %d requests\n", s.name, s.requests)
		gP50, gP95, gP99, gRPS, gErr := benchRequest(s.method, "http://localhost:"+goPort+s.path, s.body, s.requests)
		fP50, fP95, fP99, fRPS, fErr := benchRequest(s.method, "http://localhost:"+pithPort+s.path, s.body, s.requests)
		printRow("go", gP50, gP95, gP99, gRPS, gErr)
		printRow("pith", fP50, fP95, fP99, fRPS, fErr)
		if gP50 > 0 {
			fmt.Printf("  ratio    p50=%.1fx     p95=%.1fx     p99=%.1fx\n", fP50/gP50, fP95/gP95, fP99/gP99)
		}
		fmt.Println()
	}
}
