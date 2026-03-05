.PHONY: build release test run clean fmt check run-examples self-host

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

run-examples: build
	@echo "--- deterministic examples (diff against expected output) ---"
	@fail=0; \
	for f in examples/expected/*.txt; do \
		name=$$(basename "$$f" .txt); \
		actual=$$(timeout 10 ./zig-out/bin/forge run "examples/$$name.fg" 2>/dev/null); \
		expected=$$(cat "$$f"); \
		if [ "$$actual" = "$$expected" ]; then \
			echo "ok   $$name"; \
		else \
			echo "FAIL $$name"; \
			echo "  expected:"; echo "$$expected" | head -5; \
			echo "  actual:"; echo "$$actual" | head -5; \
			fail=1; \
		fi; \
	done; \
	echo "--- non-deterministic examples (exit code only) ---"; \
	for name in stdlib_new file_io iteration operators; do \
		timeout 10 ./zig-out/bin/forge run "examples/$$name.fg" >/dev/null 2>&1; \
		if [ $$? -eq 0 ]; then \
			echo "ok   $$name"; \
		else \
			echo "FAIL $$name (non-zero exit)"; \
			fail=1; \
		fi; \
	done; \
	echo "--- test examples (forge test) ---"; \
	for name in test_example stdlib_new_test; do \
		timeout 10 ./zig-out/bin/forge test "examples/$$name.fg" >/dev/null 2>&1; \
		if [ $$? -eq 0 ]; then \
			echo "ok   $$name"; \
		else \
			echo "FAIL $$name (tests failed)"; \
			fail=1; \
		fi; \
	done; \
	if [ $$fail -eq 1 ]; then exit 1; fi; \
	echo "all examples passed"

self-host: build
	./zig-out/bin/forge build self-host/forge_main.fg
