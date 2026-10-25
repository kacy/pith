// go counterpart to bench/http_head_read.pith: http request-head reading,
// isolated from the socket. the same stream of keepalive GETs sits in memory,
// a bufio.Reader of 4096 bytes reads it (the pith arm chunks its reader the
// same way), and net/http.ReadRequest, the request reader net/http's server
// uses, parses one request per call. the checksum folds the head length and
// the method of every request, as the pith arm does.
//
// ReadRequest does more per request than the pith reader: it canonicalizes
// header names into a map and parses the target into a url.URL. that is the
// work a go server pays per request, so it is what this arm measures.
//
// build with: go build -o bench/http_head_read_go bench/http_head_read.go
// run with:   ./bench/http_head_read_go <requests>
package main

import (
	"bufio"
	"bytes"
	"fmt"
	"net/http"
	"os"
	"strconv"
	"time"
)

func requestsFromArgs() int {
	if len(os.Args) > 1 {
		if n, err := strconv.Atoi(os.Args[1]); err == nil && n > 0 {
			return n
		}
	}
	return 20000
}

func buildStream(count int) []byte {
	var out bytes.Buffer
	for i := 0; i < count; i++ {
		// the id varies so no two heads are byte-identical
		out.WriteString("GET /item?id=" + strconv.Itoa(12345+i%1000) + " HTTP/1.1\r\n")
		out.WriteString("Host: x\r\n")
		out.WriteString("Connection: keep-alive\r\n")
		out.WriteString("\r\n")
	}
	return out.Bytes()
}

func main() {
	count := requestsFromArgs()
	stream := buildStream(count)
	src := bytes.NewReader(stream)
	reader := bufio.NewReaderSize(src, 4096)
	// how far into the stream the reader has handed bytes out
	consumed := func() int { return len(stream) - src.Len() - reader.Buffered() }
	start := time.Now()
	checksum := 0
	read := 0
	for read < count {
		before := consumed()
		req, err := http.ReadRequest(reader)
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		checksum += consumed() - before + len(req.Method)
		read++
	}
	elapsed := time.Since(start).Milliseconds()
	fmt.Println("http head read benchmark")
	fmt.Println("requests=" + strconv.Itoa(read))
	fmt.Println("elapsed_ms=" + strconv.FormatInt(elapsed, 10))
	fmt.Println("checksum=" + strconv.Itoa(checksum))
}
