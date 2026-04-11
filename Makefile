.PHONY: build self-host bootstrap bootstrap-verify bootstrap-ir-checks bootstrap-ir-checks-only bootstrap-ir-fixed-point bootstrap-ir-fixed-point-only bootstrap-ir-invariants bootstrap-ir-invariants-only run-examples run-examples-self run-examples-self-only run-regressions run-regressions-only run-regressions-self run-regressions-self-only run-live-websocket-tests run-live-websocket-tests-self-only parity-examples parity-examples-only check-parse-invalid check-parse-invalid-only check-parse-invalid-self-host check-parse-invalid-self-host-only check-invalid check-invalid-only check-invalid-self-host check-invalid-self-host-only cli-regressions cli-regressions-only cli-regressions-self cli-regressions-self-only ir-contract-regressions ir-contract-regressions-only test clean

NONDETERMINISTIC_EXAMPLES := net_basics net_echo
EXPECTED_EXAMPLES := $(filter-out $(addprefix examples/expected/,$(addsuffix .txt,$(NONDETERMINISTIC_EXAMPLES))),$(wildcard examples/expected/*.txt))
REGRESSION_EXPECTED := $(wildcard tests/expected/*.txt)
LIVE_WEBSOCKET_EXPECTED := $(wildcard tests/live/expected/*.txt)
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

IR_FIXED_POINT_SOURCES := \
	examples/hello.fg \
	examples/concurrency.fg \
	tests/cases/test_suite.fg \
	tests/cases/test_imported_globals_init.fg \
	tests/cases/test_module_alias_calls.fg \
	tests/cases/test_imported_io_methods.fg \
	tests/cases/test_io_file_streams.fg \
	tests/cases/test_http_request_bytes.fg \
	tests/cases/test_http_websocket_app.fg \
	tests/cases/test_websocket_wire.fg

BOOTSTRAP_IR_REBUILD_TARGETS := \
	self-host/forge_main.fg \
	self-host/ir_driver.fg

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
	@$(MAKE) --no-print-directory bootstrap-ir-checks-only
	echo "bootstrap verified"

# keep the ir hardening checks grouped so bootstrap drift is easy to spot
bootstrap-ir-checks: self-host bootstrap-ir-checks-only

bootstrap-ir-checks-only:
	@echo "--- verifying combined ir contract ---"
	@$(MAKE) --no-print-directory ir-contract-regressions-only
	@echo "--- verifying combined ir invariants ---"
	@$(MAKE) --no-print-directory bootstrap-ir-invariants-only
	@echo "--- verifying ir fixed point on deterministic corpus ---"
	@$(MAKE) --no-print-directory bootstrap-ir-fixed-point-only

bootstrap-ir-invariants: self-host bootstrap-ir-invariants-only

bootstrap-ir-invariants-only:
	@echo "--- combined ir invariant checks ---"
	@pass=0; fail=0; \
	if timeout 15 ./self-host/ir_driver --combined tests/cases/test_imported_globals_init.fg | awk 'BEGIN { init=0; call=0 } /^func [A-Za-z0-9_]+___init_globals_[0-9]+(_[0-9]+)? / { init=1 } /^call 900000 [A-Za-z0-9_]+___init_globals_[0-9]+(_[0-9]+)? int 0/ { call=1 } END { if (init && call) exit 0; exit 1 }'; then \
		pass=$$((pass+1)); echo "ok   imported init globals wiring"; \
	else \
		echo "FAIL imported init globals wiring"; fail=$$((fail+1)); \
	fi; \
	if timeout 15 ./self-host/ir_driver --combined examples/concurrency.fg | awk 'BEGIN { m=0; w=0; s=0 } /^call / && $$3=="Mutex" && $$4=="opaque:Mutex" { m=1 } /^call / && $$3=="WaitGroup" && $$4=="opaque:WaitGroup" { w=1 } /^call / && $$3=="Semaphore" && $$4=="opaque:Semaphore" { s=1 } END { if (m && w && s) exit 0; exit 1 }'; then \
		pass=$$((pass+1)); echo "ok   sync primitive retkind invariants"; \
	else \
		echo "FAIL sync primitive retkind invariants"; fail=$$((fail+1)); \
	fi; \
	if timeout 15 ./self-host/ir_driver --combined examples/generic_interfaces.fg | awk 'BEGIN { value=0; x=0; bad=0 } /^field / && NF==4 { bad=1 } /^field / && $$5=="T" && $$6=="value" { value=1 } /^field / && $$5=="int" && $$6=="x" { x=1 } END { if (value && x && !bad) exit 0; exit 1 }'; then \
		pass=$$((pass+1)); echo "ok   imported struct field metadata"; \
	else \
		echo "FAIL imported struct field metadata"; fail=$$((fail+1)); \
	fi; \
	if timeout 15 ./self-host/ir_driver --combined tests/cases/test_websocket_session.fg | awk 'BEGIN { ok=0 } /^call / && $$4 ~ /^struct:/ { ok=1 } /^call / && $$4 ~ /^[A-Z]/ { bad=1 } END { if (ok && !bad) exit 0; exit 1 }'; then \
		pass=$$((pass+1)); echo "ok   explicit struct call retkinds"; \
	else \
		echo "FAIL explicit struct call retkinds"; fail=$$((fail+1)); \
	fi; \
	echo "$$pass passed, $$fail failed"; \
	if [ $$fail -gt 0 ]; then exit 1; fi; \
	echo "all combined ir invariant checks passed"

bootstrap-ir-fixed-point: self-host bootstrap-ir-fixed-point-only

bootstrap-ir-fixed-point-only:
	@echo "--- bootstrap ir fixed point ---"
	@tmpdir=$$(mktemp -d /tmp/forge-ir-fixed-point-XXXXXX); \
	pass=0; fail=0; \
	trap 'rm -rf "$$tmpdir"' EXIT; \
	if [ ! -x ./self-host/ir_driver ]; then \
		timeout 120 ./target/release/forge build self-host/ir_driver.fg >/dev/null; \
	fi; \
	for target in $(BOOTSTRAP_IR_REBUILD_TARGETS); do \
		timeout 120 ./self-host/forge_main build "$$target" >/dev/null; \
	done; \
	cp ./self-host/forge_main "$$tmpdir/forge_main_stage1"; \
	cp ./self-host/ir_driver "$$tmpdir/ir_driver_stage1"; \
	for target in $(BOOTSTRAP_IR_REBUILD_TARGETS); do \
		timeout 120 ./self-host/forge_main build "$$target" >/dev/null; \
	done; \
	for src in $(IR_FIXED_POINT_SOURCES); do \
		stage1=$$(timeout 20 "$$tmpdir/ir_driver_stage1" --combined "$$src" 2>/dev/null); \
		stage1_status=$$?; \
		stage2=$$(timeout 20 ./self-host/ir_driver --combined "$$src" 2>/dev/null); \
		stage2_status=$$?; \
		if [ $$stage1_status -eq 0 ] && [ $$stage2_status -eq 0 ] && [ "$$stage1" = "$$stage2" ]; then \
			pass=$$((pass+1)); \
			echo "ok   $$src"; \
		else \
			echo "FAIL $$src"; \
			fail=$$((fail+1)); \
		fi; \
	done; \
	echo "$$pass passed, $$fail failed"; \
	if [ $$fail -gt 0 ]; then exit 1; fi; \
	echo "bootstrap ir fixed point verified"

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
		actual=$$(timeout 60 ./self-host/forge_main run "examples/$$name.fg" 2>/dev/null); \
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
		actual=$$(timeout 60 ./self-host/forge_main run "tests/cases/$$name.fg" 2>/dev/null); \
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

run-live-websocket-tests: build
	@echo "--- live websocket smoke tests (Cranelift backend) ---"
	@pass=0; fail=0; \
	for f in $(LIVE_WEBSOCKET_EXPECTED); do \
		name=$$(basename "$$f" .txt); \
		actual=$$(timeout 15 ./target/release/forge run "tests/live/$$name.fg" 2>/dev/null); \
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
	echo "all live websocket smoke tests passed"

run-live-websocket-tests-self-only:
	@echo "--- live websocket smoke tests (self-hosted compiler) ---"
	@pass=0; fail=0; \
	for f in $(LIVE_WEBSOCKET_EXPECTED); do \
		name=$$(basename "$$f" .txt); \
		actual=$$(timeout 15 ./self-host/forge_main run "tests/live/$$name.fg" 2>/dev/null); \
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
	echo "all self-hosted live websocket smoke tests passed"

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

ir-contract-regressions: self-host ir-contract-regressions-only

ir-contract-regressions-only:
	@echo "--- combined ir contract checks ---"
	@pass=0; fail=0; \
	if timeout 15 ./self-host/ir_driver --combined tests/cases/test_suite.fg | awk '/^field / && NF==4 { bad=1 } END { exit bad }'; then \
		pass=$$((pass+1)); echo "ok   no legacy short fields"; \
	else \
		echo "FAIL no legacy short fields"; fail=$$((fail+1)); \
	fi; \
	if timeout 15 ./self-host/ir_driver --combined examples/http_api.fg | awk '/^call / && ($$3=="tcp_connect" || $$3=="file_open_read" || $$3=="process_spawn" || $$3=="parse_int") && $$4 != "result_int" { bad=1 } END { exit bad }'; then \
		pass=$$((pass+1)); echo "ok   builtin result retkinds"; \
	else \
		echo "FAIL builtin result retkinds"; fail=$$((fail+1)); \
	fi; \
	if timeout 15 ./self-host/ir_driver --combined examples/concurrency.fg | awk 'BEGIN { m=0; w=0; s=0; bad=0 } /^call / && $$3=="Mutex" { if ($$4=="opaque:Mutex") m=1; else bad=1 } /^call / && $$3=="WaitGroup" { if ($$4=="opaque:WaitGroup") w=1; else bad=1 } /^call / && $$3=="Semaphore" { if ($$4=="opaque:Semaphore") s=1; else bad=1 } END { if (!m || !w || !s || bad) exit 1 }'; then \
		pass=$$((pass+1)); echo "ok   sync primitive opaque retkinds"; \
	else \
		echo "FAIL sync primitive opaque retkinds"; fail=$$((fail+1)); \
	fi; \
	if timeout 15 ./self-host/ir_driver --combined tests/cases/test_io_file_streams.fg | awk '/^call / && $$4 ~ /^[A-Z]/ { bad=1 } END { exit bad }'; then \
		pass=$$((pass+1)); echo "ok   no bare struct call retkinds"; \
	else \
		echo "FAIL no bare struct call retkinds"; fail=$$((fail+1)); \
	fi; \
	echo "$$pass passed, $$fail failed"; \
	if [ $$fail -gt 0 ]; then exit 1; fi; \
	echo "all combined ir contract checks passed"

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
	@echo "=== Step 6: verify combined ir contract ==="
	@$(MAKE) --no-print-directory ir-contract-regressions-only
	@echo "=== Step 7: build self-hosted compiler via Cranelift ==="
	./target/release/forge build self-host/forge_main.fg
	@echo "=== Step 8: run regression cases through self-hosted compiler ==="
	@$(MAKE) --no-print-directory run-regressions-self-only
	@echo "=== Step 9: run invalid parse examples through self-hosted parser ==="
	@$(MAKE) --no-print-directory check-parse-invalid-self-host-only
	@echo "=== Step 10: compare native and self-hosted example outputs ==="
	@$(MAKE) --no-print-directory parity-examples-only
	@echo "=== Step 11: run invalid examples through self-hosted checker ==="
	@$(MAKE) --no-print-directory check-invalid-self-host-only
	@echo "=== Step 12: run self-host cli regressions ==="
	@$(MAKE) --no-print-directory cli-regressions-self-only
	@echo "=== Step 13: self-hosted compiler works ==="
	./self-host/forge_main version
	./self-host/forge_main lex examples/hello.fg > /dev/null
	./self-host/forge_main parse examples/hello.fg > /dev/null
	@echo "=== all tests passed ==="

clean:
	cargo clean
	rm -rf .forge-build
