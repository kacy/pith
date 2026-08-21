// A minimal TLS peer used to interop-test the pith TLS stack against Go's
// crypto/tls. It acts as either a server (accept one connection, echo one
// message) or a client (connect, send "hi", print the negotiated version and
// cipher, read the echo). Versions are pinned so the test can drive 1.2 and 1.3
// explicitly.
package main

import (
	"crypto/tls"
	"crypto/x509"
	"fmt"
	"io"
	"os"
)

func ver(s string) uint16 {
	switch s {
	case "1.2":
		return tls.VersionTLS12
	case "1.3":
		return tls.VersionTLS13
	}
	return 0
}

func verName(v uint16) string {
	switch v {
	case tls.VersionTLS12:
		return "tls1.2"
	case tls.VersionTLS13:
		return "tls1.3"
	}
	return "unknown"
}

func main() {
	if len(os.Args) < 2 {
		fmt.Println("usage: tls_peer server|client ...")
		os.Exit(2)
	}
	switch os.Args[1] {
	case "server":
		// server <port> <cert> <key> <minver> <maxver>
		cert, err := tls.LoadX509KeyPair(os.Args[3], os.Args[4])
		if err != nil {
			fmt.Println("cert-load-error:", err)
			os.Exit(1)
		}
		cfg := &tls.Config{Certificates: []tls.Certificate{cert}, MinVersion: ver(os.Args[5]), MaxVersion: ver(os.Args[6])}
		ln, err := tls.Listen("tcp", "127.0.0.1:"+os.Args[2], cfg)
		if err != nil {
			fmt.Println("listen-error:", err)
			os.Exit(1)
		}
		conn, err := ln.Accept()
		if err != nil {
			os.Exit(1)
		}
		buf := make([]byte, 64)
		n, _ := conn.Read(buf)
		conn.Write([]byte("echo:" + string(buf[:n])))
		conn.Close()
	case "client":
		// client <host:port> <cafile> <servername> <minver> <maxver>
		ca, err := os.ReadFile(os.Args[3])
		if err != nil {
			fmt.Println("ca-read-error:", err)
			os.Exit(1)
		}
		pool := x509.NewCertPool()
		if !pool.AppendCertsFromPEM(ca) {
			fmt.Println("ca-parse-error")
			os.Exit(1)
		}
		cfg := &tls.Config{RootCAs: pool, ServerName: os.Args[4], MinVersion: ver(os.Args[5]), MaxVersion: ver(os.Args[6])}
		conn, err := tls.Dial("tcp", os.Args[2], cfg)
		if err != nil {
			fmt.Println("dial-error:", err)
			os.Exit(1)
		}
		st := conn.ConnectionState()
		conn.Write([]byte("hi"))
		buf := make([]byte, 64)
		n, _ := conn.Read(buf)
		if err != nil && err != io.EOF {
			fmt.Println("read-error:", err)
			os.Exit(1)
		}
		fmt.Printf("%s %s %s\n", verName(st.Version), tls.CipherSuiteName(st.CipherSuite), string(buf[:n]))
		conn.Close()
	default:
		fmt.Println("unknown mode")
		os.Exit(2)
	}
}
