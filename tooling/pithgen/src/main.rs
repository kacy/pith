// pithgen: grammar-directed pith program generator with crash oracles.
//
//   pithgen gen --seed N [--out DIR]           print or write one program
//   pithgen run --seeds A..B --pith PATH --out DIR [--valgrind] [--keep-all]
//
// oracle classes:
//   check-crash     pith check dies by signal, or fails with no diagnostic
//   build-fail      pith check accepts but pith build fails (always a bug)
//   build-crash     pith build dies by signal
//   run-crash       the built binary dies by SIGSEGV/SIGABRT/SIGILL
//   run-silent      the binary exits without reaching the final marker and
//                   without a controlled runtime error message
//   run-hang        the binary outlives its timeout (recorded, not fatal)
//   valgrind        optional: valgrind reports errors on the built binary

mod gen;
mod rng;

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct RunResult {
    code: Option<i32>,
    signal: Option<i32>,
    timed_out: bool,
    stdout: String,
    stderr: String,
}

fn run_with_timeout(
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

fn signal_name(sig: i32) -> &'static str {
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
fn signature_of(line: &str) -> String {
    // the "unknown load source 'bN' in <generated fn>" error names a
    // per-seed specialized function; collapse the whole tail so every
    // instance of the same root bug shares one signature
    if line.contains("unknown load source") {
        return "ir consumer: unknown load source in a re-emitted generic body".to_string();
    }
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            // collapse digit runs, but keep error codes like E240 intact
            let keep = out.ends_with('E');
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
    out.trim().to_string()
}

/// the most informative line of a failure: prefer known bug markers, then
/// any error line, then the last non-empty line
fn key_line(stdout: &str, stderr: &str) -> String {
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

fn first_error_code(stdout: &str, stderr: &str) -> String {
    let combined = format!("{}\n{}", stdout, stderr);
    if let Some(pos) = combined.find("error[E") {
        let rest = &combined[pos + 6..];
        let code: String = rest.chars().take_while(|c| *c != ']').collect();
        return code;
    }
    "none".into()
}

struct Finding {
    seed: u64,
    class: String,
    signature: String,
    detail: String,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: pithgen gen --seed N [--out DIR] | pithgen run --seeds A..B --pith PATH --out DIR [--valgrind] [--keep-all]");
        std::process::exit(2);
    }
    match args[1].as_str() {
        "gen" => cmd_gen(&args[2..]),
        "run" => cmd_run(&args[2..]),
        other => {
            eprintln!("unknown subcommand: {}", other);
            std::process::exit(2);
        }
    }
}

fn flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
}

fn cmd_gen(args: &[String]) {
    let seed: u64 = flag_value(args, "--seed")
        .unwrap_or("0")
        .parse()
        .expect("--seed must be a u64");
    let prog = gen::generate(seed);
    if let Some(out) = flag_value(args, "--out") {
        let dir = PathBuf::from(out);
        fs::create_dir_all(&dir).expect("create out dir");
        for (name, content) in &prog.files {
            fs::write(dir.join(name), content).expect("write file");
        }
        println!("wrote {} file(s) to {}", prog.files.len(), out);
    } else {
        for (name, content) in &prog.files {
            println!("==== {} ====", name);
            print!("{}", content);
        }
    }
}

