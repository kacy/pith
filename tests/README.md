This directory is for regression and negative test fixtures.

- `cases/` contains deterministic regression programs
- `expected/` contains expected output snapshots for `cases/`
- `aborts/` contains programs that are meant to die, with the diagnostic and
  exit status each one dies with frozen in `aborts/expected/`; the `.ir` cases
  there are hand-written combined IR for the failure modes source cannot reach
- `live/` contains opt-in live network, TLS/HTTP2/gRPC, database, and public-chain tests
- `invalid/` contains checker-invalid programs and expected error codes
- `invalid_parse/` contains parser-invalid programs and expected error codes
- `diagnostics/` contains programs whose rendered diagnostics, caret line
  included, are frozen as text, so a diagnostic that moves to another column
  is caught where a code comparison would pass
- `leaks/` contains the ownership shapes the leak gate measures
- `count_marks/` contains the golden IR count-mark fixture checked by `make count-marks-check-only`
- `pending/` holds reproducers for known-failing compiler defects (not globbed by CI gates)
- `testrunner/` contains `pith test` runner output, tag, filter, and JSON goldens (`make test-runner-goldens`)
- `green/` contains green-runtime scheduling and preemption tests
- `lsp/` contains language-server transcript cases and their expected frames
- `interop/` contains the TLS conformance and interop harnesses
- `wycheproof/` contains the imported crypto test vectors
- `package_deps/` contains multi-module import and manifest fixtures
- `data/` contains shared fixture input
