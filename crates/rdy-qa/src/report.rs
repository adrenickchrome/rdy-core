//! The gate report: cargo-test output parsing, guarded defect ids, and the markdown / JSON
//! rendering that `cargo qa` writes to `target/qa/`.
use crate::perf::{Baseline, Measure, Verdict};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

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
/// The area is the `<area>` of a `Running tests/<area>/main.rs (...)` header; any other header
/// (the `Running unittests ...` ones) sets no area.
pub fn guarded_ids(list_output: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut area: Option<String> = None;
    for line in list_output.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Running ") {
            area = rest
                .split_whitespace()
                .find_map(|w| w.strip_prefix("tests/"))
                .and_then(|f| f.strip_suffix("/main.rs"))
                .filter(|a| !a.is_empty() && !a.contains('/'))
                .map(str::to_string);
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

/// The `--only` area that runs the perf tier alone.
pub const PERF_AREA: &str = "perf";

/// What one `cargo qa` run executes. The release build of `rdy-cli` always runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// fmt, clippy and the workspace tests (outside `rdy-qa`).
    pub checks: bool,
    /// The `rdy-qa` area test binaries (`tests/<area>/main.rs`) to run.
    pub files: Vec<String>,
    /// The `rdy-qa` unit tests.
    pub lib: bool,
    /// The perf-regression tier.
    pub perf: bool,
    /// The guarded-id listing for the report.
    pub guarded: bool,
}

/// The plan for `--only <area>` (or a full run), given the `rdy-qa` areas.
/// - no `--only`: everything (perf unless `--skip-perf`);
/// - `--only perf`: the build and the perf tier only;
/// - `--only <area>`: fmt, clippy, the workspace tests, that area's `rdy-qa` test binary, the
///   `rdy-qa` unit tests and the perf tier (unless `--skip-perf`).
pub fn plan(only: Option<&str>, files: &[String], skip_perf: bool) -> Result<Plan, String> {
    match only {
        Some(PERF_AREA) if skip_perf => Err("--only perf with --skip-perf runs nothing".into()),
        Some(PERF_AREA) => Ok(Plan {
            checks: false,
            files: vec![],
            lib: false,
            perf: true,
            guarded: false,
        }),
        _ => {
            let chosen: Vec<String> = files
                .iter()
                .filter(|f| only.is_none_or(|a| area_of(f) == a))
                .cloned()
                .collect();
            if let Some(a) = only
                && chosen.is_empty()
            {
                let mut known: BTreeSet<&str> = files.iter().map(|f| area_of(f)).collect();
                known.insert(PERF_AREA);
                let known: Vec<&str> = known.into_iter().collect();
                return Err(format!("unknown area `{a}`; known: {}", known.join(", ")));
            }
            Ok(Plan {
                checks: true,
                files: chosen,
                lib: true,
                perf: !skip_perf,
                guarded: true,
            })
        }
    }
}

