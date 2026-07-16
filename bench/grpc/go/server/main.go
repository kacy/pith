// a tls grpc echo server shared by all three benchmark clients (pith, go, rust).
// it serves the localhost fixture cert so every client can trust one ca.
package main

import (
	"context"
	"flag"
	"log"
	"net"

	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"

	"pithgrpcbench/echopb"
)

type echoServer struct {
	echopb.UnimplementedEchoServer
}

func (s *echoServer) Unary(ctx context.Context, req *echopb.EchoRequest) (*echopb.EchoResponse, error) {
	return &echopb.EchoResponse{Payload: req.Payload}, nil
}

func main() {
	addr := flag.String("addr", "127.0.0.1:50051", "listen address")
	cert := flag.String("cert", "certs/localhost.crt", "server certificate")
	key := flag.String("key", "certs/localhost.key", "server private key")
	flag.Parse()

	creds, err := credentials.NewServerTLSFromFile(*cert, *key)
	if err != nil {
		log.Fatalf("load tls: %v", err)
	}

	lis, err := net.Listen("tcp", *addr)
	if err != nil {
		log.Fatalf("listen: %v", err)
	}

	s := grpc.NewServer(grpc.Creds(creds))
	echopb.RegisterEchoServer(s, &echoServer{})
	log.Printf("echo server listening on %s", *addr)
	if err := s.Serve(lis); err != nil {
		log.Fatalf("serve: %v", err)
	}
}
