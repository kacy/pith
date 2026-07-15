.PHONY: build self-host self-host-ir-driver bootstrap bootstrap-verify bootstrap-ir-checks bootstrap-ir-checks-only bootstrap-ir-fixed-point bootstrap-ir-fixed-point-only bootstrap-ir-invariants bootstrap-ir-invariants-only run-examples run-examples-self run-examples-self-only run-regressions run-regressions-only run-regressions-self run-regressions-self-only run-live-websocket-tests run-live-websocket-tests-self-only db-live-tests parity-examples parity-examples-only check-parse-invalid check-parse-invalid-only check-parse-invalid-self-host check-parse-invalid-self-host-only check-invalid check-invalid-only check-invalid-self-host check-invalid-self-host-only cli-regressions cli-regressions-only cli-regressions-self cli-regressions-self-only ir-contract-regressions ir-contract-regressions-only test-std-self test-std-self-only test-self-host-only test-fast-self status-audit check-no-panics safety-check fuzz-check fuzz memcheck test clean

NONDETERMINISTIC_EXAMPLES := net_basics net_echo redis_client
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
DB_LIVE_CASES := db_postgres_live db_mysql_live db_redis_live
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

check-no-panics:
	bash tooling/check_no_panics.sh

safety-check: build check-no-panics
	cargo test --manifest-path cranelift/Cargo.toml --workspace --locked
	./target/release/pith run tests/cases/test_channel_runtime.pith
	./target/release/pith run tests/cases/test_concurrent_runtime.pith
	./target/release/pith run tests/cases/test_select_runtime.pith
	./target/release/pith run tests/cases/test_mutex_runtime.pith

# build the self-hosted compiler using the Cranelift backend
self-host: build
	./target/release/pith build self-host/pith_main.pith

self-host-ir-driver: build
	./target/release/pith build self-host/ir_driver.pith

# regenerate the tracked ir seed that lets a fresh clone build ir_driver.
# run this after changes to the ir contract or the emitter so the seed
# keeps working without an existing ir_driver binary.
refresh-bootstrap-seed: self-host-ir-driver
	PITH_DUMP_IR=self-host/bootstrap/ir_driver.ir ./target/release/pith build self-host/ir_driver.pith

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

# colocated live database tests, run through the test harness. each test skips
# when its server is unreachable, so this target stays green with or without a
# running postgres/mysql/redis — it verifies behavior where the servers exist.
db-live-tests: build
	@echo "--- live database tests (test harness) ---"
	@fail=0; \
	for name in $(DB_LIVE_CASES); do \
		if ! ./target/release/pith test "tests/live/$$name.pith"; then \
			fail=1; \
		fi; \
	done; \
	if [ $$fail -ne 0 ]; then exit 1; fi; \
	echo "all live database tests passed"

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

# --- sitegen golden check ---
# builds the dogfood site generator, runs it over the sample site, and
# diffs the outputs that pin the whole pipeline

sitegen-check:
	@echo "--- sitegen golden check ---"
	@./target/release/pith build tools/sitegen/sitegen.pith > /dev/null
	@tmpdir=$$(mktemp -d /tmp/pith-sitegen-XXXXXX); \
	trap 'rm -rf "$$tmpdir"' EXIT; \
	./tools/sitegen/sitegen tools/sitegen/sample "$$tmpdir" > /dev/null; \
	diff -u tools/sitegen/expected/index.html "$$tmpdir/index.html" && \
	diff -u tools/sitegen/expected/feed.json "$$tmpdir/feed.json" && \
	diff -u tools/sitegen/expected/post-hello-world.html "$$tmpdir/posts/hello-world.html" && \
	diff -u tools/sitegen/expected/post-on-ownership.html "$$tmpdir/posts/on-ownership.html" && \
	diff -u tools/sitegen/expected/tag-memory.html "$$tmpdir/tags/memory.html" && \
	echo "sitegen output matches golden files"

# --- logscan golden check ---
# builds the log analyzer, runs it over the sample log both plain and
# gzipped, and diffs the report and csv export

