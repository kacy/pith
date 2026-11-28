use crate::args::{file_args, parse_args, ArgError, Command, TestOpts};
use crate::driver::{
    driver_abi_path, ir_driver_older_than_seed, ir_driver_staleness, ir_runtime_abi,
    seed_runtime_abi, stamp_if_ir_driver,
};
use crate::pipeline::{
    check_readable_file, default_exe_path, object_from_ir, validate_combined_ir_contract,
};
use crate::temp_run::{create_private_dir, run_temp_paths, TempBuildDir};
use pith_codegen::runtime_imports::RUNTIME_ABI;
use std::fs;
use std::time::{Duration, SystemTime};

fn temp_test_dir() -> TempBuildDir {
    TempBuildDir::create(&std::env::temp_dir(), false).expect("create temp test dir")
}

#[test]
fn accepts_explicit_call_and_field_contracts() {
    let ir = "func main 0 unknown\ncall 1 greet string 1 0\nfield 2 1 0 Point name\n";
    assert!(validate_combined_ir_contract(ir).is_ok());
}

#[test]
fn rejects_legacy_call_without_retkind() {
    let ir = "call 1 greet 1 0\n";
    assert!(validate_combined_ir_contract(ir).is_err());
}

#[test]
fn rejects_field_without_offset() {
    let ir = "field 2 1 Point name\n";
    assert!(validate_combined_ir_contract(ir).is_err());
}

#[test]
fn a_driver_older_than_its_seed_is_stale() {
    let tmp = temp_test_dir();
    let dir = tmp.path();
    fs::create_dir_all(dir.join("bootstrap")).unwrap();
    let seed = dir.join("bootstrap/ir_driver.ir");
    let driver = dir.join("ir_driver");
    fs::write(&seed, "").unwrap();
    fs::write(&driver, "").unwrap();
    let set = |path: &std::path::Path, secs: u64| {
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    };
    let (d, b) = (dir.to_str().unwrap(), driver.to_str().unwrap());

    set(&seed, 2_000);
    set(&driver, 1_000);
    assert!(ir_driver_older_than_seed(d, b));

    set(&driver, 3_000);
    assert!(!ir_driver_older_than_seed(d, b));

    fs::remove_file(&seed).unwrap();
    assert!(!ir_driver_older_than_seed(d, b));
}

#[test]
fn a_driver_is_stale_without_a_matching_abi_record() {
    let tmp = temp_test_dir();
    let dir = tmp.path();
    fs::create_dir_all(dir.join("bootstrap")).unwrap();
    let driver = dir.join("ir_driver");
    fs::write(&driver, "").unwrap();
    let (d, b) = (dir.to_str().unwrap(), driver.to_str().unwrap());

    let reason = ir_driver_staleness(d, b).expect("no record is stale");
    assert!(reason.contains("has no runtime ABI record"), "{reason}");

    fs::write(driver_abi_path(b), "0123456789abcdef").unwrap();
    let reason = ir_driver_staleness(d, b).expect("another table is stale");
    assert!(
        reason.contains("built against runtime ABI 0123456789abcdef"),
        "{reason}"
    );

    fs::write(driver_abi_path(b), RUNTIME_ABI).unwrap();
    assert_eq!(ir_driver_staleness(d, b), None);
}

#[test]
fn the_seed_records_its_runtime_abi_on_its_first_line() {
    let tmp = temp_test_dir();
    let dir = tmp.path();
    fs::create_dir_all(dir.join("bootstrap")).unwrap();
    let seed = dir.join("bootstrap/ir_driver.ir");
    let d = dir.to_str().unwrap();
    fs::write(&seed, "runtime_abi feedface00000000\nfunc main 0 int\n").unwrap();
    assert_eq!(seed_runtime_abi(d).as_deref(), Some("feedface00000000"));
    fs::write(&seed, "func main 0 int\nruntime_abi feedface00000000\n").unwrap();
    assert_eq!(seed_runtime_abi(d), None);
    assert_eq!(ir_runtime_abi(""), None);
}

#[test]
fn ir_for_another_runtime_abi_is_refused() {
    let ir = "runtime_abi 0000000000000000\nfunc main 0 int\niconst 1 0\nret 1\nendfunc\n";
    let err = object_from_ir(ir).expect_err("another table must be refused");
    assert!(
        err.contains("compiled against runtime ABI 0000000000000000"),
        "{err}"
    );
    let ir = format!(
        "runtime_abi {}\nfunc main 0 int\niconst 1 0\nret 1\nendfunc\n",
        RUNTIME_ABI
    );
    assert!(object_from_ir(&ir).is_ok());
}

