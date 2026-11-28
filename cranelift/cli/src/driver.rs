//! `ir_driver` discovery, ABI/seed staleness verification, atomic `.abi`
//! stamping, and bootstrap from the tracked IR seed.

use crate::pipeline::object_from_ir;
use crate::toolchain::{checkout_root, installed_lib, self_host_dirs};
use pith_codegen::runtime_imports::RUNTIME_ABI;
use std::env;
use std::fs;
use std::path::Path;

/// The first line of the IR the cli dumps, and so of the bootstrap seed:
/// `runtime_abi <hash>`, the runtime function table it was compiled against.
/// The consumer skips the line like any other it does not know.
pub(crate) const IR_ABI_PREFIX: &str = "runtime_abi ";

/// The runtime ABI an IR text says it was compiled against, if it says.
pub(crate) fn ir_runtime_abi(ir_text: &str) -> Option<&str> {
    ir_text
        .lines()
        .next()?
        .strip_prefix(IR_ABI_PREFIX)
        .map(str::trim)
}

/// The runtime ABI of the seed under `dir`, read from its first line only.
pub(crate) fn seed_runtime_abi(dir: &str) -> Option<String> {
    use std::io::BufRead;
    let file = fs::File::open(format!("{}/bootstrap/ir_driver.ir", dir)).ok()?;
    let mut first = String::new();
    std::io::BufReader::new(file).read_line(&mut first).ok()?;
    ir_runtime_abi(&first).map(str::to_string)
}

/// Where the runtime ABI a driver binary was built against is recorded.
pub(crate) fn driver_abi_path(driver: &str) -> String {
    format!("{}.abi", driver)
}

/// Record that a freshly built ir_driver matches this cli's runtime table.
/// Any other executable is left alone.
pub(crate) fn stamp_if_ir_driver(exe_path: &str) {
    if Path::new(exe_path).file_name().and_then(|n| n.to_str()) == Some("ir_driver") {
        let abi_path = driver_abi_path(exe_path);
        let tmp_path = format!("{}.{}.tmp", abi_path, std::process::id());
        if let Err(e) =
            fs::write(&tmp_path, RUNTIME_ABI).and_then(|_| fs::rename(&tmp_path, &abi_path))
        {
            let _ = fs::remove_file(&tmp_path);
            eprintln!(
                "warning: could not record the runtime ABI of {}: {}",
                exe_path, e
            );
        }
    }
}

/// Why a driver under `dir` must not be used as it is, or None when it can.
/// A driver built against another runtime table emits calls this cli may
/// lower to the wrong function or not at all, and a driver older than the
/// seed predates the emitter the checkout holds.
pub(crate) fn ir_driver_staleness(dir: &str, driver: &str) -> Option<String> {
    match fs::read_to_string(driver_abi_path(driver)) {
        Ok(abi) if abi.trim() == RUNTIME_ABI => {}
        Ok(abi) => {
            return Some(format!(
                "{} was built against runtime ABI {}, and this pith has {}",
                driver,
                abi.trim(),
                RUNTIME_ABI
            ))
        }
        Err(_) => return Some(format!("{} has no runtime ABI record", driver)),
    }
    if ir_driver_older_than_seed(dir, driver) {
        return Some(format!(
            "{} is older than {}/bootstrap/ir_driver.ir",
            driver, dir
        ));
    }
    None
}

/// The refusal for a seed made against another runtime table: building it
/// would bake calls this cli cannot resolve, or resolves differently, into
/// the driver that compiles everything else.
pub(crate) fn seed_abi_mismatch(dir: &str, seed_abi: Option<&str>) -> String {
    format!(
        "{}/bootstrap/ir_driver.ir was generated against runtime ABI {}, and this pith was built against {}. \
         rebuild pith from this checkout (cargo build --release). after changing \
         cranelift/runtime-abi/runtime_functions.txt, rebuild the driver from source \
         (pith build self-host/ir_driver.pith) and run make refresh-bootstrap-seed",
        dir,
        seed_abi.unwrap_or("(none recorded)"),
        RUNTIME_ABI
    )
}

