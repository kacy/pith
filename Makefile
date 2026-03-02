.PHONY: build test run clean fmt check

build:
	zig build

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
