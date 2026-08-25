.PHONY: tls-live-interop tls-go-interop tls-rustls-interop tls-bogo tls-bogo-gate pithgen-check build self-host self-host-ir-driver bootstrap bootstrap-verify bootstrap-ir-checks bootstrap-ir-checks-only bootstrap-ir-fixed-point bootstrap-ir-fixed-point-only bootstrap-ir-invariants bootstrap-ir-invariants-only run-examples run-examples-self run-examples-self-only run-regressions run-regressions-only run-regressions-self run-regressions-self-only run-live-websocket-tests run-live-websocket-tests-self-only db-live-tests parity-examples parity-examples-only check-parse-invalid check-parse-invalid-only check-parse-invalid-self-host check-parse-invalid-self-host-only check-invalid check-invalid-only check-invalid-self-host check-invalid-self-host-only cli-regressions cli-regressions-only cli-regressions-self cli-regressions-self-only ir-contract-regressions ir-contract-regressions-only test-std-self test-std-self-only test-self-host-only test-fast-self status-audit check-no-panics safety-check fuzz-check fuzz green-smoke green-threadlocal green-pingpong green-producer-consumer green-waitgroup green-mutex green-semaphore green-barrier green-await-fanin green-echo green-starvation green-pinned-fairness green-tests verify-green-corpus verify-green-corpus-only verify-osthread-corpus verify-osthread-corpus-only docsite docsite-check lsp-check lsp-check-only zstd-pure-bench zstd-encode-check memcheck leak-check leak-check-only test clean


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
DB_LIVE_CASES := db_postgres_live db_mysql_live db_redis_live db_postgres_pool_live db_mysql_pool_live db_postgres_tx_live db_mysql_tx_live
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
	yaml_ops \
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

# the world-stopping cycle-gc tests run one at a time in their own process:
# a stop request parks every gated thread in the binary, and the collector
# thread a forced collect spawns lives for the rest of the process, so these
# tests cannot share a parallel test binary with unrelated suites.
test-cycle-gc:
	cargo test --manifest-path cranelift/Cargo.toml -p pith-runtime --locked -- --ignored --test-threads=1 cycle:: collections::map::tests::buffered_map

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
	if timeout 15 ./self-host/ir_driver --combined tests/cases/test_byte_compare_scan.pith | awk 'BEGIN { b=0 } /^call [0-9]+ pith_cstring_byte_at / { b=1 } END { if (b) exit 0; exit 1 }'; then \
		pass=$$((pass+1)); echo "ok   single-char comparison lowers to the byte read"; \
	else \
		echo "FAIL single-char comparison lowers to the byte read"; fail=$$((fail+1)); \
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
	@echo "self-host pith lines (library code): $$(git ls-files 'self-host/*.pith' | xargs awk -f tools/loc.awk)"
	@echo "self-host pith lines (test blocks): $$(git ls-files 'self-host/*.pith' | xargs awk -v want=test -f tools/loc.awk)"
	@echo "std pith lines (library code): $$(git ls-files 'std/*.pith' | xargs awk -f tools/loc.awk)"
	@echo "std pith lines (test blocks): $$(git ls-files 'std/*.pith' | xargs awk -v want=test -f tools/loc.awk)"
	@echo "tracked cranelift rust lines (library code): $$(git ls-files 'cranelift/*.rs' | xargs awk -f tools/loc-rs.awk)"
	@echo "tracked cranelift rust lines (test modules): $$(git ls-files 'cranelift/*.rs' | xargs awk -v want=test -f tools/loc-rs.awk)"
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

# --- stdlib reference site ---
# builds the doc extractor and renders docs/site/index.html from the
# comments in std/. regenerate whenever a pub item's docs change.

docsite:
	@./target/release/pith build tools/docsite/docsite.pith > /dev/null
	@./tools/docsite/docsite . docs/site

# --- docsite golden check ---
# runs the extractor over a fixture stdlib and diffs the markup. the fixture
# ships its own minimal assets so the golden pins generated html rather than
# the real stylesheet, which would churn on every design change.

docsite-check:
	@echo "--- docsite golden check ---"
	@./target/release/pith build tools/docsite/docsite.pith > /dev/null
	@tmpdir=$$(mktemp -d /tmp/pith-docsite-XXXXXX); \
	trap 'rm -rf "$$tmpdir"' EXIT; \
	./tools/docsite/docsite tools/docsite/sample "$$tmpdir" tools/docsite/sample/assets > /dev/null; \
	diff -u tools/docsite/expected/index.html "$$tmpdir/index.html" && \
	echo "docsite output matches golden files"

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

# --- lsp golden check ---
# replays the json-rpc transcript cases through `pith_main lsp` with
# debouncing disabled and diffs the response bodies against golden files

lsp-check: build self-host
	@$(MAKE) --no-print-directory lsp-check-only

lsp-check-only:
	@echo "--- lsp golden check ---"
	@./tooling/lsp_check.sh

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

# --- protogen golden check ---
# builds the proto3 code generator, regenerates the sample module and checks it
# is byte-for-byte the committed one (determinism), runs the generated code's
# round-trip and wire-vector tests, and valgrinds a driver over encode/decode

protogen-check:
	@echo "--- protogen golden check ---"
	@./target/release/pith build tools/protogen/protogen.pith > /dev/null
	@tmpdir=$$(mktemp -d /tmp/pith-protogen-XXXXXX); \
	trap 'rm -rf "$$tmpdir"' EXIT; \
	./tools/protogen/protogen tools/protogen/sample.proto "$$tmpdir/sample_gen.pith" > /dev/null; \
	diff -u tools/protogen/sample_gen.pith "$$tmpdir/sample_gen.pith" && \
	./target/release/pith test tools/protogen/protogen_test.pith && \
	if command -v valgrind > /dev/null; then \
		./target/release/pith build tools/protogen/protogen_memcheck.pith > /dev/null; \
		PITH_STRUCT_FREELIST=0 valgrind --error-exitcode=99 --leak-check=no --errors-for-leak-kinds=none -q ./tools/protogen/protogen_memcheck > /dev/null && \
		echo "protogen output matches golden, tests pass, valgrind clean"; \
	else \
		echo "protogen output matches golden and tests pass (valgrind absent)"; \
	fi

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
	@echo "--- tls/x509 parser fuzz (mutated real inputs, deterministic) ---"
	@./target/release/pith build tools/fuzz-tls/fuzz_tls.pith > /dev/null
	@./tools/fuzz-tls/fuzz_tls --count 400 --seed 1

# open-ended fuzzing: generated + corpus mutation. pass --count / --seed
# to explore. known findings live in the bulletproof plan; use this to
# hunt for new silent seams.
fuzz: build
	@./target/release/pith build tools/fuzz/fuzz.pith > /dev/null
	@./tools/fuzz/fuzz --count 300 --build-every 5