logscan-check:
	@echo "--- logscan golden check ---"
	@./target/release/pith build tools/logscan/logscan.pith > /dev/null
	@tmpdir=$$(mktemp -d /tmp/pith-logscan-XXXXXX); \
	trap 'rm -rf "$$tmpdir"' EXIT; \
	./tools/logscan/logscan tools/logscan/sample/access.log "$$tmpdir/paths.csv" | grep -v "^csv written" > "$$tmpdir/report.txt"; \
	diff -u tools/logscan/expected/report.txt "$$tmpdir/report.txt" && \
	diff -u tools/logscan/expected/paths.csv "$$tmpdir/paths.csv" && \
	gzip -c tools/logscan/sample/access.log > "$$tmpdir/sys.gz" && \
	./tools/logscan/logscan "$$tmpdir/sys.gz" | grep -v "^csv written" > "$$tmpdir/report_gz.txt" && \
	diff -u tools/logscan/expected/report.txt "$$tmpdir/report_gz.txt" && \
	echo "logscan output matches golden files"

# --- apic golden check ---
# builds the json api client and its fixture server, runs the client
# against the live server, and diffs the outputs

apic-check:
	@echo "--- apic golden check ---"
	@./target/release/pith build tools/apic/apic.pith > /dev/null
	@./target/release/pith build tools/apic/fixture_server.pith > /dev/null
	@tmpdir=$$(mktemp -d /tmp/pith-apic-XXXXXX); \
	trap 'rm -rf "$$tmpdir"' EXIT; \
	./tools/apic/fixture_server 8047 3 & \
	server_pid=$$!; \
	sleep 1; \
	./tools/apic/apic localhost:8047/item?id=7 --extract name > "$$tmpdir/extract.txt"; \
	./tools/apic/apic localhost:8047/item?id=7 --pretty > "$$tmpdir/pretty.txt"; \
	./tools/apic/apic localhost:8047/echo --post '{"ping":1}' --extract received.ping > "$$tmpdir/post.txt"; \
	wait $$server_pid; \
	diff -u tools/apic/expected/extract.txt "$$tmpdir/extract.txt" && \
	diff -u tools/apic/expected/pretty.txt "$$tmpdir/pretty.txt" && \
	diff -u tools/apic/expected/post.txt "$$tmpdir/post.txt" && \
	echo "apic output matches golden files"

# --- parq golden check ---
# builds the parallel task runner and diffs its report; the
# workers-used line depends on scheduling and is excluded

parq-check:
	@echo "--- parq golden check ---"
	@./target/release/pith build tools/parq/parq.pith > /dev/null
	@tmpdir=$$(mktemp -d /tmp/pith-parq-XXXXXX); \
	trap 'rm -rf "$$tmpdir"' EXIT; \
	./tools/parq/parq tools/parq/sample/jobs.txt 4 | grep -v "^workers used" > "$$tmpdir/report.txt"; \
	diff -u tools/parq/expected/report.txt "$$tmpdir/report.txt" && \
	./tools/parq/parq tools/parq/sample/jobs.txt 1 | grep -v "^workers used" > "$$tmpdir/report1.txt"; \
	diff -u tools/parq/expected/report.txt "$$tmpdir/report1.txt" && \
	echo "parq output matches golden files"

# the ci gate: a fixed seed over both generated programs and mutated
# corpus cases, deterministic and green. it asserts the frontend never
# crashes, exits silent, or drops a valid program. the two silent-input
# gaps the mutation half first surfaced are now fixed (E243 for stray
# characters, E247 for missing modules), so the mutation half joins the
# gate. use `make fuzz` for a wider, longer search.
fuzz-check: build
	@echo "--- fuzz check (generated + mutated corpus, deterministic) ---"
	@./target/release/pith build tools/fuzz/fuzz.pith > /dev/null
	@./tools/fuzz/fuzz --count 120 --build-every 8

# open-ended fuzzing: generated + corpus mutation. pass --count / --seed
# to explore. known findings live in the bulletproof plan; use this to
# hunt for new silent seams.
fuzz: build
	@./target/release/pith build tools/fuzz/fuzz.pith > /dev/null
	@./tools/fuzz/fuzz --count 300 --build-every 5

