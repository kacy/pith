// `cfg(sanitize = "...")` is unstable even where `-Zsanitizer` is accepted, so
// the sanitizer a build was made with is passed on as a plain cfg instead:
// `pith_tsan` for thread, `pith_asan` for address. cargo hands build scripts
// the target's cfgs, and `-Zsanitizer=thread` adds `sanitize="thread"`.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(pith_tsan)");
    println!("cargo:rustc-check-cfg=cfg(pith_asan)");
    let sanitizers = std::env::var("CARGO_CFG_SANITIZE").unwrap_or_default();
    for sanitizer in sanitizers.split(',') {
        match sanitizer {
            "thread" => println!("cargo:rustc-cfg=pith_tsan"),
            "address" => println!("cargo:rustc-cfg=pith_asan"),
            _ => {}
        }
    }
}