# the typed-program gate: pithgen builds well-typed programs and asserts
# every one checks, builds, and runs to its final marker — checked means
# buildable means runnable. the seed range is fixed and known clean, so
# any finding is a regression in the checker, the emitter, or the
# runtime's ownership paths. use tooling/pithgen directly for open-ended
# hunts and wider ranges.
pithgen-check: build self-host self-host-ir-driver
	@echo "--- pithgen check (typed programs, fixed seeds, deterministic) ---"
	@cargo build --release --quiet --manifest-path tooling/pithgen/Cargo.toml
	@rm -rf .pith-build/pithgen-check
	@tooling/pithgen/target/release/pithgen run --seeds 0..150 \
		--pith ./target/release/pith --out "$(CURDIR)/.pith-build/pithgen-check" \
		--fail-on-findings

# --- green-thread smoke test ---
# the green backend (PITH_GREEN=1) must produce byte-identical output to the
# os-thread backend (PITH_GREEN=0) on independent, non-coordinating tasks. this
# builds the fan-out/join smoke program once and compares the two runs.
#
# every green-* target below pins BOTH sides of the comparison explicitly. green
# is the default on linux, so a run that just says `pith run` is now the green
# side, and a differential that relied on the default for its reference would be
# comparing green against green and passing for free.
green-smoke: build
	@echo "--- green-thread smoke (byte-identical off vs on) ---"
	@off=$$(PITH_GREEN=0 ./target/release/pith run tests/green/smoke.pith 2>/dev/null); \
	on=$$(PITH_GREEN=1 ./target/release/pith run tests/green/smoke.pith 2>/dev/null); \
	if [ "$$off" = "$$on" ]; then \
		echo "ok   identical output: $$on"; \
	else \
		echo "FAIL output differs"; \
		echo "  os-thread: $$off"; \
		echo "  green:     $$on"; \
		exit 1; \
	fi

# --- green-thread threadlocal isolation test ---
# `threadlocal` module globals must be per-task under the green backend: one
# worker OS thread runs many tasks, so they must not share the worker's storage.
# each task reads a threadlocal (fresh 0), adds its id, and returns it — correct
# storage yields "bad 0" and output byte-identical to the os-thread backend.
# before P1b this failed green (tasks saw a sibling's leftover value).
green-threadlocal: build
	@echo "--- green-thread threadlocal isolation (byte-identical off vs on) ---"
	@off=$$(PITH_GREEN=0 ./target/release/pith run tests/green/threadlocal.pith 2>/dev/null); \
	on=$$(PITH_GREEN=1 ./target/release/pith run tests/green/threadlocal.pith 2>/dev/null); \
	if [ "$$off" = "$$on" ]; then \
		echo "ok   identical output: $$on"; \
	else \
		echo "FAIL output differs"; \
		echo "  os-thread: $$off"; \
		echo "  green:     $$on"; \
		exit 1; \
	fi

# --- green-thread channel coordination tests (P2) ---
# these two tasks are NOT independent: they coordinate through channels, so each
# blocks waiting on the other. we force a single worker (PITH_GREEN_WORKERS=1) so
# the pre-P2 "block the worker" behavior would deadlock outright — the first task
# to block parks the only worker and nothing else can run. P2 makes a would-block
# channel op yield the coroutine instead, so one worker cooperatively runs both
# tasks to completion. output must stay byte-identical to the os-thread backend.
green-pingpong: build
	@echo "--- green-thread channel ping-pong (byte-identical off vs on) ---"
	@off=$$(PITH_GREEN=0 ./target/release/pith run tests/green/pingpong.pith 2>/dev/null); \
	on=$$(PITH_GREEN=1 PITH_GREEN_WORKERS=1 ./target/release/pith run tests/green/pingpong.pith 2>/dev/null); \
	if [ "$$off" = "$$on" ]; then \
		echo "ok   identical output: $$on"; \
	else \
		echo "FAIL output differs"; \
		echo "  os-thread: $$off"; \
		echo "  green:     $$on"; \
		exit 1; \
	fi

green-producer-consumer: build
	@echo "--- green-thread producer/consumer (byte-identical off vs on) ---"
	@off=$$(PITH_GREEN=0 ./target/release/pith run tests/green/producer_consumer.pith 2>/dev/null); \
	on=$$(PITH_GREEN=1 PITH_GREEN_WORKERS=1 ./target/release/pith run tests/green/producer_consumer.pith 2>/dev/null); \
	if [ "$$off" = "$$on" ]; then \
		echo "ok   identical output: $$on"; \
	else \
		echo "FAIL output differs"; \
		echo "  os-thread: $$off"; \
		echo "  green:     $$on"; \
		exit 1; \
	fi

# --- green-thread blocking-primitive coordination tests (P2b) ---
# P2 made channel ops yield the green worker instead of parking it; P2b extends
# that to waitgroup, semaphore, and mutex. each of these programs coordinates
# green tasks through one of those primitives such that a would-block op must
# yield or the run deadlocks.
#
# waitgroup and mutex are pinned to a single worker (PITH_GREEN_WORKERS=1): a
# green task blocks on the primitive while it is the only worker, so the pre-P2b
# "park the worker" behavior would hang outright. P2b yields instead and the
# program completes with output byte-identical to the os-thread backend.
green-waitgroup: build
	@echo "--- green-thread waitgroup fan-out (byte-identical off vs on) ---"
	@off=$$(PITH_GREEN=0 ./target/release/pith run tests/green/waitgroup.pith 2>/dev/null); \
	on=$$(PITH_GREEN=1 PITH_GREEN_WORKERS=1 ./target/release/pith run tests/green/waitgroup.pith 2>/dev/null); \
	if [ "$$off" = "$$on" ]; then \
		echo "ok   identical output: $$on"; \
	else \
		echo "FAIL output differs"; \
		echo "  os-thread: $$off"; \
		echo "  green:     $$on"; \
		exit 1; \
	fi

green-mutex: build
	@echo "--- green-thread mutex shared-counter (byte-identical off vs on) ---"
	@off=$$(PITH_GREEN=0 ./target/release/pith run tests/green/mutex.pith 2>/dev/null); \
	on=$$(PITH_GREEN=1 PITH_GREEN_WORKERS=1 ./target/release/pith run tests/green/mutex.pith 2>/dev/null); \
	if [ "$$off" = "$$on" ]; then \
		echo "ok   identical output: $$on"; \
	else \
		echo "FAIL output differs"; \
		echo "  os-thread: $$off"; \
		echo "  green:     $$on"; \
		exit 1; \
	fi

