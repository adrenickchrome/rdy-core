//! `cargo qa`: the full regression and hardening gate, with a report in `target/qa/`.
use clap::Parser;
use rdy_qa::gate_steps::{
    Allowances, AreaEnv, BINARY_JOBS, BUILD_STEP, CEILING_FACTOR, ChildOutput, Clock, GateStep,
    HEAVY_JOBS, SERIAL_AREAS, Stages, TestBinary, TestExe, UNBOUNDED, budget_row, build_steps,
    cargo, discover_command, fail_tail, fmt_step, gate_exit, is_heavy, launch_area, list_guarded,
    not_started, on_real_disk, over_target_row, parse_test_binaries, probe_envs, run_areas,
    run_areas_capped, run_bounded, run_stage, stale_units,
};
use rdy_qa::perf::{self, Baseline, Measure};
use rdy_qa::report::{self, Area, Gate, Report, StageRecord, Timing};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "qa-gate", about = "rdy's regression and hardening gate")]
struct Args {
    /// Multiply proptest case counts by 10 and run the slow sweeps (before a milestone).
    #[arg(long)]
    deep: bool,
    /// Narrow the run to one area. `perf`: the locked fetch (`cargo fetch --locked`), the release
    /// build and the perf tier only. Any other area (model, store, schema, engine, io, mcp, cli,
    /// render, ...): the locked fetch (`cargo fetch --locked`), the release build, fmt, clippy,
    /// the workspace tests, that area's rdy-qa test binary (`tests/<area>/main.rs`), the rdy-qa
    /// unit tests and the perf tier (unless --skip-perf).
    #[arg(long, value_name = "AREA")]
    only: Option<String>,
    /// Skip the perf-regression tier.
    #[arg(long)]
    skip_perf: bool,
    /// Rewrite this machine class's perf baseline (after an intended performance change).
    #[arg(long)]
    rebaseline: bool,
    /// Fail a test whose prerequisite is missing (Node, a case-insensitive filesystem, a
    /// non-root user) instead of letting it skip. Sets `RDY_QA_REQUIRE_ALL=1` on the test runs.
    #[arg(long)]
    require_all: bool,
    /// Run one crate's tests or one rdy-qa area, with only the builds they need: no fmt, clippy,
    /// workspace tests, perf or report file. `<crate|area>[::<filter>]`; for a crate, a first
    /// filter segment that names a test target (`tests/<name>.rs`) runs that target alone.
    /// Fails when no test matches, and ends with the replay command.
    #[arg(long, value_name = "CRATE|AREA[::FILTER]", conflicts_with_all = ["only", "rebaseline", "skip_perf"])]
    focus: Option<String>,
    /// Run every stage even after a failure. By default a failed step skips the steps that need
    /// it (each shows `SKIPPED (after <step> failed)`), so a broken build does not run the tests.
    #[arg(long)]
    all_stages: bool,
    /// Put every test child's temporary files in this directory (a RAM disk or tmpfs): it sets
    /// `TMPDIR` (Unix) or `TEMP` and `TMP` (Windows), and `RDY_QA_SHORT_TMP`. The binaries in
    /// `REAL_DISK_BINARIES` keep the real temp directory. The directory must exist.
    #[arg(long, value_name = "DIR")]
    tmp_root: Option<PathBuf>,
    /// The test tier. `release`: the run a release reads; fmt, clippy and the Python CI tests
    /// run on Linux only, each test binary's time is checked against its allowance in
    /// `gate-allowances.json`, the tests of the nightly tier are not built, and the perf tier
    /// does not run (except for `--only perf`). `nightly`: every test of both tiers
    /// (`--features nightly-tests`) and the perf tier, and each binary's nightly-only count is
    /// checked against `gate-allowances.json`.
    #[arg(long, value_enum)]
    tier: Option<Tier>,
    /// The run's time target in seconds, the builds included; required with `--tier release`.
    /// A run past it finishes: the report adds a warning row `OVER TARGET: <n> s, target <t> s`
    /// with the slowest binaries, and the verdict stays as it is.
    #[arg(long, value_name = "SECONDS", required_if_eq("tier", "release"))]
    budget_secs: Option<u64>,
    /// The hard stop in seconds; by default twice `--budget-secs`. Each child's deadline is the
    /// time left before it. When it is reached the gate stops the running children, adds a failed
    /// `BUDGET` row that names the stage and exits 30.
    #[arg(long, value_name = "SECONDS", requires = "budget_secs")]
    ceiling_secs: Option<u64>,
    /// Run only the test binaries of one platform's tier: the binaries that the `<platform>`
    /// list of `gate-allowances.json` names. `windows`: the seams, store, daemon, CLI, working
    /// copy, io and release binaries. The report names every binary it left out.
    #[arg(long, value_enum, value_name = "PLATFORM")]
    platform_tier: Option<PlatformTier>,
    /// Run the failed test binaries of this report once more, one at a time, inside the time the
    /// first run left of `--budget-secs`. It refuses when more than three binaries failed, when
    /// a build stage failed or when any other row failed. The report keeps both results; after a
    /// good retry the run is `PASS (retried: <names>)`. Give it the run's `--tmp-root` and
    /// `--require-all`.
    #[arg(long, value_name = "REPORT", requires = "budget_secs", conflicts_with_all = ["focus", "only", "rebaseline"])]
    retry_failed: Option<PathBuf>,
}

/// The test tiers of `--tier`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Tier {
    Release,
    Nightly,
}

/// Whether the perf tier stays out of this run: the release tier runs it only for `--only perf`.
fn release_skips_perf(tier: Option<Tier>, only: Option<&str>) -> bool {
    tier == Some(Tier::Release) && only != Some(report::PERF_AREA)
}

/// The platform tiers of `--platform-tier`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum PlatformTier {
    Windows,
}

impl PlatformTier {
    /// The tier's list name in `gate-allowances.json`.
    fn name(self) -> &'static str {
        match self {
            PlatformTier::Windows => "windows",
        }
    }
}

/// The checked `--tmp-root`, set once in `main`.
static TMP_ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();

