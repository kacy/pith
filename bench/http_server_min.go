// minimal go counterpart to bench/http_server.pith: the same routes and body
// as bench/http_server.go, served by a hand-rolled http/1.1 keep-alive loop
// on net and bufio instead of net/http. it is the ceiling for what go can do
// with about as little per-request work as the pith server does: parse the
// request line, skip the headers to the blank line, answer with a status
// line, content-type and content-length. no date header, no header map, no
// timeouts. it frames get requests only: a content-length body is skipped,
// a chunked body is not understood.
//
// build with: go build -o bench/http_server_min bench/http_server_min.go
package main

import (
	"bufio"
	"bytes"
	"fmt"
	"net"
	"os"
	"strconv"
	"strings"
)

var kinds = []string{"widget", "gadget", "gizmo", "doodad", "contraption"}

// itemName and itemJSON are copied from bench/http_server.go so that each
// server builds as a single file; keep them in step.
func itemName(id int) string {
	return kinds[id%len(kinds)] + "-" + strconv.Itoa(id)
}

func itemJSON(id int) string {
	name := itemName(id)
	price := (id * 37) % 10000
	tags := strings.Split(name, "-")
	var b strings.Builder
	b.WriteString("{\"id\":" + strconv.Itoa(id))
	b.WriteString(",\"name\":\"" + name + "\"")
	b.WriteString(",\"price\":" + strconv.Itoa(price))
	b.WriteString(",\"tags\":[")
	for i, tag := range tags {
		if i > 0 {
			b.WriteString(",")
		}
		b.WriteString("\"" + tag + "\"")
	}
	b.WriteString("]}")
	return b.String()
}

// queryParam returns the value of name in a raw query string, or "".
func queryParam(query, name string) string {
	for query != "" {
		var part string
		part, query, _ = strings.Cut(query, "&")
		key, value, ok := strings.Cut(part, "=")
		if ok && key == name {
			return value
		}
	}
	return ""
}

var (
	headerContentLength = []byte("content-length:")
	headerConnection    = []byte("connection:")
)

// hasPrefixFold reports whether line starts with prefix, ignoring ascii case.
// prefix must be lowercase.
func hasPrefixFold(line, prefix []byte) bool {
	if len(line) < len(prefix) {
		return false
	}
	for i, c := range prefix {
		b := line[i]
		if 'A' <= b && b <= 'Z' {
			b += 'a' - 'A'
		}
		if b != c {
			return false
		}
	}
	return true
}

func writeResponse(w *bufio.Writer, status, contentType, body string) {
	w.WriteString("HTTP/1.1 ")
	w.WriteString(status)
	w.WriteString("\r\nContent-Type: ")
	w.WriteString(contentType)
	w.WriteString("\r\nContent-Length: ")
	w.WriteString(strconv.Itoa(len(body)))
	w.WriteString("\r\n\r\n")
	w.WriteString(body)
}

func route(w *bufio.Writer, method, target string) {
	path, query, _ := strings.Cut(target, "?")
	switch {
	case method == "GET" && path == "/item":
		id, _ := strconv.Atoi(queryParam(query, "id"))
		writeResponse(w, "200 OK", "application/json", itemJSON(id))
	case method == "GET" && path == "/health":
		writeResponse(w, "200 OK", "application/json", `{"ok":true}`)
	default:
		writeResponse(w, "404 Not Found", "text/plain", "404 page not found\n")
	}
}

func serve(conn net.Conn) {
	defer conn.Close()
	r := bufio.NewReaderSize(conn, 4096)
	w := bufio.NewWriterSize(conn, 4096)
	for {
		line, err := r.ReadSlice('\n')
		if err != nil {
			return
		}
		// the request line is "method target version"; copy the two fields
		// out before the next read reuses the reader's buffer.
		fields := bytes.Fields(line)
		if len(fields) < 2 {
			return
		}
		method := string(fields[0])
		target := string(fields[1])
		closeAfter := len(fields) > 2 && string(fields[2]) == "HTTP/1.0"
		bodyLen := 0
		for {
			h, err := r.ReadSlice('\n')
			if err != nil {
				return
			}
			h = bytes.TrimRight(h, "\r\n")
			if len(h) == 0 {
				break
			}
			if hasPrefixFold(h, headerContentLength) {
				bodyLen, _ = strconv.Atoi(string(bytes.TrimSpace(h[len(headerContentLength):])))
			} else if hasPrefixFold(h, headerConnection) {
				v := string(bytes.TrimSpace(h[len(headerConnection):]))
				closeAfter = strings.EqualFold(v, "close") || (closeAfter && !strings.EqualFold(v, "keep-alive"))
			}
		}
		if bodyLen > 0 {
			if _, err := r.Discard(bodyLen); err != nil {
				return
			}
		}
		route(w, method, target)
		if err := w.Flush(); err != nil || closeAfter {
			return
		}
	}
}

func main() {
	port := "8080"
	if len(os.Args) > 1 {
		port = os.Args[1]
	}
	ln, err := net.Listen("tcp", ":"+port)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	fmt.Println("listening on " + port)
	for {
		conn, err := ln.Accept()
		if err != nil {
			continue
		}
		go serve(conn)
	}
}