# the semaphore contention only materializes with more than one worker (a single
# worker runs the tasks serially and never overlaps in the critical section), so
# this one runs at the default worker count. with permits < tasks the workers
# repeatedly block acquiring the permit, exercising the green acquire/release
# yield-and-wake path; the completion total must match the os-thread backend.
green-semaphore: build
	@echo "--- green-thread semaphore contention (byte-identical off vs on) ---"
	@off=$$(PITH_GREEN=0 ./target/release/pith run tests/green/semaphore.pith 2>/dev/null); \
	on=$$(PITH_GREEN=1 ./target/release/pith run tests/green/semaphore.pith 2>/dev/null); \
	if [ "$$off" = "$$on" ]; then \
		echo "ok   identical output: $$on"; \
	else \
		echo "FAIL output differs"; \
		echo "  os-thread: $$off"; \
		echo "  green:     $$on"; \
		exit 1; \
	fi

# --- green-thread barrier drain/release test ---
# several workers park on one channel while a coordinator drains their arrivals
# and then releases them. this is the shape that live-locked the single worker:
# a channel op woke every parked green task, not just the opposite role, so two
# workers parked on the release channel woke each other forever and starved the
# coordinator. it completed at two+ workers and on the os-thread backend, so we
# run it at BOTH a single worker and the default count — a regression here can
# only be caught at one worker.
green-barrier: build
	@echo "--- green-thread barrier drain/release (byte-identical off vs on, 1 and default workers) ---"
	@off=$$(PITH_GREEN=0 ./target/release/pith run tests/green/barrier.pith 2>/dev/null); \
	on1=$$(PITH_GREEN=1 PITH_GREEN_WORKERS=1 ./target/release/pith run tests/green/barrier.pith 2>/dev/null); \
	onN=$$(PITH_GREEN=1 ./target/release/pith run tests/green/barrier.pith 2>/dev/null); \
	if [ "$$off" = "$$on1" ] && [ "$$off" = "$$onN" ]; then \
		echo "ok   identical output at 1 and default workers: $$on1"; \
	else \
		echo "FAIL output differs"; \
		echo "  os-thread:        $$off"; \
		echo "  green 1 worker:   $$on1"; \
		echo "  green default:    $$onN"; \
		exit 1; \
	fi

# --- green-thread await fan-in test (P2c) ---
# a green coordinator task spawns K green children and awaits each. this is the
# case the P2/P2b work left as a follow-up: awaiting from inside a green task
# hard-parked the worker, so at a single worker the coordinator parks the only
# worker awaiting its first child and the child can never run — a deadlock. P2c
# makes await yield the coroutine instead, so one worker cooperatively runs the
# coordinator and its children to completion. we run at BOTH a single worker and
# the default count — the one-worker hang can only be caught at one worker.
green-await-fanin: build
	@echo "--- green-thread await fan-in (byte-identical off vs on, 1 and default workers) ---"
	@off=$$(PITH_GREEN=0 ./target/release/pith run tests/green/await_fanin.pith 2>/dev/null); \
	on1=$$(PITH_GREEN=1 PITH_GREEN_WORKERS=1 ./target/release/pith run tests/green/await_fanin.pith 2>/dev/null); \
	onN=$$(PITH_GREEN=1 ./target/release/pith run tests/green/await_fanin.pith 2>/dev/null); \
	if [ "$$off" = "$$on1" ] && [ "$$off" = "$$onN" ]; then \
		echo "ok   identical output at 1 and default workers: $$on1"; \
	else \
		echo "FAIL output differs"; \
		echo "  os-thread:        $$off"; \
		echo "  green 1 worker:   $$on1"; \
		echo "  green default:    $$onN"; \
		exit 1; \
	fi

# --- green-thread tcp echo (P3a: the epoll reactor) ---
# a green echo server and a green client run as spawned tasks over a real tcp
# socket. every socket op that would block — accept, connect, read, write —
# yields the green task to the epoll reactor instead of parking its worker OS
# thread. at a single worker the server and client share one worker, so the
# reactor must hand it back and forth between them; before P3a a would-block
# socket op parked the only worker and this deadlocked. we run it at BOTH one
# and the default worker count, since a reactor handoff regression only shows at
# one worker. the summary is input-determined, so it is byte-identical to the
# os-thread backend.
green-echo: build
	@echo "--- green-thread tcp echo (byte-identical off vs on, 1 and default workers) ---"
	@off=$$(PITH_GREEN=0 ./target/release/pith run tests/green/echo.pith 2>/dev/null); \
	on1=$$(PITH_GREEN=1 PITH_GREEN_WORKERS=1 ./target/release/pith run tests/green/echo.pith 2>/dev/null); \
	onN=$$(PITH_GREEN=1 ./target/release/pith run tests/green/echo.pith 2>/dev/null); \
	if [ "$$off" = "$$on1" ] && [ "$$off" = "$$onN" ]; then \
		echo "ok   identical output at 1 and default workers: $$on1"; \
	else \
		echo "FAIL output differs"; \
		echo "  os-thread:        $$off"; \
		echo "  green 1 worker:   $$on1"; \
		echo "  green default:    $$onN"; \
		exit 1; \
	fi

# --- green-thread cooperative preemption (P5: the safe-point + monitor) ---
# a compute-only task spins in a tight loop with no yield point while N reporter
# tasks must each bump a shared counter. WITHOUT preemption the spinner holds its
# worker forever and the reporters never run, so at a single worker
# (PITH_GREEN_WORKERS=1) the program hangs. the green runs here compile with
# PITH_GREEN_PREEMPT=1 so the backend inserts safe-points at loop back-edges; the
# monitor then deschedules the overrunning spinner onto its worker's lowest-
# priority queue and the reporters run. the os-thread run needs no safe-points, so
# `off` compiles at the default (zero-overhead) setting. the printed total is the
# fixed counter value, so all three runs are byte-identical. we run the green side
# at BOTH one and the default worker count — the single-worker hang can only be
# caught at one worker.
green-starvation: build
	@echo "--- green-thread cooperative preemption (byte-identical off vs on, 1 and default workers) ---"
	@off=$$(PITH_GREEN=0 ./target/release/pith run tests/green/starvation.pith 2>/dev/null); \
	on1=$$(PITH_GREEN_PREEMPT=1 PITH_GREEN=1 PITH_GREEN_WORKERS=1 ./target/release/pith run tests/green/starvation.pith 2>/dev/null); \
	onN=$$(PITH_GREEN_PREEMPT=1 PITH_GREEN=1 ./target/release/pith run tests/green/starvation.pith 2>/dev/null); \
	if [ "$$off" = "$$on1" ] && [ "$$off" = "$$onN" ]; then \
		echo "ok   identical output at 1 and default workers: $$on1"; \
	else \
		echo "FAIL output differs"; \
		echo "  os-thread:        $$off"; \
		echo "  green 1 worker:   $$on1"; \
		echo "  green default:    $$onN"; \
		exit 1; \
	fi

