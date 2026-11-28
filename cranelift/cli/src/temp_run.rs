//! Private 0o700 temporary build directory (RAII), Ctrl-C SIGINT handling,
//! and child exit-status formatting for `pith run` and `pith test`.

use std::fs;
use std::path::{Path, PathBuf};

/// A private temporary build directory removed automatically on drop unless
/// `keep` is set (`PITH_KEEP_RUN_ARTIFACTS`).
pub(crate) struct TempBuildDir {
    path: PathBuf,
    keep: bool,
}

impl TempBuildDir {
    pub(crate) fn create(parent: &Path, keep: bool) -> std::io::Result<Self> {
        let path = create_private_dir(parent)?;
        Ok(Self { path, keep })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempBuildDir {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// A new directory under `parent` that only this user can enter, for
/// `pith run` and `pith test` to build in. The name carries a random part,
/// and creating the directory fails when anything exists at the path, a
/// symlink included, so nothing planted in a shared temp directory is
/// followed or overwritten, and other users cannot reach the object and
/// binary inside it.
pub(crate) fn create_private_dir(parent: &Path) -> std::io::Result<PathBuf> {
    use std::hash::{BuildHasher, Hasher};
    #[cfg(unix)]
    use std::os::unix::fs::DirBuilderExt;

    let mut attempts = 0;
    loop {
        // every RandomState is seeded from the OS, so this is a fresh
        // random number without a dependency
        let random = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        let dir = parent.join(format!("pith_run_{}_{:016x}", std::process::id(), random));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);
        match builder.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempts < 16 => {
                attempts += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

pub(crate) fn run_temp_paths(dir: &Path, source_path: &str) -> (PathBuf, PathBuf) {
    let stem = Path::new(source_path).file_stem().unwrap_or("main".as_ref());
    let exe_path = dir.join(stem);
    // Append `.o` instead of replacing extension so `<name>.o.pith` never
    // collides with `exe_path == obj_path`.
    let obj_path = dir.join(format!("{}.o", stem.to_string_lossy()));
    (exe_path, obj_path)
}

#[cfg(unix)]
pub(crate) mod unix_sig {
    const SIGINT: i32 = 2;
    const SIG_IGN: usize = 1;
    const SIG_ERR: usize = usize::MAX;

    unsafe extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
    }

    pub(crate) struct IgnoreSigint(usize);

    impl IgnoreSigint {
        /// Ignore SIGINT in the parent while waiting on the child so Ctrl-C
        /// interrupts the child (which inherited SIG_DFL at spawn) and lets
        /// `TempBuildDir` remove its temporary directory before exiting.
        /// Must be entered AFTER `Command::spawn()`.
        pub(crate) fn enter() -> Self {
            // SAFETY: Setting SIGINT disposition to SIG_IGN is async-signal-safe
            // and restored unconditionally in `Drop`.
            let prev = unsafe { signal(SIGINT, SIG_IGN) };
            Self(prev)
        }
    }

    impl Drop for IgnoreSigint {
        fn drop(&mut self) {
            if self.0 != SIG_ERR {
                // SAFETY: Restoring the previous signal disposition returned by `signal`.
                unsafe {
                    signal(SIGINT, self.0);
                }
            }
        }
    }
}

pub(crate) fn describe_exit(status: &std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            let name = match sig {
                4 => " (SIGILL, illegal instruction)",
                6 => " (SIGABRT, abort)",
                7 => " (SIGBUS, bus error)",
                8 => " (SIGFPE, arithmetic exception)",
                9 => " (SIGKILL, killed)",
                11 => " (SIGSEGV, segmentation fault)",
                13 => " (SIGPIPE, broken pipe)",
                15 => " (SIGTERM, terminated)",
                _ => "",
            };
            return format!("crashed with signal {}{}", sig, name);
        }
    }
    match status.code() {
        Some(c) => format!("failed with exit code {}", c),
        None => "failed".to_string(),
    }
}

/// The status `pith run` and `pith test` exit with when the program failed.
/// A program that exited passes its code through. One killed by a signal
/// has no code and would otherwise leave a bare exit 1 with nothing said,
/// so this names the signal on stderr and exits with 128 plus its number,
/// the status a shell reports for it.
pub(crate) fn child_exit_code(path: &str, status: &std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    eprintln!("pith: {} {}", path, describe_exit(status));
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        128 + status.signal().unwrap_or(0)
    }
    #[cfg(not(unix))]
    {
        1
    }
}
