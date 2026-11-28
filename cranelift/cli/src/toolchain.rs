//! Toolchain layout and self-hosted frontend discovery.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub(crate) fn combined_output(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("{}{}", stdout, stderr)
}

/// The root of an installed toolchain: `PITH_HOME` if set, otherwise the
/// directory above this binary when it holds `lib/pith_main`, as in
/// `<home>/bin/pith`. A cargo build inside a checkout has no such directory.
pub(crate) fn install_home() -> Option<PathBuf> {
    if let Some(home) = env::var_os("PITH_HOME").filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(home));
    }
    let exe = fs::canonicalize(env::current_exe().ok()?).ok()?;
    let home = exe.parent()?.parent()?;
    home.join("lib/pith_main")
        .exists()
        .then(|| home.to_path_buf())
}

/// The checkout a cargo build of this binary sits in: `<root>/target/release/pith`
/// with `<root>/self-host` beside `target`. None for an install.
pub(crate) fn checkout_root() -> Option<PathBuf> {
    let exe = fs::canonicalize(env::current_exe().ok()?).ok()?;
    let root = exe.parent()?.parent()?.parent()?;
    root.join("self-host").is_dir().then(|| root.to_path_buf())
}

/// Resolve `PITH_HOME` for child processes without mutating this process's
/// environment: when the caller did not set `PITH_HOME`, fall back to the
/// install root or the checkout this binary was built in so spawned compilers
/// and drivers can locate `std` from any working directory.
pub(crate) fn apply_pith_home(command: &mut Command) {
    if env::var_os("PITH_HOME").is_none() {
        if let Some(home) = install_home().or_else(checkout_root) {
            command.env("PITH_HOME", home);
        }
    }
}

/// The self-host directories that may hold the ir driver, the self-hosted
/// compiler and the seed: the working directory's checkout first, so the
/// in-repo workflow and a directory with its own seed behave as before, then
/// the checkout this binary was built in, so a source build works from any
/// directory.
pub(crate) fn self_host_dirs() -> Vec<String> {
    let mut dirs = vec!["./self-host".to_string(), "../self-host".to_string()];
    if let Some(root) = checkout_root() {
        dirs.push(root.join("self-host").to_string_lossy().into_owned());
    }
    dirs
}

/// A file under the install's `lib/`, if there is an install and it has one.
pub(crate) fn installed_lib(name: &str) -> Option<String> {
    let path = install_home()?.join("lib").join(name);
    path.exists().then(|| path.to_string_lossy().into_owned())
}

/// Find the self-hosted compiler executable.
pub(crate) fn find_self_hosted_compiler() -> Result<String, String> {
    if let Ok(path) = env::var("PITH_SELF_HOST") {
        if Path::new(&path).exists() {
            return Ok(path);
        }
        return Err(format!(
            "PITH_SELF_HOST is set to {}, which does not exist",
            path
        ));
    }
    if let Some(path) = installed_lib("pith_main") {
        return Ok(path);
    }

    let mut candidates: Vec<String> = self_host_dirs()
        .iter()
        .map(|dir| format!("{}/pith_main", dir))
        .collect();
    candidates.push("./pith_main".to_string());
    if let Some(found) = candidates.iter().find(|c| Path::new(c).exists()) {
        return Ok(found.clone());
    }
    Err(format!(
        "self-hosted compiler not found; tried {}. Build it with `make self-host`, or set PITH_SELF_HOST to its path",
        candidates.join(", ")
    ))
}

/// Run a self-hosted compiler subcommand (`parse` or `lex`) on `path` and
/// capture its stdout.
pub(crate) fn run_frontend_capture(subcommand: &str, path: &str) -> Result<String, String> {
    let compiler = find_self_hosted_compiler()?;

    let mut cmd = Command::new(&compiler);
    apply_pith_home(&mut cmd);
    let output = cmd
        .args([subcommand, path])
        .output()
        .map_err(|e| format!("Failed to run compiler: {}", e))?;

    if !output.status.success() {
        if subcommand == "lex" {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Lex error: {}", stderr));
        }
        let combined = combined_output(&output);
        if combined.trim().is_empty() {
            return Err("Parse error".to_string());
        }
        return Err(combined);
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
