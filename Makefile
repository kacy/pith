.PHONY: build test run clean fmt check

build:
	zig build

check: build

test:
	zig build test

run:
	zig build run

clean:
	rm -rf .zig-cache zig-out

fmt:
	zig fmt src/
