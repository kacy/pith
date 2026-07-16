#!/usr/bin/env bash
# build the go server + go client and the rust client for the grpc benchmark.
# the pith client needs no build step — run.sh runs it through the pith compiler.
# prerequisites: go, cargo, protoc.
set -euo pipefail
cd "$(dirname "$0")"
export PATH="$PATH:$(go env GOPATH)/bin"

# go grpc codegen plugins (installed on demand)
command -v protoc-gen-go      >/dev/null || go install google.golang.org/protobuf/cmd/protoc-gen-go@latest
command -v protoc-gen-go-grpc >/dev/null || go install google.golang.org/grpc/cmd/protoc-gen-go-grpc@latest

# regenerate the go stubs from the proto
protoc -I proto \
  --go_out=go      --go_opt=module=pithgrpcbench \
  --go-grpc_out=go --go-grpc_opt=module=pithgrpcbench \
  proto/echo.proto

# go server + client
( cd go && go mod tidy && go build -o ../bin/server ./server && go build -o ../bin/goclient ./client )

# rust (tonic) client
( cd rust && cargo build --release )

echo "built. run from the repo root:  bash bench/grpc/run.sh"