# --- green-thread wake affinity fairness ---
# waking a task onto the worker that woke it is a big win for a ping-pong pair,
# but the affinity slot holds exactly one task: a second same-worker wake
# displaces the first to the back of the fifo. without that displacement a hot
# pair keeps re-claiming the slot and any task queued behind it never runs, so at
# a single worker this hangs instead of printing. preemption cannot rescue it —
# each half of the pair re-parks long before it overruns its quantum.
green-pinned-fairness: build
	@echo "--- green-thread wake affinity fairness (byte-identical off vs on, 1 and default workers) ---"
	@off=$$(PITH_GREEN=0 ./target/release/pith run tests/green/pinned_fairness.pith 2>/dev/null); \
	on1=$$(PITH_GREEN=1 PITH_GREEN_WORKERS=1 ./target/release/pith run tests/green/pinned_fairness.pith 2>/dev/null); \
	onN=$$(PITH_GREEN=1 ./target/release/pith run tests/green/pinned_fairness.pith 2>/dev/null); \
	if [ "$$off" = "$$on1" ] && [ "$$off" = "$$onN" ]; then \
		echo "ok   identical output at 1 and default workers: $$(echo $$on1)"; \
	else \
		echo "FAIL output differs"; \
		echo "  os-thread:        $$off"; \
		echo "  green 1 worker:   $$on1"; \
		echo "  green default:    $$onN"; \
		exit 1; \
	fi

# --- green-thread test suite ---
# the deterministic, bounded green-thread tests, gathered so ci can run them as
# one step. each compares PITH_GREEN=0 against PITH_GREEN=1 and must match
# byte-for-byte. green-echo is intentionally left out: it binds a
# fixed loopback port and races a sleep to let the server start, which is too
# flaky for a shared runner.
green-tests: green-smoke green-threadlocal green-pingpong green-producer-consumer green-waitgroup green-mutex green-semaphore green-barrier green-await-fanin green-starvation green-pinned-fairness
	@echo "all green-thread tests passed"

# --- full corpus under each backend ---
# the dedicated green-tests above prove the coordination primitives; these two
# targets prove the whole deterministic regression corpus produces the same
# output on either backend. the expected files are the fixed os-thread answers,
# so a run whose output drifts from its expected file is a correctness bug in
# that backend, not a timing artifact.
#
# both targets name their backend explicitly rather than leaning on the default.
# green is the default on linux and `make test` therefore already covers it at
# the default worker count, but that is a property of the host, not of the
# target: verify-green-corpus has to hold on a mac too, and the os-thread pass
# is the only coverage the PITH_GREEN=0 opt-out gets on a linux runner.
#
# green runs twice, at the default worker count and pinned to one worker, since
# a single-worker deadlock (the shape that turned up a read deadline bug) can
# only be caught at one worker. os threads have no such knob and run once.
#
# a case that legitimately differs under green (nondeterministic scheduling
# order that its expected output happens to encode) belongs in
# GREEN_CORPUS_EXCLUDE with a reason, not in a loosened compare.
GREEN_CORPUS_EXCLUDE :=
GREEN_CORPUS_EXPECTED := $(filter-out $(addprefix tests/expected/,$(addsuffix .txt,$(GREEN_CORPUS_EXCLUDE))),$(REGRESSION_EXPECTED))

verify-green-corpus: build verify-green-corpus-only

verify-green-corpus-only:
	@echo "--- regression corpus under the green backend (default + 1 worker) ---"
	@pass=0; fail=0; skip=0; \
	for f in $(GREEN_CORPUS_EXPECTED); do \
		name=$$(basename "$$f" .txt); \
		src="tests/cases/$$name.pith"; \
		[ -f "$$src" ] || { skip=$$((skip+1)); continue; }; \
		expected=$$(cat "$$f"); \
		gN=$$(timeout 60 env PITH_GREEN=1 ./target/release/pith run "$$src" 2>/dev/null); \
		g1=$$(timeout 60 env PITH_GREEN=1 PITH_GREEN_WORKERS=1 ./target/release/pith run "$$src" 2>/dev/null); \
		if [ "$$gN" = "$$expected" ] && [ "$$g1" = "$$expected" ]; then \
			pass=$$((pass+1)); \
		elif [ "$$gN" != "$$expected" ]; then \
			echo "FAIL $$name (green default workers)"; fail=$$((fail+1)); \
		else \
			echo "FAIL $$name (green 1 worker)"; fail=$$((fail+1)); \
		fi; \
	done; \
	echo "$$pass passed, $$fail failed, $$skip without a source"; \
	if [ $$fail -gt 0 ]; then exit 1; fi; \
	echo "green corpus matches the expected output"

verify-osthread-corpus: build verify-osthread-corpus-only

verify-osthread-corpus-only:
	@echo "--- regression corpus under the os-thread backend (PITH_GREEN=0) ---"
	@pass=0; fail=0; skip=0; \
	for f in $(GREEN_CORPUS_EXPECTED); do \
		name=$$(basename "$$f" .txt); \
		src="tests/cases/$$name.pith"; \
		[ -f "$$src" ] || { skip=$$((skip+1)); continue; }; \
		expected=$$(cat "$$f"); \
		actual=$$(timeout 60 env PITH_GREEN=0 ./target/release/pith run "$$src" 2>/dev/null); \
		if [ "$$actual" = "$$expected" ]; then \
			pass=$$((pass+1)); \
		else \
			echo "FAIL $$name (os threads)"; fail=$$((fail+1)); \
		fi; \
	done; \
	echo "$$pass passed, $$fail failed, $$skip without a source"; \
	if [ $$fail -gt 0 ]; then exit 1; fi; \
	echo "os-thread corpus matches its expected output"

# --- memcheck ---
# run a curated set of memory-management-heavy programs under valgrind
# with an error exit code, so a use-after-free or an out-of-bounds read
# fails ci even when the program happens to print the right answer (an
# enum-payload overread did exactly that until it was caught here). the
# full example + regression corpus is valgrind-clean; this subset keeps
# the gate fast.
#
# MEMCHECK_CASES runs on whatever backend is the default here, which on linux is
# green. MEMCHECK_OSTHREAD_CASES is the handful whose whole subject is the
# os-thread task machinery (its slab reclaim, its join state, the ownership of
# values handed across a real thread), so those run again with PITH_GREEN=0 —
# otherwise the flip would quietly stop testing the code they were written for.
MEMCHECK_OSTHREAD_CASES := \
	tests/cases/test_os_thread_spawn_reclaim tests/cases/test_await_ownership \
	tests/cases/test_channel_fanout_ownership \
	tests/cases/test_channel_try_send_ownership

