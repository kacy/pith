// the per-case oracle pipeline, shared by `run` and `reduce`: write the
// program, run pith check / pith build / the built binary, and classify what
// happened. also home to the wrong-output comparator and the run-crash
// fault-site classifier.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::gen::ExpLine;

pub struct RunResult {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
}

pub fn run_with_timeout(
    program: &str,
    args: &[&str],
    cwd: &Path,
    envs: &[(String, String)],
    timeout: Duration,
) -> std::io::Result<RunResult> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn()?;
    let mut out_pipe = child.stdout.take().unwrap();
    let mut err_pipe = child.stderr.take().unwrap();
    let out_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = out_pipe.read_to_string(&mut buf);
        buf
    });
    let err_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = err_pipe.read_to_string(&mut buf);
        buf
    });
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if start.elapsed() > timeout {
            timed_out = true;
            let _ = child.kill();
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = out_thread.join().unwrap_or_default();
    let stderr = err_thread.join().unwrap_or_default();
    #[cfg(unix)]
    let sig = {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    };
    #[cfg(not(unix))]
    let sig: Option<i32> = None;
    Ok(RunResult {
        code: status.code(),
        signal: sig,
        timed_out,
        stdout,
        stderr,
    })
}

pub fn signal_name(sig: i32) -> &'static str {
    match sig {
        4 => "SIGILL",
        6 => "SIGABRT",
        8 => "SIGFPE",
        9 => "SIGKILL",
        11 => "SIGSEGV",
        _ => "signal",
    }
}

/// normalize compiler/runtime output into a stable signature for dedup:
/// strip paths, line/col numbers, addresses, and generated identifiers
pub fn signature_of(line: &str) -> String {
    // the "unknown load source 'bN' in <generated fn>" error names a
    // per-seed specialized function; collapse the whole tail so every
    // instance of the same root bug shares one signature
    if line.contains("unknown load source") {
        return "ir consumer: unknown load source in a re-emitted generic body".to_string();
    }
    normalize_digits(line, true).trim().to_string()
}