/// Find the IR driver to compile `source` with. Ok(None) means none was
/// found and the seed should build one.
pub(crate) fn find_ir_driver(source: &str) -> Result<Option<String>, String> {
    if let Ok(path) = env::var("PITH_IR_DRIVER") {
        if Path::new(&path).exists() {
            return Ok(Some(path));
        }
        return Err(format!(
            "PITH_IR_DRIVER is set to {}, which does not exist",
            path
        ));
    }
    if let Some(path) = installed_lib("ir_driver") {
        return Ok(Some(path));
    }

    for dir in &self_host_dirs() {
        let dir = dir.as_str();
        let candidate = format!("{}/ir_driver", dir);
        if !Path::new(&candidate).exists() {
            // the working directory's checkout builds its driver from its
            // own seed rather than borrowing the one this binary was built
            // beside
            if Path::new(&format!("{}/bootstrap/ir_driver.ir", dir)).exists() {
                return Ok(None);
            }
            continue;
        }
        let Some(reason) = ir_driver_staleness(dir, &candidate) else {
            return Ok(Some(candidate));
        };
        let seed_abi = seed_runtime_abi(dir);
        if seed_abi.as_deref() == Some(RUNTIME_ABI) {
            eprintln!("{}; rebuilding it from the seed", reason);
            return Ok(None);
        }
        // building the driver from source is how a changed runtime table
        // reaches a new driver and then the seed, so the old driver may do
        // that one job
        if Path::new(source).file_name().and_then(|n| n.to_str()) == Some("ir_driver.pith") {
            eprintln!("{}; using it to build the new driver", reason);
            return Ok(Some(candidate));
        }
        return Err(format!(
            "{}. {}",
            reason,
            seed_abi_mismatch(dir, seed_abi.as_deref())
        ));
    }
    Ok(None)
}

/// Every emitter change regenerates the seed, so a seed newer than the binary
/// means a pull or checkout moved the compiler under it. The old binary still
/// emits the old IR, which the current backend can miscompile (a none optional
/// read as a heap shell, for one), so it is rebuilt from the seed instead.
pub(crate) fn ir_driver_older_than_seed(dir: &str, driver: &str) -> bool {
    let modified = |path: &str| fs::metadata(path).and_then(|m| m.modified()).ok();
    match (
        modified(&format!("{}/bootstrap/ir_driver.ir", dir)),
        modified(driver),
    ) {
        (Some(seed), Some(binary)) => seed > binary,
        _ => false,
    }
}

/// The ir_driver binary is itself compiled pith, so a fresh clone has no way
/// to build it: pith build needs ir_driver to get IR. The tracked IR seed
/// breaks that cycle: build ir_driver once from the seed, then everything
/// else builds from source as usual. A stale binary is rebuilt the same way
/// (see ir_driver_staleness), and a seed made against another runtime table
/// is refused.
pub(crate) fn bootstrap_ir_driver_from_seed() -> Result<String, String> {
    use pith_codegen::linker::build_executable;

    let dirs = self_host_dirs();
    for dir in &dirs {
        let dir = dir.as_str();
        let seed = format!("{}/bootstrap/ir_driver.ir", dir);
        if !Path::new(&seed).exists() {
            continue;
        }
        let seed_abi = seed_runtime_abi(dir);
        if seed_abi.as_deref() != Some(RUNTIME_ABI) {
            return Err(seed_abi_mismatch(dir, seed_abi.as_deref()));
        }
        eprintln!("building ir_driver from {}", seed);
        let ir_text = fs::read_to_string(&seed).map_err(|e| format!("read {}: {}", seed, e))?;
        let (bytes, _) =
            object_from_ir(&ir_text).map_err(|e| format!("seed build failed: {}", e))?;
        // link under a per-process name and rename into place, so parallel
        // builds never run a half-written binary
        let tmp = format!("{}/ir_driver.{}", dir, std::process::id());
        let obj_path = format!("{}.o", tmp);
        let exe_path = format!("{}/ir_driver", dir);
        fs::write(&obj_path, &bytes).map_err(|e| format!("seed object: {}", e))?;
        let linked = build_executable(&obj_path, &tmp);
        let _ = fs::remove_file(&obj_path);
        linked.map_err(|e| format!("seed link failed: {}", e))?;
        if let Err(e) = fs::rename(&tmp, &exe_path) {
            let _ = fs::remove_file(&tmp);
            return Err(format!("seed install failed: {}", e));
        }
        stamp_if_ir_driver(&exe_path);
        return Ok(exe_path);
    }
    Err(format!(
        "No IR driver found: none of {} holds ir_driver or bootstrap/ir_driver.ir. Build one with `make self-host-ir-driver`, or set PITH_IR_DRIVER to its path",
        dirs.join(", ")
    ))
}

/// Resolve a fresh checkout `ir_driver` to pass via `PITH_IR_DRIVER` when
/// delegating commands to `pith_main`.
pub(crate) fn resolve_driver_for_frontend() -> Option<String> {
    if env::var("PITH_IR_DRIVER").is_ok() {
        return None;
    }
    let root = checkout_root()?;
    let dir = root.join("self-host");
    let ir_driver = dir.join("ir_driver");
    if !ir_driver.exists() {
        return None;
    }
    let dir_str = dir.to_string_lossy();
    let driver_str = ir_driver.to_string_lossy();
    if ir_driver_staleness(&dir_str, &driver_str).is_none() {
        Some(driver_str.into_owned())
    } else if seed_runtime_abi(&dir_str).as_deref() == Some(RUNTIME_ABI) {
        bootstrap_ir_driver_from_seed().ok()
    } else {
        None
    }
}