# --- memcheck ---
# run a curated set of memory-management-heavy programs under valgrind
# with an error exit code, so a use-after-free or an out-of-bounds read
# fails ci even when the program happens to print the right answer (an
# enum-payload overread did exactly that until it was caught here). the
# full example + regression corpus is valgrind-clean; this subset keeps
# the gate fast.
MEMCHECK_CASES := \
	tests/cases/test_match_payload tests/cases/test_combo_enums_deep \
	tests/cases/test_fn_value_positions tests/cases/test_global_fn_value \
	tests/cases/test_optional_collections tests/cases/test_nested_optional_literals \
	tests/cases/test_optional_value tests/cases/test_closure_struct_return \
	tests/cases/test_combo_closures tests/cases/test_combo_structs_deep \
	tests/cases/test_generic_to_string tests/cases/test_map_int_string_buffered \
	tests/cases/test_enum_payload_churn tests/cases/test_ownership_stress \
	tests/cases/test_enum_scope_release \
	tests/cases/test_field_reassign_release tests/cases/test_discarded_result \
	tests/cases/test_generic_method_return tests/cases/test_generic_fnvalue_return \
	tests/cases/test_fnvalue_field_pool tests/cases/test_closure_capture_escape \
	tests/cases/test_closure_lifecycle tests/cases/test_error_path_cleanup \
	tests/cases/test_atomic_context tests/cases/test_tuple_ownership \
	tests/cases/test_struct_closure_field tests/cases/test_generic_struct_field_dtor \
	tests/cases/test_iterator_drain tests/cases/test_method_fresh_string_return \
	tests/cases/test_lazy_adapter_pipeline tests/cases/test_named_local_adapter_chain \
	tests/cases/test_loop_var_shadows_fn tests/cases/test_weak_reference \
	tests/cases/test_http2_concurrent_events

memcheck: build
	@echo "--- memcheck (valgrind, curated) ---"
	@command -v valgrind > /dev/null || { echo "valgrind not installed; skipping"; exit 0; }
	@fail=0; \
	for base in $(MEMCHECK_CASES); do \
		./target/release/pith build "$$base.pith" > /dev/null 2>&1 || { echo "FAIL build $$base"; fail=1; continue; }; \
		if valgrind --error-exitcode=99 --leak-check=no --errors-for-leak-kinds=none -q "$$base" > /dev/null 2>/tmp/pith-memcheck.txt; then \
			echo "ok   $$base"; \
		else \
			echo "FAIL $$base (valgrind)"; head -6 /tmp/pith-memcheck.txt; fail=1; \
		fi; \
	done; \
	if [ $$fail -ne 0 ]; then exit 1; fi; \
	echo "all memcheck cases clean"

# --- gzip interop check ---
# both directions against the system tool: pith reads gzip's output
# (covered in logscan-check too) and gzip reads pith's

