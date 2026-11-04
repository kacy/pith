// the go counterpart of examples/gateway, for bench/gateway_bench.sh. it does
// what the pith gateway does on these routes, with net/http and the standard
// library: strip the route prefix, pick an upstream round robin, skip one behind
// an open breaker, retry a repeatable request on the next upstream, check an
// es256 bearer token against a jwks document on /auth, and count each outcome.
// bodies are buffered, as the pith gateway buffers them, so a retry can resend
// them.
//
//	go build -o gateway_go bench/gateway.go
//	./gateway_go <port> <jwks url> <upstream>...
package main

import (
	"bytes"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math/big"
	"net"
	"net/http"
	"os"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

const (
	breakerFailures = 5
	breakerResetMs  = 10000
	timeout         = 5 * time.Second
)

type breaker struct {
	mu       sync.Mutex
	failures int
	open     bool
	openedAt time.Time
	probing  bool
}

func (b *breaker) allow() bool {
	b.mu.Lock()
	defer b.mu.Unlock()
	if !b.open {
		return true
	}
	if time.Since(b.openedAt) >= breakerResetMs*time.Millisecond && !b.probing {
		b.probing = true
		return true
	}
	return false
}

func (b *breaker) success() {
	b.mu.Lock()
	b.failures, b.open, b.probing = 0, false, false
	b.mu.Unlock()
}

func (b *breaker) failure() {
	b.mu.Lock()
	b.failures++
	if b.probing || b.failures >= breakerFailures {
		b.open, b.probing, b.openedAt = true, false, time.Now()
	}
	b.mu.Unlock()
}

type upstream struct {
	addr string
	br   breaker
	ok   atomic.Int64
	bad  atomic.Int64
}

type gateway struct {
	upstreams []*upstream
	next      atomic.Int64
	client    *http.Client
	key       *ecdsa.PublicKey
	issuer    string
	audience  string
}

var hopByHop = map[string]bool{
	"Connection": true, "Keep-Alive": true, "Proxy-Authenticate": true, "Proxy-Authorization": true,
	"Proxy-Connection": true, "Te": true, "Trailer": true, "Transfer-Encoding": true, "Upgrade": true,
}

func (g *gateway) pick(tried []bool) int {
	n := int64(len(g.upstreams))
	start := g.next.Add(1)
	for i := int64(0); i < n; i++ {
		index := (start + i) % n
		if !tried[index] && g.upstreams[index].br.allow() {
			return int(index)
		}
	}
	return -1
}

func repeatable(method string) bool {
	switch method {
	case "GET", "HEAD", "OPTIONS", "PUT", "DELETE":
		return true
	}
	return false
}

func (g *gateway) forward(w http.ResponseWriter, r *http.Request, target, subject string) {
	body, err := io.ReadAll(r.Body)
	if err != nil {
		http.Error(w, "bad request", http.StatusBadRequest)
		return
	}
	attempts := 1
	if repeatable(r.Method) {
		attempts = 2
	}
	tried := make([]bool, len(g.upstreams))
	status, text := http.StatusServiceUnavailable, "no upstream available"
	for attempt := 0; attempt < attempts; attempt++ {
		index := g.pick(tried)
		if index < 0 {
			break
		}
		tried[index] = true
		chosen := g.upstreams[index]
		out, err := http.NewRequest(r.Method, "http://"+chosen.addr+target, bytes.NewReader(body))
		if err != nil {
			http.Error(w, "bad request", http.StatusBadRequest)
			return
		}
		for name, values := range r.Header {
			if !hopByHop[name] && name != "X-Auth-Subject" && !strings.HasPrefix(name, "X-Forwarded-") {
				out.Header[name] = values
			}
		}
		out.Header.Set("X-Forwarded-Host", r.Host)
		out.Header.Set("X-Forwarded-Proto", "http")
		if subject != "" {
			out.Header.Set("X-Auth-Subject", subject)
		}
		answer, err := g.client.Do(out)
		if err != nil {
			chosen.br.failure()
			chosen.bad.Add(1)
			status, text = http.StatusBadGateway, "bad gateway"
			continue
		}
		data, err := io.ReadAll(answer.Body)
		answer.Body.Close()
		if err != nil {
			chosen.br.failure()
			chosen.bad.Add(1)
			status, text = http.StatusBadGateway, "bad gateway"
			continue
		}
		if answer.StatusCode == 502 || answer.StatusCode == 503 || answer.StatusCode == 504 {
			chosen.br.failure()
			chosen.bad.Add(1)
			status, text = answer.StatusCode, string(data)
			continue
		}
		chosen.br.success()
		chosen.ok.Add(1)
		for name, values := range answer.Header {
			if !hopByHop[name] && name != "Content-Length" {
				w.Header()[name] = values
			}
		}
		w.WriteHeader(answer.StatusCode)
		w.Write(data)
		return
	}
	http.Error(w, text, status)
}

type claims struct {
	Sub string `json:"sub"`
	Iss string `json:"iss"`
	Aud string `json:"aud"`
	Exp int64  `json:"exp"`
}

func (g *gateway) verify(token string) (string, error) {
	parts := strings.Split(token, ".")
	if len(parts) != 3 {
		return "", errors.New("malformed")
	}
	sig, err := base64.RawURLEncoding.DecodeString(parts[2])
	if err != nil || len(sig) != 64 {
		return "", errors.New("bad signature")
	}
	digest := sha256.Sum256([]byte(parts[0] + "." + parts[1]))
	if !ecdsa.Verify(g.key, digest[:], new(big.Int).SetBytes(sig[:32]), new(big.Int).SetBytes(sig[32:])) {
		return "", errors.New("bad signature")
	}
	payload, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return "", err
	}
	var c claims
	if err := json.Unmarshal(payload, &c); err != nil {
		return "", err
	}
	if c.Iss != g.issuer || c.Aud != g.audience || c.Exp <= time.Now().Unix() {
		return "", errors.New("rejected")
	}
	return c.Sub, nil
}