/// collapse digit runs to `N`; with `keep_error_codes`, a run right after an
/// `E` (as in E240) survives
pub fn normalize_digits(line: &str, keep_error_codes: bool) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            let keep = keep_error_codes && out.ends_with('E');
            if keep {
                out.push(c);
                while let Some(d) = chars.peek() {
                    if d.is_ascii_digit() {
                        out.push(*d);
                        chars.next();
                    } else {
                        break;
                    }
                }
            } else {
                while let Some(d) = chars.peek() {
                    if d.is_ascii_digit() {
                        chars.next();
                    } else {
                        break;
                    }
                }
                out.push('N');
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// the most informative line of a failure: prefer known bug markers, then
/// any error line, then the last non-empty line
pub fn key_line(stdout: &str, stderr: &str) -> String {
    let combined = format!("{}\n{}", stdout, stderr);
    let markers = [
        "IR consumer verifier error",
        "IR contract error",
        "error getting IR",
        "Error getting IR",
        "verifier error",
        "internal error",
    ];
    for m in &markers {
        if let Some(l) = combined.lines().find(|l| l.contains(m)) {
            return l.trim().to_string();
        }
    }
    if let Some(l) = combined.lines().find(|l| l.contains("error")) {
        return l.trim().to_string();
    }
    combined
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

pub fn first_error_code(stdout: &str, stderr: &str) -> String {
    let combined = format!("{}\n{}", stdout, stderr);
    if let Some(pos) = combined.find("error[E") {
        let rest = &combined[pos + 6..];
        let code: String = rest.chars().take_while(|c| *c != ']').collect();
        return code;
    }
    "none".into()
}

pub fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

pub struct Finding {
    pub seed: u64,
    pub class: String,
    pub signature: String,
    pub detail: String,
    /// for wrong-output: the raw first-mismatch pair (expected, actual),
    /// with "<missing>" / "<end>" standing in for an absent line
    pub wo_pair: Option<(String, String)>,
}

/// the first divergence between actual stdout and the predicted lines
pub struct Mismatch {
    pub idx: usize,
    pub expected: String, // raw text, or "<end>" when actual overran
    pub actual: String,   // raw text, or "<missing>" when actual fell short
}

pub fn compare_output(actual: &str, expected: &[ExpLine]) -> Option<Mismatch> {
    let act: Vec<&str> = actual.lines().collect();
    for i in 0..expected.len().max(act.len()) {
        match (expected.get(i), act.get(i)) {
            (Some(e), Some(a)) => {
                if !e.wildcard && e.text != *a {
                    return Some(Mismatch {
                        idx: i,
                        expected: e.text.clone(),
                        actual: (*a).to_string(),
                    });
                }
            }
            (Some(e), None) => {
                return Some(Mismatch {
                    idx: i,
                    expected: if e.wildcard { "<any>".into() } else { e.text.clone() },
                    actual: "<missing>".into(),
                });
            }
            (None, Some(a)) => {
                return Some(Mismatch {
                    idx: i,
                    expected: "<end>".into(),
                    actual: (*a).to_string(),
                });
            }
            (None, None) => unreachable!(),
        }
    }
    None
}

/// expected-vs-actual context around a mismatch, for finding.txt
pub fn mismatch_context(actual: &str, expected: &[ExpLine], idx: usize) -> String {
    let act: Vec<&str> = actual.lines().collect();
    let lo = idx.saturating_sub(2);
    let hi = (idx + 3).min(expected.len().max(act.len()));
    let mut out = String::new();
    for i in lo..hi {
        let e = match expected.get(i) {
            Some(l) if l.wildcard => "<any>".to_string(),
            Some(l) => l.text.clone(),
            None => "<end>".to_string(),
        };
        let a = match act.get(i) {
            Some(l) => (*l).to_string(),
            None => "<missing>".to_string(),
        };
        let mark = if i == idx { ">>" } else { "  " };
        out.push_str(&format!("{} line {}: expected '{}' actual '{}'\n", mark, i, e, a));
    }
    out
}

/// coarse classification of a fault address parsed from valgrind output
pub fn classify_addr(addr: u64) -> &'static str {
    if addr < 0x1000 {
        "null-ish"
    } else if addr < 0x100000 {
        "small-int-as-pointer"
    } else {
        "other"
    }
}

pub fn parse_fault_addr(s: &str) -> Option<u64> {
    // valgrind spells the address either as "Address 0xN is not stack'd..."
    // under an Invalid read/write, or "Access not within mapped region at
    // address 0xN" when the segfault escapes its checks
    for line in s.lines() {
        for pat in ["at address 0x", "Address 0x"] {
            if let Some(p) = line.find(pat) {
                let hex: String = line[p + pat.len()..]
                    .chars()
                    .take_while(|c| c.is_ascii_hexdigit())
                    .collect();
                if !hex.is_empty() {
                    if let Ok(v) = u64::from_str_radix(&hex, 16) {
                        return Some(v);
                    }
                }
            }
        }
    }
    None
}

/// rerun a crashed binary under valgrind and classify the fault site.
/// tolerates valgrind being absent or the crash not reproducing: None then.
pub fn fault_site_class(exe: &str, cwd: &Path, envs: &[(String, String)]) -> Option<&'static str> {
    let vg = run_with_timeout(
        "valgrind",
        &["--error-exitcode=9", "--quiet", exe],
        cwd,
        envs,
        Duration::from_secs(60),
    )
    .ok()?;
    parse_fault_addr(&vg.stderr).map(classify_addr)
}

pub struct Toolchain {
    pub pith: PathBuf,
    pub self_host: PathBuf,
    pub envs: Vec<(String, String)>,
}

/// locate the self-hosted compiler and ir driver next to the pith binary
pub fn locate_toolchain(pith: &Path) -> Toolchain {
    let pith = fs::canonicalize(pith).expect("pith binary not found");
    let mut repo_root = pith.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    while !repo_root.join("self-host").join("pith_main").exists() {
        if !repo_root.pop() {
            eprintln!("could not find self-host/pith_main above {}", pith.display());
            std::process::exit(2);
        }
    }
    let self_host = repo_root.join("self-host").join("pith_main");
    let ir_driver = repo_root.join("self-host").join("ir_driver");
    let envs = vec![
        (
            "PITH_SELF_HOST".to_string(),
            self_host.to_string_lossy().to_string(),
        ),
        (
            "PITH_IR_DRIVER".to_string(),
            ir_driver.to_string_lossy().to_string(),
        ),
    ];
    Toolchain { pith, self_host, envs }
}

pub struct CaseOutcome {
    pub findings: Vec<Finding>,
    pub hang: Option<String>,
    pub check_accepted: bool,
    pub rejection: Option<String>, // error code when pith check rejected
    pub run_stdout: Option<String>, // stdout of a complete exit-0 run
}

