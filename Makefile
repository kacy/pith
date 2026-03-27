.PHONY: build self-host bootstrap bootstrap-verify run-examples test clean

# --- primary build (Cranelift native backend) ---

build:
	cargo build --release

# build the self-hosted compiler using the Cranelift backend
self-host: build
	./target/release/forge build self-host/forge_main.fg

# rebuild the self-hosted compiler using the Cranelift-compiled version of itself
bootstrap: self-host
	@echo "--- stage 1: compile with current Cranelift binary ---"
	./target/release/forge build self-host/forge_main.fg
	@echo "--- stage 1 binary test ---"
	./self-host/forge_main version

# verify that the Cranelift-compiled compiler produces identical output
bootstrap-verify: self-host
	@echo "--- comparing Cranelift-compiled vs cargo-compiled on all examples ---"
	@pass=0; fail=0; \
	for f in examples/expected/*.txt; do \
		name=$$(basename "$$f" .txt); \
		actual=$$(timeout 15 ./self-host/forge_main run "examples/$$name.fg" 2>/dev/null | grep -v '^\[DEBUG\]'); \
		expected=$$(cat "$$f"); \
		if [ "$$actual" = "$$expected" ]; then \
			pass=$$((pass+1)); \
		else \
			echo "FAIL $$name"; \
			fail=$$((fail+1)); \
		fi; \
	done; \
	echo "$$pass passed, $$fail failed"; \
	if [ $$fail -gt 0 ]; then exit 1; fi; \
	echo "bootstrap verified"

# --- example validation ---

run-examples: build
	@echo "--- deterministic examples (Cranelift backend) ---"
	@pass=0; fail=0; \
	for f in examples/expected/*.txt; do \
		name=$$(basename "$$f" .txt); \
		actual=$$(timeout 15 ./target/release/forge run "examples/$$name.fg" 2>/dev/null); \
		expected=$$(cat "$$f"); \
		if [ "$$actual" = "$$expected" ]; then \
			pass=$$((pass+1)); \
			echo "ok   $$name"; \
		else \
			echo "FAIL $$name"; \
			fail=$$((fail+1)); \
		fi; \
	done; \
	echo "$$pass passed, $$fail failed"; \
	if [ $$fail -gt 0 ]; then exit 1; fi; \
	echo "all examples passed"

# --- full test suite ---

test: build
	@echo "=== Step 1: run all deterministic examples ==="
	@pass=0; fail=0; \
	for f in examples/expected/*.txt; do \
		name=$$(basename "$$f" .txt); \
		actual=$$(timeout 15 ./target/release/forge run "examples/$$name.fg" 2>/dev/null); \
		expected=$$(cat "$$f"); \
		if [ "$$actual" = "$$expected" ]; then \
			pass=$$((pass+1)); \
			echo "ok   $$name"; \
		else \
			echo "FAIL $$name"; \
			fail=$$((fail+1)); \
		fi; \
	done; \
	echo "$$pass passed, $$fail failed"; \
	if [ $$fail -gt 0 ]; then exit 1; fi
	@echo "=== Step 2: build self-hosted compiler via Cranelift ==="
	./target/release/forge build self-host/forge_main.fg
	@echo "=== Step 3: self-hosted compiler works ==="
	./self-host/forge_main version
	./self-host/forge_main lex examples/hello.fg > /dev/null
	./self-host/forge_main parse examples/hello.fg > /dev/null
	@echo "=== all tests passed ==="

clean:
	cargo clean
	rm -rf .forge-build
