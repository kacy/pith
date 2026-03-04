.PHONY: build release test run clean fmt check self-host

build:
	zig build

release:
	zig build release

check: build
	@for f in examples/*.fg; do \
		result=$$(./zig-out/bin/forge check "$$f" 2>&1); \
		if [ "$$result" != "ok" ]; then \
			echo "FAIL $$f"; echo "$$result"; exit 1; \
		fi; \
		echo "ok   $$f"; \
	done

test:
	zig build test

run:
	zig build run

clean:
	rm -rf .zig-cache zig-out

fmt:
	zig fmt src/

self-host: build
	./zig-out/bin/forge build self-host/forge_main.fg