/// `c` with its temporary files under the gate's `--tmp-root`, when there is one.
fn on_tmp_root(mut c: Command) -> Command {
    if let Some(Some(root)) = TMP_ROOT.get() {
        c.envs(rdy_qa::gate_steps::tmp_root_vars(root));
    }
    c
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

/// The run's one clock, from the start of `main`.
static CLOCK: OnceLock<Clock> = OnceLock::new();
/// The stage that was running when the run's budget was spent.
static SPENT_IN: OnceLock<String> = OnceLock::new();
/// The gate's start, and every timed child so far, for the report's `## Timing` table.
static GATE_START: OnceLock<Instant> = OnceLock::new();
static RECORDS: Mutex<Vec<StageRecord>> = Mutex::new(Vec::new());
/// Each test binary the gate started, and its seconds.
static BINARY_TIMES: Mutex<Vec<(String, f64)>> = Mutex::new(Vec::new());

/// Runs a command with bounded output under `deadline`, keeping the exit status and the two
/// streams apart. Past the deadline the child is ended and its output names the stage.
fn step_out(name: &str, mut cmd: Command, deadline: Duration) -> ChildOutput {
    eprintln!("==> {name}");
    if let Some(out) = budget_spent(name) {
        return out;
    }
    cmd.current_dir(root());
    let t = Instant::now();
    let out = run_stage(name, cmd, deadline);
    record(name, t.elapsed().as_secs_f64(), out.ok);
    note_spent(name, &out);
    out
}

/// What a child of `stage` returns instead of starting, once the run's budget is spent.
fn budget_spent(stage: &str) -> Option<ChildOutput> {
    let clock = CLOCK.get()?;
    clock.spent().then(|| {
        SPENT_IN.get_or_init(|| stage.to_string());
        not_started(stage)
    })
}

/// Names `stage` as the one the budget was spent in, when its child ran out of the run's time.
fn note_spent(stage: &str, out: &ChildOutput) {
    if out.timed_out && CLOCK.get().is_some_and(Clock::spent) {
        SPENT_IN.get_or_init(|| stage.to_string());
    }
}

/// Adds one timed child to the report's `## Timing` table.
fn record(label: &str, secs: f64, ok: bool) {
    if let (Some(start), Ok(mut r)) = (GATE_START.get(), RECORDS.lock()) {
        r.push(StageRecord {
            label: label.into(),
            secs,
            ok,
            ended_at: start.elapsed().as_secs_f64(),
        });
    }
}

/// The deadline of the next child: the time left on the run's clock.
fn left() -> Duration {
    CLOCK.get().map_or(UNBOUNDED, Clock::left)
}

/// Runs a command, returning (success, stdout + stderr combined).
fn step(name: &str, cmd: Command, deadline: Duration) -> (bool, String) {
    let out = step_out(name, cmd, deadline);
    (out.ok, out.combined())
}

fn git(args: &[&str]) -> Option<String> {
    let o = Command::new("git")
        .args(args)
        .current_dir(root())
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// The `rdy-qa` test directory: one test binary per area, `tests/<area>/main.rs`.
fn tests_dir() -> PathBuf {
    root().join("crates/rdy-qa/tests")
}

fn baseline_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("qa-baseline.json")
}

type Classes = BTreeMap<String, Baseline>;

/// An absent file is an empty map; a present but unreadable or unparseable file is an error
/// (and is never rewritten).
fn read_baselines() -> Result<Classes, String> {
    match std::fs::read_to_string(baseline_path()) {
        Ok(text) => report::parse_baselines(&text).map_err(|e| format!("qa-baseline.json: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Classes::new()),
        Err(e) => Err(format!("qa-baseline.json: {e}")),
    }
}

fn write_baselines(classes: &Classes) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(&serde_json::json!({ "classes": classes }))
        .expect("baseline json");
    std::fs::write(baseline_path(), text + "\n")
}

/// Runs the perf workload and compares it; returns (verdicts, note, ok).
fn perf_tier(
    rebaseline: bool,
    deep: bool,
    machine: &str,
    notes: &mut Vec<String>,
) -> (Vec<perf::Verdict>, String, bool) {
    let mut c = cargo(&["run", "-q", "-p", "rdy-qa", "--release", "--bin", "qa-perf"]);
    if deep {
        // the large tier (about 50k blocks, the `_50k` ops)
        c.env("RDY_QA_DEEP", "1");
    }
    let out = step_out("perf (qa-perf)", c, left());
    eprint!("{}", out.stderr);
    if !out.ok {
        let why = if out.timed_out {
            report::tail(&out.stderr, 1)
        } else {
            format!("qa-perf exited with {}", out.status_text())
        };
        return (vec![], why, false);
    }
    let stdout = out.stdout;
    let measures: Vec<Measure> = match stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
    {
        Some(Ok(m)) => m,
        _ => return (vec![], "qa-perf printed no measurements".into(), false),
    };

    if measures.is_empty() {
        return (vec![], "qa-perf measured nothing".into(), false);
    }
    let mut classes = match read_baselines() {
        Ok(c) => c,
        Err(e) => {
            return (
                vec![],
                format!("Baseline unusable, left untouched: {e}"),
                false,
            );
        }
    };
    let missing = !classes.contains_key(machine);
    let verdicts;
    let note;
    let decision = perf::tier_decision(missing, rebaseline);
    if decision == perf::TierDecision::NotCompared {
        // nothing is written: a machine class needs two runs within 10%, then --rebaseline
        verdicts = perf::not_compared(&measures);
        note = perf::not_compared_note(machine);
    } else if decision.writes_baseline() {
        verdicts = perf::compare(None, &measures, perf::Mode::Record);
        // the measured ops replace their entries and every other op is kept, so a typical-tier
        // rebaseline never deletes the `_50k` baseline
        let merged = perf::merge_baseline(classes.get(machine), machine, &measures);
        classes.insert(machine.to_string(), merged);
        if let Err(e) = write_baselines(&classes) {
            return (
                verdicts,
                format!("could not write the baseline: {e}"),
                false,
            );
        }
        note =
            format!("Rebaselined `{machine}` in crates/rdy-qa/qa-baseline.json (commit the diff).");
    } else {
        // a typical run does not measure the `_50k` ops: their absence is not drift
        let base = classes.get(machine).map(|b| {
            if deep {
                b.clone()
            } else {
                perf::typical_only(b)
            }
        });
        if let Some(b) = &base {
            notes.extend(report::baseline_drift(b, &measures));
        }
        verdicts = perf::compare(base.as_ref(), &measures, perf::Mode::Compare);
        note = format!("Compared against the baseline for `{machine}`.");
    }
    let ok = verdicts.iter().all(|v| v.pass);
    (verdicts, note, ok)
}

/// The test binaries of one run, by key, and the environment Cargo gives each one.
struct Launch<'a> {
    args: &'a Args,
    envs: &'a BTreeMap<PathBuf, AreaEnv>,
    tmp_root: Option<&'a Path>,
}

impl Launch<'_> {
    /// Runs the test binary `bin` directly, as `cargo test` would, for the report row `label`:
    /// Cargo's environment for it, `RDY_QA_REQUIRE_ALL` and `RDY_QA_DEEP` when the gate runs
    /// with them, and the temp root unless the binary is a real-disk binary.
    fn run(&self, bin: &TestBinary, label: &str) -> (ChildOutput, Duration) {
        let Some(env) = self.envs.get(&canonical(&bin.path)) else {
            let out = ChildOutput {
                stderr: format!(
                    "Cargo gave no environment for {} ({})",
                    bin.key(),
                    bin.path.display()
                ),
                ..ChildOutput::default()
            };
            return (out, Duration::ZERO);
        };
        let mut env = env.clone();
        if self.args.require_all {
            env.vars
                .insert(rdy_qa::skip::REQUIRE_ALL.to_string(), "1".to_string());
        }
        if self.args.deep {
            env.vars.insert("RDY_QA_DEEP".to_string(), "1".to_string());
        }
        if let Some(root) = self.tmp_root
            && !on_real_disk(&bin.package, &bin.name)
        {
            for (k, v) in rdy_qa::gate_steps::tmp_root_vars(root) {
                env.vars.insert(k.into(), v.to_string_lossy().into_owned());
            }
        }
        if let Some(out) = budget_spent(label) {
            return (out, Duration::ZERO);
        }
        let run = launch_area(&bin.path, &env, &[], label, left());
        record(label, run.elapsed.as_secs_f64(), run.out.ok);
        note_spent(label, &run.out);
        if let Ok(mut t) = BINARY_TIMES.lock() {
            t.push((bin.key(), run.elapsed.as_secs_f64()));
        }
        (run.out, run.elapsed)
    }
}