MEMCHECK_CASES := \
	tests/cases/test_match_payload tests/cases/test_combo_enums_deep \
	tests/cases/test_fn_value_positions tests/cases/test_global_fn_value \
	tests/cases/test_fn_value_ownership tests/cases/test_closure_list_ownership \
	tests/cases/test_optional_collections tests/cases/test_nested_optional_literals \
	tests/cases/test_optional_value tests/cases/test_closure_struct_return \
	tests/cases/test_combo_closures tests/cases/test_combo_structs_deep \
	tests/cases/test_generic_to_string tests/cases/test_map_int_string_buffered \
	tests/cases/test_enum_payload_churn tests/cases/test_ownership_stress \
	tests/cases/test_enum_scope_release \
	tests/cases/test_field_reassign_release tests/cases/test_discarded_result \
	tests/cases/test_global_rebind_ownership \
	tests/cases/test_generic_method_return tests/cases/test_generic_fnvalue_return \
	tests/cases/test_fnvalue_field_pool tests/cases/test_closure_capture_escape \
	tests/cases/test_closure_lifecycle tests/cases/test_error_path_cleanup \
	tests/cases/test_defer_leak tests/cases/test_errdefer_leak \
	tests/cases/test_atomic_context tests/cases/test_tuple_ownership \
	tests/cases/test_channel_try_send_ownership \
	tests/cases/test_channel_fanout_ownership \
	tests/cases/test_empty_list_field_ownership \
	tests/cases/test_list_bytes_ownership \
	tests/cases/test_container_eviction_release \
	tests/cases/test_sql_numeric \
	tests/cases/test_index_key_ownership \
	tests/cases/test_owned_container_arg_release \
	tests/cases/test_direct_store_ownership \
	tests/cases/test_struct_store_ownership \
	tests/cases/test_generic_out_list_ownership \
	tests/cases/test_nested_generic_sort_ownership \
	tests/cases/test_list_transform_ownership \
	tests/cases/test_result_unwrap_or_fallback_leak \
	tests/cases/test_result_arg_borrow_leak \
	tests/cases/test_generic_enum_match \
	tests/cases/test_closure_many_captures \
	tests/cases/test_closure_capture_boundaries \
	tests/cases/test_optional_match_ownership \
	tests/cases/test_optional_payload_eq \
	tests/cases/test_tail_match_defer \
	tests/cases/test_argument_literal_ownership \
	tests/cases/test_borrowed_operand_extraction \
	tests/cases/test_unwrap_or_borrowed_fallback \
	tests/cases/test_task_result_payloads \
	tests/cases/test_struct_closure_field tests/cases/test_generic_struct_field_dtor \
	tests/cases/test_generic_instance_dtor \
	tests/cases/test_generic_optional_return \
	tests/cases/test_generic_variant_slots tests/cases/test_generic_field_kinds \
	tests/cases/test_generic_instance_type_args \
	tests/cases/test_generic_call_shapes tests/cases/test_generic_boundary \
	tests/cases/test_iterator_drain tests/cases/test_method_fresh_string_return \
	tests/cases/test_lazy_adapter_pipeline tests/cases/test_named_local_adapter_chain \
	tests/cases/test_loop_var_shadows_fn tests/cases/test_weak_reference \
	tests/cases/test_http2_concurrent_events tests/cases/test_http2_send_body \
	tests/cases/test_http2_server_roundtrip tests/cases/test_http2_tls_roundtrip \
	tests/cases/test_http2_close_frees_fd \
	tests/cases/test_http2_threaded_body_paths \
	tests/cases/test_http1_tls_fallback \
	tests/cases/test_protobuf_roundtrip tests/cases/test_module_ctor_heap_field \
	tests/cases/test_catch_heap_no_leak tests/cases/test_list_struct_no_leak \
	tests/cases/test_optional_temp_release \
	tests/cases/test_list_optional_element_search \
	tests/cases/test_loop_iter_early_return \
	tests/cases/test_loop_var_slot_isolation \
	tests/cases/test_spawn_in_loop_capture \
	tests/cases/test_result_ok_reclaim \
	tests/cases/test_os_thread_spawn_reclaim \
	tests/cases/test_await_ownership \
	tests/cases/test_await_optional_shell \
	tests/cases/test_index_search_shell \
	tests/cases/test_discarded_optional_result \
	tests/cases/test_yaml_structure tests/cases/test_yaml_malformed \
	tests/cases/test_yaml_derived_decode \
	tests/cases/test_concurrent_group \
	tests/cases/test_map_value_ownership \
	tests/cases/test_web_session_ownership \
	tests/cases/test_tls_server_config_release

memcheck: build
	@echo "--- memcheck (valgrind, curated) ---"
	@command -v valgrind > /dev/null || { echo "valgrind not installed; skipping"; exit 0; }
	@fail=0; \
	for base in $(MEMCHECK_CASES); do \
		./target/release/pith build "$$base.pith" > /dev/null 2>&1 || { echo "FAIL build $$base"; fail=1; continue; }; \
		if PITH_STRUCT_FREELIST=0 valgrind --error-exitcode=99 --leak-check=no --errors-for-leak-kinds=none -q "$$base" > /dev/null 2>/tmp/pith-memcheck.txt; then \
			echo "ok   $$base"; \
		else \
			echo "FAIL $$base (valgrind)"; head -6 /tmp/pith-memcheck.txt; fail=1; \
		fi; \
	done; \
	for base in $(MEMCHECK_OSTHREAD_CASES); do \
		./target/release/pith build "$$base.pith" > /dev/null 2>&1 || { echo "FAIL build $$base"; fail=1; continue; }; \
		if PITH_GREEN=0 PITH_STRUCT_FREELIST=0 valgrind --error-exitcode=99 --leak-check=no --errors-for-leak-kinds=none -q "$$base" > /dev/null 2>/tmp/pith-memcheck.txt; then \
			echo "ok   $$base (os threads)"; \
		else \
			echo "FAIL $$base (valgrind, os threads)"; head -6 /tmp/pith-memcheck.txt; fail=1; \
		fi; \
	done; \
	if [ $$fail -ne 0 ]; then exit 1; fi; \
	echo "all memcheck cases clean"

# --- leak growth gate ---
# valgrind above catches a use-after-free but is run with its leak check off
# on purpose, because the runtime's freelists, stack pool and arenas live for
# the whole process and drown a real leak in "still reachable" noise. this
# target covers the other half: each case under tests/leaks/ churns one
# ownership shape at two round counts and its peak resident set has to be the
# same either way. the details, and how to add a case, are in
# tooling/leak_check.sh and docs/ownership.md.

leak-check: build leak-check-only

leak-check-only:
	@bash tooling/leak_check.sh

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

# --- zstd interop check ---
# both directions against the system tool: pith reads zstd's output at two
# levels, and zstd reads pith's

