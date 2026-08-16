// built-in reducer: regenerate a seed, confirm the finding, then delta-debug
// at statement granularity — drop one top-level item or one main-body
// statement at a time, keep the removal when the same finding persists, and
// iterate to a fixed point. multi-module cases also get one attempt at
// inlining the helper modules into main.

use std::fs;
use std::path::{Path, PathBuf};

use crate::gen;
use crate::oracle;

#[derive(Clone)]
pub struct Chunk {
    pub file: usize,
    pub text: String, // includes trailing newline(s)
    pub protected: bool,
}

/// split the program files into droppable chunks: every top-level item is one
/// chunk, and fn main's body is split per statement. the `fn main():` header
/// and the final `print("done")` marker are protected.
pub fn split_chunks(files: &[(String, String)]) -> Vec<Chunk> {
    let mut chunks: Vec<Chunk> = Vec::new();
    for (fi, (_, content)) in files.iter().enumerate() {
        let lines: Vec<&str> = content.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            if line.trim().is_empty() {
                // stray blank line between items: glue onto the previous chunk
                if let Some(c) = chunks.last_mut() {
                    c.text.push('\n');
                }
                i += 1;
                continue;
            }
            if line == "fn main():" {
                chunks.push(Chunk {
                    file: fi,
                    text: format!("{}\n", line),
                    protected: true,
                });
                i += 1;
                // main body: one chunk per top-level statement (indent 4),
                // continuation lines (indent > 4) attach to their statement
                while i < lines.len() {
                    let mut text = format!("{}\n", lines[i]);
                    let protected = lines[i].trim() == "print(\"done\")";
                    i += 1;
                    while i < lines.len()
                        && (lines[i].trim().is_empty() || indent_of(lines[i]) > 4)
                    {
                        text.push_str(lines[i]);
                        text.push('\n');
                        i += 1;
                    }
                    chunks.push(Chunk { file: fi, text, protected });
                }
            } else {
                // a top-level item: header line plus everything indented or
                // blank under it
                let mut text = format!("{}\n", line);
                i += 1;
                while i < lines.len()
                    && (lines[i].trim().is_empty() || indent_of(lines[i]) > 0)
                {
                    text.push_str(lines[i]);
                    text.push('\n');
                    i += 1;
                }
                chunks.push(Chunk {
                    file: fi,
                    text,
                    protected: false,
                });
            }
        }
    }
    chunks
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// rebuild the files from the surviving chunks; files left with no content
/// are dropped entirely
pub fn assemble(names: &[String], chunks: &[Chunk], removed: &[bool]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = names.iter().map(|n| (n.clone(), String::new())).collect();
    for (i, c) in chunks.iter().enumerate() {
        if !removed[i] {
            out[c.file].1.push_str(&c.text);
        }
    }
    out.retain(|(_, content)| !content.trim().is_empty());
    out
}

/// the fixed-point removal loop, generic over the "does it still fail" probe
/// so it can be unit-tested with a mock. returns the removal mask and the
/// number of probe runs.
pub fn reduce_fixed_point(
    n_chunks: usize,
    protected: &dyn Fn(usize) -> bool,
    still_fails: &mut dyn FnMut(&[bool]) -> bool,
) -> (Vec<bool>, usize) {
    let mut removed = vec![false; n_chunks];
    let mut runs = 0;
    loop {
        let mut progressed = false;
        for i in 0..n_chunks {
            if removed[i] || protected(i) {
                continue;
            }
            removed[i] = true;
            runs += 1;
            if still_fails(&removed) {
                progressed = true;
            } else {
                removed[i] = false;
            }
        }
        if !progressed {
            break;
        }
    }
    (removed, runs)
}

/// inline the helper modules into main.pith: drop the import lines, splice
/// the helper declarations (minus `pub `) ahead of main's own, and erase the
/// module aliases at token boundaries
pub fn inline_modules(files: &[(String, String)]) -> Option<Vec<(String, String)>> {
    if files.len() < 2 {
        return None;
    }
    let mut helper_decls = String::new();
    let mut main_src = None;
    for (name, content) in files {
        if name == "main.pith" {
            main_src = Some(content.clone());
            continue;
        }
        for line in content.lines() {
            if line.starts_with('#') || line.starts_with("import ") || line.starts_with("from ") {
                continue;
            }
            let line = line.strip_prefix("pub ").unwrap_or(line);
            helper_decls.push_str(line);
            helper_decls.push('\n');
        }
        helper_decls.push('\n');
    }
    let main_src = main_src?;
    let mut merged = String::new();
    for line in main_src.lines() {
        if line.starts_with("import ") || line.starts_with("from ") {
            continue;
        }
        merged.push_str(line);
        merged.push('\n');
    }
    let mut all = helper_decls + &merged;
    for alias in ["ma.", "mb.", "mx."] {
        all = strip_alias(&all, alias);
    }
    Some(vec![("main.pith".to_string(), all)])
}