gzip-interop-check:
	@echo "--- gzip interop check ---"
	@tmpdir=$$(mktemp -d /tmp/pith-gzip-XXXXXX); \
	trap 'rm -rf "$$tmpdir"' EXIT; \
	printf 'import std.fs as fs\nimport std.compress.gzip as gzip\n\nfn main() -> Int!:\n    raw := fs.read_bytes("README.md")!\n    fs.write_bytes("'"$$tmpdir"'/out.gz", gzip.compress(raw))!\n    return 0\n' > "$$tmpdir/pack.pith"; \
	./target/release/pith run "$$tmpdir/pack.pith" > /dev/null 2>&1 && \
	gunzip -c "$$tmpdir/out.gz" | cmp - README.md && \
	echo "system gunzip reads pith output byte-identical"

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
	printf 'test "a first":\n    assert(true)\ntest "b fails":\n    assert(false)\ntest "c after failure":\n    assert(true)\n' > "$$tmpdir/harness.pith"; \
	set +e; \
	out=$$(./target/release/pith test "$$tmpdir/harness.pith" 2>&1); \
	status=$$?; \
	set -e; \
	if [ $$status -ne 0 ] && echo "$$out" | grep -q "c after failure ... ok" && echo "$$out" | grep -q "2 passed, 1 failed"; then pass=$$((pass+1)); echo "ok   test harness runs every test after a failure"; else echo "FAIL test harness record-and-continue"; echo "$$out"; fail=$$((fail+1)); fi; \
	printf 'test "equal lists match":\n    assert_eq([1, 2, 3], [1, 2, 3])\ntest "unequal lists differ":\n    assert_eq([1, 2], [1, 3])\n' > "$$tmpdir/eq.pith"; \
	set +e; \
	out=$$(./target/release/pith test "$$tmpdir/eq.pith" 2>&1); \
	status=$$?; \
	set -e; \
	if [ $$status -ne 0 ] && echo "$$out" | grep -q "equal lists match ... ok" && echo "$$out" | grep -q "\[1, 2\] != \[1, 3\]"; then pass=$$((pass+1)); echo "ok   assert_eq compares collections by value"; else echo "FAIL assert_eq value equality"; echo "$$out"; fail=$$((fail+1)); fi; \
	set +e; \
	out=$$(./target/release/pith test "$$tmpdir/harness.pith" --filter "first" 2>&1); \
	status=$$?; \
	set -e; \
	if [ $$status -eq 0 ] && echo "$$out" | grep -q "a first ... ok" && ! echo "$$out" | grep -q "c after failure" && echo "$$out" | grep -q "1 passed, 0 failed, 2 filtered out"; then pass=$$((pass+1)); echo "ok   test --filter runs only matching tests"; else echo "FAIL test --filter"; echo "$$out"; fail=$$((fail+1)); fi; \
	printf 'test "runs":\n    assert(true)\ntest "skips":\n    skip_test("not today")\n    assert(false)\n' > "$$tmpdir/skip.pith"; \
	set +e; \
	out=$$(./target/release/pith test "$$tmpdir/skip.pith" 2>&1); \
	status=$$?; \
	set -e; \
	if [ $$status -eq 0 ] && echo "$$out" | grep -q "skips ... skipped (not today)" && echo "$$out" | grep -q "1 passed, 0 failed, 1 skipped"; then pass=$$((pass+1)); echo "ok   skip_test skips a test without failing the run"; else echo "FAIL skip_test"; echo "$$out"; fail=$$((fail+1)); fi; \
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
	printf '## doc comment stays doubled\n#regular gets a space\nfn main():\n    print("x")\n' > "$$tmpdir/fmt_doc.pith"; \
	./self-host/pith_main fmt "$$tmpdir/fmt_doc.pith" >/dev/null 2>&1; \
	if head -1 "$$tmpdir/fmt_doc.pith" | grep -q '^## doc' && sed -n 2p "$$tmpdir/fmt_doc.pith" | grep -q '^# regular'; then \
		pass=$$((pass+1)); echo "ok   fmt preserves doc comments"; \
	else \
		echo "FAIL fmt preserves doc comments"; fail=$$((fail+1)); \
	fi; \
	echo "$$pass passed, $$fail failed"; \
	if [ $$fail -gt 0 ]; then exit 1; fi; \
	echo "all self-host cli regressions passed"

ir-contract-regressions: self-host ir-contract-regressions-only

ir-contract-regressions-only:
	@echo "--- combined ir contract checks ---"
	@pass=0; fail=0; \
	http_ir=$$(mktemp /tmp/pith-http-api-ir-XXXXXX); \
	higher_order_ir=$$(mktemp /tmp/pith-higher-order-ir-XXXXXX); \
	list_query_ir=$$(mktemp /tmp/pith-list-query-ir-XXXXXX); \
	trap 'rm -f "$$http_ir" "$$higher_order_ir" "$$list_query_ir"' EXIT; \
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
	if timeout 15 ./self-host/ir_driver --combined examples/higher_order.pith > "$$higher_order_ir" && ! grep -Eq 'pith_list_(map|filter|reduce|each)' "$$higher_order_ir"; then \
		pass=$$((pass+1)); echo "ok   higher-order lists self-host"; \
	else \
		echo "FAIL higher-order lists self-host"; fail=$$((fail+1)); \
	fi; \
	if timeout 15 ./self-host/ir_driver --combined examples/string_collection_methods.pith > "$$list_query_ir" && ! grep -Eq 'list_(is_empty|contains|index_of)' "$$list_query_ir"; then \
		pass=$$((pass+1)); echo "ok   list query methods self-host"; \
	else \
		echo "FAIL list query methods self-host"; fail=$$((fail+1)); \
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