fn cmd_run(args: &[String]) {
    let seeds = flag_value(args, "--seeds").expect("--seeds A..B required");
    let (a, b) = {
        let parts: Vec<&str> = seeds.split("..").collect();
        if parts.len() != 2 {
            eprintln!("--seeds must look like 0..200");
            std::process::exit(2);
        }
        (
            parts[0].parse::<u64>().expect("bad seed range"),
            parts[1].parse::<u64>().expect("bad seed range"),
        )
    };
    let pith = flag_value(args, "--pith").expect("--pith PATH required");
    let pith = fs::canonicalize(pith).expect("pith binary not found");
    let out_dir = PathBuf::from(flag_value(args, "--out").expect("--out DIR required"));
    let valgrind = args.iter().any(|a| a == "--valgrind");
    let keep_all = args.iter().any(|a| a == "--keep-all");

    // locate the self-hosted compiler and ir driver next to the pith binary
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

    let cases_dir = out_dir.join("cases");
    let findings_dir = out_dir.join("findings");
    fs::create_dir_all(&cases_dir).expect("create cases dir");
    fs::create_dir_all(&findings_dir).expect("create findings dir");

    let mut total = 0u64;
    let mut accepted = 0u64;
    let mut rejected = 0u64;
    let mut rejection_codes: BTreeMap<String, (u64, u64)> = BTreeMap::new(); // code -> (count, sample seed)
    let mut findings: Vec<Finding> = Vec::new();
    let mut hangs: Vec<(u64, String)> = Vec::new();
    let started = Instant::now();

    for seed in a..b {
        total += 1;
        let case_dir = cases_dir.join(format!("case_{}", seed));
        let _ = fs::remove_dir_all(&case_dir);
        fs::create_dir_all(&case_dir).expect("create case dir");
        let prog = gen::generate(seed);
        for (name, content) in &prog.files {
            fs::write(case_dir.join(name), content).expect("write case file");
        }

        let mut case_findings: Vec<Finding> = Vec::new();
        let pith_s = pith.to_string_lossy().to_string();

        // oracle: check
        let check = run_with_timeout(
            &pith_s,
            &["check", "main.pith"],
            &case_dir,
            &envs,
            Duration::from_secs(30),
        )
        .expect("spawn pith check");
        let check_out = format!("{}{}", check.stdout, check.stderr);
        let check_ok = check.code == Some(0) && !check.timed_out;
        if check.timed_out {
            hangs.push((seed, "check timeout".into()));
        } else if let Some(sig) = check.signal {
            case_findings.push(Finding {
                seed,
                class: "check-crash".into(),
                signature: format!("pith check died: {}", signal_name(sig)),
                detail: tail(&check_out, 6),
            });
        } else if !check_ok && !check_out.contains("error[") {
            // the wrapper swallows a checker crash into a silent nonzero exit;
            // rerun the self-hosted checker directly to catch the signal
            let direct = run_with_timeout(
                &self_host.to_string_lossy(),
                &["check", "main.pith"],
                &case_dir,
                &envs,
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
            case_findings.push(Finding {
                seed,
                class: "check-crash".into(),
                signature: sig_desc,
                detail: tail(&format!("{}{}", direct.stdout, direct.stderr), 6),
            });
        } else if !check_ok {
            rejected += 1;
            let code = first_error_code(&check.stdout, &check.stderr);
            let entry = rejection_codes.entry(code).or_insert((0, seed));
            entry.0 += 1;
        } else {
            accepted += 1;
        }

        // oracle: build after a clean check
        let mut built = false;
        if check_ok && case_findings.is_empty() {
            let build = run_with_timeout(
                &pith_s,
                &["build", "main.pith"],
                &case_dir,
                &envs,
                Duration::from_secs(90),
            )
            .expect("spawn pith build");
            if build.timed_out {
                hangs.push((seed, "build timeout".into()));
            } else if let Some(sig) = build.signal {
                case_findings.push(Finding {
                    seed,
                    class: "build-crash".into(),
                    signature: format!("pith build died: {}", signal_name(sig)),
                    detail: tail(&format!("{}{}", build.stdout, build.stderr), 6),
                });
            } else if build.code != Some(0) {
                // check accepted, build refused: always a compiler bug
                let kl = key_line(&build.stdout, &build.stderr);
                case_findings.push(Finding {
                    seed,
                    class: "build-fail".into(),
                    signature: signature_of(&kl),
                    detail: tail(&format!("{}{}", build.stdout, build.stderr), 6),
                });
            } else {
                built = true;
            }
        }

        // oracle: run the produced binary
        if built {
            let exe = case_dir.join("main");
            if exe.exists() {
                let run = run_with_timeout(
                    &exe.to_string_lossy(),
                    &[],
                    &case_dir,
                    &envs,
                    Duration::from_secs(10),
                )
                .expect("spawn built binary");
                let combined = format!("{}{}", run.stdout, run.stderr);
                if run.timed_out {
                    hangs.push((seed, "run timeout (possible hang/deadlock)".into()));
                } else if let Some(sig) = run.signal {
                    case_findings.push(Finding {
                        seed,
                        class: "run-crash".into(),
                        signature: format!("binary died: {}", signal_name(sig)),
                        detail: tail(&combined, 6),
                    });
                } else if run.code == Some(0) && !run.stdout.contains("done") {
                    case_findings.push(Finding {
                        seed,
                        class: "run-silent".into(),
                        signature: "exit 0 without reaching the final marker".into(),
                        detail: tail(&combined, 6),
                    });
                } else if run.code != Some(0)
                    && !combined.contains("pith runtime error")
                    && !combined.contains("runtime error")
                {
                    case_findings.push(Finding {
                        seed,
                        class: "run-silent".into(),
                        signature: format!("nonzero exit {:?} with no runtime error message", run.code),
                        detail: tail(&combined, 6),
                    });
                } else if valgrind {
                    let vg = run_with_timeout(
                        "valgrind",
                        &["--error-exitcode=9", "--quiet", &exe.to_string_lossy()],
                        &case_dir,
                        &envs,
                        Duration::from_secs(60),
                    );
                    if let Ok(vg) = vg {
                        if vg.code == Some(9) {
                            case_findings.push(Finding {
                                seed,
                                class: "valgrind".into(),
                                signature: signature_of(&key_line(&vg.stdout, &vg.stderr)),
                                detail: tail(&vg.stderr, 10),
                            });
                        }
                    }
                }
            }
        }

        // preserve findings, drop clean cases (the scratchpad is ram-backed)
        if !case_findings.is_empty() {
            let dst = findings_dir.join(format!("seed_{}_{}", seed, case_findings[0].class));
            let _ = fs::remove_dir_all(&dst);
            fs::create_dir_all(&dst).expect("create finding dir");
            for (name, content) in &prog.files {
                fs::write(dst.join(name), content).expect("copy finding file");
            }
            let mut report = String::new();
            for f in &case_findings {
                report.push_str(&format!(
                    "seed: {}\nclass: {}\nsignature: {}\noutput tail:\n{}\n---\n",
                    f.seed, f.class, f.signature, f.detail
                ));
            }
            fs::write(dst.join("finding.txt"), &report).expect("write finding report");
            let mut line = String::new();
            for f in &case_findings {
                line.push_str(&format!("{}\t{}\t{}\n", f.seed, f.class, f.signature));
            }
            let report_file = findings_dir.join("report.txt");
            let prev = fs::read_to_string(&report_file).unwrap_or_default();
            fs::write(&report_file, prev + &line).expect("append report");
            findings.extend(case_findings);
        }
        let _ = fs::remove_dir_all(&case_dir);
        if keep_all {
            fs::create_dir_all(&case_dir).ok();
            for (name, content) in &prog.files {
                fs::write(case_dir.join(name), content).ok();
            }
        }

        if total % 50 == 0 {
            eprintln!(
                "[{}/{}] accepted {} rejected {} findings {} ({:.0}s)",
                total,
                b - a,
                accepted,
                rejected,
                findings.len(),
                started.elapsed().as_secs_f32()
            );
        }
    }

    // summary
    println!("== pithgen summary ==");
    println!("seeds: {}..{} ({} programs)", a, b, total);
    let checked = accepted + rejected;
    println!(
        "check-accepted: {} / {} attempted ({:.1}%)",
        accepted,
        checked,
        if checked > 0 {
            accepted as f64 * 100.0 / checked as f64
        } else {
            0.0
        }
    );
    if !rejection_codes.is_empty() {
        println!("rejections by code:");
        let mut codes: Vec<(&String, &(u64, u64))> = rejection_codes.iter().collect();
        codes.sort_by(|x, y| y.1 .0.cmp(&x.1 .0));
        for (code, (count, sample)) in codes {
            println!("  {}: {} (sample seed {})", code, count, sample);
        }
    }
    if !hangs.is_empty() {
        println!("hangs/timeouts: {}", hangs.len());
        for (seed, what) in hangs.iter().take(10) {
            println!("  seed {}: {}", seed, what);
        }
    }
    // dedup findings by (class, signature)
    let mut dedup: BTreeMap<(String, String), Vec<u64>> = BTreeMap::new();
    for f in &findings {
        dedup
            .entry((f.class.clone(), f.signature.clone()))
            .or_default()
            .push(f.seed);
    }
    println!("findings: {} total, {} distinct signatures", findings.len(), dedup.len());
    for ((class, sig), seeds) in &dedup {
        let shown: Vec<String> = seeds.iter().take(5).map(|s| s.to_string()).collect();
        println!(
            "  [{}] x{} seeds [{}{}]\n      {}",
            class,
            seeds.len(),
            shown.join(","),
            if seeds.len() > 5 { ",..." } else { "" },
            sig
        );
    }
    println!("elapsed: {:.0}s", started.elapsed().as_secs_f32());
}

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}
