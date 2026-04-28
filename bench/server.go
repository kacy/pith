package main

import (
	"fmt"
	"io"
	"math"
	"net"
	"net/http"
	"strconv"
	"strings"
)

// singleConnListener wraps a net.Listener to handle one connection at a time
// (no goroutines per connection) for fair comparison with Pith's sequential model
type singleConnListener struct {
	ln net.Listener
}

func (s *singleConnListener) Accept() (net.Conn, error) { return s.ln.Accept() }
func (s *singleConnListener) Close() error              { return s.ln.Close() }
func (s *singleConnListener) Addr() net.Addr             { return s.ln.Addr() }

func main() {
	mux := http.NewServeMux()

	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/html")
		w.Header().Set("Connection", "close")
		w.Write([]byte("<h1>Hello from Go!</h1>"))
	})

	mux.HandleFunc("/json", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Header().Set("Connection", "close")
		w.Write([]byte(`{"status":"ok","lang":"go"}`))
	})

	mux.HandleFunc("/compute", func(w http.ResponseWriter, r *http.Request) {
		nStr := r.URL.Query().Get("n")
		n, err := strconv.Atoi(nStr)
		if err != nil || n < 1 {
			n = 1000
		}
		sum := 0.0
		for i := 0; i < n; i++ {
			sum += math.Sin(float64(i) * 0.001)
		}
		w.Header().Set("Content-Type", "application/json")
		w.Header().Set("Connection", "close")
		fmt.Fprintf(w, `{"n":%d,"result":%.6f}`, n, sum)
	})

	mux.HandleFunc("/echo", func(w http.ResponseWriter, r *http.Request) {
		msg := r.URL.Query().Get("msg")
		if msg == "" {
			msg = "hello"
		}
		w.Header().Set("Content-Type", "text/plain")
		w.Header().Set("Connection", "close")
		io.WriteString(w, strings.Repeat(msg+" ", 10))
	})

	ln, err := net.Listen("tcp", ":9001")
	if err != nil {
		fmt.Println("Failed to listen:", err)
		return
	}
	fmt.Println("Go server on :9001")

	// Serve sequentially (one connection at a time, like Pith)
	srv := &http.Server{Handler: mux}
	srv.SetKeepAlivesEnabled(false)
	srv.Serve(ln)
}
