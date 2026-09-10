//! the environment a pith program sees, and the reason it is not libc's.
//!
//! `setenv` is not safe to call once a process has more than one thread. glibc
//! keeps the environment in a `char **` it reallocates when a new name is added
//! and compacts in place on `unsetenv`, and `getenv` walks that array without
//! taking the lock `setenv` holds. a reader that loaded the old array pointer
//! and is walking it when a writer frees it reads freed memory. the runtime
//! cannot avoid having concurrent readers: `getaddrinfo` on the resolver pool
//! consults `RES_OPTIONS`, `LOCALDOMAIN` and `HOSTALIASES`, `execvp` in a
//! spawning child reads `PATH`, and any C library linked into the program may
//! read whatever it likes. rust's own env lock does not help — it is private to
//! rust-side accesses and libc has never heard of it.
//!
//! so the runtime stops writing to libc's environment. `os.set_env` and
//! `os.unset_env` record their effect in an overlay this module owns, and
//! `env.get` consults the overlay before falling back to the process
//! environment the program was started with. a child process gets the overlay
//! merged into its environment when it is spawned, so what the program set is
//! what the child inherits.
//!
//! what that costs: a C library reading the real environment inside this
//! process does not see a variable the program set. `RES_OPTIONS` set from pith
//! no longer reaches the resolver, for instance; it has to be in the
//! environment the process starts with. that applies whether the write happens
//! before or after the first task, so there is no threshold to reason about.
//!
//! the overlay is not a snapshot of the environment: a name it has never been
//! told about is read from libc on every lookup, so a variable something else
//! changed is still seen. only names the program itself wrote come from the
//! table.

use std::collections::BTreeMap;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, RwLock};

/// what the program did to one name. `Some(value)` is a `set_env`, `None` an
/// `unset_env` — the tombstone matters, because without it an unset name would
/// fall through to libc and read the value the process started with.
type Entry = Option<String>;

/// names the program has written, in name order so `apply` and `entries`
/// produce the same sequence on every run.
fn table() -> &'static RwLock<BTreeMap<String, Entry>> {
    static TABLE: OnceLock<RwLock<BTreeMap<String, Entry>>> = OnceLock::new();
    TABLE.get_or_init(|| RwLock::new(BTreeMap::new()))
}

/// false until the first write. a program that never calls `set_env` — nearly
/// all of them — reads one relaxed atomic per lookup and never touches the lock.
static WRITTEN: AtomicBool = AtomicBool::new(false);

/// read the lock, ignoring poisoning. a panic while the map was borrowed cannot
/// leave it torn (every mutation is a single `insert`), so refusing to read
/// afterwards would turn a survivable panic into a dead environment.
fn read() -> std::sync::RwLockReadGuard<'static, BTreeMap<String, Entry>> {
    table().read().unwrap_or_else(|err| err.into_inner())
}

fn write() -> std::sync::RwLockWriteGuard<'static, BTreeMap<String, Entry>> {
    table().write().unwrap_or_else(|err| err.into_inner())
}

/// the value of `name` as the program sees it: the overlay when the program has
/// written that name, and the process environment otherwise.
pub fn get(name: &str) -> Option<String> {
    if WRITTEN.load(Ordering::Acquire) {
        if let Some(entry) = read().get(name) {
            return entry.clone();
        }
    }
    std::env::var(name).ok()
}

/// record `name = value`. deliberately does not call `setenv`.
pub fn set(name: &str, value: &str) {
    write().insert(name.to_string(), Some(value.to_string()));
    WRITTEN.store(true, Ordering::Release);
}

/// record that `name` is unset. deliberately does not call `unsetenv`.
pub fn unset(name: &str) {
    write().insert(name.to_string(), None);
    WRITTEN.store(true, Ordering::Release);
}

/// merge the overlay into a child's environment.
///
/// call this before any per-command `env` override, so an override the caller
/// asked for wins over what the program set process-wide — `Command` keeps the
/// last write for a name.
pub fn apply(command: &mut Command) {
    if !WRITTEN.load(Ordering::Acquire) {
        return;
    }
    for (name, entry) in read().iter() {
        match entry {
            Some(value) => command.env(name, value),
            None => command.env_remove(name),
        };
    }
}