/// remove `alias` wherever it starts at a token boundary (so `ma.` goes but
/// the `ma.` inside `Gamma.` stays)
fn strip_alias(s: &str, alias: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with(alias) {
            let boundary = i == 0 || {
                let prev = bytes[i - 1] as char;
                !prev.is_ascii_alphanumeric() && prev != '_' && prev != '.'
            };
            if boundary {
                i += alias.len();
                continue;
            }
        }
        let c = s[i..].chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    out
}

struct Target {
    class: String,
    signature: String,
    // for wrong-output: the raw first-mismatch pair from the full program
    wo_pair: Option<(String, String)>,
}

/// does this outcome still exhibit the target finding?
fn still_has_target(outcome: &oracle::CaseOutcome, target: &Target) -> bool {
    if target.class == "wrong-output" {
        let stdout = match &outcome.run_stdout {
            Some(s) => s,
            None => return false,
        };
        let (exp, act) = target.wo_pair.as_ref().unwrap();
        if act == "<missing>" {
            // the bug was a line that never printed: it persists while the
            // expected line stays absent from a completed run
            return !stdout.lines().any(|l| l == exp);
        }
        // the bug printed a specific wrong line: it persists while that
        // exact line is still produced (and the right one is not)
        stdout.lines().any(|l| l == act) && !stdout.lines().any(|l| l == exp)
    } else {
        outcome
            .findings
            .iter()
            .any(|f| f.class == target.class && f.signature == target.signature)
    }
}