/// run one generated case through every oracle. `expected` gates the
/// wrong-output comparison; None skips it (the reducer compares on its own).
pub fn run_case(
    seed: u64,
    files: &[(String, String)],
    expected: Option<&[ExpLine]>,
    tc: &Toolchain,
    case_dir: &Path,
    valgrind_mem: bool,
) -> CaseOutcome {
    let _ = fs::remove_dir_all(case_dir);
    fs::create_dir_all(case_dir).expect("create case dir");
    // a relative out dir would otherwise break the exe spawn below: the
    // program path is resolved after the child's chdir into the case dir
    let case_dir = &fs::canonicalize(case_dir).expect("canonicalize case dir");
    for (name, content) in files {
        fs::write(case_dir.join(name), content).expect("write case file");
    }

    let mut findings: Vec<Finding> = Vec::new();
    let mut hang: Option<String> = None;
    let mut rejection: Option<String> = None;
    let mut run_stdout: Option<String> = None;
    let pith_s = tc.pith.to_string_lossy().to_string();

    // oracle: check
    let check = run_with_timeout(
        &pith_s,
        &["check", "main.pith"],
        case_dir,
        &tc.envs,
        Duration::from_secs(30),
    )
    .expect("spawn pith check");
    let check_out = format!("{}{}", check.stdout, check.stderr);
    let check_ok = check.code == Some(0) && !check.timed_out;
    if check.timed_out {
        hang = Some("check timeout".into());
    } else if let Some(sig) = check.signal {
        findings.push(Finding {
            seed,
            class: "check-crash".into(),
            signature: format!("pith check died: {}", signal_name(sig)),
            detail: tail(&check_out, 6),
            wo_pair: None,
        });
    } else if !check_ok && !check_out.contains("error[") {
        // the wrapper swallows a checker crash into a silent nonzero exit;
        // rerun the self-hosted checker directly to catch the signal
        let direct = run_with_timeout(
            &tc.self_host.to_string_lossy(),
            &["check", "main.pith"],
            case_dir,
            &tc.envs,
            Duration::from_secs(30),
        )
        .expect("spawn pith_main check");
        let sig_desc = match direct.signal {
            Some(sig) => format!("pith_main check died: {}", signal_name(sig)),
            None => format!(
                "pith check failed with no diagnostic (pith_main exit {:?})",
                direct.code
            ),
        };
        findings.push(Finding {
            seed,
            class: "check-crash".into(),
            signature: sig_desc,
            detail: tail(&format!("{}{}", direct.stdout, direct.stderr), 6),
            wo_pair: None,
        });
    } else if !check_ok {
        rejection = Some(first_error_code(&check.stdout, &check.stderr));
    }

    // oracle: build after a clean check
    let mut built = false;
    if check_ok && findings.is_empty() {
        let build = run_with_timeout(
            &pith_s,
            &["build", "main.pith"],
            case_dir,
            &tc.envs,
            Duration::from_secs(90),
        )
        .expect("spawn pith build");
        if build.timed_out {
            hang = Some("build timeout".into());
        } else if let Some(sig) = build.signal {
            findings.push(Finding {
                seed,
                class: "build-crash".into(),
                signature: format!("pith build died: {}", signal_name(sig)),
                detail: tail(&format!("{}{}", build.stdout, build.stderr), 6),
                wo_pair: None,
            });
        } else if build.code != Some(0) {
            // check accepted, build refused: always a compiler bug
            let kl = key_line(&build.stdout, &build.stderr);
            findings.push(Finding {
                seed,
                class: "build-fail".into(),
                signature: signature_of(&kl),
                detail: tail(&format!("{}{}", build.stdout, build.stderr), 6),
                wo_pair: None,
            });
        } else {
            built = true;
        }
    }

    // oracle: run the produced binary
    if built {
        let exe = case_dir.join("main");
        if exe.exists() {
            let exe_s = exe.to_string_lossy().to_string();
            let run = run_with_timeout(&exe_s, &[], case_dir, &tc.envs, Duration::from_secs(10))
                .expect("spawn built binary");
            let combined = format!("{}{}", run.stdout, run.stderr);
            if run.timed_out {
                hang = Some("run timeout (possible hang/deadlock)".into());
            } else if let Some(sig) = run.signal {
                // dedup by fault site: a valgrind rerun tells the null-ish
                // dereference apart from the small-int-as-pointer one
                let signature = match fault_site_class(&exe_s, case_dir, &tc.envs) {
                    Some(cls) => format!("binary died: {} [{}]", signal_name(sig), cls),
                    None => format!("binary died: {}", signal_name(sig)),
                };
                findings.push(Finding {
                    seed,
                    class: "run-crash".into(),
                    signature,
                    detail: tail(&combined, 6),
                    wo_pair: None,
                });
            } else if run.code == Some(0) && !run.stdout.contains("done") {
                findings.push(Finding {
                    seed,
                    class: "run-silent".into(),
                    signature: "exit 0 without reaching the final marker".into(),
                    detail: tail(&combined, 6),
                    wo_pair: None,
                });
            } else if run.code != Some(0)
                && !combined.contains("pith runtime error")
                && !combined.contains("runtime error")
            {
                findings.push(Finding {
                    seed,
                    class: "run-silent".into(),
                    signature: format!("nonzero exit {:?} with no runtime error message", run.code),
                    detail: tail(&combined, 6),
                    wo_pair: None,
                });
            } else if run.code == Some(0) {
                run_stdout = Some(run.stdout.clone());
                // oracle: differential output. the generator predicted every
                // line, so any divergence on a clean run is a wrong answer.
                if let Some(exp) = expected {
                    if let Some(mm) = compare_output(&run.stdout, exp) {
                        let ne = normalize_digits(&mm.expected, false);
                        let na = normalize_digits(&mm.actual, false);
                        findings.push(Finding {
                            seed,
                            class: "wrong-output".into(),
                            signature: format!(
                                "wrong-output at line {}: expected '{}' got '{}'",
                                mm.idx, ne, na
                            ),
                            detail: mismatch_context(&run.stdout, exp, mm.idx),
                            wo_pair: Some((mm.expected, mm.actual)),
                        });
                    }
                }
                if findings.is_empty() && valgrind_mem {
                    let vg = run_with_timeout(
                        "valgrind",
                        &["--error-exitcode=9", "--quiet", &exe_s],
                        case_dir,
                        &tc.envs,
                        Duration::from_secs(60),
                    );
                    if let Ok(vg) = vg {
                        if vg.code == Some(9) {
                            findings.push(Finding {
                                seed,
                                class: "valgrind".into(),
                                signature: signature_of(&key_line(&vg.stdout, &vg.stderr)),
                                detail: tail(&vg.stderr, 10),
                                wo_pair: None,
                            });
                        }
                    }
                }
            }
        }
    }

    CaseOutcome {
        findings,
        hang,
        check_accepted: check_ok,
        rejection,
        run_stdout,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(v: &[(&str, bool)]) -> Vec<ExpLine> {
        v.iter()
            .map(|(t, w)| ExpLine {
                text: t.to_string(),
                wildcard: *w,
            })
            .collect()
    }

    #[test]
    fn compare_matches_exactly() {
        let exp = lines(&[("a: 1", false), ("b: 2", false)]);
        assert!(compare_output("a: 1\nb: 2\n", &exp).is_none());
    }

    #[test]
    fn compare_flags_first_divergence() {
        let exp = lines(&[("a: 1", false), ("b: 2", false), ("c: 3", false)]);
        let mm = compare_output("a: 1\nb: 9\nc: 3\n", &exp).unwrap();
        assert_eq!(mm.idx, 1);
        assert_eq!(mm.expected, "b: 2");
        assert_eq!(mm.actual, "b: 9");
    }

    #[test]
    fn compare_skips_wildcards() {
        let exp = lines(&[("a: 1", false), ("", true), ("c: 3", false)]);
        assert!(compare_output("a: 1\nanything\nc: 3\n", &exp).is_none());
    }

    #[test]
    fn compare_catches_missing_and_extra() {
        let exp = lines(&[("a: 1", false), ("b: 2", false)]);
        let mm = compare_output("a: 1\n", &exp).unwrap();
        assert_eq!((mm.idx, mm.actual.as_str()), (1, "<missing>"));
        let mm = compare_output("a: 1\nb: 2\nextra\n", &exp).unwrap();
        assert_eq!((mm.idx, mm.expected.as_str()), (2, "<end>"));
    }

    #[test]
    fn fault_addr_parses_both_valgrind_forms() {
        let a = "==1== Invalid read of size 8\n==1==  Address 0x10 is not stack'd, malloc'd or (recently) free'd\n";
        assert_eq!(parse_fault_addr(a), Some(0x10));
        let b = "==1==  Access not within mapped region at address 0x4F22A8\n";
        assert_eq!(parse_fault_addr(b), Some(0x4F22A8));
        assert_eq!(parse_fault_addr("nothing here"), None);
    }

    #[test]
    fn addr_classes() {
        assert_eq!(classify_addr(0x0), "null-ish");
        assert_eq!(classify_addr(0x8), "null-ish");
        assert_eq!(classify_addr(0x4f22), "small-int-as-pointer");
        assert_eq!(classify_addr(0x7f0012345678), "other");
    }
}