/// `engine_splitbrain` -> `engine`; the `render_quality*` files are their own area, apart from
/// `render`.
pub fn area_of(stem: &str) -> &str {
    if stem.starts_with("render_quality") {
        "render_quality"
    } else {
        stem.split('_').next().unwrap_or(stem)
    }
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

/// The paths directly under `dir`, sorted; none when it cannot be read.
fn dir_paths(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn is_rs_file(p: &Path) -> bool {
    p.is_file() && p.extension().is_some_and(|e| e == "rs")
}

/// True when `dir` holds a `.rs` file at any depth (a directory without one is data).
fn holds_rs(dir: &Path) -> bool {
    dir_paths(dir)
        .iter()
        .any(|p| is_rs_file(p) || (p.is_dir() && holds_rs(p)))
}

/// The `rdy-qa` test areas: the sorted names of the directories directly under `tests_dir`
/// that hold a `main.rs`, one test binary each.
pub fn test_areas(tests_dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = dir_paths(tests_dir)
        .iter()
        .filter(|p| p.join("main.rs").is_file())
        .map(|p| name_of(p))
        .collect();
    v.sort();
    v
}

/// What is wrong with the test layout (one test binary per area, `tests/<area>/main.rs`
/// with one `mod <stem>;` line per test file), one message per problem, sorted. Empty when
/// the layout is right. A test file without its `mod` line compiles and never runs.
pub fn layout_problems(tests_dir: &Path) -> Vec<String> {
    let mut out = vec![];
    for p in dir_paths(tests_dir) {
        let name = name_of(&p);
        if is_rs_file(&p) {
            out.push(format!(
                "tests/{name} is not in an area: run rdy-split-tests.py"
            ));
            continue;
        }
        if !p.is_dir() || !holds_rs(&p) {
            continue;
        }
        let main = p.join("main.rs");
        if !main.is_file() {
            out.push(format!("tests/{name}/ has test files but no main.rs"));
            continue;
        }
        let text = std::fs::read_to_string(&main).unwrap_or_default();
        let mut mods: BTreeMap<String, usize> = BTreeMap::new();
        for line in text.lines() {
            if let Some(stem) = line
                .trim()
                .strip_prefix("mod ")
                .and_then(|r| r.strip_suffix(';'))
            {
                *mods.entry(stem.trim().to_string()).or_default() += 1;
            }
        }
        for (stem, n) in &mods {
            if *n > 1 {
                out.push(format!("tests/{name}/main.rs: mod {stem} twice"));
            }
            if !p.join(format!("{stem}.rs")).is_file() && !p.join(stem).join("mod.rs").is_file() {
                out.push(format!("tests/{name}/main.rs: mod {stem} has no file"));
            }
        }
        for f in dir_paths(&p).iter().filter(|f| is_rs_file(f)) {
            let stem = f
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if stem != "main" && !mods.contains_key(&stem) {
                out.push(format!(
                    "tests/{name}/{stem}.rs has no mod line in tests/{name}/main.rs: it never runs"
                ));
            }
        }
    }
    out.sort();
    out
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
    /// Observations that do not affect the verdict (baseline drift, a failed listing).
    pub notes: Vec<String>,
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
            mark(area_ok(a)),
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

    if !r.notes.is_empty() {
        let _ = writeln!(s, "\n## Notes\n");
        for n in &r.notes {
            let _ = writeln!(s, "- {n}");
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

/// The verdict: every gate passed, every area ran tests and passed, and perf is fine.
/// A failed binary with no named test, or an area that ran zero tests, is a failure.
pub fn overall(gates: &[Gate], areas: &[Area], perf_ok: bool) -> bool {
    gates.iter().all(|g| g.pass) && areas.iter().all(area_ok) && perf_ok
}

/// An area passes when its binaries exited zero, no test failed, and it ran at least one test.
pub fn area_ok(a: &Area) -> bool {
    a.ok && a.failed == 0 && a.passed + a.failed + a.ignored > 0
}

/// True when a parsed test run saw no tests at all (a build that "passed" but ran nothing).
pub fn ran_nothing(run: &TestRun) -> bool {
    run.passed + run.failed + run.ignored == 0
}

/// The last `n` non-empty lines of `out`, for a failed gate's detail.
pub fn tail(out: &str, n: usize) -> String {
    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Parses the baseline file text. An error means the file is unusable and must not be
/// rewritten.
pub fn parse_baselines(text: &str) -> Result<BTreeMap<String, Baseline>, String> {
    #[derive(serde::Deserialize)]
    struct File {
        classes: BTreeMap<String, Baseline>,
    }
    serde_json::from_str::<File>(text)
        .map(|f| f.classes)
        .map_err(|e| e.to_string())
}

/// Ops measured but absent from the baseline, and baseline ops no longer measured.
pub fn baseline_drift(base: &Baseline, measured: &[Measure]) -> Vec<String> {
    let mut notes = vec![];
    for m in measured.iter().filter(|m| !base.ops.contains_key(&m.op)) {
        notes.push(format!(
            "perf op `{}` is measured but not in the baseline",
            m.op
        ));
    }
    for op in base
        .ops
        .keys()
        .filter(|o| !measured.iter().any(|m| &m.op == *o))
    {
        notes.push(format!("baseline op `{op}` is no longer measured"));
    }
    notes
}

pub fn json(r: &Report) -> String {
    serde_json::to_string_pretty(r).expect("report serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn area_of_keeps_render_quality_whole() {
        assert_eq!(area_of("render_quality_css"), "render_quality");
        assert_eq!(area_of("render_quality"), "render_quality");
        assert_eq!(area_of("render_site"), "render");
    }

    #[test]
    fn only_narrows_the_run_and_perf_is_an_area() {
        let files: Vec<String> = ["cli", "engine", "engine_splitbrain", "mcp"]
            .map(String::from)
            .to_vec();
        let all = plan(None, &files, false).unwrap();
        assert_eq!(
            all,
            Plan {
                checks: true,
                files: files.clone(),
                lib: true,
                perf: true,
                guarded: true,
            }
        );
        assert!(!plan(None, &files, true).unwrap().perf, "--skip-perf");
        let engine = plan(Some("engine"), &files, false).unwrap();
        assert_eq!(engine.files, vec!["engine", "engine_splitbrain"]);
        assert!(engine.checks && engine.lib && engine.perf);
        // `--only perf`: the build and the perf tier, nothing else
        assert_eq!(
            plan(Some("perf"), &files, false).unwrap(),
            Plan {
                checks: false,
                files: vec![],
                lib: false,
                perf: true,
                guarded: false,
            }
        );
        assert!(
            plan(Some("perf"), &files, true).is_err(),
            "would run nothing"
        );
        let e = plan(Some("nope"), &files, false).unwrap_err();
        assert!(e.contains("perf") && e.contains("engine"), "{e}");
    }

    #[test]
    fn parses_cargo_test_output() {
        let out = "running 3 tests\ntest a ... ok\ntest b ... FAILED\ntest c ... ignored\n\ntest result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out\n\nrunning 2 tests\ntest d ... ok\ntest e ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n";
        let r = parse_cargo_test(out);
        assert_eq!((r.passed, r.failed, r.ignored), (3, 1, 1));
        assert_eq!(r.failures, vec!["b".to_string()]);
    }

    #[test]
    fn collects_guarded_ids_per_area() {
        let list = "     Running tests/engine/main.rs (target/release/deps/engine-abc)\nengine_write::ord1_append_after_a_tie: test\nengine::plain_test: test\n     Running tests/render_quality/main.rs (target/release/deps/render_quality-def)\nrender_quality_pages::rq1_x: test\n     Running unittests src/lib.rs (target/release/deps/rdy_qa-0)\nreport::tests::tlay1_x: test\n";
        let g = guarded_ids(list);
        assert_eq!(g["engine"], ["ord1".to_string()].into());
        assert_eq!(g["render_quality"], ["rq1".to_string()].into());
        assert_eq!(g.len(), 2, "unit tests are not an area: {g:?}");
    }

    fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let p = d.path().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        d
    }

    #[test]
    fn tlay1_the_real_tree_has_no_layout_problem() {
        let tests = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
        assert_eq!(layout_problems(&tests), Vec::<String>::new());
        assert!(test_areas(&tests).contains(&"engine".to_string()));
        assert!(test_areas(&tests).contains(&"render_quality".to_string()));
    }

    #[test]
    fn tlay1_a_file_without_its_mod_line_is_named() {
        let d = tree(&[
            ("engine/main.rs", "//! doc\nmod engine_a;\n"),
            ("engine/engine_a.rs", ""),
            ("engine/engine_b.rs", ""),
        ]);
        let p = layout_problems(d.path());
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("engine/engine_b.rs"), "{p:?}");
    }

    #[test]
    fn tlay1_a_mod_line_without_its_file_is_named() {
        let d = tree(&[
            ("engine/main.rs", "mod engine_a;\nmod engine_gone;\n"),
            ("engine/engine_a.rs", ""),
        ]);
        let p = layout_problems(d.path());
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("engine_gone"), "{p:?}");
    }

    #[test]
    fn tlay1_a_file_listed_twice_is_named() {
        let d = tree(&[
            ("engine/main.rs", "mod engine_a;\nmod engine_a;\n"),
            ("engine/engine_a.rs", ""),
        ]);
        let p = layout_problems(d.path());
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("engine_a"), "{p:?}");
    }

    #[test]
    fn tlay1_a_top_level_test_file_and_an_area_without_main_are_named() {
        let d = tree(&[("wc_new.rs", ""), ("gate/gate_alts.rs", "")]);
        let p = layout_problems(d.path());
        assert_eq!(p.len(), 2, "{p:?}");
        assert!(p.iter().any(|x| x.contains("wc_new.rs")), "{p:?}");
        assert!(p.iter().any(|x| x.contains("gate/")), "{p:?}");
    }

    #[test]
    fn tlay1_a_data_directory_is_not_an_area() {
        let d = tree(&[
            ("golden/site/index.html", "<html>"),
            ("model/main.rs", "mod model;\n"),
            ("model/model.rs", ""),
        ]);
        assert_eq!(layout_problems(d.path()), Vec::<String>::new());
        assert_eq!(test_areas(d.path()), vec!["model".to_string()]);
    }

    fn area(ok: bool, passed: usize, failed: usize) -> Area {
        Area {
            name: "a".into(),
            passed,
            failed,
            ok,
            ..Area::default()
        }
    }

    fn gate(pass: bool) -> Gate {
        Gate {
            name: "g".into(),
            pass,
            detail: String::new(),
        }
    }

    #[test]
    fn the_verdict_fails_on_anything_wrong() {
        assert!(overall(&[gate(true)], &[area(true, 3, 0)], true));
        assert!(
            !overall(&[gate(false)], &[area(true, 3, 0)], true),
            "failed gate"
        );
        assert!(
            !overall(&[gate(true)], &[area(false, 3, 0)], true),
            "binary failed, no named test"
        );
        assert!(
            !overall(&[gate(true)], &[area(true, 0, 0)], true),
            "zero tests"
        );
        assert!(
            !overall(&[gate(true)], &[area(true, 3, 1)], true),
            "failed test"
        );
        assert!(
            !overall(&[gate(true)], &[area(true, 3, 0)], false),
            "perf failure"
        );
    }

    #[test]
    fn an_unparseable_baseline_is_an_error_and_a_good_one_parses() {
        assert!(parse_baselines("{ not json").is_err());
        assert!(parse_baselines("{}").is_err());
        assert!(parse_baselines("{\"classes\": {}}").unwrap().is_empty());
    }

    #[test]
    fn drift_names_added_and_removed_ops() {
        let m = |op: &str| Measure {
            op: op.into(),
            median_ms: 1.0,
            p95_ms: 1.0,
            runs: 1,
        };
        let base = Baseline {
            machine: "x".into(),
            ops: [
                ("old".to_string(), m("old")),
                ("same".to_string(), m("same")),
            ]
            .into(),
        };
        let d = baseline_drift(&base, &[m("same"), m("new")]);
        assert_eq!(d.len(), 2);
        assert!(d[0].contains("`new`") && d[1].contains("`old`"));
    }

    #[test]
    fn tail_keeps_the_last_lines() {
        assert_eq!(tail("a\n\nb\nc\n", 2), "b\nc");
    }
}