/// The allowances of this platform's test binaries, read once in `main`.
static ALLOWANCES: OnceLock<Allowances> = OnceLock::new();

/// The path of the allowances file.
fn allowances_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("gate-allowances.json")
}

/// The allowance of the binary `key` on this platform, in seconds; zero when it has none. The
/// binaries with the largest allowance start first, so the last one to start is a short one.
fn allowance(key: &str) -> f64 {
    ALLOWANCES
        .get()
        .and_then(|a| a.platforms.get(std::env::consts::OS))
        .and_then(|l| l.get(key))
        .copied()
        .unwrap_or(0.0)
}

/// Each test binary the gate started and its seconds, slowest first.
fn binary_times() -> Vec<(String, f64)> {
    let mut t = BINARY_TIMES.lock().map(|t| t.clone()).unwrap_or_default();
    t.sort_by(|a, b| b.1.total_cmp(&a.1));
    t
}

/// `path`, canonical when it can be.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// `--retry-failed`: runs the failed test binaries of the first run's report once more, one at
/// a time, on the clock's time left, and writes the report with both results: the first run's
/// files are kept beside it as `report.first.json` and `report.first.md`. Returns the exit code.
fn retry(args: &Args, path: &Path, first: &serde_json::Value, tmp_root: Option<&Path>) -> i32 {
    use rdy_qa::retry::{Retried, apply, markdown, targets};
    let spent = || CLOCK.get().is_some_and(Clock::spent);
    let targets = match targets(first) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("--retry-failed: {e}");
            return 2;
        }
    };
    eprintln!("==> retry: {}", targets.join(", "));
    let mut stages = Stages::new(false);
    let (gates, out) = build_steps(&mut stages, |l, c, _| step(l, c, left()));
    if let Some(g) = gates.iter().find(|g| !g.pass) {
        eprintln!("--retry-failed: {} failed: {}", g.name, g.detail);
        return gate_exit(false, spent());
    }
    let binaries = parse_test_binaries(&out);
    let envs: BTreeMap<PathBuf, AreaEnv> = match probe_envs(|c| step_out("env probe", c, left())) {
        Ok(found) => found
            .into_iter()
            .map(|(exe, env)| (canonical(&exe), env))
            .collect(),
        Err(e) => {
            eprintln!("--retry-failed: {e}");
            return gate_exit(false, spent());
        }
    };
    let launch = Launch {
        args,
        envs: &envs,
        tmp_root,
    };
    let mut rows: Vec<Retried> = vec![];
    for key in &targets {
        let row = match binaries.iter().find(|b| &b.key() == key) {
            Some(b) => {
                let (out, elapsed) = launch.run(b, &format!("retry {key}"));
                let run = report::parse_cargo_test(&out.combined());
                if !out.ok {
                    eprintln!("{}", report::tail(&out.combined(), 40));
                }
                Retried {
                    binary: key.clone(),
                    ok: out.ok && run.failed == 0,
                    exit: out.status_text(),
                    passed: run.passed,
                    failed: run.failed,
                    secs: elapsed.as_secs_f64(),
                }
            }
            None => Retried {
                binary: key.clone(),
                ok: false,
                exit: "not built".into(),
                passed: 0,
                failed: 0,
                secs: 0.0,
            },
        };
        eprintln!(
            "==> retry {key}: {} ({:.1} s)",
            if row.ok { "PASS" } else { "FAIL" },
            row.secs
        );
        rows.push(row);
    }
    let mut rep = first.clone();
    let pass = apply(&mut rep, &rows);
    let dir = path.parent().unwrap_or(Path::new("."));
    let md = dir.join("report.md");
    let first_md = std::fs::read_to_string(&md).unwrap_or_default();
    let written = std::fs::copy(path, dir.join("report.first.json"))
        .and_then(|_| std::fs::write(dir.join("report.first.md"), &first_md))
        .and_then(|_| {
            std::fs::write(
                path,
                serde_json::to_string_pretty(&rep).expect("report serializes"),
            )
        })
        .and_then(|_| std::fs::write(&md, markdown(&first_md, &rows, pass)));
    if let Err(e) = written {
        eprintln!("--retry-failed: could not write the report: {e}");
        return 2;
    }
    println!("{}", md.display());
    println!("{}", rdy_qa::retry::verdict(pass, &rows));
    gate_exit(pass, spent())
}

