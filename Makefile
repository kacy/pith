.PHONY: build release test run clean fmt check run-examples self-host bootstrap bootstrap-verify

# --- zig bootstrap (will be retired) ---

build:
	zig build

release:
	zig build release

test:
	zig build test

run:
	zig build run

clean:
	rm -rf .zig-cache zig-out

fmt:
	zig fmt bootstrap/

check: build
	@for f in examples/*.fg; do \
		result=$$(./zig-out/bin/forge check "$$f" 2>&1); \
		if [ "$$result" != "ok" ]; then \
			echo "FAIL $$f"; echo "$$result"; exit 1; \
		fi; \
		echo "ok   $$f"; \
	done

# --- self-hosted compiler ---

# build the self-hosted compiler using the zig bootstrap
self-host: build
	./zig-out/bin/forge build self-host/forge_main.fg

# rebuild the self-hosted compiler using itself (fixed-point bootstrap)
bootstrap:
	@echo "--- bootstrapping self-hosted compiler ---"
	@cp self-host/.forge-build/forge_main.c /tmp/forge_pre_bootstrap.c 2>/dev/null || true
	./self-host/forge_main build self-host/forge_main.fg
	@echo "--- verifying fixed point ---"
	@if diff -q /tmp/forge_pre_bootstrap.c self-host/.forge-build/forge_main.c >/dev/null 2>&1; then \
		echo "fixed point reached — C output identical"; \
	else \
		echo "warning: C output differs (expected on first bootstrap)"; \
	fi
	@echo "bootstrap complete: self-host/forge_main"

# verify that the self-hosted compiler reaches a fixed point
bootstrap-verify:
	@echo "--- stage 1: compile with current binary ---"
	./self-host/forge_main build self-host/forge_main.fg
	cp self-host/.forge-build/forge_main.c /tmp/forge_stage1.c
	@echo "--- stage 2: compile with newly built binary ---"
	./self-host/forge_main build self-host/forge_main.fg
	cp self-host/.forge-build/forge_main.c /tmp/forge_stage2.c
	@echo "--- comparing stages ---"
	@diff /tmp/forge_stage1.c /tmp/forge_stage2.c && echo "fixed point verified!" || (echo "FAILED: output differs between stages"; exit 1)

# --- example validation ---

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
	for name in stdlib_new file_io iteration operators fs_ops time_rand tcp_echo channels process_ops; do \
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

# run examples using the self-hosted compiler
run-examples-self: self-host
	@echo "--- deterministic examples (self-hosted) ---"
	@fail=0; \
	for f in examples/expected/*.txt; do \
		name=$$(basename "$$f" .txt); \
		actual=$$(timeout 15 ./self-host/forge_main run "examples/$$name.fg" 2>/dev/null); \
		expected=$$(cat "$$f"); \
		if [ "$$actual" = "$$expected" ]; then \
			echo "ok   $$name"; \
		else \
			echo "FAIL $$name"; \
			fail=1; \
		fi; \
	done; \
	echo "--- non-deterministic examples (self-hosted, exit code only) ---"; \
	for name in stdlib_new file_io iteration operators fs_ops time_rand tcp_echo channels process_ops; do \
		timeout 15 ./self-host/forge_main run "examples/$$name.fg" >/dev/null 2>&1; \
		if [ $$? -eq 0 ]; then \
			echo "ok   $$name"; \
		else \
			echo "FAIL $$name (non-zero exit)"; \
			fail=1; \
		fi; \
	done; \
	echo "--- test examples (self-hosted) ---"; \
	for name in test_example stdlib_new_test; do \
		timeout 15 ./self-host/forge_main test "examples/$$name.fg" >/dev/null 2>&1; \
		if [ $$? -eq 0 ]; then \
			echo "ok   $$name"; \
		else \
			echo "FAIL $$name (tests failed)"; \
			fail=1; \
		fi; \
	done; \
	if [ $$fail -eq 1 ]; then exit 1; fi; \
	echo "all examples passed (self-hosted)"