/// the overlay's contents, in name order. only the tests need this; the two
/// runtime callers reach for `get` and `apply`.
#[cfg(test)]
fn entries() -> Vec<(String, Entry)> {
    read()
        .iter()
        .map(|(name, entry)| (name.clone(), entry.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    /// `PATH` is the one name in this module's tests that is not unique to a
    /// test, because the tombstone test needs a name libc actually holds. the
    /// tests that touch it take this lock, so a sibling never sees the shadow.
    fn path_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    /// tests share one process and one overlay, so every test works on names
    /// nothing else touches.
    fn unique(tag: &str) -> String {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        format!(
            "PITH_ENV_OVERLAY_TEST_{}_{}",
            tag,
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn unwritten_name_falls_through_to_the_process_environment() {
        // PATH is set for every process this test can run in.
        let _guard = path_lock().lock().unwrap_or_else(|err| err.into_inner());
        assert_eq!(get("PATH"), std::env::var("PATH").ok());
        assert_eq!(get(&unique("missing")), None);
    }

    #[test]
    fn a_set_name_reads_back() {
        let name = unique("set");
        assert_eq!(get(&name), None);
        set(&name, "value");
        assert_eq!(get(&name).as_deref(), Some("value"));
        set(&name, "");
        assert_eq!(get(&name).as_deref(), Some(""));
    }

    #[test]
    fn an_unset_name_reads_back_absent_even_when_libc_has_it() {
        // the overlay must shadow the process environment rather than fall
        // through to it, or unsetting an inherited variable would do nothing.
        let _guard = path_lock().lock().unwrap_or_else(|err| err.into_inner());
        assert!(std::env::var("PATH").is_ok());
        let name = "PATH";
        let restore = get(name);
        unset(name);
        assert_eq!(get(name), None);

        // put the table back the way the rest of the suite expects it.
        match restore {
            Some(value) => set(name, &value),
            None => unset(name),
        }
        assert_eq!(get(name), std::env::var("PATH").ok());
    }

    #[test]
    fn set_never_touches_libc() {
        let name = unique("libc");
        set(&name, "overlay-only");
        assert_eq!(get(&name).as_deref(), Some("overlay-only"));
        assert!(std::env::var(&name).is_err());
    }

    #[test]
    fn apply_gives_a_child_the_overlay() {
        let set_name = unique("child_set");
        set(&set_name, "inherited");
        let unset_name = unique("child_unset");
        unset(&unset_name);

        let mut command = Command::new("/bin/sh");
        apply(&mut command);
        command.arg("-c").arg(format!(
            "printf '%s|%s' \"${{{}-ABSENT}}\" \"${{{}-ABSENT}}\"",
            set_name, unset_name
        ));
        let output = command.output().expect("child");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "inherited|ABSENT",
            "the child did not see the overlay"
        );
    }

    #[test]
    fn a_per_command_override_wins_over_the_overlay() {
        let name = unique("override");
        set(&name, "outer");

        let mut command = Command::new("/bin/sh");
        apply(&mut command);
        command.env(&name, "inner");
        command
            .arg("-c")
            .arg(format!("printf '%s' \"${{{}-ABSENT}}\"", name));
        let output = command.output().expect("child");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "inner");
    }

    #[test]
    fn concurrent_set_and_get_agree() {
        // every thread owns its own names, so the only thing under test is the
        // table: a reader must never see a torn or missing entry for a name a
        // writer has already finished writing, and must never fault.
        const THREADS: usize = 8;
        const ROUNDS: usize = 500;
        let prefix = unique("concurrent");

        std::thread::scope(|scope| {
            for thread in 0..THREADS {
                let prefix = prefix.clone();
                scope.spawn(move || {
                    for round in 0..ROUNDS {
                        let name = format!("{}_{}_{}", prefix, thread, round);
                        let value = format!("v{}", round);
                        set(&name, &value);
                        assert_eq!(get(&name).as_deref(), Some(value.as_str()));

                        // a name a sibling may be writing right now: whatever
                        // comes back, the read must complete.
                        let sibling =
                            format!("{}_{}_{}", prefix, (thread + 1) % THREADS, round);
                        let _ = get(&sibling);

                        // and a name the table has never held, so the
                        // fall-through to libc runs under the same contention.
                        let _ = get("PITH_ENV_OVERLAY_TEST_NEVER_WRITTEN");
                    }
                });
            }
        });

        let names: Vec<String> = entries()
            .into_iter()
            .map(|(name, _)| name)
            .filter(|name| name.starts_with(&prefix))
            .collect();
        assert_eq!(names.len(), THREADS * ROUNDS);
        for thread in 0..THREADS {
            for round in 0..ROUNDS {
                let name = format!("{}_{}_{}", prefix, thread, round);
                assert_eq!(get(&name).as_deref(), Some(format!("v{}", round).as_str()));
            }
        }
    }
}
