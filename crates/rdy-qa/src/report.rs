//! The gate report: cargo-test output parsing, guarded defect ids, and the markdown / JSON
//! rendering that `cargo qa` writes to `target/qa/`.
use crate::perf::{Baseline, Measure, Status, Verdict};
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
    /// One entry per skip line (`skip::SKIP_MARK`), `<test>: <reason>`.
    pub skipped: Vec<String>,
    pub failures: Vec<String>,
    /// Each failing test's first panic, `<location>: <message>`, cut to `PANIC_CHARS`.
    pub panics: BTreeMap<String, String>,
}

/// The most characters of a panic message the report keeps.
const PANIC_CHARS: usize = 300;

/// Sums every `test result:` line, collects the skip lines (`skip::SKIP_MARK`), collects the names of `test <name> ... FAILED` lines, and
/// keeps each failing test's first panic: in a `---- <name> stdout ----` block, the line after
/// `thread '<thread>' panicked at <location>:` is the message (`panic_location`).
pub fn parse_cargo_test(output: &str) -> TestRun {
    let mut run = TestRun::default();
    // the test whose captured output is being read, and the location of its panic once seen
    let mut block: Option<String> = None;
    let mut panic_at: Option<String> = None;
    for line in output.lines() {
        if let Some(i) = line.find(crate::skip::SKIP_MARK) {
            let rest = &line[i + crate::skip::SKIP_MARK.len()..];
            run.skipped.push(rest.trim().to_string());
        }
        if let Some(at) = panic_at.take() {
            if let Some(name) = &block
                && !run.panics.contains_key(name)
            {
                let text = format!("{at}: {}", line.trim());
                run.panics
                    .insert(name.clone(), text.chars().take(PANIC_CHARS).collect());
            }
            continue;
        }
        if let Some(name) = line
            .strip_prefix("---- ")
            .and_then(|r| r.strip_suffix(" stdout ----"))
        {
            block = Some(name.to_string());
            continue;
        }
        if block.is_some()
            && let Some(at) = panic_location(line)
        {
            panic_at = Some(at.to_string());
            continue;
        }
        let line = line.trim();
        if line == "failures:" || line.starts_with("test result:") {
            block = None;
        }
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

/// `<location>` of a `thread '<thread>' panicked at <location>:` line. Newer toolchains print the
/// thread id too (`thread '<thread>' (<id>) panicked at`). A panic on a thread the test spawned
/// counts: it is often the cause of the test's own panic.
fn panic_location(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("thread '")?;
    let (_, rest) = rest.split_once("' ")?;
    let rest = match rest.strip_prefix('(') {
        Some(r) => {
            let (id, r) = r.split_once(") ")?;
            id.chars().all(|c| c.is_ascii_digit()).then_some(r)?
        }
        None => rest,
    };
    rest.strip_prefix("panicked at ")?.strip_suffix(':')
}

/// The pinned-defect ids (`ord1`, `blob1`, ...) of `cargo test -- --list` output, per area.
/// The area is `area_of(<binary>)` for a `Running tests/<binary>/main.rs (...)` header, so the
/// two engine binaries are the one `engine` area; any other header (the `Running unittests ...`
/// ones) sets no area.
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
                .map(|a| area_of(a).to_string());
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

/// The plan for `--only <area>` (or a full run), given the `rdy-qa` test binaries.
/// - no `--only`: everything (perf unless `--skip-perf`);
/// - `--only perf`: the build and the perf tier only;
/// - `--only <area>`: fmt, clippy, the workspace tests, that area's `rdy-qa` test binaries, the
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
        None if files.is_empty() => Err(
            "no rdy-qa test areas under crates/rdy-qa/tests: the gate would run no area binary"
                .into(),
        ),
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

/// Test binaries that report under another area: the two engine binaries are one area.
const GROUPED: &[(&str, &str)] = &[("engine_read", "engine")];

/// The area of the test binary `tests/<dir>/main.rs`: its directory name, unless `GROUPED` maps
/// it to another area (`engine_read` -> `engine`). `entity_search` is its own area.
pub fn area_of(dir: &str) -> &str {
    GROUPED
        .iter()
        .find(|(d, _)| *d == dir)
        .map_or(dir, |(_, area)| area)
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

/// The `rdy-qa` test binaries: the sorted names of the directories directly under `tests_dir`
/// that hold a `main.rs`, one test binary each. An area (`area_of`) may be more than one
/// binary: `engine` and `engine_read` are the `engine` area.
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
    // read here, not through `dir_paths`: an unreadable tests dir must not look like an empty one
    if let Err(e) = std::fs::read_dir(tests_dir) {
        return vec![format!("tests/ cannot be read: {e}")];
    }
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
    /// Tests that ran no check because a prerequisite is missing (`TestRun::skipped`).
    pub skipped: usize,
    /// The skip lines, `<test>: <reason>`.
    pub skip_lines: Vec<String>,
    /// False when a test binary exited non-zero, even if no test is named as failing
    /// (a compile error, a crash).
    pub ok: bool,
    pub failures: Vec<String>,
    /// Each failing test's first panic (`TestRun::panics`).
    pub panics: BTreeMap<String, String>,
}

/// Adds one `rdy-qa` test binary's run to the report's area rows, under `area_of(binary)`: an
/// area of more than one binary (`engine`, `engine_read`) is one row with their sums.
pub fn add_run(areas: &mut Vec<Area>, binary: &str, ok: bool, run: TestRun) {
    let name = area_of(binary).to_string();
    match areas.iter_mut().find(|a| a.name == name) {
        Some(a) => {
            a.passed += run.passed;
            a.failed += run.failed;
            a.ignored += run.ignored;
            a.skipped += run.skipped.len();
            a.skip_lines.extend(run.skipped);
            a.ok &= ok;
            a.failures.extend(run.failures);
            for (name, panic) in run.panics {
                a.panics.entry(name).or_insert(panic);
            }
        }
        None => areas.push(Area {
            name,
            passed: run.passed,
            failed: run.failed,
            ignored: run.ignored,
            skipped: run.skipped.len(),
            skip_lines: run.skipped,
            ok,
            failures: run.failures,
            panics: run.panics,
        }),
    }
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

/// What the perf table's tail column is.
pub const TAIL_NOTE: &str = "p95 is the run at index floor(0.95 x runs) in sorted order (the slowest run when there are 20 runs or fewer). A spec target stated as p99 is checked against this column.";

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
        "\n## Areas\n\n| Area | Result | Passed | Failed | Ignored | Skipped |\n|---|---|---|---|---|---|"
    );
    for a in &r.areas {
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} | {} |",
            a.name,
            mark(area_ok(a)),
            a.passed,
            a.failed,
            a.ignored,
            a.skipped
        );
    }
    for a in r.areas.iter().filter(|a| !a.skip_lines.is_empty()) {
        let _ = writeln!(s, "\nSkipped in `{}`:", a.name);
        for l in &a.skip_lines {
            let _ = writeln!(s, "- {l}");
        }
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
            match a.panics.get(f) {
                Some(p) => {
                    let _ = writeln!(s, "- `{f}`: {p}");
                }
                None => {
                    let _ = writeln!(s, "- `{f}`");
                }
            }
        }
    }

    let _ = writeln!(s, "\n## Perf\n\n{}\n", r.perf_note);
    if !r.perf.is_empty() {
        let _ = writeln!(
            s,
            "| Op | Median ms | p95 ms | Runs | Baseline ms | Delta | Result | Spec target |\n|---|---|---|---|---|---|---|---|"
        );
        for v in &r.perf {
            let _ = writeln!(
                s,
                "| {} | {:.3} | {:.3} | {} | {} | {} | {} | {} |",
                v.op,
                v.median_ms,
                v.p95_ms,
                v.runs,
                v.baseline_ms.map_or("-".into(), |b| format!("{b:.3}")),
                v.delta_pct.map_or("-".into(), |d| format!("{d:+.0}%")),
                v.status.label(),
                v.spec_target.as_deref().unwrap_or("-")
            );
        }
        let n = |st: Status| r.perf.iter().filter(|v| v.status == st).count();
        let _ = writeln!(
            s,
            "\n{TAIL_NOTE}\n\n{} pass, {} recorded, {} unbaselined, {} not compared, {} regressed, {} over a ceiling.",
            n(Status::Pass),
            n(Status::Recorded),
            n(Status::Unbaselined),
            n(Status::NotCompared),
            n(Status::Regressed),
            n(Status::OverCeiling),
        );
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
        assert_eq!(area_of("render_quality"), "render_quality");
        assert_eq!(area_of("render"), "render");
    }

    #[test]
    fn only_narrows_the_run_and_perf_is_an_area() {
        let files: Vec<String> = ["cli", "engine", "engine_read", "mcp"]
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
        assert_eq!(engine.files, vec!["engine", "engine_read"]);
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
        let list = "     Running tests/engine/main.rs (target/release/deps/engine-abc)\nengine_write::ord1_append_after_a_tie: test\nengine::plain_test: test\n     Running tests/engine_read/main.rs (target/release/deps/engine_read-abd)\nengine_search::xr1_x: test\n     Running tests/render_quality/main.rs (target/release/deps/render_quality-def)\nrender_quality_pages::rq1_x: test\n     Running unittests src/lib.rs (target/release/deps/rdy_qa-0)\nreport::tests::tlay1_x: test\n";
        let g = guarded_ids(list);
        // Both engine binaries are the one `engine` area.
        assert_eq!(g["engine"], ["ord1".to_string(), "xr1".to_string()].into());
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
    fn tlay1_only_engine_runs_both_engine_binaries() {
        // The engine area is two binaries so that one test edit rebuilds half of it.
        // `--only engine` still means all of it (R-2).
        let tests = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
        let areas = test_areas(&tests);
        let engine = plan(Some("engine"), &areas, true).unwrap();
        assert_eq!(engine.files, vec!["engine", "engine_read"]);
        // The report has one `engine` row with both binaries' counts (R-2: the report's areas).
        let mut rows = vec![];
        let run = |passed, failed, ignored, failures: &[&str]| TestRun {
            passed,
            failed,
            ignored,
            failures: failures.iter().map(|f| f.to_string()).collect(),
            panics: Default::default(),
            skipped: vec![],
        };
        add_run(&mut rows, "engine", true, run(134, 0, 0, &[]));
        add_run(
            &mut rows,
            "engine_read",
            false,
            run(147, 1, 1, &["engine_search::x"]),
        );
        add_run(&mut rows, "lib", true, run(47, 0, 9, &[]));
        let rows: Vec<_> = rows
            .iter()
            .map(|a| {
                (
                    a.name.as_str(),
                    a.passed,
                    a.failed,
                    a.ignored,
                    a.ok,
                    a.failures.clone(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            vec![
                (
                    "engine",
                    281,
                    1,
                    1,
                    false,
                    vec!["engine_search::x".to_string()]
                ),
                ("lib", 47, 0, 9, true, vec![]),
            ]
        );
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
    fn msw6_a_full_run_with_no_test_areas_is_an_error() {
        assert!(
            plan(None, &[], false).is_err(),
            "a full gate with no area binary must not run"
        );
        assert!(plan(None, &[], true).is_err());
        assert!(
            plan(Some("perf"), &[], false).is_ok(),
            "--only perf needs no area"
        );
        let d = tempfile::tempdir().unwrap();
        let gone = d.path().join("missing");
        assert!(
            !layout_problems(&gone).is_empty(),
            "an unreadable tests dir is a layout problem"
        );
    }

    #[test]
    fn msw6_an_underscored_area_dir_is_its_own_area() {
        let files: Vec<String> = ["engine", "engine_read", "entity", "entity_search"]
            .map(String::from)
            .to_vec();
        assert_eq!(area_of("entity_search"), "entity_search");
        assert_eq!(
            area_of("engine_read"),
            "engine",
            "the two engine binaries are one area"
        );
        assert_eq!(area_of("render_quality"), "render_quality");
        assert_eq!(
            plan(Some("entity_search"), &files, true).unwrap().files,
            vec!["entity_search"]
        );
        assert_eq!(
            plan(Some("entity"), &files, true).unwrap().files,
            vec!["entity"]
        );
        assert_eq!(
            plan(Some("engine"), &files, true).unwrap().files,
            vec!["engine", "engine_read"]
        );
    }

    #[test]
    fn msw6_a_failing_test_keeps_its_panic_message() {
        let out = "test a::b ... FAILED\n\nfailures:\n\n---- a::b stdout ----\n\nthread 'a::b' panicked at crates/x.rs:3:5:\nassertion failed: boom\nnote: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n\nfailures:\n    a::b\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n";
        let run = parse_cargo_test(out);
        assert_eq!(run.failures, vec!["a::b".to_string()]);
        assert_eq!(
            run.panics.get("a::b").map(String::as_str),
            Some("crates/x.rs:3:5: assertion failed: boom")
        );
    }

    #[test]
    fn msw6_a_panic_line_with_a_thread_id_reaches_the_report() {
        // rustc 1.99 prints the thread id; a spawned worker's panic comes first and is kept
        let out = "test kill2_x ... FAILED\n\nfailures:\n\n---- kill2_x stdout ----\nthread '<unnamed>' (4242) panicked at crates/w.rs:9:1:\nworker failed\nthread 'kill2_x' (4241) panicked at crates/t.rs:2:2:\njoin failed\n\nfailures:\n    kill2_x\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n";
        let run = parse_cargo_test(out);
        assert_eq!(
            run.panics.get("kill2_x").map(String::as_str),
            Some("crates/w.rs:9:1: worker failed")
        );
        let long = format!(
            "---- a stdout ----\nthread 'a' panicked at x.rs:1:1:\n{}\n",
            "y".repeat(400)
        );
        assert_eq!(parse_cargo_test(&long).panics["a"].chars().count(), 300);
        let mut areas = vec![];
        add_run(&mut areas, "daemon", false, run);
        let r = Report {
            revision: "r".into(),
            dirty: false,
            machine: "m".into(),
            mode: "full".into(),
            duration_secs: 0.0,
            pass: false,
            gates: vec![],
            areas,
            perf: vec![],
            perf_note: String::new(),
            failing_seeds: vec![],
            notes: vec![],
            guarded: BTreeMap::new(),
        };
        assert!(
            markdown(&r).contains("- `kill2_x`: crates/w.rs:9:1: worker failed\n"),
            "{}",
            markdown(&r)
        );
    }

    #[test]
    fn hd6_the_perf_table_names_each_status_and_the_tail() {
        use crate::perf::{Measure, Mode, compare, not_compared};
        let mk = |op: &str| Measure {
            op: op.into(),
            median_ms: 1.0,
            p95_ms: 1.0,
            runs: 20,
        };
        let mut perf = compare(None, &[mk("log_full")], Mode::Compare);
        perf.extend(not_compared(&[mk("history_100")]));
        let r = Report {
            revision: "r".into(),
            dirty: false,
            machine: "m".into(),
            mode: "full".into(),
            duration_secs: 0.0,
            pass: false,
            gates: vec![],
            areas: vec![],
            perf,
            perf_note: String::new(),
            failing_seeds: vec![],
            notes: vec![],
            guarded: BTreeMap::new(),
        };
        let md = markdown(&r);
        assert!(
            md.contains("| log_full | 1.000 | 1.000 | 20 | - | - | UNBASELINED |"),
            "{md}"
        );
        assert!(
            md.contains("| history_100 | 1.000 | 1.000 | 20 | - | - | RECORDED, NOT COMPARED |"),
            "{md}"
        );
        assert!(md.contains(TAIL_NOTE), "{md}");
        assert!(
            md.contains(
                "0 pass, 0 recorded, 1 unbaselined, 1 not compared, 0 regressed, 0 over a ceiling."
            ),
            "{md}"
        );
    }

    #[test]
    fn hd6_b_the_tail_note_states_the_rule_the_code_uses() {
        // measure_timed takes index floor(0.95 * runs), capped at the last run. That is not the
        // nearest-rank rule (ceil(0.95 * runs)), so the note must not claim it.
        assert!(!TAIL_NOTE.contains("nearest"), "{TAIL_NOTE}");
        assert!(
            TAIL_NOTE.contains("index floor(0.95 x runs) in sorted order"),
            "{TAIL_NOTE}"
        );
        assert!(
            TAIL_NOTE.contains("the slowest run when there are 20 runs or fewer"),
            "{TAIL_NOTE}"
        );
        assert!(TAIL_NOTE.contains("p99"), "{TAIL_NOTE}");
    }

    #[test]
    fn hd10_skip_lines_are_counted_per_area() {
        let out = "running 2 tests\nRDY-QA-SKIP dia1_x: node is not installed\ntest dia1_x ... ok\n\
                   test other ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n";
        let run = parse_cargo_test(out);
        assert_eq!(
            run.skipped,
            vec!["dia1_x: node is not installed".to_string()]
        );
        let mut areas = vec![];
        add_run(&mut areas, "render", true, run);
        assert_eq!(areas[0].skipped, 1);
        let r = Report {
            revision: "r".into(),
            dirty: false,
            machine: "m".into(),
            mode: "full".into(),
            duration_secs: 0.0,
            pass: true,
            gates: vec![],
            areas,
            perf: vec![],
            perf_note: String::new(),
            failing_seeds: vec![],
            notes: vec![],
            guarded: BTreeMap::new(),
        };
        let md = markdown(&r);
        assert!(
            md.contains("| Passed | Failed | Ignored | Skipped |"),
            "{md}"
        );
        assert!(md.contains("| render | PASS | 2 | 0 | 0 | 1 |"), "{md}");
        assert!(
            md.contains("Skipped in `render`:\n- dia1_x: node is not installed\n"),
            "{md}"
        );
    }

    #[test]
    fn tail_keeps_the_last_lines() {
        assert_eq!(tail("a\n\nb\nc\n", 2), "b\nc");
    }
}
