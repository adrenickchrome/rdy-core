//! The gate report: cargo-test output parsing, guarded defect ids, and the markdown / JSON
//! rendering that `cargo qa` writes to `target/qa/`.
use crate::perf::Verdict;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// Totals from one or more `test result:` lines of cargo-test output.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct TestRun {
    pub passed: usize,
    pub failed: usize,
    pub ignored: usize,
    pub failures: Vec<String>,
}

/// Sums every `test result:` line and collects the names of `test <name> ... FAILED` lines.
pub fn parse_cargo_test(output: &str) -> TestRun {
    let mut run = TestRun::default();
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("test result:") {
            for part in rest.split([';', '.']) {
                let mut words = part.split_whitespace();
                if let (Some(n), Some(what)) = (words.next(), words.next())
                    && let Ok(n) = n.parse::<usize>()
                {
                    match what {
                        "passed" => run.passed += n,
                        "failed" => run.failed += n,
                        "ignored" => run.ignored += n,
                        _ => {}
                    }
                }
            }
        } else if let Some(rest) = line.strip_prefix("test ")
            && let Some(name) = rest.strip_suffix(" ... FAILED")
        {
            run.failures.push(name.to_string());
        }
    }
    run
}

/// The pinned-defect ids (`ord1`, `blob1`, ...) of `cargo test -- --list` output, per area.
/// The area is the part of the test file's stem before the first `_`.
pub fn guarded_ids(list_output: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut area: Option<String> = None;
    for line in list_output.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Running ") {
            area = rest
                .split_whitespace()
                .find_map(|w| w.strip_prefix("tests/"))
                .and_then(|f| f.strip_suffix(".rs"))
                .map(|stem| area_of(stem).to_string());
            continue;
        }
        let (Some(area), Some(name)) = (&area, t.strip_suffix(": test")) else {
            continue;
        };
        if let Some(id) = guard_id(name) {
            out.entry(area.clone()).or_default().insert(id);
        }
    }
    out
}

/// `engine_splitbrain` -> `engine`.
pub fn area_of(stem: &str) -> &str {
    stem.split('_').next().unwrap_or(stem)
}

/// `ord1_append_after_a_tie` -> `ord1` (`^[a-z]+\d+_`).
fn guard_id(name: &str) -> Option<String> {
    let name = name.rsplit("::").next().unwrap_or(name);
    let (id, _) = name.split_once('_')?;
    let letters = id.chars().take_while(|c| c.is_ascii_lowercase()).count();
    let digits = id.len() - letters;
    (letters > 0 && digits > 0 && id[letters..].chars().all(|c| c.is_ascii_digit()))
        .then(|| id.to_string())
}

/// One build/lint/test gate step.
#[derive(Clone, Debug, Serialize)]
pub struct Gate {
    pub name: String,
    pub pass: bool,
    /// Passed/failed/ignored counts for test steps; empty otherwise.
    pub detail: String,
}

/// One area of the `rdy-qa` hardening suite.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Area {
    pub name: String,
    pub passed: usize,
    pub failed: usize,
    pub ignored: usize,
    /// False when a test binary exited non-zero, even if no test is named as failing
    /// (a compile error, a crash).
    pub ok: bool,
    pub failures: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub revision: String,
    pub dirty: bool,
    pub machine: String,
    pub mode: String,
    pub duration_secs: f64,
    pub pass: bool,
    pub gates: Vec<Gate>,
    pub areas: Vec<Area>,
    /// Perf verdicts; empty when the tier was skipped or failed to run.
    pub perf: Vec<Verdict>,
    /// What happened to perf: skipped, recorded a baseline, compared, or why it failed.
    pub perf_note: String,
    pub failing_seeds: Vec<String>,
    pub guarded: BTreeMap<String, BTreeSet<String>>,
}

/// Lines of proptest output that name a minimal failing input.
pub fn failing_seeds(output: &str) -> Vec<String> {
    output
        .lines()
        .filter(|l| l.contains("minimal failing input"))
        .map(|l| l.trim().to_string())
        .collect()
}

