// pithgen: grammar-directed pith program generator with crash oracles and a
// differential output oracle.
//
//   pithgen gen --seed N [--out DIR]           print or write one program
//   pithgen run --seeds A..B --pith PATH --out DIR [--valgrind] [--keep-all]
//               [--fail-on-findings]
//   pithgen reduce --seed N --pith PATH --out DIR [--class CLASS]
//
// oracle classes:
//   check-crash     pith check dies by signal, or fails with no diagnostic
//   build-fail      pith check accepts but pith build fails (always a bug)
//   build-crash     pith build dies by signal
//   run-crash       the built binary dies by SIGSEGV/SIGABRT/SIGILL; a
//                   valgrind rerun folds the fault-site class into the
//                   signature (null-ish / small-int-as-pointer / other)
//   run-silent      the binary exits without reaching the final marker and
//                   without a controlled runtime error message
//   wrong-output    the binary runs clean but its stdout differs from the
//                   lines the generator predicted while building the program
//   run-hang        the binary outlives its timeout (recorded, not fatal)
//   valgrind        optional: memcheck on the built binary

mod eval;
mod gen;
mod oracle;
mod rng;

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use oracle::Finding;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!(
            "usage: pithgen gen --seed N [--out DIR] | pithgen run --seeds A..B --pith PATH --out DIR [--valgrind] [--keep-all] [--fail-on-findings] | pithgen reduce --seed N --pith PATH --out DIR [--class CLASS]"
        );
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

fn expected_text(prog: &gen::Program) -> String {
    let mut out = String::new();
    for l in &prog.expected {
        if l.wildcard {
            out.push_str("<any>\n");
        } else {
            out.push_str(&l.text);
            out.push('\n');
        }
    }
    out
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
        fs::write(dir.join("expected.txt"), expected_text(&prog)).expect("write expected");
        println!(
            "wrote {} file(s) + expected.txt to {}",
            prog.files.len(),
            out
        );
    } else {
        for (name, content) in &prog.files {
            println!("==== {} ====", name);
            print!("{}", content);
        }
        println!("==== expected stdout ====");
        print!("{}", expected_text(&prog));
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
    let out_dir = PathBuf::from(flag_value(args, "--out").expect("--out DIR required"));
    let valgrind = args.iter().any(|a| a == "--valgrind");
    let keep_all = args.iter().any(|a| a == "--keep-all");
    // gate mode: any finding is a failure. the ci smoke runs a fixed seed
    // range that is known clean, so a nonzero exit means a regression.
    let fail_on_findings = args.iter().any(|a| a == "--fail-on-findings");

    let tc = oracle::locate_toolchain(std::path::Path::new(pith));

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
    let mut expected_lines = 0u64;
    let mut wildcard_lines = 0u64;
    let started = Instant::now();

    for seed in a..b {
        total += 1;
        let case_dir = cases_dir.join(format!("case_{}", seed));
        let prog = gen::generate(seed);
        expected_lines += prog.expected.len() as u64;
        wildcard_lines += prog.expected.iter().filter(|l| l.wildcard).count() as u64;

        let outcome = oracle::run_case(seed, &prog.files, Some(&prog.expected), &tc, &case_dir, valgrind);
        if let Some(what) = outcome.hang {
            hangs.push((seed, what));
        }
        if outcome.check_accepted {
            accepted += 1;
        } else if let Some(code) = outcome.rejection {
            rejected += 1;
            let entry = rejection_codes.entry(code).or_insert((0, seed));
            entry.0 += 1;
        }

        // preserve findings, drop clean cases (the scratchpad is ram-backed)
        let case_findings = outcome.findings;
        if !case_findings.is_empty() {
            let dst = findings_dir.join(format!("seed_{}_{}", seed, case_findings[0].class));
            let _ = fs::remove_dir_all(&dst);
            fs::create_dir_all(&dst).expect("create finding dir");
            for (name, content) in &prog.files {
                fs::write(dst.join(name), content).expect("copy finding file");
            }
            fs::write(dst.join("expected.txt"), expected_text(&prog)).expect("copy expected");
            if let Some(stdout) = &outcome.run_stdout {
                fs::write(dst.join("stdout.txt"), stdout).expect("copy stdout");
            }
            let mut report = String::new();
            for f in &case_findings {
                report.push_str(&format!(
                    "seed: {}\nclass: {}\nsignature: {}\ndetail:\n{}\n---\n",
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
            fs::write(case_dir.join("expected.txt"), expected_text(&prog)).ok();
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
    println!(
        "expected lines: {} total, {} wildcard ({:.2}%)",
        expected_lines,
        wildcard_lines,
        if expected_lines > 0 {
            wildcard_lines as f64 * 100.0 / expected_lines as f64
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
    if fail_on_findings && !findings.is_empty() {
        std::process::exit(1);
    }
}