func (g *gateway) serve(w http.ResponseWriter, r *http.Request) {
	path := r.URL.EscapedPath()
	subject := ""
	if path == "/auth" || strings.HasPrefix(path, "/auth/") {
		header := r.Header.Get("Authorization")
		if !strings.HasPrefix(header, "Bearer ") {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		sub, err := g.verify(header[7:])
		if err != nil {
			http.Error(w, "unauthorized", http.StatusUnauthorized)
			return
		}
		subject = sub
		path = strings.TrimPrefix(path, "/auth")
		if path == "" {
			path = "/"
		}
	}
	target := path
	if r.URL.RawQuery != "" {
		target += "?" + r.URL.RawQuery
	}
	g.forward(w, r, target, subject)
}

func fetchKey(url string) (*ecdsa.PublicKey, error) {
	response, err := http.Get(url)
	if err != nil {
		return nil, err
	}
	defer response.Body.Close()
	var set struct {
		Keys []struct {
			X string `json:"x"`
			Y string `json:"y"`
		} `json:"keys"`
	}
	if err := json.NewDecoder(response.Body).Decode(&set); err != nil {
		return nil, err
	}
	if len(set.Keys) == 0 {
		return nil, errors.New("empty key set")
	}
	x, err := base64.RawURLEncoding.DecodeString(set.Keys[0].X)
	if err != nil {
		return nil, err
	}
	y, err := base64.RawURLEncoding.DecodeString(set.Keys[0].Y)
	if err != nil {
		return nil, err
	}
	return &ecdsa.PublicKey{Curve: elliptic.P256(), X: new(big.Int).SetBytes(x), Y: new(big.Int).SetBytes(y)}, nil
}

func main() {
	if len(os.Args) < 4 {
		fmt.Fprintln(os.Stderr, "usage: gateway_go <port> <jwks url> <upstream>...")
		os.Exit(2)
	}
	key, err := fetchKey(os.Args[2])
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	g := &gateway{key: key, issuer: "example-idp", audience: "gateway"}
	for _, address := range os.Args[3:] {
		g.upstreams = append(g.upstreams, &upstream{addr: address})
	}
	g.client = &http.Client{
		Timeout: timeout,
		Transport: &http.Transport{
			DialContext:         (&net.Dialer{Timeout: time.Second}).DialContext,
			MaxIdleConnsPerHost: 256,
			MaxIdleConns:        0,
			IdleConnTimeout:     30 * time.Second,
			DisableCompression:  true,
		},
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
	}
	mux := http.NewServeMux()
	mux.HandleFunc("/healthz", func(w http.ResponseWriter, r *http.Request) { w.Write([]byte("ok")) })
	mux.HandleFunc("/", g.serve)
	server := &http.Server{Addr: ":" + os.Args[1], Handler: mux}
	if err := server.ListenAndServe(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