zstd-interop-check:
	@echo "--- zstd interop check ---"
	@tmpdir=$$(mktemp -d /tmp/pith-zstd-XXXXXX); \
	trap 'rm -rf "$$tmpdir"' EXIT; \
	printf 'import std.fs as fs\nimport std.compress.zstd as zstd\n\nfn main() -> Int!:\n    raw := fs.read_bytes("README.md")!\n    fs.write_bytes("'"$$tmpdir"'/out.zst", zstd.compress(raw))!\n    return 0\n' > "$$tmpdir/pack.pith"; \
	./target/release/pith run "$$tmpdir/pack.pith" > /dev/null 2>&1 && \
	zstd -d -c "$$tmpdir/out.zst" | cmp - README.md && \
	echo "system zstd reads pith output byte-identical"; \
	zstd -3 -c README.md > "$$tmpdir/sys3.zst"; \
	zstd -19 -c README.md > "$$tmpdir/sys19.zst"; \
	printf 'import std.fs as fs\nimport std.compress.zstd as zstd\n\nfn main() -> Int!:\n    a := zstd.decompress(fs.read_bytes("'"$$tmpdir"'/sys3.zst")!)!\n    b := zstd.decompress(fs.read_bytes("'"$$tmpdir"'/sys19.zst")!)!\n    fs.write_bytes("'"$$tmpdir"'/back3", a)!\n    fs.write_bytes("'"$$tmpdir"'/back19", b)!\n    return 0\n' > "$$tmpdir/unpack.pith"; \
	./target/release/pith run "$$tmpdir/unpack.pith" > /dev/null 2>&1 && \
	cmp "$$tmpdir/back3" README.md && cmp "$$tmpdir/back19" README.md && \
	echo "pith reads system zstd output at levels 3 and 19 byte-identical"

# --- pure-pith zstd decoder interop ---
# the pure decoder (no libzstd) against frames the system tool produced,
# across the shapes that exercise every block and literals type: raw blocks
# from incompressible input, huffman literals from wide-alphabet text,
# multi-block frames with carried repeat-offset state, and the edge sizes.

# throughput of the pure decoder against the crate-backed kernel, on a
# corpus built from the repo itself. see bench/zstd_codec.pith for why the
# highly-compressible case is reported but not treated as the headline.
zstd-pure-bench: build
	@./target/release/pith run bench/zstd_codec.pith

zstd-pure-check:
	@echo "--- pure-pith zstd decoder interop ---"
	@tmpdir=$$(mktemp -d /tmp/pith-zstdpure-XXXXXX); \
	trap 'rm -rf "$$tmpdir"' EXIT; \
	cp README.md "$$tmpdir/text.bin"; \
	head -c 120000 /dev/urandom > "$$tmpdir/random.bin"; \
	python3 -c "import sys; sys.stdout.write(''.join(chr(32+(i*37)%95) for i in range(150000)))" > "$$tmpdir/spread.bin"; \
	python3 -c "import sys; sys.stdout.write('ab'*40000)" > "$$tmpdir/repeat.bin"; \
	printf '' > "$$tmpdir/empty.bin"; \
	printf 'a' > "$$tmpdir/one.bin"; \
	for f in text random spread repeat empty one; do \
	  for lvl in 1 5 19; do zstd -$$lvl -c "$$tmpdir/$$f.bin" > "$$tmpdir/$$f.$$lvl.zst" 2>/dev/null; done; \
	done; \
	printf 'import std.fs as fs\nimport std.compress.zstd_pure_frame as zf\n\nfn main() -> Int!:\n    mut ok := 0\n    for name in ["text", "random", "spread", "repeat", "empty", "one"]:\n        raw := fs.read_bytes("'"$$tmpdir"'/" + name + ".bin")!\n        for lvl in ["1", "5", "19"]:\n            got := zf.decompress_bounded(fs.read_bytes("'"$$tmpdir"'/" + name + "." + lvl + ".zst")!, 4000000)!\n            if got != raw:\n                fail "mismatch: " + name + " -" + lvl\n            ok = ok + 1\n    print("{ok} system zstd frames decoded byte-identical by the pure decoder")\n    return 0\n' > "$$tmpdir/check.pith"; \
	./target/release/pith run "$$tmpdir/check.pith"

# --- pure-pith zstd encoder interop ---
# the pure encoder against the system tool: pith compresses every corpus
# shape (including the block-boundary and single-byte-run edges), the system
# zstd binary decompresses each frame, and the result must byte-compare to
# the input. the pure decoder must also read every frame back, and the size
# table against `zstd -3` keeps the ratios honest.

