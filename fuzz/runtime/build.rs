// link clang's libFuzzer into every target. each bin defines
// LLVMFuzzerTestOneInput itself, so no wrapper crate is needed. the archive
// is found through clang unless LIBFUZZER_LIB names it.
use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=LIBFUZZER_LIB");
    let lib = std::env::var("LIBFUZZER_LIB").unwrap_or_else(|_| {
        Command::new("clang")
            .arg("--print-file-name=libclang_rt.fuzzer.a")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    });
    if !Path::new(&lib).is_file() {
        panic!("libFuzzer not found: set LIBFUZZER_LIB to libclang_rt.fuzzer-<arch>.a");
    }
    println!("cargo:rustc-link-arg-bins={lib}");
    println!("cargo:rustc-link-arg-bins=-lstdc++");
}
