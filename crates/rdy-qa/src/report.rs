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
    /// Each failing test's first panic: its `thread '...' panicked at ...:` header line and the
    /// lines after it, cut to `PANIC_LINES` lines and `PANIC_BYTES` bytes.
    pub panics: BTreeMap<String, String>,
}

/// The most lines of a panic the report keeps, the truncation marker included.
const PANIC_LINES: usize = 40;
/// The most bytes of a panic the report keeps, the truncation marker included.
const PANIC_BYTES: usize = 16384;
/// The last line of a panic cut to the two limits above.
const PANIC_CUT: &str = "[panic text truncated]";

/// One panic being read, bounded as it grows: no line is copied whole before it is checked.
#[derive(Default)]
struct Excerpt {
    text: String,
    lines: usize,
    cut: bool,
}

impl Excerpt {
    /// Appends `line`. The first line that would pass either limit cuts the excerpt: it keeps
    /// room for one marker line, drops what does not fit before it, and ignores all later lines.
    fn push(&mut self, line: &str) {
        if self.cut {
            return;
        }
        let sep = usize::from(self.lines > 0);
        if self.lines < PANIC_LINES && self.text.len() + sep + line.len() <= PANIC_BYTES {
            self.text.push_str(&"\n"[..sep]);
            self.text.push_str(line);
            self.lines += 1;
            return;
        }
        self.cut = true;
        let keep_lines = PANIC_LINES - 1;
        let keep_bytes = PANIC_BYTES - 1 - PANIC_CUT.len();
        // the part of this line that still fits before the marker
        if self.lines < keep_lines {
            let room = keep_bytes.saturating_sub(self.text.len() + sep);
            let end = line.floor_char_boundary(room.min(line.len()));
            if end > 0 {
                self.text.push_str(&"\n"[..sep]);
                self.text.push_str(&line[..end]);
            }
        }
        self.text
            .truncate(self.text.floor_char_boundary(keep_bytes));
        let mut lines = self.text.matches('\n').count() + usize::from(!self.text.is_empty());
        while lines > keep_lines {
            self.text.truncate(self.text.rfind('\n').unwrap_or(0));
            lines -= 1;
        }
        if !self.text.is_empty() {
            self.text.push('\n');
        }
        self.text.push_str(PANIC_CUT);
    }
}

/// Where the current `---- <name> stdout ----` block stands in reading its first panic.
enum Panic {
    /// No panic header yet.
    Waiting,
    /// Reading the first panic's lines.
    Reading(Excerpt),
    /// The first panic is read, or the test already has one: the rest of the block is ignored.
    Done,
}