#[test]
fn the_default_output_strips_only_the_trailing_extension() {
    assert_eq!(default_exe_path("hello.pith"), Some("hello"));
    assert_eq!(
        default_exe_path("foo.pith/hello.pith"),
        Some("foo.pith/hello")
    );
    assert_eq!(default_exe_path("a.pith.pith"), Some("a.pith"));
    assert_eq!(default_exe_path("hello"), None);
    assert_eq!(default_exe_path("foo.pith/hello"), None);
    assert_eq!(default_exe_path(".pith"), None);
    assert_eq!(default_exe_path("dir/.pith"), None);
}

#[test]
fn build_takes_one_file_and_an_output_path() {
    let args = |list: &[&str]| list.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    let parsed = |list: &[&str], output: bool| file_args("build", &args(list), output);
    let ok = |path: &str, out: Option<&str>| Ok((path.to_string(), out.map(str::to_string)));

    assert_eq!(parsed(&["a.pith"], true), ok("a.pith", None));
    assert_eq!(parsed(&["a.pith", "-o", "out"], true), ok("a.pith", Some("out")));
    assert_eq!(parsed(&["-o", "out", "a.pith"], true), ok("a.pith", Some("out")));
    assert_eq!(parsed(&["-"], true), ok("-", None));

    let err = |list: &[&str], output: bool| parsed(list, output).unwrap_err();
    assert_eq!(err(&[], true), "build requires a file argument");
    assert_eq!(err(&["a.pith", "-o"], true), "-o needs an output path");
    assert_eq!(
        err(&["a.pith", "-o", "x", "-o", "y"], true),
        "-o is given more than once"
    );
    assert_eq!(
        err(&["a.pith", "--release"], true),
        "unknown option '--release' for build"
    );
    assert_eq!(
        err(&["a.pith", "-o", "x"], false),
        "unknown option '-o' for build"
    );
    assert_eq!(
        err(&["a.pith", "b.pith"], true),
        "build takes one file, and 'b.pith' is a second one"
    );
}

#[test]
fn parse_test_command_options_without_mutating_env() {
    let argv = |list: &[&str]| list.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    let cmd = parse_args(&argv(&[
        "pith",
        "test",
        "--filter",
        "math",
        "--tag=fast",
        "--tag",
        "unit",
        "--exclude-tag=slow",
        "--json",
        "foo.pith",
    ]))
    .expect("valid test args");
    assert_eq!(
        cmd,
        Command::Test(TestOpts {
            file: "foo.pith".to_string(),
            filter: Some("math".to_string()),
            selected_tags: vec!["fast".to_string(), "unit".to_string()],
            excluded_tags: vec!["slow".to_string()],
            json: true,
        })
    );
    assert_eq!(
        parse_args(&argv(&["pith", "test", "--filter"])),
        Err(ArgError::Usage("--filter needs a value".to_string()))
    );
    assert_eq!(
        parse_args(&argv(&["pith", "test"])),
        Err(ArgError::Usage("test requires a file argument".to_string()))
    );
}

#[test]
fn a_missing_or_directory_source_is_not_readable() {
    let tmp = temp_test_dir();
    let dir = tmp.path();
    let file = dir.join("a.pith");
    fs::write(&file, "fn main():\n    return\n").unwrap();
    assert!(check_readable_file(file.to_str().unwrap()).is_ok());
    let missing = check_readable_file(dir.join("none.pith").to_str().unwrap()).unwrap_err();
    assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
    let is_dir = check_readable_file(dir.to_str().unwrap()).unwrap_err();
    assert_eq!(is_dir.kind(), std::io::ErrorKind::IsADirectory);
}

#[test]
fn each_run_directory_is_new_and_private() {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    let parent = temp_test_dir();
    let a = create_private_dir(parent.path()).unwrap();
    let b = create_private_dir(parent.path()).unwrap();
    assert_ne!(a, b);
    #[cfg(unix)]
    assert_eq!(fs::metadata(&a).unwrap().permissions().mode() & 0o077, 0);
}

#[test]
fn indented_legacy_call_and_field_are_rejected() {
    assert!(validate_combined_ir_contract("    call 1 greet 1 0\n").is_err());
    assert!(validate_combined_ir_contract("\tfield 2 1 Point name\n").is_err());
    assert!(validate_combined_ir_contract("  call 1 greet string 1 0\n").is_ok());
}

#[test]
fn dot_o_pith_source_keeps_distinct_object_and_exe_paths() {
    let dir = std::path::Path::new("/tmp/pith-run-test");
    let (exe, obj) = run_temp_paths(dir, "sample.o.pith");
    assert_ne!(exe, obj);
    assert_eq!(exe, dir.join("sample.o"));
    assert_eq!(obj, dir.join("sample.o.o"));
}

#[test]
fn stamp_if_ir_driver_writes_abi_atomically() {
    let tmp = temp_test_dir();
    let driver = tmp.path().join("ir_driver");
    fs::write(&driver, "").unwrap();
    let driver_str = driver.to_str().unwrap();
    stamp_if_ir_driver(driver_str);
    assert_eq!(
        fs::read_to_string(driver_abi_path(driver_str)).unwrap(),
        RUNTIME_ABI
    );
}