/// Each test binary's count of nightly-tier tests (`mod nightly`), from its `--list`, by key;
/// and one line for each listing that failed.
fn nightly_counts(binaries: &[TestBinary]) -> (BTreeMap<String, usize>, Vec<String>) {
    let outs = run_areas(
        binaries,
        |_| false,
        BINARY_JOBS,
        |b| {
            budget_spent("list nightly tests").unwrap_or_else(|| {
                run_stage(
                    "list nightly tests",
                    rdy_qa::gate_steps::list_command(&b.path, &root()),
                    left(),
                )
            })
        },
    );
    let mut counts = BTreeMap::new();
    let mut problems = vec![];
    for (b, out) in binaries.iter().zip(outs) {
        if out.ok {
            counts.insert(b.key(), rdy_qa::gate_steps::nightly_count(&out.stdout));
        } else {
            problems.push(format!(
                "nightly listing of {} failed: {}",
                b.key(),
                out.status_text()
            ));
        }
    }
    (counts, problems)
}

fn main() {
    // one clock for the whole run, the builds included: under `cargo qa` it starts with the
    // Cargo that built this gate
    let now = Instant::now();
    let start = rdy_qa::gate_steps::process_elapsed()
        .and_then(|e| now.checked_sub(e))
        .unwrap_or(now);
    let args = Args::parse();
    // a retry has the time that the first run left of the budget
    let first = args.retry_failed.as_deref().map(|path| {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("--retry-failed {}: {e}", path.display()))
            .and_then(|t| {
                serde_json::from_str::<serde_json::Value>(&t)
                    .map_err(|e| format!("--retry-failed {}: {e}", path.display()))
            });
        text.unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(2);
        })
    });
    let ceiling = args
        .ceiling_secs
        .or_else(|| args.budget_secs.map(|b| b * u64::from(CEILING_FACTOR)));
    let (target, ceiling) = match &first {
        Some(report) => (
            args.budget_secs
                .map(|b| rdy_qa::retry::time_left(b, report)),
            ceiling.map(|c| rdy_qa::retry::time_left(c, report)),
        ),
        None => (args.budget_secs, ceiling),
    };
    let clock = *CLOCK.get_or_init(|| {
        Clock::started(
            start,
            target.map(Duration::from_secs),
            ceiling.map(Duration::from_secs),
        )
    });
    if args.tier == Some(Tier::Nightly) {
        rdy_qa::gate_steps::set_nightly_tier();
    }
    if cfg!(windows)
        && let Err(e) = rdy_qa::gate_steps::step_aside()
    {
        eprintln!("could not move the running gate aside: {e}");
        std::process::exit(2);
    }
    if let Some(f) = &args.focus {
        let mut env = vec![];
        if args.require_all {
            env.push((rdy_qa::skip::REQUIRE_ALL, "1"));
        }
        if args.deep {
            env.push(("RDY_QA_DEEP", "1"));
        }
        std::process::exit(rdy_qa::focus::main(f, &root(), &env, run_bounded));
    }
    // the gate builds in the gate profile and finds its probe runner beside itself
    let own = std::env::current_exe()
        .ok()
        .and_then(|e| rdy_qa::gate_steps::profile_dir_of(&e));
    if own.as_deref() != Some(rdy_qa::gate_steps::GATE_PROFILE) {
        eprintln!(
            "qa-gate runs in the gate profile, not {}: start it with `cargo qa`",
            own.as_deref().unwrap_or("an unknown profile")
        );
        std::process::exit(2);
    }
    let tmp_root = match args
        .tmp_root
        .as_deref()
        .map(rdy_qa::gate_steps::check_tmp_root)
    {
        Some(Err(e)) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
        Some(Ok(dir)) => Some(dir),
        None => None,
    };
    TMP_ROOT.get_or_init(|| tmp_root.clone());
    let allowances = std::fs::read_to_string(allowances_path())
        .map_err(|e| format!("gate-allowances.json: {e}"))
        .and_then(|t| Allowances::parse(&t));
    let allowance_error = allowances.as_ref().err().cloned();
    ALLOWANCES.get_or_init(|| allowances.unwrap_or_default());
    GATE_START.get_or_init(|| start);
    let tier_keys: Option<BTreeSet<String>> = match args.platform_tier {
        None => None,
        Some(p) => match ALLOWANCES.get().map(|a| a.platform_tier(p.name())) {
            Some(Ok(keys)) => Some(keys),
            Some(Err(e)) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
            None => None,
        },
    };
    if let (Some(path), Some(report)) = (&args.retry_failed, &first) {
        std::process::exit(retry(&args, path, report, tmp_root.as_deref()));
    }
    if now > start {
        record("qa-gate build", (now - start).as_secs_f64(), true);
    }
    // A crashed run must not leave a stale verdict behind.
    for f in ["report.md", "report.json"] {
        let _ = std::fs::remove_file(root().join("target/qa").join(f));
    }
    let mut notes: Vec<String> = vec![];
    // The release tier leaves the perf tier to the nightly run, unless `--only perf` asks for it.
    let release_skips_perf = release_skips_perf(args.tier, args.only.as_deref());
    let mut plan = match report::plan(
        args.only.as_deref(),
        &report::test_areas(&tests_dir()),
        args.skip_perf || release_skips_perf,
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    // A platform tier runs only the rdy-qa areas its list names; the nightly run runs the rest.
    let (tier_files, mut outside_tier) =
        Allowances::split_tier_areas(&plan.files, tier_keys.as_ref());
    plan.files = tier_files;

    let machine = perf::machine_class();
    // a failed step skips the steps that need it, unless --all-stages
    let mut stages = Stages::new(args.all_stages);
    // Lints and the Python CI tests do not depend on the machine (apart from `cfg(windows)`
    // code, which a Windows run lints), so the release tier runs them on Linux only.
    let lint_here = args.tier != Some(Tier::Release) || std::env::consts::OS == "linux";
    // The Python CI tests need no build, so they run first, in the default fast gate and
    // `--only <area>` (the plan's checks), never in `--focus` or `--only perf`.
    let python = if plan.checks && lint_here {
        match rdy_qa::gate_steps::python_ci_step(std::env::consts::OS, |l, c, _| {
            step(l, on_tmp_root(c), left())
        }) {
            Ok(g) => Some(g),
            Err(line) => {
                eprintln!("{line}");
                notes.push(line);
                None
            }
        }
    } else {
        None
    };
    let mut gates: Vec<Gate> = python.into_iter().collect();
    if let Some(e) = allowance_error {
        gates.push(Gate::ran("gate allowances", false, e));
    }
    // the one build: the `rdy` binary, every crate's tests and every rdy-qa binary, in the
    // gate profile
    let (prerequisites, build_out) = build_steps(&mut stages, |l, c, _| step(l, c, left()));
    gates.extend(prerequisites);
    let build_ok = !gates.iter().any(|g| g.name == BUILD_STEP && !g.pass);
    let binaries = if build_ok {
        parse_test_binaries(&build_out)
    } else {
        vec![]
    };
    let mut areas: Vec<Area> = vec![];
    let mut seeds = vec![];

    // the stages after the prerequisites, and what each needs
    let fetch = "cargo fetch --locked";
    let build = BUILD_STEP;
    let clippy = GateStep::new("cargo clippy -D warnings", &[fetch], Duration::ZERO);
    let env_step = GateStep::new("test environment", &[build], Duration::ZERO);
    let workspace = GateStep::new(
        "cargo test --workspace --exclude rdy-qa",
        &[build, &clippy.id, &env_step.id],
        Duration::ZERO,
    );
    let discovery = GateStep::new("rdy-qa test binaries", &[build, &clippy.id], Duration::ZERO);
    let lib = GateStep::new(
        "rdy-qa unit tests",
        &[build, &clippy.id, &env_step.id],
        Duration::ZERO,
    );
    let perf_step = GateStep::new("perf", &[build], Duration::ZERO);
    let area_step = |binary: &str| {
        GateStep::new(
            format!("rdy-qa {binary}"),
            &[&discovery.id, &env_step.id],
            Duration::ZERO,
        )
    };

    let mut gate = |name: &str, ok: bool, detail: String| {
        gates.push(Gate::ran(name, ok, detail));
    };

    if plan.checks && lint_here {
        // one check per workspace member: a single command line is too long for Windows
        let fmt = fmt_step(|l, c, _| step(l, c, left()));
        gate(&fmt.name, fmt.pass, fmt.detail);

        match stages.blocked(&clippy) {
            Some(root) => gates.push(stages.skip(&clippy, root)),
            None => {
                let (ok, out) = step(
                    "clippy",
                    cargo(&[
                        "clippy",
                        "--workspace",
                        "--all-targets",
                        "--",
                        "-D",
                        "warnings",
                    ]),
                    left(),
                );
                stages.record(&clippy, ok);
                gates.push(Gate::ran(&clippy.id, ok, fail_tail(ok, &out)));
                if !ok {
                    eprintln!("{out}");
                }
            }
        }
    } else if plan.checks {
        notes.push(format!(
            "fmt, clippy and the Python CI tests: not run on {} in the release tier; the Linux job runs them",
            std::env::consts::OS
        ));
    }

    // Cargo's own environment for each test binary: Cargo runs every test target once through
    // the probe runner, which records it.
    let mut envs: BTreeMap<PathBuf, AreaEnv> = BTreeMap::new();
    let needs_env = plan.checks || plan.lib || !plan.files.is_empty();
    if needs_env {
        match stages.blocked(&env_step) {
            Some(root) => gates.push(stages.skip(&env_step, root)),
            None => {
                let probed = probe_envs(|c| step_out("env probe", c, left()));
                let ok = probed.is_ok();
                match probed {
                    Ok(found) => {
                        envs = found
                            .into_iter()
                            .map(|(exe, env)| (canonical(&exe), env))
                            .collect();
                        gates.push(Gate::ran(&env_step.id, true, String::new()));
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        gates.push(Gate::ran(&env_step.id, false, e));
                    }
                }
                stages.record(&env_step, ok);
            }
        }
    }
    let launch = Launch {
        args: &args,
        envs: &envs,
        tmp_root: tmp_root.as_deref(),
    };
    // A platform tier runs only the binaries its list names; the nightly run runs the rest.
    let in_tier = |key: &str| tier_keys.as_ref().is_none_or(|k| k.contains(key));

    // the workspace's test binaries, every crate's but rdy-qa's
    let mut ws_bins: Vec<&TestBinary> = vec![];
    let mut ws_ran = false;
    if plan.checks {
        match stages.blocked(&workspace) {
            Some(root) => gates.push(stages.skip(&workspace, root)),
            None => {
                ws_ran = true;
                for b in binaries.iter().filter(|b| b.package != "rdy-qa") {
                    if in_tier(&b.key()) {
                        ws_bins.push(b);
                    } else {
                        outside_tier.push(b.key());
                    }
                }
            }
        }
    }

    let mut gate = |name: &str, ok: bool, detail: String| {
        gates.push(Gate::ran(name, ok, detail));
    };

    // A test file without its `mod` line in its area's `main.rs` compiles and never runs.
    if plan.checks || !plan.files.is_empty() {
        let problems = report::layout_problems(&tests_dir());
        gate(
            "rdy-qa test layout",
            problems.is_empty(),
            problems.join("\n"),
        );
        // Unix-only code lives only in the seam modules (FW-XP1 D-1RXEA).
        let problems = rdy_qa::seams::seam_problems(&root());
        gate("unix seams", problems.is_empty(), problems.join("\n"));
    }

    // Cargo's artifact record of the build names every test executable. A binary it does not
    // name is missing, and its area fails.
    let qa_exes: Vec<&TestBinary> = binaries
        .iter()
        .filter(|b| b.package == "rdy-qa" && b.kind == "test")
        .collect();
    let mut discovery_ok = true;
    let mut discovery_ran = false;
    if plan.guarded || !plan.files.is_empty() {
        if let Some(root) = stages.blocked(&discovery) {
            discovery_ok = false;
            gates.push(stages.skip(&discovery, root));
        } else {
            discovery_ran = true;
            let names: Vec<String> = qa_exes.iter().map(|e| e.name.clone()).collect();
            let problems = report::missing_binaries(&plan.files, &names);
            discovery_ok = problems.is_empty();
            gates.push(Gate::ran(&discovery.id, discovery_ok, problems.join("\n")));
            stages.record(&discovery, discovery_ok);
        }
    }

    // The hardening suite; each area reports as one row (an area of two binaries, `engine` and
    // `engine_read`, sums them, and each binary keeps its own evidence row). After a failed
    // discovery each binary is a skipped row instead, unless --all-stages. The rows and areas
    // keep the plan's order however the binaries ran.
    enum Pending {
        Skipped(Gate),
        Missing(Option<Gate>),
        Run(Option<Gate>),
    }
    let mut pending = Vec::with_capacity(plan.files.len());
    let mut to_run: Vec<String> = vec![];
    for binary in &plan.files {
        let step_def = area_step(binary);
        if let Some(root) = stages.blocked(&step_def) {
            pending.push(Pending::Skipped(stages.skip(&step_def, root)));
            continue;
        }
        if !qa_exes.iter().any(|e| &e.name == binary) {
            pending.push(Pending::Missing(None));
            continue;
        }
        let mut pre = None;
        if binary == "release" {
            match rdy_qa::release_preflight::run(&root(), git(&["rev-parse", "--short", "HEAD"])) {
                Ok(rev) => {
                    pre = Some(Gate::ran(
                        "release preflight",
                        true,
                        format!("revision {rev}"),
                    ))
                }
                Err(e) => {
                    eprintln!("release preflight refused: {e}");
                    pending.push(Pending::Missing(Some(Gate::ran(
                        "release preflight",
                        false,
                        e,
                    ))));
                    continue;
                }
            }
        }
        to_run.push(binary.clone());
        pending.push(Pending::Run(pre));
    }

    // the rdy-qa unit tests
    let mut lib_ran = false;
    let lib_bin = binaries
        .iter()
        .find(|b| b.package == "rdy-qa" && b.kind == "lib");
    if plan.lib {
        if let Some(root) = stages.blocked(&lib) {
            gates.push(stages.skip(&lib, root));
        } else if lib_bin.is_none_or(|b| in_tier(&b.key())) {
            lib_ran = true;
        } else {
            outside_tier.push("rdy-qa/lib/rdy_qa".into());
        }
    }

    // Every test binary of the run shares one pool: the workspace's, the rdy-qa areas' and the
    // rdy-qa unit tests. The largest allowances start first, so the last one to start is a short
    // one; with none, the real-disk binaries, which are slow. A binary in SERIAL_AREAS runs alone
    // after the pool. The results come back in the order of the jobs.
    enum Job<'a> {
        Workspace(&'a TestBinary),
        Area(&'a str),
        Lib,
    }
    let job_key = |j: &Job| match j {
        Job::Workspace(b) => b.key(),
        Job::Area(a) => format!("rdy-qa/test/{a}"),
        Job::Lib => "rdy-qa/lib/rdy_qa".into(),
    };
    let mut jobs: Vec<Job> = ws_bins.iter().map(|b| Job::Workspace(b)).collect();
    jobs.extend(to_run.iter().map(|a| Job::Area(a)));
    if lib_ran {
        jobs.push(Job::Lib);
    }
    jobs.sort_by_key(|j| match j {
        Job::Workspace(b) => !on_real_disk(&b.package, &b.name),
        Job::Area(a) => !on_real_disk("rdy-qa", a),
        Job::Lib => true,
    });
    jobs.sort_by(|a, b| allowance(&job_key(b)).total_cmp(&allowance(&job_key(a))));
    let alone = |j: &Job| matches!(j, Job::Area(a) if SERIAL_AREAS.contains(a));
    let t = Instant::now();
    if !jobs.is_empty() {
        eprintln!(
            "==> test pool: {} binaries, {BINARY_JOBS} at a time",
            jobs.len()
        );
    }
    let heavy = |j: &Job| match j {
        Job::Workspace(b) => is_heavy(&b.package, &b.kind, &b.name),
        Job::Area(a) => is_heavy("rdy-qa", "test", a),
        Job::Lib => false,
    };
    let ran = run_areas_capped(&jobs, alone, BINARY_JOBS, heavy, HEAVY_JOBS, |j| match j {
        Job::Workspace(b) => launch.run(b, &format!("workspace {}", b.key())).0,
        Job::Area(binary) => {
            let id = area_step(binary).id;
            let exe = qa_exes
                .iter()
                .find(|e| e.name == *binary)
                .expect("only found binaries run");
            eprintln!("==> {id}");
            let (out, elapsed) = launch.run(exe, &id);
            eprintln!("==> {id}: {:.1} s", elapsed.as_secs_f64());
            out
        }
        Job::Lib => {
            eprintln!("==> {}", lib.id);
            match lib_bin {
                Some(exe) => launch.run(exe, &lib.id).0,
                None => ChildOutput {
                    stderr: "the build named no rdy-qa unit-test executable".into(),
                    ..ChildOutput::default()
                },
            }
        }
    });
    let pool_secs = t.elapsed().as_secs_f64();
    let jobs_empty = jobs.is_empty();
    if !jobs.is_empty() {
        record("test pool", pool_secs, ran.iter().all(|o| o.ok));
    }
    let mut failed_binaries: Vec<String> = vec![];
    let mut ws_out: Vec<(&TestBinary, ChildOutput)> = vec![];
    let mut area_out: BTreeMap<String, ChildOutput> = BTreeMap::new();
    let mut lib_out: Option<ChildOutput> = None;
    for (j, out) in jobs.iter().zip(ran) {
        if !out.ok {
            failed_binaries.push(job_key(j));
        }
        match j {
            Job::Workspace(b) => ws_out.push((b, out)),
            Job::Area(a) => {
                area_out.insert(a.to_string(), out);
            }
            Job::Lib => lib_out = Some(out),
        }
    }
    failed_binaries.sort();
    if !outside_tier.is_empty() {
        outside_tier.sort();
        notes.push(format!(
            "{} test binaries are outside the {} tier and did not run (the nightly run runs them): {}",
            outside_tier.len(),
            args.platform_tier.map_or("platform", PlatformTier::name),
            outside_tier.join(", ")
        ));
    }

    if ws_ran {
        // the report rows keep the binaries' key order
        ws_out.sort_by_key(|a| a.0.key());
        let mut text = String::new();
        let mut failed = vec![];
        for (b, out) in &ws_out {
            text.push_str(&out.combined());
            if !out.ok {
                failed.push(format!("{} ({})", b.key(), out.status_text()));
            }
            if out.timed_out {
                notes.push(report::tail(&out.stderr, 1));
            }
        }
        let ok = failed.is_empty() && !ws_out.is_empty();
        let run = report::parse_cargo_test(&text);
        seeds.extend(report::failing_seeds(&text));
        let mut detail = report::workspace_test_detail(ok, &run, &text);
        if !failed.is_empty() {
            detail += &format!("\nfailed binaries: {}", failed.join(", "));
        }
        let empty = report::ran_nothing(&run);
        stages.record(&workspace, ok && !empty);
        gates.push(Gate::ran(&workspace.id, ok && !empty, detail));
    }

    for (binary, item) in plan.files.iter().zip(pending) {
        let out = match item {
            Pending::Skipped(g) => {
                gates.push(g);
                continue;
            }
            Pending::Missing(pre) => {
                gates.extend(pre);
                report::add_missing(&mut areas, binary);
                continue;
            }
            Pending::Run(pre) => {
                gates.extend(pre);
                area_out.remove(binary).expect("one output per run")
            }
        };
        let text = out.combined();
        let run = report::parse_cargo_test(&text);
        seeds.extend(report::failing_seeds(&text));
        if !out.ok && run.failures.is_empty() {
            eprintln!("{text}");
        }
        if out.dropped > 0 {
            notes.push(format!(
                "`{binary}` output cut: {} bytes dropped from the front",
                out.dropped
            ));
        }
        if out.timed_out {
            notes.push(report::tail(&out.stderr, 1));
        }
        report::add_run_with_exit(&mut areas, binary, out.ok, &out.status_text(), run);
    }
    if !discovery_ok && (discovery_ran || args.all_stages) {
        // a binary that Cargo did not name has no run above; name it in its area
        for binary in &plan.files {
            if !areas
                .iter()
                .any(|a| a.binaries.iter().any(|b| &b.name == binary))
                && !gates.iter().any(|g| g.name == area_step(binary).id)
            {
                report::add_missing(&mut areas, binary);
            }
        }
    }

    if let Some(out) = lib_out {
        let text = out.combined();
        let run = report::parse_cargo_test(&text);
        seeds.extend(report::failing_seeds(&text));
        if !out.ok && run.failures.is_empty() {
            eprintln!("{text}");
        }
        if out.timed_out {
            notes.push(report::tail(&out.stderr, 1));
        }
        report::add_run_with_exit(&mut areas, "lib", out.ok, &out.status_text(), run);
    }

    // The tests must not have changed the build: a second build of the same command is fresh.
    if build_ok && (plan.checks || !plan.files.is_empty()) {
        let out = step_out("fresh check", discover_command(), left());
        let stale = if out.ok {
            stale_units(&out.stdout)
        } else {
            vec![format!("the build exited with {}", out.status_text())]
        };
        gates.push(Gate::ran(
            "the gate build stays fresh",
            stale.is_empty(),
            stale.join("\n"),
        ));
    }

    let (perf_verdicts, perf_note, perf_ok) = if !plan.perf {
        let why = if release_skips_perf {
            "Skipped: the release tier leaves perf to the nightly tier (`cargo qa --tier nightly`)."
        } else {
            "Skipped (`--skip-perf`)."
        };
        (vec![], why.to_string(), true)
    } else if let Some(root) = stages.blocked(&perf_step) {
        gates.push(stages.skip(&perf_step, root.clone()));
        (vec![], format!("SKIPPED (after {root} failed)"), false)
    } else {
        perf_tier(args.rebaseline, args.deep, &machine, &mut notes)
    };

    // Each executable lists its own tests, run directly (no shell); its stdout is the listing.
    let guarded = if plan.guarded && discovery_ran {
        eprintln!("==> list guarded tests");
        let dir = root().join("crates/rdy-qa");
        let exes: Vec<TestExe> = qa_exes
            .iter()
            .map(|b| TestExe {
                name: b.name.clone(),
                path: b.path.clone(),
            })
            .collect();
        let (guarded, problems) = list_guarded(&exes, &dir, |c| {
            budget_spent("list guarded tests")
                .unwrap_or_else(|| run_stage("list guarded tests", c, left()))
        });
        for p in problems {
            notes.push(format!("guarded-id listing failed: {p}"));
        }
        guarded
    } else {
        BTreeMap::new()
    };

    let dirty = git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    let mut mode = if args.deep { "deep" } else { "standard" }.to_string();
    if let Some(a) = &args.only {
        mode += &format!(", only {a}");
    }
    if args.all_stages {
        mode += ", all stages";
    }
    // In the release tier each test binary's time and each stage's time is held to its
    // allowance, and the report says how many tests the nightly tier holds.
    if args.tier == Some(Tier::Release)
        && !args.deep
        && let Some(a) = ALLOWANCES.get()
    {
        let (rows, note) = a.check(std::env::consts::OS, &binary_times());
        gates.extend(rows);
        notes.extend(note);
        let build: f64 = RECORDS
            .lock()
            .map(|r| {
                r.iter()
                    .filter(|r| r.label == "qa-gate build" || r.label == "build")
                    .map(|r| r.secs)
                    .sum()
            })
            .unwrap_or(0.0);
        if build_ok && !jobs_empty {
            gates.extend(a.check_stages(std::env::consts::OS, build, pool_secs));
        }
        notes.extend(a.nightly_note());
    }
    // In the nightly tier each binary's nightly-only count is the allowances file's.
    if args.tier == Some(Tier::Nightly)
        && build_ok
        && let Some(a) = ALLOWANCES.get()
    {
        let (counts, problems) = nightly_counts(&binaries);
        notes.extend(problems);
        let rows = a.check_nightly(&counts);
        let total: usize = counts.values().sum();
        gates.push(Gate::ran(
            "nightly-only counts",
            rows.is_empty(),
            format!("{total} nightly-only tests listed"),
        ));
        gates.extend(rows);
    }
    // A spent budget fails the run, whatever ran: the row names the stage it was spent in.
    let spent = SPENT_IN.get().cloned().or_else(|| {
        clock.spent().then(|| {
            RECORDS
                .lock()
                .ok()
                .and_then(|r| r.last().map(|r| r.label.clone()))
                .unwrap_or_else(|| "report".into())
        })
    });
    if let (Some(stage), Some(secs)) = (&spent, clock.budget_secs()) {
        gates.push(budget_row(secs, stage));
    } else if let Some((secs, target)) = clock.over_target() {
        gates.push(over_target_row(secs, target, &binary_times()));
    }
    let pass = report::overall(&gates, &areas, perf_ok);
    let rep = Report {
        revision: rdy_qa::release_preflight::recorded_revision(
            git(&["rev-parse", "--short", "HEAD"]).as_deref(),
            std::env::var("RDY_QA_SOURCE_REVISION").ok().as_deref(),
        )
        .unwrap_or_else(|| "unknown".into()),
        dirty,
        machine,
        mode,
        duration_secs: start.elapsed().as_secs_f64(),
        pass,
        gates,
        areas,
        perf: perf_verdicts,
        perf_note,
        failing_seeds: seeds,
        notes,
        guarded,
        os_excluded: report::os_excluded(),
        failed_binaries,
        timing: Timing {
            binaries: binary_times(),
            ..Timing::from_records(
                &RECORDS.lock().map(|r| r.clone()).unwrap_or_default(),
                std::env::var("RDY_QA_QUEUE_SECS")
                    .ok()
                    .and_then(|v| v.parse().ok()),
            )
        },
    };

    let dir = root().join("target/qa");
    std::fs::create_dir_all(&dir).expect("create target/qa");
    std::fs::write(dir.join("report.md"), report::markdown(&rep)).expect("write report.md");
    std::fs::write(dir.join("report.json"), report::json(&rep)).expect("write report.json");
    println!("{}", dir.join("report.md").display());
    println!("{}", if pass { "PASS" } else { "FAIL" });
    std::process::exit(gate_exit(pass, spent.is_some()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(std::iter::once("qa-gate").chain(args.iter().copied()))
    }

    #[test]
    fn tier4_the_gate_takes_the_workflow_flags() {
        let a = parse(&["--tier", "nightly"]).unwrap();
        assert_eq!(a.tier, Some(Tier::Nightly));
        assert_eq!(a.budget_secs, None, "the nightly tier needs no budget");
        let a = parse(&[
            "--tier",
            "release",
            "--budget-secs",
            "630",
            "--platform-tier",
            "windows",
        ])
        .unwrap();
        assert_eq!(a.platform_tier.map(PlatformTier::name), Some("windows"));
        let a = parse(&[
            "--tier",
            "release",
            "--budget-secs",
            "630",
            "--retry-failed",
            "target/qa/report.json",
        ])
        .unwrap();
        assert_eq!(
            a.retry_failed.as_deref(),
            Some(Path::new("target/qa/report.json"))
        );
        // the release tier needs its budget, and a retry runs on the same budget clock
        assert!(parse(&["--tier", "release"]).is_err());
        assert!(parse(&["--retry-failed", "target/qa/report.json"]).is_err());
        assert!(parse(&["--platform-tier", "plan9"]).is_err());
        // the budget is the target; the ceiling is the hard stop, twice the target by default
        let a = parse(&[
            "--tier",
            "release",
            "--budget-secs",
            "630",
            "--ceiling-secs",
            "900",
        ])
        .unwrap();
        assert_eq!((a.budget_secs, a.ceiling_secs), (Some(630), Some(900)));
        assert!(
            parse(&["--ceiling-secs", "900"]).is_err(),
            "a ceiling needs a target"
        );
        assert_eq!(CEILING_FACTOR, 2);
    }

    #[test]
    fn tier5_the_release_tier_leaves_perf_to_the_nightly_tier() {
        assert!(release_skips_perf(Some(Tier::Release), None));
        assert!(release_skips_perf(Some(Tier::Release), Some("cli")));
        assert!(!release_skips_perf(Some(Tier::Release), Some("perf")));
        assert!(!release_skips_perf(Some(Tier::Nightly), None));
        assert!(!release_skips_perf(None, None));
    }

    #[test]
    fn tier4_the_windows_tier_is_in_the_allowances_file() {
        let a = Allowances::parse(&std::fs::read_to_string(allowances_path()).unwrap()).unwrap();
        let keys = a.platform_tier(PlatformTier::Windows.name()).unwrap();
        for key in [
            "rdy-store/lib/rdy_store",
            "rdy-daemon/lib/rdy_daemon",
            "rdy-cli/bin/rdy",
            "rdy-qa/test/store",
            "rdy-qa/test/daemon",
            "rdy-qa/test/wc",
            "rdy-qa/test/release",
            "rdy-mcp/test/windows_contract",
        ] {
            assert!(keys.contains(key), "the windows tier lacks {key}");
        }
        for key in [
            "rdy-engine/lib/rdy_engine",
            "rdy-qa/test/render",
            "rdy-qa/test/views",
        ] {
            assert!(!keys.contains(key), "{key} runs on Windows at night only");
        }
        // every platform's stages fit 80 % of the gate line
        assert_eq!(a.stage_problems(), Vec::<String>::new());
    }
}