zstd-encode-check:
	@echo "--- pure-pith zstd encoder interop ---"
	@tmpdir=$$(mktemp -d /tmp/pith-zstdenc-XXXXXX); \
	trap 'rm -rf "$$tmpdir"' EXIT; \
	cp README.md "$$tmpdir/text.bin"; \
	head -c 120000 /dev/urandom > "$$tmpdir/random.bin"; \
	python3 -c "import sys; sys.stdout.write(''.join(chr(32+(i*37)%95) for i in range(150000)))" > "$$tmpdir/spread.bin"; \
	python3 -c "import sys; sys.stdout.write('ab'*40000)" > "$$tmpdir/repeat.bin"; \
	printf '' > "$$tmpdir/empty.bin"; \
	printf 'a' > "$$tmpdir/one.bin"; \
	python3 -c "import sys; d=open('README.md','rb').read(); sys.stdout.buffer.write((d*(131073//len(d)+1))[:131072])" > "$$tmpdir/exact.bin"; \
	python3 -c "import sys; d=open('README.md','rb').read(); sys.stdout.buffer.write((d*(131074//len(d)+1))[:131073])" > "$$tmpdir/over.bin"; \
	head -c 200000 /dev/zero > "$$tmpdir/same.bin"; \
	printf 'import std.fs as fs\nimport std.compress.zstd_pure_encode as ze\nimport std.compress.zstd_pure_frame as zf\n\nfn main() -> Int!:\n    dir := "'"$$tmpdir"'/"\n    for name in ["text", "random", "spread", "repeat", "empty", "one", "exact", "over", "same"]:\n        raw := fs.read_bytes(dir + name + ".bin")!\n        packed := ze.compress_checked(raw)!\n        back := zf.decompress_bounded(packed, 50000000)!\n        if back != raw:\n            fail "pure-decoder round-trip mismatch: " + name\n        fs.write_bytes(dir + name + ".zst", packed)!\n    print("9 shapes compressed; the pure decoder reads each back byte-identical")\n    return 0\n' > "$$tmpdir/pack.pith"; \
	./target/release/pith run "$$tmpdir/pack.pith" && \
	for f in text random spread repeat empty one exact over same; do \
	  zstd -q -d -c "$$tmpdir/$$f.zst" | cmp - "$$tmpdir/$$f.bin" || { echo "FAIL system zstd mismatch: $$f"; exit 1; }; \
	done && \
	echo "system zstd decodes every pith-compressed frame byte-identical" && \
	echo "--- size vs zstd -3 ---" && \
	for f in text random spread repeat empty one exact over same; do \
	  zstd -q -3 -c "$$tmpdir/$$f.bin" > "$$tmpdir/$$f.sys3.zst"; \
	  printf '%-8s raw %8d  pith %8d  zstd-3 %8d\n' "$$f" "$$(wc -c < "$$tmpdir/$$f.bin")" "$$(wc -c < "$$tmpdir/$$f.zst")" "$$(wc -c < "$$tmpdir/$$f.sys3.zst")"; \
	done

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
	out=$$(./target/release/pith check --json "$$tmpdir/bad.pith" 2>/dev/null); \
	set -e; \
	case "$$out" in '['*'"code":"E'*) pass=$$((pass+1)); echo "ok   check --json diagnostics";; *) echo "FAIL check --json diagnostics"; fail=$$((fail+1));; esac; \
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
	printf 'fn BadName():\n    return\n\nfn main():\n    BadName()\n' > "$$tmpdir/lint_pos.pith"; \
	set +e; \
	lint_out=$$(./target/release/pith lint --json "$$tmpdir/lint_pos.pith" 2>/dev/null); \
	set -e; \
	case "$$lint_out" in *'"code":"E300"'*'"line":0'*) echo "FAIL lint --json positions"; fail=$$((fail+1));; *'"code":"E300"'*'"line":'*) pass=$$((pass+1)); echo "ok   lint --json positions";; *) echo "FAIL lint --json positions"; fail=$$((fail+1));; esac; \
	printf 'struct Node:\n    next: Node?\n\nfn main():\n    return\n' > "$$tmpdir/lint_cycle.pith"; \
	printf 'struct Node:\n    weak next: Node?\n\nfn main():\n    return\n' > "$$tmpdir/lint_weak.pith"; \
	set +e; \
	cyc_out=$$(./target/release/pith lint --json "$$tmpdir/lint_cycle.pith" 2>/dev/null); \
	weak_out=$$(./target/release/pith lint --json "$$tmpdir/lint_weak.pith" 2>/dev/null); \
	set -e; \
	case "$$cyc_out" in *'"code":"E306"'*) pass=$$((pass+1)); echo "ok   lint strong cycle";; *) echo "FAIL lint strong cycle"; fail=$$((fail+1));; esac; \
	case "$$weak_out" in *'"code":"E306"'*) echo "FAIL lint weak edge exempt"; fail=$$((fail+1));; *) pass=$$((pass+1)); echo "ok   lint weak edge exempt";; esac; \
	printf 'import std.net.tls as tls\n\nfn build() -> Int!:\n    config := tls.client_config()!\n    return 0\n\nfn main():\n    return\n' > "$$tmpdir/lint_open.pith"; \
	printf 'import std.net.tls as tls\n\nfn build() -> Int!:\n    config := tls.client_config()!\n    defer config.close()\n    return 0\n\nfn main():\n    return\n' > "$$tmpdir/lint_closed.pith"; \
	printf 'import std.net.tls as tls\n\nfn keep(c: tls.Config):\n    return\n\nfn build() -> Int!:\n    config := tls.client_config()!\n    keep(config)\n    return 0\n\nfn main():\n    return\n' > "$$tmpdir/lint_handoff.pith"; \
	set +e; \
	open_out=$$(./target/release/pith lint --json "$$tmpdir/lint_open.pith" 2>/dev/null); \
	closed_out=$$(./target/release/pith lint --json "$$tmpdir/lint_closed.pith" 2>/dev/null); \
	handoff_out=$$(./target/release/pith lint --json "$$tmpdir/lint_handoff.pith" 2>/dev/null); \
	set -e; \
	case "$$open_out" in *'"code":"E307"'*) pass=$$((pass+1)); echo "ok   lint unclosed resource";; *) echo "FAIL lint unclosed resource"; fail=$$((fail+1));; esac; \
	case "$$closed_out" in *'"code":"E307"'*) echo "FAIL lint deferred close exempt"; fail=$$((fail+1));; *) pass=$$((pass+1)); echo "ok   lint deferred close exempt";; esac; \
	case "$$handoff_out" in *'"code":"E307"'*) echo "FAIL lint handed-off resource exempt"; fail=$$((fail+1));; *) pass=$$((pass+1)); echo "ok   lint handed-off resource exempt";; esac; \
	printf 'mut items: List[Int] := []\n\nfn helper() -> Int:\n    mut items: List[Int] := [7]\n    return items.len()\n\nfn main():\n    print("{helper()}")\n' > "$$tmpdir/lint_shadow.pith"; \
	printf 'mut items: List[Int] := []\n\nfn helper() -> Int:\n    mut own: List[Int] := [7]\n    return own.len()\n\nfn main():\n    print("{helper()}")\n' > "$$tmpdir/lint_noshadow.pith"; \
	set +e; \
	shadow_out=$$(./target/release/pith lint --json "$$tmpdir/lint_shadow.pith" 2>/dev/null); \
	noshadow_out=$$(./target/release/pith lint --json "$$tmpdir/lint_noshadow.pith" 2>/dev/null); \
	set -e; \
	case "$$shadow_out" in *'"code":"E308"'*) pass=$$((pass+1)); echo "ok   lint shadowed global";; *) echo "FAIL lint shadowed global"; fail=$$((fail+1));; esac; \
	case "$$noshadow_out" in *'"code":"E308"'*) echo "FAIL lint distinct local exempt"; fail=$$((fail+1));; *) pass=$$((pass+1)); echo "ok   lint distinct local exempt";; esac; \
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
	printf 'from std.testing import case, each\n\ntest "rows":\n    each([\n        case("alpha", 1),\n        case("beta", 2),\n], fn(n: Int) => n > 0)\n' > "$$tmpdir/rows.pith"; \
	set +e; \
	out=$$(./target/release/pith test "$$tmpdir/rows.pith" --filter "rows / beta" 2>&1); \
	status=$$?; \
	set -e; \
	if [ $$status -eq 0 ] && echo "$$out" | grep -q "\[2\] beta" && ! echo "$$out" | grep -q "alpha" && echo "$$out" | grep -q "rows ... ok"; then pass=$$((pass+1)); echo "ok   test --filter selects one table row"; else echo "FAIL test --filter table row"; echo "$$out"; fail=$$((fail+1)); fi; \
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
	@echo "=== Step 14: tool golden checks ==="
	@$(MAKE) --no-print-directory docsite-check
	@$(MAKE) --no-print-directory sitegen-check
	@echo "=== all tests passed ==="

clean:
	cargo clean
	rm -rf .pith-build

# tls live tests, compared against goldens. the openssl cases prove the 1.2
# client and the 1.3 resumption binder interoperate with a real foreign
# implementation, which a pith-to-pith test cannot show; test_tls_echo_live is
# the in-process pith suite (echo, resumption, dynamic sni selection, mutual
# tls, optional client auth) and asserts internally, so its golden is empty.
TLS_LIVE_INTEROP_CASES := test_tls12_openssl_live test_tls_resumption_openssl_live test_tls_echo_live test_tls_large_chain_live test_tls_aes256_openssl_live test_tls_p384_openssl_live test_tls12_aes256_openssl_live

