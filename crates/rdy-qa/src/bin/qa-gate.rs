//! `cargo qa`: the full regression and hardening gate, with a report in `target/qa/`.
use clap::Parser;
use rdy_qa::gate_steps::{
    Allowances, AreaEnv, BINARY_JOBS, BUILD_STEP, ChildOutput, Clock, GateStep, SERIAL_AREAS,
    Stages, TestBinary, TestExe, UNBOUNDED, budget_row, build_steps, cargo, discover_command,
    fail_tail, fmt_step, gate_exit, launch_area, list_guarded, not_started, on_real_disk,
    parse_test_binaries, probe_envs, run_areas, run_bounded, run_stage, stale_units,
};
use rdy_qa::perf::{self, Baseline, Measure};
use rdy_qa::report::{self, Area, Gate, Report, StageRecord, Timing};
use std::collections::BTreeMap;
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
    /// run on Linux only, and each test binary's time is checked against its allowance in
    /// `gate-allowances.json`.
    #[arg(long, value_enum)]
    tier: Option<Tier>,
    /// The seconds the whole run may take, the builds included; required with `--tier release`.
    /// Each child's deadline is the time left. When it is spent the gate stops the running
    /// children, adds a failed `BUDGET` row that names the stage and exits 30.
    #[arg(long, value_name = "SECONDS", required_if_eq("tier", "release"))]
    budget_secs: Option<u64>,
}

/// The test tiers of `--tier`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Tier {
    Release,
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

fn main() {
    // one clock for the whole run, the builds included: under `cargo qa` it starts with the
    // Cargo that built this gate
    let now = Instant::now();
    let start = rdy_qa::gate_steps::process_elapsed()
        .and_then(|e| now.checked_sub(e))
        .unwrap_or(now);
    let args = Args::parse();
    let clock =
        *CLOCK.get_or_init(|| Clock::started(start, args.budget_secs.map(Duration::from_secs)));
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
    if now > start {
        record("qa-gate build", (now - start).as_secs_f64(), true);
    }
    // A crashed run must not leave a stale verdict behind.
    for f in ["report.md", "report.json"] {
        let _ = std::fs::remove_file(root().join("target/qa").join(f));
    }
    let mut notes: Vec<String> = vec![];
    let plan = match report::plan(
        args.only.as_deref(),
        &report::test_areas(&tests_dir()),
        args.skip_perf,
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

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

    if plan.checks {
        match stages.blocked(&workspace) {
            Some(root) => gates.push(stages.skip(&workspace, root)),
            None => {
                // every crate's test binaries but rdy-qa's, four at a time
                eprintln!("==> workspace tests");
                let t = Instant::now();
                // the largest allowances start first; with none, the real-disk binaries,
                // which are the slowest
                let mut mine: Vec<&TestBinary> =
                    binaries.iter().filter(|b| b.package != "rdy-qa").collect();
                mine.sort_by_key(|b| !on_real_disk(&b.package, &b.name));
                mine.sort_by(|a, b| allowance(&b.key()).total_cmp(&allowance(&a.key())));
                let outputs = run_areas(
                    &mine,
                    |_| false,
                    BINARY_JOBS,
                    |b| launch.run(b, &format!("workspace {}", b.key())).0,
                );
                let mut text = String::new();
                let mut failed_binaries = vec![];
                for (b, out) in mine.iter().zip(&outputs) {
                    text.push_str(&out.combined());
                    if !out.ok {
                        failed_binaries.push(format!("{} ({})", b.key(), out.status_text()));
                    }
                    if out.timed_out {
                        notes.push(report::tail(&out.stderr, 1));
                    }
                }
                let ok = failed_binaries.is_empty() && !mine.is_empty();
                record("workspace tests", t.elapsed().as_secs_f64(), ok);
                let run = report::parse_cargo_test(&text);
                seeds.extend(report::failing_seeds(&text));
                let mut detail = report::workspace_test_detail(ok, &run, &text);
                if !failed_binaries.is_empty() {
                    detail += &format!("\nfailed binaries: {}", failed_binaries.join(", "));
                }
                let empty = report::ran_nothing(&run);
                stages.record(&workspace, ok && !empty);
                gates.push(Gate::ran(&workspace.id, ok && !empty, detail));
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

    // Every binary outside SERIAL_AREAS shares the workers; each one in it runs alone after them.
    let alone = |b: &String| SERIAL_AREAS.contains(&b.as_str());
    // the slowest binaries start first, so the last one to start is a short one; the results
    // come back in the plan's order
    let mut order: Vec<usize> = (0..to_run.len()).collect();
    order.sort_by(|&a, &b| {
        let key = |i: usize| format!("rdy-qa/test/{}", to_run[i]);
        allowance(&key(b)).total_cmp(&allowance(&key(a)))
    });
    let ordered: Vec<String> = order.iter().map(|&i| to_run[i].clone()).collect();
    let t = Instant::now();
    let ran = run_areas(&ordered, alone, BINARY_JOBS, |binary| {
        let id = area_step(binary).id;
        let exe = qa_exes
            .iter()
            .find(|e| &e.name == binary)
            .expect("only found binaries run");
        eprintln!("==> {id}");
        let (out, elapsed) = launch.run(exe, &id);
        eprintln!("==> {id}: {:.1} s", elapsed.as_secs_f64());
        out
    });
    let mut outputs: Vec<Option<ChildOutput>> = (0..to_run.len()).map(|_| None).collect();
    for (&i, out) in order.iter().zip(ran) {
        outputs[i] = Some(out);
    }
    let outputs: Vec<ChildOutput> = outputs
        .into_iter()
        .map(|o| o.expect("every binary ran"))
        .collect();
    if !to_run.is_empty() {
        record("rdy-qa areas", t.elapsed().as_secs_f64(), true);
    }
    let mut outputs = to_run.iter().zip(outputs);
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
                let (ran, out) = outputs.next().expect("one output per run");
                debug_assert_eq!(ran, binary);
                out
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

    if plan.lib {
        if let Some(root) = stages.blocked(&lib) {
            gates.push(stages.skip(&lib, root));
        } else {
            eprintln!("==> {}", lib.id);
            let out = match binaries
                .iter()
                .find(|b| b.package == "rdy-qa" && b.kind == "lib")
            {
                Some(exe) => launch.run(exe, &lib.id).0,
                None => ChildOutput {
                    stderr: "the build named no rdy-qa unit-test executable".into(),
                    ..ChildOutput::default()
                },
            };
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
        (vec![], "Skipped (`--skip-perf`).".to_string(), true)
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
    // In the release tier each test binary's time is held to its allowance.
    if args.tier == Some(Tier::Release)
        && !args.deep
        && let Some(a) = ALLOWANCES.get()
    {
        let (rows, note) = a.check(std::env::consts::OS, &binary_times());
        gates.extend(rows);
        notes.extend(note);
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