/// Sums every `test result:` line, collects the skip lines (`skip::SKIP_MARK`), collects the names of `test <name> ... FAILED` lines, and
/// keeps each failing test's first panic: in a `---- <name> stdout ----` block, the lines from the
/// first `thread '<thread>' panicked at <location>:` header (`panic_location`), the header
/// included, up to the next output header, panic header, `failures:` or `test result:` line. A
/// panic is kept only for a test named as failing. Counters, skips and failed names are read from
/// every line, whether or not a panic is being read.
pub fn parse_cargo_test(output: &str) -> TestRun {
    let mut run = TestRun::default();
    // the test whose captured output is being read, and how far its first panic is read
    let mut block: Option<String> = None;
    let mut panic = Panic::Done;
    let mut found: BTreeMap<String, String> = BTreeMap::new();
    // stores the panic being read, once the block or the input ends
    fn flush(found: &mut BTreeMap<String, String>, block: &Option<String>, panic: &mut Panic) {
        if let (Some(name), Panic::Reading(e)) = (block, &*panic) {
            found.entry(name.clone()).or_insert_with(|| e.text.clone());
        }
        *panic = Panic::Done;
    }
    for line in output.lines() {
        if let Some(i) = line.find(crate::skip::SKIP_MARK) {
            let rest = &line[i + crate::skip::SKIP_MARK.len()..];
            run.skipped.push(rest.trim().to_string());
        }
        let header = line
            .strip_prefix("---- ")
            .and_then(|r| r.strip_suffix(" stdout ----"));
        let stop = line == "failures:" || line.starts_with("test result:");
        if let Some(name) = header {
            flush(&mut found, &block, &mut panic);
            panic = if found.contains_key(name) {
                Panic::Done
            } else {
                Panic::Waiting
            };
            block = Some(name.to_string());
        } else if let Panic::Reading(e) = &mut panic {
            if stop || panic_location(line).is_some() {
                flush(&mut found, &block, &mut panic);
            } else {
                e.push(line);
            }
        } else if matches!(panic, Panic::Waiting) && panic_location(line).is_some() {
            let mut e = Excerpt::default();
            e.push(line);
            panic = Panic::Reading(e);
        }
        if stop {
            flush(&mut found, &block, &mut panic);
            block = None;
        }
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
    flush(&mut found, &block, &mut panic);
    found.retain(|name, _| run.failures.contains(name));
    run.panics = found;
    run
}

/// The failing tests of one report area, in the given order, as a Markdown list. A test with
/// a panic excerpt is followed by a blank line and the excerpt, every line indented eight
/// spaces (an indented code block under the item, so a fence inside the excerpt ends nothing),
/// and a blank line. Empty for no names.
pub fn failure_lines(failures: &[String], panics: &BTreeMap<String, String>) -> String {
    let mut s = String::new();
    for f in failures {
        let _ = writeln!(s, "- `{f}`");
        if let Some(p) = panics.get(f) {
            s.push('\n');
            for l in p.split('\n') {
                let _ = writeln!(s, "        {l}");
            }
            s.push('\n');
        }
    }
    s
}

/// The detail of the workspace-tests gate step: the counts, then (when tests failed) the failing
/// names with their panic excerpts, then (when the step failed) the last 20 lines of `output`,
/// then a note when no test ran.
pub fn workspace_test_detail(ok: bool, run: &TestRun, output: &str) -> String {
    let mut detail = format!(
        "{} passed, {} failed, {} ignored, {} skipped",
        run.passed,
        run.failed,
        run.ignored,
        run.skipped.len()
    );
    if !run.failures.is_empty() {
        detail += "\n\n";
        detail += &failure_lines(&run.failures, &run.panics);
    }
    if !ok {
        if !detail.ends_with('\n') {
            detail.push('\n');
        }
        detail += &tail(output, 20);
    }
    if ran_nothing(run) {
        detail += " (no tests ran)";
    }
    detail
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
        s += &failure_lines(&a.failures, &a.panics);
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
            Some(
                "thread 'a::b' panicked at crates/x.rs:3:5:\nassertion failed: boom\nnote: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n"
            )
        );
    }

    #[test]
    fn msw6_a_panic_line_with_a_thread_id_reaches_the_report() {
        // rustc 1.99 prints the thread id; a spawned worker's panic comes first and is kept
        let out = "test kill2_x ... FAILED\n\nfailures:\n\n---- kill2_x stdout ----\nthread '<unnamed>' (4242) panicked at crates/w.rs:9:1:\nworker failed\nthread 'kill2_x' (4241) panicked at crates/t.rs:2:2:\njoin failed\n\nfailures:\n    kill2_x\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n";
        let run = parse_cargo_test(out);
        assert_eq!(
            run.panics.get("kill2_x").map(String::as_str),
            Some("thread '<unnamed>' (4242) panicked at crates/w.rs:9:1:\nworker failed")
        );
        let long = format!(
            "test a ... FAILED\n---- a stdout ----\nthread 'a' panicked at x.rs:1:1:\n{}\n",
            "y".repeat(400)
        );
        assert_eq!(
            parse_cargo_test(&long).panics["a"],
            format!("thread 'a' panicked at x.rs:1:1:\n{}", "y".repeat(400)),
            "the old 300-character cut is gone"
        );
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
            markdown(&r).contains(
                "- `kill2_x`\n\n        thread '<unnamed>' (4242) panicked at crates/w.rs:9:1:\n        worker failed\n\n"
            ),
            "{}",
            markdown(&r)
        );
    }

    /// A `Report` holding one area, for the rendering pins.
    fn report_of(areas: Vec<Area>) -> Report {
        Report {
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
        }
    }

    /// Cargo output of one failing test `t` whose panic body is `body`, with 4 passed and
    /// 2 ignored tests besides.
    fn one_failure(body: &str) -> String {
        format!(
            "test t ... FAILED\n\nfailures:\n\n---- t stdout ----\nthread 't' panicked at x.rs:1:1:\n{body}\nfailures:\n    t\n\ntest result: FAILED. 4 passed; 1 failed; 2 ignored; 0 measured; 0 filtered out\n"
        )
    }

    const PANIC_HEAD: &str = "thread 't' panicked at x.rs:1:1:";
    const TRUNCATED: &str = "[panic text truncated]";

    #[test]
    fn plx3_report_keeps_multiline_assertions_per_failed_test() {
        let skip = crate::skip::skip_line("s::skipped", "no tool");
        let out = [
            "running 3 tests".to_string(),
            "test a::first ... FAILED".to_string(),
            "test b::ok ... ok".to_string(),
            skip,
            "test c::second ... FAILED".to_string(),
            String::new(),
            "failures:".to_string(),
            String::new(),
            "---- a::first stdout ----".to_string(),
            // a worker thread panics first, with numeric thread ids
            "thread '<unnamed>' (11) panicked at crates/w.rs:9:1:".to_string(),
            "assertion `left == right` failed: worker".to_string(),
            "  left: 1".to_string(),
            " right: 2".to_string(),
            "thread 'a::first' (10) panicked at crates/t.rs:2:2:".to_string(),
            "called `Result::unwrap()` on an `Err` value".to_string(),
            "note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace"
                .to_string(),
            String::new(),
            "---- c::second stdout ----".to_string(),
            // the older header without a thread id
            "thread 'c::second' panicked at crates/u.rs:5:5:".to_string(),
            "assertion `left == right` failed: second".to_string(),
            "  left: \"x\"".to_string(),
            " right: \"y\"".to_string(),
            String::new(),
            "failures:".to_string(),
            "    a::first".to_string(),
            "    c::second".to_string(),
            String::new(),
            "test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out"
                .to_string(),
        ];
        let first = "thread '<unnamed>' (11) panicked at crates/w.rs:9:1:\nassertion `left == right` failed: worker\n  left: 1\n right: 2";
        let second = "thread 'c::second' panicked at crates/u.rs:5:5:\nassertion `left == right` failed: second\n  left: \"x\"\n right: \"y\"\n";
        // CRLF line ends, the last line without any end
        let crlf = out.join("\r\n");
        for text in [out.join("\n"), crlf] {
            let run = parse_cargo_test(&text);
            assert_eq!((run.passed, run.failed, run.ignored), (1, 2, 0));
            assert_eq!(run.skipped, vec!["s::skipped: no tool".to_string()]);
            assert_eq!(
                run.failures,
                vec!["a::first".to_string(), "c::second".to_string()]
            );
            assert_eq!(run.panics.len(), 2, "{:?}", run.panics);
            assert_eq!(run.panics["a::first"], first);
            assert_eq!(run.panics["c::second"], second);
        }
        // the input ends inside the panic: flushed at the end of input, no line end
        let cut = "test c ... FAILED\n---- c stdout ----\nthread 'c' panicked at x.rs:1:1:\nassertion failed: eof";
        let run = parse_cargo_test(cut);
        assert_eq!(
            run.panics["c"],
            "thread 'c' panicked at x.rs:1:1:\nassertion failed: eof"
        );
        // a panic of a test that did not fail is not kept
        let passing = "test p ... ok\n---- p stdout ----\nthread 'p' panicked at x.rs:1:1:\nexpected\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n";
        let run = parse_cargo_test(passing);
        assert!(run.panics.is_empty(), "{:?}", run.panics);
        assert_eq!((run.passed, run.failed), (1, 0));
    }

    #[test]
    fn plx3_report_bounds_panic_text_without_losing_counts() {
        let counts = |run: &TestRun| (run.passed, run.failed, run.ignored, run.failures.len());
        let want_counts = (4, 1, 2, 1);
        let lines = |n: usize| {
            (1..n)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        // 39 and 40 lines (header included) are kept whole
        for n in [39, 40] {
            let run = parse_cargo_test(&one_failure(&lines(n)));
            let p = &run.panics["t"];
            assert_eq!(p.split('\n').count(), n);
            assert_eq!(*p, format!("{PANIC_HEAD}\n{}", lines(n)));
            assert_eq!(counts(&run), want_counts);
        }
        // 41 lines: 39 kept, then the marker as the 40th line; nothing after the marker
        let run = parse_cargo_test(&one_failure(&format!(
            "{}\nthread 't' panicked at y.rs:2:2:\nSECOND PANIC",
            lines(60)
        )));
        let p = &run.panics["t"];
        assert_eq!(p.split('\n').count(), PANIC_LINES, "{p}");
        let want = format!("{PANIC_HEAD}\n{}\n{TRUNCATED}", lines(39));
        assert_eq!(*p, want);
        assert!(!p.contains("SECOND PANIC"), "{p}");
        assert_eq!(counts(&run), want_counts);
        let run = parse_cargo_test(&one_failure(&lines(41)));
        assert_eq!(run.panics["t"], want);

        // bytes: the header line, a separator and one long body line
        let room = PANIC_BYTES - PANIC_HEAD.len() - 1;
        for (extra, cut) in [(room - 1, false), (room, false), (room + 1, true)] {
            let run = parse_cargo_test(&one_failure(&"y".repeat(extra)));
            let p = &run.panics["t"];
            assert_eq!(p.ends_with(TRUNCATED), cut, "{extra}");
            assert!(p.len() <= PANIC_BYTES && p.split('\n').count() <= PANIC_LINES);
            if !cut {
                assert_eq!(p.len(), PANIC_HEAD.len() + 1 + extra);
            } else {
                assert!(p.starts_with(&format!("{PANIC_HEAD}\nyyy")), "{extra}");
                assert_eq!(p.len(), PANIC_BYTES, "all the room is used");
            }
            assert_eq!(counts(&run), want_counts);
        }

        // a multibyte character on the cut: the text stays valid and inside the bound
        for pad in ["", "a"] {
            let body = format!("{pad}{}", "é".repeat(PANIC_BYTES));
            let run = parse_cargo_test(&one_failure(&body));
            let p = &run.panics["t"];
            assert!(p.len() <= PANIC_BYTES, "{}", p.len());
            let kept = p
                .strip_suffix(&format!("\n{TRUNCATED}"))
                .expect("the marker closes the excerpt");
            let body = kept.strip_prefix(&format!("{PANIC_HEAD}\n")).unwrap();
            assert!(body.starts_with(pad) && body[pad.len()..].chars().all(|c| c == 'é'));
            assert!(std::str::from_utf8(p.as_bytes()).is_ok());
            assert_eq!(counts(&run), want_counts);
        }

        // one line of 1 MiB: bounded, with the marker, and the next panic is not read
        let huge = format!(
            "{}\nthread 't' panicked at y.rs:2:2:\nSECOND",
            "z".repeat(1 << 20)
        );
        let run = parse_cargo_test(&one_failure(&huge));
        let p = &run.panics["t"];
        assert!(p.len() <= PANIC_BYTES, "{}", p.len());
        assert!(p.ends_with(&format!("\n{TRUNCATED}")) && !p.contains("SECOND"));
        assert_eq!(p.matches("panicked at").count(), 1);
        assert_eq!(counts(&run), want_counts);
    }

    #[test]
    fn plx3_workspace_failures_keep_their_panic_excerpt() {
        let mut out = String::from(
            "running 2 tests\ntest w::ok ... ok\ntest w::bad ... FAILED\n\nfailures:\n\n---- w::bad stdout ----\nthread 'w::bad' (7) panicked at crates/w.rs:4:9:\nassertion `left == right` failed\n  left: 1\n right: 2\nnote: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n\nfailures:\n    w::bad\n\ntest result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n",
        );
        for i in 0..25 {
            out += &format!("filler {i}\n");
        }
        let run = parse_cargo_test(&out);
        assert_eq!(run.failures, vec!["w::bad".to_string()]);
        let detail = workspace_test_detail(false, &run, &out);
        let tail_text = tail(&out, 20);
        let want_excerpt = "- `w::bad`\n\n        thread 'w::bad' (7) panicked at crates/w.rs:4:9:\n        assertion `left == right` failed\n          left: 1\n         right: 2\n        note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n        \n\n";
        assert_eq!(
            detail,
            format!("1 passed, 1 failed, 0 ignored, 0 skipped\n\n{want_excerpt}{tail_text}")
        );
        assert!(!tail_text.contains("panicked at"));
        assert_eq!(detail.matches("panicked at").count(), 1);
        assert!(detail.find("panicked at").unwrap() < detail.find("filler 5").unwrap());
        // a passing step keeps its count line alone; an empty run keeps its note after the tail
        let ok = workspace_test_detail(true, &TestRun::default(), "");
        assert_eq!(
            ok,
            "0 passed, 0 failed, 0 ignored, 0 skipped (no tests ran)"
        );
        let none = workspace_test_detail(false, &TestRun::default(), "boom\n");
        assert_eq!(
            none,
            "0 passed, 0 failed, 0 ignored, 0 skipped\nboom (no tests ran)"
        );
    }

    #[test]
    fn plx3_markdown_and_json_keep_the_same_panic_excerpt() {
        // 44 body lines, one of them a Markdown fence: bounded and still under its name
        let mut body: Vec<String> = (1..45).map(|i| format!("  detail {i}")).collect();
        body[1] = "```".into();
        body[2] = "~~~ not a fence either".into();
        let out = one_failure(&body.join("\n"));
        let run = parse_cargo_test(&out);
        let excerpt = run.panics["t"].clone();
        assert_eq!(excerpt.split('\n').count(), PANIC_LINES);
        assert!(excerpt.ends_with(TRUNCATED) && excerpt.contains("\n```\n"));
        let mut areas = vec![];
        add_run(&mut areas, "daemon", false, run);
        let r = report_of(areas);
        let md = markdown(&r);
        let mut want = String::from("- `t`\n\n");
        for l in excerpt.split('\n') {
            want += &format!("        {l}\n");
        }
        want.push('\n');
        assert!(md.contains(&want), "{md}");
        // inside the excerpt every line is indented: a fence cannot close anything
        let start = md.find("- `t`\n").unwrap();
        let block = &md[start..start + want.len()];
        for l in block.lines().skip(1).filter(|l| !l.is_empty()) {
            assert!(l.starts_with("        "), "{l:?}");
        }
        assert!(
            md.lines().all(|l| l != "```" && !l.starts_with("```")),
            "{md}"
        );
        // the JSON holds the same bounded text under the same name
        let v: serde_json::Value = serde_json::from_str(&json(&r)).unwrap();
        assert_eq!(
            v["areas"][0]["panics"]["t"].as_str(),
            Some(excerpt.as_str())
        );
        assert!(excerpt.len() <= PANIC_BYTES);
        // a name with no excerpt stays alone; an unknown name draws nothing
        let none = failure_lines(&["x".to_string()], &BTreeMap::new());
        assert_eq!(none, "- `x`\n");
        assert_eq!(failure_lines(&[], &r.areas[0].panics), "");
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

    #[test]
    fn hd12_ci_runs_the_fast_gate_as_a_user_with_node() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let text = std::fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("ci.yml");
        let toolchain = std::fs::read_to_string(root.join("rust-toolchain.toml")).unwrap();
        let ci: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text).unwrap();
        let steps = ci["jobs"]["fast"]["steps"]
            .as_sequence()
            .expect("jobs.fast.steps");
        let runs: Vec<&str> = steps.iter().filter_map(|s| s["run"].as_str()).collect();
        let uses: Vec<&str> = steps.iter().filter_map(|s| s["uses"].as_str()).collect();
        assert!(
            uses.iter().any(|u| u.starts_with("actions/setup-node@")),
            "{uses:?}"
        );
        assert!(
            runs.iter().any(|r| r.contains("test \"$(id -u)\" -ne 0")),
            "a non-root check: {runs:?}"
        );
        assert!(
            runs.iter()
                .any(|r| r.contains("cargo qa --skip-perf --require-all")),
            "{runs:?}"
        );
        let channel = toolchain
            .lines()
            .find_map(|l| l.strip_prefix("channel = "))
            .unwrap()
            .trim_matches('"');
        assert!(
            runs.iter()
                .any(|r| r.contains(&format!("rustup toolchain install {channel}"))),
            "{runs:?}"
        );
        assert!(
            !text.contains("--deep") && !text.contains("--only perf"),
            "deep and perf stay off CI"
        );
    }
}