tls-live-interop: build
	@echo "--- tls live tests (openssl interop + in-process suite) ---"
	@command -v openssl >/dev/null 2>&1 || { echo "openssl not found; skipping tls live interop"; exit 0; }
	@pass=0; fail=0; \
	for name in $(TLS_LIVE_INTEROP_CASES); do \
		actual=$$(timeout 120 ./target/release/pith run "tests/live/$$name.pith" 2>/dev/null); \
		expected=$$(cat "tests/live/expected/$$name.txt"); \
		if [ "$$actual" = "$$expected" ]; then \
			pass=$$((pass+1)); echo "ok   $$name"; \
		else \
			echo "FAIL $$name"; echo "--- expected ---"; echo "$$expected"; echo "--- actual ---"; echo "$$actual"; fail=$$((fail+1)); \
		fi; \
	done; \
	if [ $$fail -ne 0 ]; then echo "$$fail tls live interop cases failed"; exit 1; fi; \
	echo "all tls live interop cases passed"

# interop against rustls (a third independent reference stack), both directions
# across tls 1.2/1.3 and rsa/ecdsa. builds the small rust peer first; rustls is
# a valuable peer precisely because it is strict about 1.2 (extended master
# secret required, aead only).
tls-rustls-interop: build
	@echo "--- tls interop (rustls) ---"
	@command -v cargo >/dev/null 2>&1 || { echo "cargo not found; skipping tls rustls interop"; exit 0; }
	@(cd tests/interop/rustls_peer && cargo build --release --quiet) || { echo "rustls peer build failed"; exit 1; }
	@actual=$$(timeout 120 ./target/release/pith run tests/live/test_tls_rustls_interop.pith 2>/dev/null); \
	expected=$$(cat tests/live/expected/test_tls_rustls_interop.txt); \
	if [ "$$actual" = "$$expected" ]; then \
		echo "ok   tls rustls interop"; \
	else \
		echo "FAIL tls rustls interop"; echo "--- expected ---"; echo "$$expected"; echo "--- actual ---"; echo "$$actual"; exit 1; \
	fi

# run the BoringSSL test runner (BoGo) against the pith shim. needs a
# boringssl checkout (pass BOGO=/path/to/boringssl) and go; not part of ci —
# the checkout is large and the suite is a workbench, not a gate. unknown
# runner flags make the shim exit 89, which the runner counts as a skip, and
# tests/interop/bogo/config.json disables the cases this stack refuses by
# design. BOGO_TESTS narrows the run, e.g. BOGO_TESTS='Basic*'.
tls-bogo: build
	@echo "--- bogo (boringssl test runner) ---"
	@command -v go >/dev/null 2>&1 || { echo "go not found; skipping bogo"; exit 0; }
	@[ -n "$(BOGO)" ] && [ -d "$(BOGO)/ssl/test/runner" ] || { echo "set BOGO=/path/to/boringssl (ssl/test/runner missing); skipping"; exit 0; }
	@./target/release/pith build tests/interop/bogo/shim.pith >/dev/null
	@cd "$(BOGO)/ssl/test/runner" && go test \
		-shim-path "$(CURDIR)/tests/interop/bogo/shim" \
		-shim-config "$(CURDIR)/tests/interop/bogo/config.json" \
		-allow-unimplemented -loose-errors -pipe \
		$(if $(BOGO_TESTS),-test "$(BOGO_TESTS)",)

# the bogo conformance gate: run the full suite and compare the failure set
# against the checked-in baseline. a failure not in the baseline is a
# regression and fails the gate; a baseline entry that now passes is reported
# so the list gets pruned. BOGO must point at a boringssl checkout.
tls-bogo-gate: build
	@echo "--- bogo conformance gate ---"
	@command -v go >/dev/null 2>&1 || { echo "go not found; cannot run the bogo gate"; exit 1; }
	@[ -n "$(BOGO)" ] && [ -d "$(BOGO)/ssl/test/runner" ] || { echo "set BOGO=/path/to/boringssl (ssl/test/runner missing)"; exit 1; }
	@./target/release/pith build tests/interop/bogo/shim.pith >/dev/null
	@cd "$(BOGO)/ssl/test/runner" && go test \
		-shim-path "$(CURDIR)/tests/interop/bogo/shim" \
		-shim-config "$(CURDIR)/tests/interop/bogo/config.json" \
		-allow-unimplemented -loose-errors -pipe > "$(CURDIR)/bogo-gate.log" 2>&1 || true
	@grep -aoE '^FAILED \(.*\)' bogo-gate.log | sed 's/FAILED (//;s/)$$//' | sort -u > bogo-gate-failures.txt
	@echo "suite: $$(grep -ac '^PASS' bogo-gate.log) passed, $$(wc -l < bogo-gate-failures.txt) failed"
	@comm -23 bogo-gate-failures.txt tests/interop/bogo/known_failures.txt > bogo-gate-new.txt; \
	comm -13 bogo-gate-failures.txt tests/interop/bogo/known_failures.txt > bogo-gate-fixed.txt; \
	if [ -s bogo-gate-fixed.txt ]; then echo "newly passing (prune from known_failures.txt):"; cat bogo-gate-fixed.txt; fi; \
	if [ -s bogo-gate-new.txt ]; then \
		echo "retrying new failures serially (the parallel run can starve cases):"; cat bogo-gate-new.txt; \
		retry=$$(paste -sd';' bogo-gate-new.txt); \
		( cd "$(BOGO)/ssl/test/runner" && go test -shim-path "$(CURDIR)/tests/interop/bogo/shim" -shim-config "$(CURDIR)/tests/interop/bogo/config.json" -allow-unimplemented -loose-errors -pipe -test "$$retry" > "$(CURDIR)/bogo-gate-retry.log" 2>&1 || true ); \
		grep -aoE '^FAILED \(.*\)' bogo-gate-retry.log | sed 's/FAILED (//;s/)$$//' | sort -u > bogo-gate-confirmed.txt; \
		if [ -s bogo-gate-confirmed.txt ]; then echo "NEW FAILURES (confirmed serially):"; cat bogo-gate-confirmed.txt; exit 1; fi; \
		echo "all new failures passed on serial retry (load flakes)"; \
	fi
	@echo "bogo gate ok: no new failures"

# interop against Go's crypto/tls (a second independent reference stack), both
# directions across tls 1.2/1.3 and rsa/ecdsa. builds the small go peer first.
tls-go-interop: build
	@echo "--- tls interop (go crypto/tls) ---"
	@command -v go >/dev/null 2>&1 || { echo "go not found; skipping tls go interop"; exit 0; }
	@(cd tests/interop/tls_peer && go build -o tls_peer .) || { echo "go build failed"; exit 1; }
	@actual=$$(timeout 120 ./target/release/pith run tests/live/test_tls_go_interop.pith 2>/dev/null); \
	expected=$$(cat tests/live/expected/test_tls_go_interop.txt); \
	if [ "$$actual" = "$$expected" ]; then \
		echo "ok   tls go interop"; \
	else \
		echo "FAIL tls go interop"; echo "--- expected ---"; echo "$$expected"; echo "--- actual ---"; echo "$$actual"; exit 1; \
	fi
