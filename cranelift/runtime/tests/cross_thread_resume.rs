//! a green task parked on one os thread and resumed on another must see the
//! resuming thread's identity, and still its own. the scenario itself lives
//! in the runtime (`green::pith_green_cross_thread_probe`); this file runs it
//! twice.
//!
//! the in-process arm links the runtime as an rlib into this test binary.
//! that catches a scheduler mistake — the wrong cells installed on resume,
//! say — but it cannot catch the hazard the probe exists for: a test binary
//! is an executable in its own right, so its thread-local accesses always
//! take the local-exec model and read off `%fs` directly, whatever the
//! workspace is configured with. what ships is `libpith_runtime.a`, and its
//! tls model is a build setting (`.cargo/config.toml`): compiled as an
//! ordinary library it fetched the thread's tls base once per function and
//! reused it — a base cached in a frame across a suspend, which is what a
//! migrated task reads stale, and what this probe reported against such an
//! archive.
//!
//! the archive arm therefore builds a c harness against
//! `target/release/libpith_runtime.a` exactly the way a pith program is
//! linked, and runs the probe there. it is skipped, loudly, when the archive
//! has not been built (`cargo build --release` produces it) or there is no c
//! compiler; ci builds before it tests, so there it always runs.

use std::path::PathBuf;
use std::process::Command;

/// what a correct resume reports for the pool the probe ran on: worker 0
/// before the park, the last worker after it, and its own identity intact.
fn expected(result: i64) -> i64 {
    let workers = ((result >> 24) & 0xff) as usize;
    pith_runtime::concurrency::green::encode_probe(workers, Some(0), Some(workers - 1), true)
}

fn describe(result: i64) -> String {
    if result < 0 {
        return "the pool has fewer than two workers".to_string();
    }
    let workers = (result >> 24) & 0xff;
    let before = (result >> 16) & 0xff;
    let after = (result >> 8) & 0xff;
    let own = result & 1 == 1;
    format!(
        "{workers} workers, worker before park = {before}, worker after resume = {after}, \
         own identity after = {own}"
    )
}

#[test]
fn in_process_resume_on_another_thread_sees_that_thread() {
    let result = pith_runtime::concurrency::green::pith_green_cross_thread_probe();
    if result < 0 {
        eprintln!("skipped: {}", describe(result));
        return;
    }
    assert_eq!(
        result,
        expected(result),
        "after a cross-thread resume the task saw: {}",
        describe(result)
    );
}

/// the workspace's release archive, if it has been built.
fn release_archive() -> Option<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        manifest.join("../../target/release/libpith_runtime.a"),
        manifest.join("../target/release/libpith_runtime.a"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

const HARNESS: &str = r#"
#include <stdio.h>
#include <stdint.h>
int64_t pith_green_cross_thread_probe(void);
int main(void) {
    printf("%lld\n", (long long)pith_green_cross_thread_probe());
    return 0;
}
"#;

#[test]
fn archive_resume_on_another_thread_sees_that_thread() {
    let Some(archive) = release_archive() else {
        eprintln!("skipped: no target/release/libpith_runtime.a (run cargo build --release first)");
        return;
    };
    let dir = std::env::temp_dir().join(format!("pith-cross-thread-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let source = dir.join("probe.c");
    let exe = dir.join("probe");
    std::fs::write(&source, HARNESS).expect("write the harness");

    let cc = std::env::var("CC").unwrap_or_else(|_| "gcc".to_string());
    let compiled = Command::new(&cc)
        .arg("-o")
        .arg(&exe)
        .arg(&source)
        .arg(&archive)
        .arg("-lpthread")
        .arg("-lm")
        .output();
    let compiled = match compiled {
        Ok(output) => output,
        Err(err) => {
            eprintln!("skipped: could not run {cc}: {err}");
            return;
        }
    };
    assert!(
        compiled.status.success(),
        "linking the probe harness against {} failed:\n{}",
        archive.display(),
        String::from_utf8_lossy(&compiled.stderr)
    );

    let run = Command::new(&exe).output().expect("run the probe harness");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        run.status.success(),
        "the probe harness died: {:?}\n{}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    let result: i64 = stdout
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("unexpected probe output: {stdout:?}"));
    if result < 0 {
        eprintln!("skipped: {}", describe(result));
        return;
    }
    assert_eq!(
        result,
        expected(result),
        "against the shipped archive, after a cross-thread resume the task saw: {}",
        describe(result)
    );
}
