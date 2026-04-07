.PHONY: build self-host bootstrap bootstrap-verify run-examples run-examples-self run-examples-self-only run-regressions run-regressions-only run-regressions-self run-regressions-self-only parity-examples parity-examples-only check-parse-invalid check-parse-invalid-only check-parse-invalid-self-host check-parse-invalid-self-host-only check-invalid check-invalid-only check-invalid-self-host check-invalid-self-host-only cli-regressions cli-regressions-only cli-regressions-self cli-regressions-self-only test clean

NONDETERMINISTIC_EXAMPLES := net_basics net_echo
EXPECTED_EXAMPLES := $(filter-out $(addprefix examples/expected/,$(addsuffix .txt,$(NONDETERMINISTIC_EXAMPLES))),$(wildcard examples/expected/*.txt))
REGRESSION_EXPECTED := $(wildcard tests/expected/*.txt)
PARSE_INVALID_EXAMPLES := $(wildcard tests/invalid_parse/*.fg)
INVALID_EXAMPLES := $(wildcard tests/invalid/*.fg)
PARITY_EXAMPLES := \
	hello \
	control_flow \
	structs \
	collection_methods \
	generics \
	lambdas \
	error_handling \
	matrix_math \
	self_host_patterns \
	wildcard_import

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
	@echo "--- verifying self-hosted compiler on deterministic examples ---"
	@$(MAKE) --no-print-directory run-examples-self-only
	@echo "--- verifying self-hosted compiler on regression cases ---"
	@$(MAKE) --no-print-directory run-regressions-self-only
	echo "bootstrap verified"

# --- example validation ---

run-examples: build
	@echo "--- deterministic examples (Cranelift backend) ---"
	@pass=0; fail=0; \
	for f in $(EXPECTED_EXAMPLES); do \
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

run-examples-self: self-host run-examples-self-only

run-examples-self-only:
	@echo "--- deterministic examples (self-hosted compiler) ---"
	@pass=0; fail=0; \
	for f in $(EXPECTED_EXAMPLES); do \
		name=$$(basename "$$f" .txt); \
		actual=$$(timeout 15 ./self-host/forge_main run "examples/$$name.fg" 2>/dev/null); \
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
	echo "all self-hosted examples passed"

run-regressions: build run-regressions-only

run-regressions-only:
	@echo "--- regression cases (Cranelift backend) ---"
	@pass=0; fail=0; \
	for f in $(REGRESSION_EXPECTED); do \
		name=$$(basename "$$f" .txt); \
		actual=$$(timeout 15 ./target/release/forge run "tests/cases/$$name.fg" 2>/dev/null); \
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
	echo "all regression cases passed"

run-regressions-self: self-host run-regressions-self-only

run-regressions-self-only:
	@echo "--- regression cases (self-hosted compiler) ---"
	@pass=0; fail=0; \
	for f in $(REGRESSION_EXPECTED); do \
		name=$$(basename "$$f" .txt); \
		actual=$$(timeout 15 ./self-host/forge_main run "tests/cases/$$name.fg" 2>/dev/null); \
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
	echo "all self-hosted regression cases passed"

parity-examples: self-host parity-examples-only

parity-examples-only:
	@echo "--- native vs self-host parity examples ---"
	@pass=0; fail=0; \
	for name in $(PARITY_EXAMPLES); do \
		expected_file="examples/expected/$$name.txt"; \
		if [ ! -f "$$expected_file" ]; then \
			echo "FAIL $$name (missing $$expected_file)"; \
			fail=$$((fail+1)); \
			continue; \
		fi; \
		native=$$(timeout 15 ./target/release/forge run "examples/$$name.fg" 2>/dev/null); \
		self_host=$$(timeout 15 ./self-host/forge_main run "examples/$$name.fg" 2>/dev/null); \
		expected=$$(cat "$$expected_file"); \
		if [ "$$native" = "$$self_host" ] && [ "$$native" = "$$expected" ]; then \
			pass=$$((pass+1)); \
			echo "ok   $$name"; \
		else \
			echo "FAIL $$name"; \
			if [ "$$native" != "$$self_host" ]; then \
				echo "native/self-host mismatch"; \
			else \
				echo "output mismatch vs expected"; \
			fi; \
			fail=$$((fail+1)); \
		fi; \
	done; \
	echo "$$pass passed, $$fail failed"; \
	if [ $$fail -gt 0 ]; then exit 1; fi; \
	echo "all parity examples passed"

check-parse-invalid: build check-parse-invalid-only

check-parse-invalid-only:
	@echo "--- invalid parse examples (parser diagnostics) ---"
	@pass=0; fail=0; \
	for f in $(PARSE_INVALID_EXAMPLES); do \
		name=$$(basename "$$f" .fg); \
		expected_file="tests/invalid_parse/expected/$$name.codes"; \
		if [ ! -f "$$expected_file" ]; then \
			echo "FAIL $$name (missing $$expected_file)"; \
			fail=$$((fail+1)); \
			continue; \
		fi; \
		set +e; \
		output=$$(timeout 15 ./target/release/forge parse "$$f" 2>&1); \
		status=$$?; \
		set -e; \
		if [ $$status -eq 0 ]; then \
			echo "FAIL $$name (unexpected success)"; \
			fail=$$((fail+1)); \
			continue; \
		fi; \
		actual=$$(printf "%s\n" "$$output" | grep -o 'E[0-9][0-9][0-9]' | sort -u || true); \
		expected=$$(sort "$$expected_file"); \
		if [ "$$actual" = "$$expected" ]; then \
			pass=$$((pass+1)); \
			echo "ok   $$name"; \
		else \
			echo "FAIL $$name"; \
			echo "expected:"; \
			printf "%s\n" "$$expected"; \
			echo "actual:"; \
			printf "%s\n" "$$actual"; \
			fail=$$((fail+1)); \
		fi; \
	done; \
	echo "$$pass passed, $$fail failed"; \
	if [ $$fail -gt 0 ]; then exit 1; fi; \
	echo "all invalid parse examples passed"

check-parse-invalid-self-host: self-host check-parse-invalid-self-host-only

check-parse-invalid-self-host-only:
	@echo "--- invalid parse examples (self-hosted parser diagnostics) ---"
	@pass=0; fail=0; \
	for f in $(PARSE_INVALID_EXAMPLES); do \
		name=$$(basename "$$f" .fg); \
		expected_file="tests/invalid_parse/expected/$$name.codes"; \
		if [ ! -f "$$expected_file" ]; then \
			echo "FAIL $$name (missing $$expected_file)"; \
			fail=$$((fail+1)); \
			continue; \
		fi; \
		set +e; \
		output=$$(timeout 15 ./self-host/forge_main parse "$$f" 2>&1); \
		status=$$?; \
		set -e; \
		if [ $$status -eq 0 ]; then \
			echo "FAIL $$name (unexpected success)"; \
			fail=$$((fail+1)); \
			continue; \
		fi; \
		actual=$$(printf "%s\n" "$$output" | grep -o 'E[0-9][0-9][0-9]' | sort -u || true); \
		expected=$$(sort "$$expected_file"); \
		if [ "$$actual" = "$$expected" ]; then \
			pass=$$((pass+1)); \
			echo "ok   $$name"; \
		else \
			echo "FAIL $$name"; \
			echo "expected:"; \
			printf "%s\n" "$$expected"; \
			echo "actual:"; \
			printf "%s\n" "$$actual"; \
			fail=$$((fail+1)); \
		fi; \
	done; \
	echo "$$pass passed, $$fail failed"; \
	if [ $$fail -gt 0 ]; then exit 1; fi; \
	echo "all self-host invalid parse examples passed"

check-invalid: build check-invalid-only

check-invalid-only:
	@echo "--- invalid examples (checker diagnostics) ---"
	@pass=0; fail=0; \
	for f in $(INVALID_EXAMPLES); do \
		name=$$(basename "$$f" .fg); \
		expected_file="tests/invalid/expected/$$name.codes"; \
		if [ ! -f "$$expected_file" ]; then \
			echo "FAIL $$name (missing $$expected_file)"; \
			fail=$$((fail+1)); \
			continue; \
		fi; \
		set +e; \
		output=$$(timeout 15 ./target/release/forge check "$$f" 2>&1); \
		status=$$?; \
		set -e; \
		if [ $$status -eq 0 ]; then \
			echo "FAIL $$name (unexpected success)"; \
			fail=$$((fail+1)); \
			continue; \
		fi; \
		actual=$$(printf "%s\n" "$$output" | grep -o 'E[0-9][0-9][0-9]' | sort -u || true); \
		expected=$$(sort "$$expected_file"); \
		if [ "$$actual" = "$$expected" ]; then \
			pass=$$((pass+1)); \
			echo "ok   $$name"; \
		else \
			echo "FAIL $$name"; \
			echo "expected:"; \
			printf "%s\n" "$$expected"; \
			echo "actual:"; \
			printf "%s\n" "$$actual"; \
			fail=$$((fail+1)); \
		fi; \
	done; \
	echo "$$pass passed, $$fail failed"; \
	if [ $$fail -gt 0 ]; then exit 1; fi; \
	echo "all invalid examples passed"

check-invalid-self-host: self-host check-invalid-self-host-only

check-invalid-self-host-only:
	@echo "--- invalid examples (self-hosted checker diagnostics) ---"
	@pass=0; fail=0; \
	for f in $(INVALID_EXAMPLES); do \
		name=$$(basename "$$f" .fg); \
		expected_file="tests/invalid/expected/$$name.codes"; \
		if [ ! -f "$$expected_file" ]; then \
			echo "FAIL $$name (missing $$expected_file)"; \
			fail=$$((fail+1)); \
			continue; \
		fi; \
		set +e; \
		output=$$(timeout 15 ./self-host/forge_main check "$$f" 2>&1); \
		status=$$?; \
		set -e; \
		if [ $$status -eq 0 ]; then \
			echo "FAIL $$name (unexpected success)"; \
			fail=$$((fail+1)); \
			continue; \
		fi; \
		actual=$$(printf "%s\n" "$$output" | grep -o 'E[0-9][0-9][0-9]' | sort -u || true); \
		expected=$$(sort "$$expected_file"); \
		if [ "$$actual" = "$$expected" ]; then \
			pass=$$((pass+1)); \
			echo "ok   $$name"; \
		else \
			echo "FAIL $$name"; \
			echo "expected:"; \
			printf "%s\n" "$$expected"; \
			echo "actual:"; \
			printf "%s\n" "$$actual"; \
			fail=$$((fail+1)); \
		fi; \
	done; \
	echo "$$pass passed, $$fail failed"; \
	if [ $$fail -gt 0 ]; then exit 1; fi; \
	echo "all self-host invalid examples passed"

# --- cli regressions ---

cli-regressions: build cli-regressions-only

cli-regressions-only:
	@echo "--- cli regressions (native) ---"
	@tmpdir=$$(mktemp -d /tmp/forge-cli-regressions-XXXXXX); \
	trap 'rm -rf "$$tmpdir"' EXIT; \
	printf 'fn main() -> Int!:\n    return missing_name\n' > "$$tmpdir/bad.fg"; \
	printf 'test "broken":\n    assert_eq(1 + 1, 3)\n' > "$$tmpdir/fail_test.fg"; \
	pass=0; fail=0; \
	set +e; \
	./target/release/forge >/dev/null 2>&1; \
	status=$$?; \
	set -e; \
	if [ $$status -ne 0 ]; then pass=$$((pass+1)); echo "ok   no args fail"; else echo "FAIL no args fail"; fail=$$((fail+1)); fi; \
	set +e; \
	./target/release/forge run "$$tmpdir/bad.fg" >/dev/null 2>&1; \
	status=$$?; \
	set -e; \
	if [ $$status -ne 0 ]; then pass=$$((pass+1)); echo "ok   run compile failure"; else echo "FAIL run compile failure"; fail=$$((fail+1)); fi; \
	set +e; \
	./target/release/forge build "$$tmpdir/bad.fg" >/dev/null 2>&1; \
	status=$$?; \
	set -e; \
	if [ $$status -ne 0 ]; then pass=$$((pass+1)); echo "ok   build compile failure"; else echo "FAIL build compile failure"; fail=$$((fail+1)); fi; \
	set +e; \
	./target/release/forge check "$$tmpdir/bad.fg" >/dev/null 2>&1; \
	status=$$?; \
	set -e; \
	if [ $$status -ne 0 ]; then pass=$$((pass+1)); echo "ok   check failure"; else echo "FAIL check failure"; fail=$$((fail+1)); fi; \
	set +e; \
	./target/release/forge test tests/cases/test_test_declarations.fg >/dev/null 2>&1; \
	status=$$?; \
	set -e; \
	if [ $$status -eq 0 ]; then pass=$$((pass+1)); echo "ok   test declarations pass"; else echo "FAIL test declarations pass"; fail=$$((fail+1)); fi; \
	set +e; \
	./target/release/forge test "$$tmpdir/fail_test.fg" >/dev/null 2>&1; \
	status=$$?; \
	set -e; \
	if [ $$status -ne 0 ]; then pass=$$((pass+1)); echo "ok   test declarations fail"; else echo "FAIL test declarations fail"; fail=$$((fail+1)); fi; \
	echo "$$pass passed, $$fail failed"; \
	if [ $$fail -gt 0 ]; then exit 1; fi; \
	echo "all native cli regressions passed"

cli-regressions-self: self-host cli-regressions-self-only

cli-regressions-self-only:
	@echo "--- cli regressions (self-hosted wrapper) ---"
	@tmpdir=$$(mktemp -d /tmp/forge-cli-regressions-self-XXXXXX); \
	trap 'rm -rf "$$tmpdir"' EXIT; \
	printf 'fn main() -> Int!:\n    return missing_name\n' > "$$tmpdir/bad.fg"; \
	printf 'test "broken":\n    assert_eq(1 + 1, 3)\n' > "$$tmpdir/fail_test.fg"; \
	pass=0; fail=0; \
	set +e; \
	./self-host/forge_main run "$$tmpdir/bad.fg" >/dev/null 2>&1; \
	status=$$?; \
	set -e; \
	if [ $$status -ne 0 ]; then pass=$$((pass+1)); echo "ok   run compile failure"; else echo "FAIL run compile failure"; fail=$$((fail+1)); fi; \
	set +e; \
	./self-host/forge_main check "$$tmpdir/bad.fg" >/dev/null 2>&1; \
	status=$$?; \
	set -e; \
	if [ $$status -ne 0 ]; then pass=$$((pass+1)); echo "ok   check failure"; else echo "FAIL check failure"; fail=$$((fail+1)); fi; \
	set +e; \
	./self-host/forge_main test tests/cases/test_test_declarations.fg >/dev/null 2>&1; \
	status=$$?; \
	set -e; \
	if [ $$status -eq 0 ]; then pass=$$((pass+1)); echo "ok   test declarations pass"; else echo "FAIL test declarations pass"; fail=$$((fail+1)); fi; \
	set +e; \
	./self-host/forge_main test "$$tmpdir/fail_test.fg" >/dev/null 2>&1; \
	status=$$?; \
	set -e; \
	if [ $$status -ne 0 ]; then pass=$$((pass+1)); echo "ok   test declarations fail"; else echo "FAIL test declarations fail"; fail=$$((fail+1)); fi; \
	echo "$$pass passed, $$fail failed"; \
	if [ $$fail -gt 0 ]; then exit 1; fi; \
	echo "all self-host cli regressions passed"

# --- full test suite ---

test: build
	@echo "=== Step 1: run all deterministic examples ==="
	@pass=0; fail=0; \
	for f in $(EXPECTED_EXAMPLES); do \
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
	@echo "=== Step 2: run regression cases ==="
	@$(MAKE) --no-print-directory run-regressions-only
	@echo "=== Step 3: run invalid parse examples ==="
	@$(MAKE) --no-print-directory check-parse-invalid-only
	@echo "=== Step 4: run invalid checker examples ==="
	@$(MAKE) --no-print-directory check-invalid-only
	@echo "=== Step 5: run cli regressions ==="
	@$(MAKE) --no-print-directory cli-regressions-only
	@echo "=== Step 6: build self-hosted compiler via Cranelift ==="
	./target/release/forge build self-host/forge_main.fg
	@echo "=== Step 7: run regression cases through self-hosted compiler ==="
	@$(MAKE) --no-print-directory run-regressions-self-only
	@echo "=== Step 8: run invalid parse examples through self-hosted parser ==="
	@$(MAKE) --no-print-directory check-parse-invalid-self-host-only
	@echo "=== Step 9: compare native and self-hosted example outputs ==="
	@$(MAKE) --no-print-directory parity-examples-only
	@echo "=== Step 10: run invalid examples through self-hosted checker ==="
	@$(MAKE) --no-print-directory check-invalid-self-host-only
	@echo "=== Step 11: run self-host cli regressions ==="
	@$(MAKE) --no-print-directory cli-regressions-self-only
	@echo "=== Step 12: self-hosted compiler works ==="
	./self-host/forge_main version
	./self-host/forge_main lex examples/hello.fg > /dev/null
	./self-host/forge_main parse examples/hello.fg > /dev/null
	@echo "=== all tests passed ==="

clean:
	cargo clean
	rm -rf .forge-build