pub fn cmd_reduce(args: &[String]) {
    let flag = |name: &str| -> Option<&str> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .map(|s| s.as_str())
    };
    let seed: u64 = flag("--seed")
        .expect("--seed N required")
        .parse()
        .expect("--seed must be a u64");
    let pith = flag("--pith").expect("--pith PATH required");
    let out_dir = PathBuf::from(flag("--out").expect("--out DIR required"));
    let want_class = flag("--class").map(|s| s.to_string());

    let tc = oracle::locate_toolchain(Path::new(pith));
    fs::create_dir_all(&out_dir).expect("create out dir");
    let work = out_dir.join("work");

    // confirm the finding on the full program first
    let prog = gen::generate(seed);
    let outcome = oracle::run_case(seed, &prog.files, Some(&prog.expected), &tc, &work, false);
    let finding = match &want_class {
        Some(c) => outcome.findings.iter().find(|f| &f.class == c),
        None => outcome.findings.first(),
    };
    let finding = match finding {
        Some(f) => f,
        None => {
            eprintln!(
                "seed {} does not reproduce a{} finding (got: {})",
                seed,
                want_class
                    .as_deref()
                    .map(|c| format!(" {}", c))
                    .unwrap_or_default(),
                if outcome.findings.is_empty() {
                    "clean run".to_string()
                } else {
                    outcome
                        .findings
                        .iter()
                        .map(|f| f.class.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            );
            std::process::exit(2);
        }
    };
    let target = Target {
        class: finding.class.clone(),
        signature: finding.signature.clone(),
        wo_pair: finding.wo_pair.clone(),
    };
    println!(
        "seed {} reproduces [{}] {}",
        seed, target.class, target.signature
    );

    // one attempt at collapsing the helper modules into main
    let mut files = prog.files.clone();
    if let Some(inlined) = inline_modules(&files) {
        let oc = oracle::run_case(seed, &inlined, None, &tc, &work, false);
        if still_has_target(&oc, &target) {
            println!("helper modules inlined into main");
            files = inlined;
        }
    }

    // statement-level delta debugging to a fixed point
    let names: Vec<String> = files.iter().map(|(n, _)| n.clone()).collect();
    let chunks = split_chunks(&files);
    let protected: Vec<bool> = chunks.iter().map(|c| c.protected).collect();
    let mut probe_runs = 0usize;
    let (removed, runs) = reduce_fixed_point(
        chunks.len(),
        &|i| protected[i],
        &mut |mask| {
            probe_runs += 1;
            let candidate = assemble(&names, &chunks, mask);
            let oc = oracle::run_case(seed, &candidate, None, &tc, &work, false);
            still_has_target(&oc, &target)
        },
    );
    let minimal = assemble(&names, &chunks, &removed);
    let _ = fs::remove_dir_all(&work);

    let orig_lines: usize = prog.files.iter().map(|(_, c)| c.lines().count()).sum();
    let min_lines: usize = minimal.iter().map(|(_, c)| c.lines().count()).sum();
    for (name, content) in &minimal {
        fs::write(out_dir.join(name), content).expect("write minimal file");
    }
    let dropped = removed.iter().filter(|r| **r).count();
    let mut report = format!(
        "seed: {}\nclass: {}\nsignature: {}\n",
        seed, target.class, target.signature
    );
    if let Some((e, a)) = &target.wo_pair {
        report.push_str(&format!("expected line: {}\nactual line: {}\n", e, a));
    }
    report.push_str(&format!(
        "chunks: {} total, {} dropped\nlines: {} -> {}\nprobe runs: {}\n",
        chunks.len(),
        dropped,
        orig_lines,
        min_lines,
        runs
    ));
    report.push_str("repro: pith build main.pith && ./main\n");
    fs::write(out_dir.join("report.txt"), &report).expect("write report");
    println!(
        "reduced {} -> {} lines ({} of {} chunks dropped, {} probe runs)",
        orig_lines,
        min_lines,
        dropped,
        chunks.len(),
        runs
    );
    println!("minimal case written to {}", out_dir.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_point_reaches_minimal_pair() {
        // failure needs chunks 1 AND 3 together; everything else is noise
        let mut probe = |mask: &[bool]| !mask[1] && !mask[3];
        let (removed, _) = reduce_fixed_point(5, &|_| false, &mut probe);
        assert_eq!(removed, vec![true, false, true, false, true]);
    }

    #[test]
    fn fixed_point_respects_protection() {
        // failure persists no matter what, so everything unprotected goes
        let mut probe = |_: &[bool]| true;
        let (removed, _) = reduce_fixed_point(4, &|i| i == 0 || i == 3, &mut probe);
        assert_eq!(removed, vec![false, true, true, false]);
    }

    #[test]
    fn fixed_point_iterates_until_stable() {
        // chunk 0 only becomes removable once chunk 2 is gone: the probe
        // rejects removing 0 while 2 is present, so a second pass is needed
        let mut probe = |mask: &[bool]| {
            if mask[0] && !mask[2] {
                return false;
            }
            true
        };
        let (removed, runs) = reduce_fixed_point(3, &|_| false, &mut probe);
        assert_eq!(removed, vec![true, true, true]);
        assert!(runs > 3, "needs a second pass, got {} runs", runs);
    }

    #[test]
    fn fixed_point_keeps_everything_when_nothing_reproduces() {
        let mut probe = |_: &[bool]| false;
        let (removed, runs) = reduce_fixed_point(3, &|_| false, &mut probe);
        assert_eq!(removed, vec![false, false, false]);
        assert_eq!(runs, 3);
    }

    #[test]
    fn chunks_split_items_and_main_statements() {
        let src = "\
# generated program
struct Pack0:
    label: String

fn blend1(a: Int) -> Int:
    return a

fn main():
    s0 := Pack0(\"x\")
    print(\"p1: {s0.label}\")
    if s0.label.len() > 0:
        print(\"long\")
    print(\"done\")
";
        let files = vec![("main.pith".to_string(), src.to_string())];
        let chunks = split_chunks(&files);
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        // comment, struct, fn, main header, 3 statements + done
        assert_eq!(chunks.len(), 8, "chunks: {:?}", texts);
        assert!(chunks[3].protected && chunks[3].text == "fn main():\n");
        assert!(chunks[7].protected && chunks[7].text.contains("done"));
        // the if-statement keeps its indented body attached
        assert!(chunks[6].text.contains("if s0") && chunks[6].text.contains("long"));
        // dropping the if-statement chunk reassembles cleanly
        let names = vec!["main.pith".to_string()];
        let mut removed = vec![false; chunks.len()];
        removed[6] = true;
        let out = assemble(&names, &chunks, &removed);
        assert!(!out[0].1.contains("long"));
        assert!(out[0].1.contains("print(\"done\")"));
    }

    #[test]
    fn alias_stripping_respects_token_boundaries() {
        let s = "av0 := ma.blend3(1)\ne := Phase.Gamma\nx := mb.carry(ma.weigh(2))\n";
        let t = strip_alias(&strip_alias(s, "ma."), "mb.");
        assert_eq!(t, "av0 := blend3(1)\ne := Phase.Gamma\nx := carry(weigh(2))\n");
    }

    #[test]
    fn inline_merges_helper_into_main() {
        let files = vec![
            (
                "genmod_a.pith".to_string(),
                "# generated helper module\npub struct Vault1:\n    tag: Int\n\npub fn carry2(a: Int) -> Int:\n    return a\n\n".to_string(),
            ),
            (
                "main.pith".to_string(),
                "# generated program\nimport genmod_a as ma\nfrom genmod_a import Vault1\n\nfn main():\n    v := ma.carry2(3)\n    print(\"p0: {v}\")\n    print(\"done\")\n".to_string(),
            ),
        ];
        let out = inline_modules(&files).unwrap();
        assert_eq!(out.len(), 1);
        let src = &out[0].1;
        assert!(src.contains("struct Vault1:"));
        assert!(!src.contains("pub struct"));
        assert!(!src.contains("import "));
        assert!(src.contains("v := carry2(3)"));
    }
}