fn mark(pass: bool) -> &'static str {
    if pass { "PASS" } else { "FAIL" }
}

pub fn markdown(r: &Report) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# rdy QA gate: {}\n", mark(r.pass));
    let _ = writeln!(
        s,
        "- revision: {}{}",
        r.revision,
        if r.dirty { " (dirty)" } else { "" }
    );
    let _ = writeln!(s, "- machine: {}", r.machine);
    let _ = writeln!(s, "- mode: {}", r.mode);
    let _ = writeln!(s, "- duration: {:.1} s", r.duration_secs);
    let _ = writeln!(s, "- verdict: **{}**\n", mark(r.pass));

    let _ = writeln!(
        s,
        "## Build gates\n\n| Step | Result | Detail |\n|---|---|---|"
    );
    for g in &r.gates {
        let _ = writeln!(s, "| {} | {} | {} |", g.name, mark(g.pass), g.detail);
    }

    let _ = writeln!(
        s,
        "\n## Areas\n\n| Area | Result | Passed | Failed | Ignored |\n|---|---|---|---|---|"
    );
    for a in &r.areas {
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} |",
            a.name,
            mark(a.ok && a.failed == 0),
            a.passed,
            a.failed,
            a.ignored
        );
    }
    for a in r.areas.iter().filter(|a| !a.failures.is_empty() || !a.ok) {
        let _ = writeln!(s, "\nFailing in `{}`:", a.name);
        if a.failures.is_empty() {
            let _ = writeln!(
                s,
                "- (the test binary failed without naming a test; see its output)"
            );
        }
        for f in &a.failures {
            let _ = writeln!(s, "- `{f}`");
        }
    }

    let _ = writeln!(s, "\n## Perf\n\n{}\n", r.perf_note);
    if !r.perf.is_empty() {
        let _ = writeln!(
            s,
            "| Op | Median ms | p95 ms | Baseline ms | Delta | Result | Spec target |\n|---|---|---|---|---|---|---|"
        );
        for v in &r.perf {
            let _ = writeln!(
                s,
                "| {} | {:.3} | {:.3} | {} | {} | {} | {} |",
                v.op,
                v.median_ms,
                v.p95_ms,
                v.baseline_ms.map_or("-".into(), |b| format!("{b:.3}")),
                v.delta_pct.map_or("-".into(), |d| format!("{d:+.0}%")),
                mark(v.pass),
                v.spec_target.as_deref().unwrap_or("-")
            );
        }
    }

    let _ = writeln!(s, "\n## Failing seeds\n");
    if r.failing_seeds.is_empty() {
        let _ = writeln!(s, "None.");
    }
    for f in &r.failing_seeds {
        let _ = writeln!(s, "- `{f}`");
    }

    let _ = writeln!(s, "\n## Guarded defect ids\n");
    if r.guarded.is_empty() {
        let _ = writeln!(s, "None listed.");
    }
    for (area, ids) in &r.guarded {
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        let _ = writeln!(s, "- {}: {}", area, ids.join(", "));
    }
    s
}

pub fn json(r: &Report) -> String {
    serde_json::to_string_pretty(r).expect("report serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cargo_test_output() {
        let out = "running 3 tests\ntest a ... ok\ntest b ... FAILED\ntest c ... ignored\n\ntest result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out\n\nrunning 2 tests\ntest d ... ok\ntest e ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n";
        let r = parse_cargo_test(out);
        assert_eq!((r.passed, r.failed, r.ignored), (3, 1, 1));
        assert_eq!(r.failures, vec!["b".to_string()]);
    }

    #[test]
    fn collects_guarded_ids_per_area() {
        let list = "     Running tests/engine.rs (target/release/deps/engine-abc)\nord1_append_after_a_tie: test\nplain_test: test\n     Running tests/store.rs (target/release/deps/store-def)\nblob1_x: test\nblob1_y: test\n";
        let g = guarded_ids(list);
        assert_eq!(g["engine"], ["ord1".to_string()].into());
        assert_eq!(g["store"], ["blob1".to_string()].into());
    }
}
