.PHONY: build self-host self-host-ir-driver bootstrap bootstrap-verify bootstrap-ir-checks bootstrap-ir-checks-only bootstrap-ir-fixed-point bootstrap-ir-fixed-point-only bootstrap-ir-invariants bootstrap-ir-invariants-only run-examples run-examples-self run-examples-self-only run-regressions run-regressions-only run-regressions-self run-regressions-self-only run-live-websocket-tests run-live-websocket-tests-self-only parity-examples parity-examples-only check-parse-invalid check-parse-invalid-only check-parse-invalid-self-host check-parse-invalid-self-host-only check-invalid check-invalid-only check-invalid-self-host check-invalid-self-host-only cli-regressions cli-regressions-only cli-regressions-self cli-regressions-self-only ir-contract-regressions ir-contract-regressions-only test-std-self test-std-self-only test-self-host-only test-fast-self status-audit test clean

NONDETERMINISTIC_EXAMPLES := net_basics net_echo
EXPECTED_EXAMPLES := $(filter-out $(addprefix examples/expected/,$(addsuffix .txt,$(NONDETERMINISTIC_EXAMPLES))),$(wildcard examples/expected/*.txt))
SLOW_NATIVE_EXAMPLES := csv_ops http_api http_apps http_websocket_app websocket_chat websocket_echo
REGRESSION_EXPECTED := $(wildcard tests/expected/*.txt)
SLOW_NATIVE_REGRESSIONS := \
	test_http_app_helpers \
	test_http_websocket_app \
	test_websocket_accept_buffered \
	test_websocket_bytes \
	test_websocket_fragmentation \
	test_websocket_frames \
	test_websocket_handshake \
	test_websocket_session \
	test_websocket_wire
FAST_REGRESSION_EXPECTED := $(filter-out $(addprefix tests/expected/,$(addsuffix .txt,$(SLOW_NATIVE_REGRESSIONS))),$(REGRESSION_EXPECTED))
LIVE_EXPECTED := $(wildcard tests/live/expected/*.txt)
LIVE_CASES := $(basename $(notdir $(LIVE_EXPECTED)))
LIVE_WEBSOCKET_EXPECTED := $(LIVE_EXPECTED)
LIVE_WEBSOCKET_CASES := $(LIVE_CASES)
PARSE_INVALID_EXAMPLES := $(wildcard tests/invalid_parse/*.pith)
INVALID_EXAMPLES := $(wildcard tests/invalid/*.pith)
PARITY_EXAMPLES := \
	hello \
	control_flow \
	structs \
	collection_methods \
	generics \
	lambdas \
	error_handling \
	json_ops \
	toml_ops \
	http_parsing \
	uuid_ops \
	matrix_math \
	self_host_patterns \
	wildcard_import

IR_FIXED_POINT_SOURCES := \
	examples/hello.pith \
	examples/concurrency.pith \
	tests/cases/test_suite.pith \
	tests/cases/test_imported_globals_init.pith \
	tests/cases/test_module_alias_calls.pith \
	tests/cases/test_imported_io_methods.pith \
	tests/cases/test_io_file_streams.pith \
	tests/cases/test_http_request_bytes.pith \
	tests/cases/test_http_websocket_app.pith \
	tests/cases/test_websocket_wire.pith

BOOTSTRAP_IR_REBUILD_TARGETS := \
	self-host/pith_main.pith \
	self-host/ir_driver.pith

# --- primary build (Cranelift native backend) ---

build:
	cargo build --release

# build the self-hosted compiler using the Cranelift backend
self-host: build
	./target/release/pith build self-host/pith_main.pith

self-host-ir-driver: build
	./target/release/pith build self-host/ir_driver.pith

# rebuild the self-hosted compiler using the Cranelift-compiled version of itself
bootstrap: self-host
	@echo "--- stage 1: compile with current Cranelift binary ---"
	./target/release/pith build self-host/pith_main.pith
	@echo "--- stage 1 binary test ---"
	./self-host/pith_main version

# verify that the Cranelift-compiled compiler produces identical output
bootstrap-verify: self-host
	@echo "--- verifying self-hosted compiler on deterministic examples ---"
	@$(MAKE) --no-print-directory run-examples-self-only
	@$(MAKE) --no-print-directory self-host-ir-driver
	@echo "--- verifying colocated std tests ---"
	@$(MAKE) --no-print-directory test-std-self-only
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
	if timeout 15 ./self-host/ir_driver --combined tests/cases/test_imported_globals_init.pith | awk 'BEGIN { init=0; call=0 } /^func [A-Za-z0-9_]+___init_globals_[0-9]+(_[0-9]+)? / { init=1 } /^call 900000 [A-Za-z0-9_]+___init_globals_[0-9]+(_[0-9]+)? int 0/ { call=1 } END { if (init && call) exit 0; exit 1 }'; then \
		pass=$$((pass+1)); echo "ok   imported init globals wiring"; \
	else \
		echo "FAIL imported init globals wiring"; fail=$$((fail+1)); \
	fi; \
	if timeout 15 ./self-host/ir_driver --combined examples/concurrency.pith | awk 'BEGIN { m=0; w=0; s=0 } /^call / && $$3=="Mutex" && $$4=="opaque:Mutex" { m=1 } /^call / && $$3=="WaitGroup" && $$4=="opaque:WaitGroup" { w=1 } /^call / && $$3=="Semaphore" && $$4=="opaque:Semaphore" { s=1 } END { if (m && w && s) exit 0; exit 1 }'; then \
		pass=$$((pass+1)); echo "ok   sync primitive retkind invariants"; \
	else \
		echo "FAIL sync primitive retkind invariants"; fail=$$((fail+1)); \
	fi; \
	if timeout 15 ./self-host/ir_driver --combined examples/generic_interfaces.pith | awk 'BEGIN { value=0; x=0; bad=0 } /^field / && NF==4 { bad=1 } /^field / && ($$5=="T" || $$5=="Point") && $$6=="value" { value=1 } /^field / && $$5=="int" && $$6=="x" { x=1 } END { if (value && x && !bad) exit 0; exit 1 }'; then \
		pass=$$((pass+1)); echo "ok   imported struct field metadata"; \
	else \
		echo "FAIL imported struct field metadata"; fail=$$((fail+1)); \
	fi; \
	if timeout 60 ./self-host/ir_driver --combined tests/cases/test_websocket_session.pith | awk 'BEGIN { ok=0 } /^call / && $$4 ~ /^struct:/ { ok=1 } /^call / && $$4 ~ /^[A-Z]/ { bad=1 } END { if (ok && !bad) exit 0; exit 1 }'; then \
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
	@tmpdir=$$(mktemp -d /tmp/pith-ir-fixed-point-XXXXXX); \
	pass=0; fail=0; \
	trap 'rm -rf "$$tmpdir"' EXIT; \
	if [ ! -x ./self-host/ir_driver ]; then \
		timeout 120 ./target/release/pith build self-host/ir_driver.pith >/dev/null; \
	fi; \
	for target in $(BOOTSTRAP_IR_REBUILD_TARGETS); do \
		timeout 120 ./self-host/pith_main build "$$target" >/dev/null; \
	done; \
	cp ./self-host/pith_main "$$tmpdir/pith_main_stage1"; \
	cp ./self-host/ir_driver "$$tmpdir/ir_driver_stage1"; \
	for target in $(BOOTSTRAP_IR_REBUILD_TARGETS); do \
		timeout 120 ./self-host/pith_main build "$$target" >/dev/null; \
	done; \
	for src in $(IR_FIXED_POINT_SOURCES); do \
		stage1=$$(timeout 60 "$$tmpdir/ir_driver_stage1" --combined "$$src" 2>/dev/null); \
		stage1_status=$$?; \
		stage2=$$(timeout 60 ./self-host/ir_driver --combined "$$src" 2>/dev/null); \
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
	@tmpdir=$$(mktemp -d /tmp/pith-native-examples-XXXXXX); \
	trap 'rm -rf "$$tmpdir"' EXIT; \
	pass=0; fail=0; \
	for f in $(EXPECTED_EXAMPLES); do \
		name=$$(basename "$$f" .txt); \
		case " $(SLOW_NATIVE_EXAMPLES) " in \
			*" $$name "*) \
				if timeout 120 ./target/release/pith build "examples/$$name.pith" >/dev/null 2>/dev/null; then \
					actual=$$(timeout 15 "./examples/$$name" 2>/dev/null); \
					expected=$$(cat "$$f"); \
					if [ "$$actual" = "$$expected" ]; then \
						pass=$$((pass+1)); \
						echo "ok   $$name"; \
					else \
						echo "FAIL $$name"; \
						fail=$$((fail+1)); \
					fi; \
				else \
					echo "FAIL $$name"; \
					fail=$$((fail+1)); \
				fi ;; \
			*) \
				actual=$$(timeout 60 ./target/release/pith run "examples/$$name.pith" 2>/dev/null); \
				expected=$$(cat "$$f"); \
				if [ "$$actual" = "$$expected" ]; then \
					pass=$$((pass+1)); \
					echo "ok   $$name"; \
				else \
					echo "FAIL $$name"; \
					fail=$$((fail+1)); \
				fi ;; \
		esac; \
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
		actual=$$(timeout 60 ./self-host/pith_main run "examples/$$name.pith" 2>/dev/null); \
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
	for f in $(FAST_REGRESSION_EXPECTED); do \
		name=$$(basename "$$f" .txt); \
		actual=$$(timeout 60 ./target/release/pith run "tests/cases/$$name.pith" 2>/dev/null); \
		expected=$$(cat "$$f"); \
		if [ "$$actual" = "$$expected" ]; then \
			pass=$$((pass+1)); \
			echo "ok   $$name"; \
		else \
			echo "FAIL $$name"; \
			fail=$$((fail+1)); \
		fi; \
	done; \
	for name in $(SLOW_NATIVE_REGRESSIONS); do \
		expected_file="tests/expected/$$name.txt"; \
		if [ ! -f "$$expected_file" ]; then \
			echo "FAIL $$name (missing $$expected_file)"; \
			fail=$$((fail+1)); \
			continue; \
		fi; \
		if timeout 120 ./target/release/pith build "tests/cases/$$name.pith" >/dev/null 2>/dev/null; then \
			actual=$$(timeout 15 "./tests/cases/$$name" 2>/dev/null); \
			expected=$$(cat "$$expected_file"); \
			if [ "$$actual" = "$$expected" ]; then \
				pass=$$((pass+1)); \
				echo "ok   $$name"; \
			else \
				echo "FAIL $$name"; \
				fail=$$((fail+1)); \
			fi; \
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
		actual=$$(timeout 60 ./self-host/pith_main run "tests/cases/$$name.pith" 2>/dev/null); \
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

test-std-self: self-host self-host-ir-driver test-std-self-only

test-std-self-only:
	@echo "--- colocated std tests (self-hosted compiler) ---"
	@pass=0; fail=0; \
	files=$$(find std -name '*.pith' -print | sort); \
	for f in $$files; do \
		if ! grep -q '^[[:space:]]*test "' "$$f"; then \
			continue; \
		fi; \
		if timeout 60 ./self-host/pith_main test "$$f" >/tmp/pith-test-out 2>&1; then \
			pass=$$((pass+1)); \
			echo "ok   $$f"; \
		else \
			echo "FAIL $$f"; \
			cat /tmp/pith-test-out; \
			fail=$$((fail+1)); \
		fi; \
	done; \
	echo "$$pass passed, $$fail failed"; \
	if [ $$fail -gt 0 ]; then exit 1; fi

test-self-host-only:
	@echo "--- colocated self-host tests ---"
	@pass=0; fail=0; \
	files=$$(find self-host -name '*.pith' -print | sort); \
	for f in $$files; do \
		if ! grep -q '^[[:space:]]*test "' "$$f"; then \
			continue; \
		fi; \
		if timeout 60 ./self-host/pith_main test "$$f" >/tmp/pith-test-out 2>&1; then \
			pass=$$((pass+1)); \
			echo "ok   $$f"; \
		else \
			echo "FAIL $$f"; \
			cat /tmp/pith-test-out; \
			fail=$$((fail+1)); \
		fi; \
	done; \
	echo "$$pass passed, $$fail failed"; \
	if [ $$fail -gt 0 ]; then exit 1; fi

test-fast-self: self-host self-host-ir-driver
	@$(MAKE) --no-print-directory test-std-self-only
	@$(MAKE) --no-print-directory test-self-host-only
	@$(MAKE) --no-print-directory run-regressions-self-only

run-live-websocket-tests: build
	@echo "--- live smoke tests (Cranelift backend) ---"
	@pass=0; fail=0; \
	for name in $(LIVE_WEBSOCKET_CASES); do \
		expected_file="tests/live/expected/$$name.txt"; \
		if timeout 120 ./target/release/pith build "tests/live/$$name.pith" >/dev/null 2>/dev/null; then \
			actual=$$(timeout 15 "./tests/live/$$name" 2>/dev/null); \
			expected=$$(cat "$$expected_file"); \
			if [ "$$actual" = "$$expected" ]; then \
				pass=$$((pass+1)); \
				echo "ok   $$name"; \
			else \
				echo "FAIL $$name"; \
				fail=$$((fail+1)); \
			fi; \
		else \
			echo "FAIL $$name"; \
			fail=$$((fail+1)); \
		fi; \
	done; \
	echo "$$pass passed, $$fail failed"; \
	if [ $$fail -gt 0 ]; then exit 1; fi; \
	echo "all live smoke tests passed"

run-live-websocket-tests-self-only:
	@echo "--- live smoke tests (self-hosted compiler) ---"
	@pass=0; fail=0; \
	for name in $(LIVE_WEBSOCKET_CASES); do \
		expected_file="tests/live/expected/$$name.txt"; \
		if timeout 120 ./self-host/pith_main build "tests/live/$$name.pith" >/dev/null 2>/dev/null; then \
			actual=$$(timeout 15 "./tests/live/$$name" 2>/dev/null); \
			expected=$$(cat "$$expected_file"); \
			if [ "$$actual" = "$$expected" ]; then \
				pass=$$((pass+1)); \
				echo "ok   $$name"; \
			else \
				echo "FAIL $$name"; \
				fail=$$((fail+1)); \
			fi; \
		else \
			echo "FAIL $$name"; \
			fail=$$((fail+1)); \
		fi; \
	done; \
	echo "$$pass passed, $$fail failed"; \
	if [ $$fail -gt 0 ]; then exit 1; fi; \
	echo "all self-hosted live smoke tests passed"

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
		native=$$(timeout 15 ./target/release/pith run "examples/$$name.pith" 2>/dev/null); \
		self_host=$$(timeout 15 ./self-host/pith_main run "examples/$$name.pith" 2>/dev/null); \
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

status-audit:
	@echo "examples: $$(find examples -maxdepth 1 -name '*.pith' | wc -l)"
	@echo "deterministic snapshots: $$(find examples/expected -name '*.txt' | wc -l)"
	@echo "regression snapshots: $$(find tests/expected -name '*.txt' | wc -l)"
	@echo "std modules: $$(find std -name '*.pith' | wc -l)"
	@echo "self-host pith lines: $$(git ls-files 'self-host/*.pith' | xargs wc -l | tail -1 | awk '{print $$1}')"
	@echo "std pith lines: $$(git ls-files 'std/**/*.pith' 'std/*.pith' | xargs wc -l | tail -1 | awk '{print $$1}')"
	@echo "tracked cranelift rust lines: $$(git ls-files 'cranelift/**/*.rs' | xargs wc -l | tail -1 | awk '{print $$1}')"
	@echo "example .to_string() sites: $$(rg -o '\.to_string\(' examples -g '*.pith' | wc -l)"
	@echo "example manual length loops: $$(rg 'while .*< .*\.len\(\)' examples -g '*.pith' | wc -l)"

check-parse-invalid: build check-parse-invalid-only

check-parse-invalid-only:
	@echo "--- invalid parse examples (parser diagnostics) ---"
	@pass=0; fail=0; \
	for f in $(PARSE_INVALID_EXAMPLES); do \
		name=$$(basename "$$f" .pith); \
		expected_file="tests/invalid_parse/expected/$$name.codes"; \
		if [ ! -f "$$expected_file" ]; then \
			echo "FAIL $$name (missing $$expected_file)"; \
			fail=$$((fail+1)); \
			continue; \
		fi; \
		set +e; \
		output=$$(timeout 15 ./target/release/pith parse "$$f" 2>&1); \
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
		name=$$(basename "$$f" .pith); \
		expected_file="tests/invalid_parse/expected/$$name.codes"; \
		if [ ! -f "$$expected_file" ]; then \
			echo "FAIL $$name (missing $$expected_file)"; \
			fail=$$((fail+1)); \
			continue; \
		fi; \
		set +e; \
		output=$$(timeout 15 ./self-host/pith_main parse "$$f" 2>&1); \
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
		name=$$(basename "$$f" .pith); \
		expected_file="tests/invalid/expected/$$name.codes"; \
		if [ ! -f "$$expected_file" ]; then \
			echo "FAIL $$name (missing $$expected_file)"; \
			fail=$$((fail+1)); \
			continue; \
		fi; \
		set +e; \
		output=$$(timeout 15 ./target/release/pith check "$$f" 2>&1); \
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
		name=$$(basename "$$f" .pith); \
		expected_file="tests/invalid/expected/$$name.codes"; \
		if [ ! -f "$$expected_file" ]; then \
			echo "FAIL $$name (missing $$expected_file)"; \
			fail=$$((fail+1)); \
			continue; \
		fi; \
		set +e; \
		output=$$(timeout 15 ./self-host/pith_main check "$$f" 2>&1); \
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
	@tmpdir=$$(mktemp -d /tmp/pith-cli-regressions-XXXXXX); \
	trap 'rm -rf "$$tmpdir"' EXIT; \
	printf 'fn main() -> Int!:\n    return missing_name\n' > "$$tmpdir/bad.pith"; \
	printf 'test "broken":\n    assert_eq(1 + 1, 3)\n' > "$$tmpdir/fail_test.pith"; \
	pass=0; fail=0; \
	set +e; \
	./target/release/pith >/dev/null 2>&1; \
	status=$$?; \
	set -e; \
	if [ $$status -ne 0 ]; then pass=$$((pass+1)); echo "ok   no args fail"; else echo "FAIL no args fail"; fail=$$((fail+1)); fi; \
	set +e; \
	./target/release/pith run "$$tmpdir/bad.pith" >/dev/null 2>&1; \
	status=$$?; \
	set -e; \
	if [ $$status -ne 0 ]; then pass=$$((pass+1)); echo "ok   run compile failure"; else echo "FAIL run compile failure"; fail=$$((fail+1)); fi; \
	set +e; \
	./target/release/pith build "$$tmpdir/bad.pith" >/dev/null 2>&1; \
	status=$$?; \
	set -e; \
	if [ $$status -ne 0 ]; then pass=$$((pass+1)); echo "ok   build compile failure"; else echo "FAIL build compile failure"; fail=$$((fail+1)); fi; \
	set +e; \
	./target/release/pith check "$$tmpdir/bad.pith" >/dev/null 2>&1; \
	status=$$?; \
	set -e; \
	if [ $$status -ne 0 ]; then pass=$$((pass+1)); echo "ok   check failure"; else echo "FAIL check failure"; fail=$$((fail+1)); fi; \
	set +e; \
	./target/release/pith test tests/cases/test_test_declarations.pith >/dev/null 2>&1; \
	status=$$?; \
	set -e; \
	if [ $$status -eq 0 ]; then pass=$$((pass+1)); echo "ok   test declarations pass"; else echo "FAIL test declarations pass"; fail=$$((fail+1)); fi; \
	set +e; \
	./target/release/pith test "$$tmpdir/fail_test.pith" >/dev/null 2>&1; \
	status=$$?; \
	set -e; \
	if [ $$status -ne 0 ]; then pass=$$((pass+1)); echo "ok   test declarations fail"; else echo "FAIL test declarations fail"; fail=$$((fail+1)); fi; \
	echo "$$pass passed, $$fail failed"; \
	if [ $$fail -gt 0 ]; then exit 1; fi; \
	echo "all native cli regressions passed"

cli-regressions-self: self-host cli-regressions-self-only

cli-regressions-self-only:
	@echo "--- cli regressions (self-hosted wrapper) ---"
	@tmpdir=$$(mktemp -d /tmp/pith-cli-regressions-self-XXXXXX); \
	trap 'rm -rf "$$tmpdir"' EXIT; \
	printf 'fn main() -> Int!:\n    return missing_name\n' > "$$tmpdir/bad.pith"; \
	printf 'test "broken":\n    assert_eq(1 + 1, 3)\n' > "$$tmpdir/fail_test.pith"; \
	pass=0; fail=0; \
	set +e; \
	./self-host/pith_main run "$$tmpdir/bad.pith" >/dev/null 2>&1; \
	status=$$?; \
	set -e; \
	if [ $$status -ne 0 ]; then pass=$$((pass+1)); echo "ok   run compile failure"; else echo "FAIL run compile failure"; fail=$$((fail+1)); fi; \
	set +e; \
	./self-host/pith_main check "$$tmpdir/bad.pith" >/dev/null 2>&1; \
	status=$$?; \
	set -e; \
	if [ $$status -ne 0 ]; then pass=$$((pass+1)); echo "ok   check failure"; else echo "FAIL check failure"; fail=$$((fail+1)); fi; \
	set +e; \
	./self-host/pith_main test tests/cases/test_test_declarations.pith >/dev/null 2>&1; \
	status=$$?; \
	set -e; \
	if [ $$status -eq 0 ]; then pass=$$((pass+1)); echo "ok   test declarations pass"; else echo "FAIL test declarations pass"; fail=$$((fail+1)); fi; \
	set +e; \
	./self-host/pith_main test "$$tmpdir/fail_test.pith" >/dev/null 2>&1; \
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
	http_ir=$$(mktemp /tmp/pith-http-api-ir-XXXXXX); \
	trap 'rm -f "$$http_ir"' EXIT; \
	if timeout 15 ./self-host/ir_driver --combined tests/cases/test_suite.pith | awk '/^field / && NF==4 { bad=1 } END { exit bad }'; then \
		pass=$$((pass+1)); echo "ok   no legacy short fields"; \
	else \
		echo "FAIL no legacy short fields"; fail=$$((fail+1)); \
	fi; \
	if timeout 60 ./self-host/ir_driver --combined examples/http_api.pith > "$$http_ir" && awk '/^call / && ($$3=="tcp_connect" || $$3=="file_open_read" || $$3=="process_spawn") && $$4 != "result_int" { bad=1 } /^call / && $$3=="parse_int" && $$4 != "tuple" { bad=1 } END { exit bad }' "$$http_ir"; then \
		pass=$$((pass+1)); echo "ok   builtin result retkinds"; \
	else \
		echo "FAIL builtin result retkinds"; fail=$$((fail+1)); \
	fi; \
	if timeout 15 ./self-host/ir_driver --combined examples/concurrency.pith | awk 'BEGIN { m=0; w=0; s=0; bad=0 } /^call / && $$3=="Mutex" { if ($$4=="opaque:Mutex") m=1; else bad=1 } /^call / && $$3=="WaitGroup" { if ($$4=="opaque:WaitGroup") w=1; else bad=1 } /^call / && $$3=="Semaphore" { if ($$4=="opaque:Semaphore") s=1; else bad=1 } END { if (!m || !w || !s || bad) exit 1 }'; then \
		pass=$$((pass+1)); echo "ok   sync primitive opaque retkinds"; \
	else \
		echo "FAIL sync primitive opaque retkinds"; fail=$$((fail+1)); \
	fi; \
	if timeout 15 ./self-host/ir_driver --combined tests/cases/test_io_file_streams.pith | awk '/^call / && $$4 ~ /^[A-Z]/ { bad=1 } END { exit bad }'; then \
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
		case " $(SLOW_NATIVE_EXAMPLES) " in \
			*" $$name "*) \
				if timeout 120 ./target/release/pith build "examples/$$name.pith" >/dev/null 2>/dev/null; then \
					actual=$$(timeout 15 "./examples/$$name" 2>/dev/null); \
					expected=$$(cat "$$f"); \
					if [ "$$actual" = "$$expected" ]; then \
						pass=$$((pass+1)); \
						echo "ok   $$name"; \
					else \
						echo "FAIL $$name"; \
						fail=$$((fail+1)); \
					fi; \
				else \
					echo "FAIL $$name"; \
					fail=$$((fail+1)); \
				fi ;; \
			*) \
				actual=$$(timeout 60 ./target/release/pith run "examples/$$name.pith" 2>/dev/null); \
				expected=$$(cat "$$f"); \
				if [ "$$actual" = "$$expected" ]; then \
					pass=$$((pass+1)); \
					echo "ok   $$name"; \
				else \
					echo "FAIL $$name"; \
					fail=$$((fail+1)); \
				fi ;; \
		esac; \
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
	./target/release/pith build self-host/pith_main.pith
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
	./self-host/pith_main version
	./self-host/pith_main lex examples/hello.pith > /dev/null
	./self-host/pith_main parse examples/hello.pith > /dev/null
	@echo "=== all tests passed ==="

clean:
	cargo clean
	rm -rf .pith-build
