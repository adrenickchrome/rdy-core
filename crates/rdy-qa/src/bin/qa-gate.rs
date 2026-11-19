//! `cargo qa`: the full regression and hardening gate, with a report in `target/qa/`.
use clap::Parser;
use rdy_qa::gate_steps::{
    ChildOutput, GateStep, StageKind, Stages, build_steps, cargo, default_deadline,
    discover_command, discovered, fail_tail, fmt_step, list_guarded, run_stage,
};
use rdy_qa::perf::{self, Baseline, Measure};
use rdy_qa::report::{self, Area, Gate, Report};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
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
    /// Run every stage even after a failure. By default a failed step skips the steps that need
    /// it (each shows `SKIPPED (after <step> failed)`), so a broken build does not run the tests.
    #[arg(long)]
    all_stages: bool,
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

/// Runs a command with bounded output under `deadline`, keeping the exit status and the two
/// streams apart. Past the deadline the child is ended and its output names the stage.
fn step_out(name: &str, mut cmd: Command, deadline: Duration) -> ChildOutput {
    eprintln!("==> {name}");
    cmd.current_dir(root());
    run_stage(name, cmd, deadline)
}

/// Runs a command, returning (success, stdout + stderr combined).
fn step(name: &str, cmd: Command, deadline: Duration) -> (bool, String) {
    let out = step_out(name, cmd, deadline);
    (out.ok, out.combined())
}

/// The default deadline of a child of `kind` on this OS.
fn deadline(kind: StageKind) -> Duration {
    default_deadline(kind, std::env::consts::OS)
}

/// Sets `RDY_QA_REQUIRE_ALL=1` on a test command when the gate runs with `--require-all`.
fn with_require_all(mut c: Command, args: &Args) -> Command {
    if args.require_all {
        c.env(rdy_qa::skip::REQUIRE_ALL, "1");
    }
    c
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
    let out = step_out("perf (qa-perf)", c, deadline(StageKind::Perf));
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

fn main() {
    let args = Args::parse();
    if cfg!(windows)
        && let Err(e) = rdy_qa::gate_steps::step_aside()
    {
        eprintln!("could not move the running gate aside: {e}");
        std::process::exit(2);
    }
    let start = Instant::now();
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
    let mut gates = build_steps(&mut stages, step);
    let mut areas: Vec<Area> = vec![];
    let mut seeds = vec![];

    // the stages after the prerequisites, and what each needs
    let fetch = "cargo fetch --locked";
    let build = "cargo build --release -p rdy-cli";
    let clippy = GateStep::new(
        "cargo clippy -D warnings",
        &[fetch],
        deadline(StageKind::Clippy),
    );
    let workspace = GateStep::new(
        "cargo test --workspace --exclude rdy-qa",
        &[build, &clippy.id],
        deadline(StageKind::WorkspaceTests),
    );
    let discovery = GateStep::new(
        "rdy-qa test binaries",
        &[build, &clippy.id],
        deadline(StageKind::Discover),
    );
    let lib = GateStep::new(
        "rdy-qa unit tests",
        &[build, &clippy.id],
        deadline(StageKind::Lib),
    );
    let perf_step = GateStep::new("perf", &[build], deadline(StageKind::Perf));
    let area_step = |binary: &str| {
        GateStep::new(
            format!("rdy-qa {binary}"),
            &[&discovery.id],
            deadline(StageKind::Area),
        )
    };

    let mut gate = |name: &str, ok: bool, detail: String| {
        gates.push(Gate::ran(name, ok, detail));
    };

    if plan.checks {
        // one check per workspace member: a single command line is too long for Windows
        let fmt = fmt_step(step);
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
                    clippy.deadline,
                );
                stages.record(&clippy, ok);
                gates.push(Gate::ran(&clippy.id, ok, fail_tail(ok, &out)));
                if !ok {
                    eprintln!("{out}");
                }
            }
        }

        match stages.blocked(&workspace) {
            Some(root) => gates.push(stages.skip(&workspace, root)),
            None => {
                let (ok, out) = step(
                    "workspace tests",
                    with_require_all(cargo(&report::WORKSPACE_TEST_ARGS), &args),
                    workspace.deadline,
                );
                let run = report::parse_cargo_test(&out);
                seeds.extend(report::failing_seeds(&out));
                let detail = report::workspace_test_detail(ok, &run, &out);
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

    // Cargo's artifact record names every test executable. A binary it does not name is missing,
    // and its area fails.
    let mut exes = vec![];
    let mut discovery_ok = true;
    let mut discovery_ran = false;
    if plan.guarded || !plan.files.is_empty() {
        if let Some(root) = stages.blocked(&discovery) {
            discovery_ok = false;
            gates.push(stages.skip(&discovery, root));
        } else {
            discovery_ran = true;
            let out = step_out(
                "discover rdy-qa test binaries",
                discover_command(),
                discovery.deadline,
            );
            match discovered(&out) {
                Ok(found) => {
                    exes = found;
                    let names: Vec<String> = exes.iter().map(|e| e.name.clone()).collect();
                    let problems = report::missing_binaries(&plan.files, &names);
                    discovery_ok = problems.is_empty();
                    gates.push(Gate::ran(&discovery.id, discovery_ok, problems.join("\n")));
                }
                Err(e) => {
                    discovery_ok = false;
                    eprintln!("{e}");
                    gates.push(Gate::ran(&discovery.id, false, e));
                }
            }
            stages.record(&discovery, discovery_ok);
            if out.dropped > 0 {
                notes.push(format!(
                    "discovery output cut: {} bytes dropped from the front",
                    out.dropped
                ));
            }
        }
    }

    // The hardening suite, one test binary at a time; each area reports as one row (an area
    // of two binaries, `engine` and `engine_read`, sums them, and each binary keeps its own
    // evidence row). After a failed discovery each binary is a skipped row instead, unless
    // --all-stages.
    for binary in &plan.files {
        let step_def = area_step(binary);
        if let Some(root) = stages.blocked(&step_def) {
            gates.push(stages.skip(&step_def, root));
            continue;
        }
        if discovery_ok && !exes.iter().any(|e| &e.name == binary) {
            // not reachable after the check above; kept so a binary is never silently skipped
            report::add_missing(&mut areas, binary);
            continue;
        }
        let mut c = with_require_all(
            cargo(&["test", "-p", "rdy-qa", "--release", "--test", binary]),
            &args,
        );
        if args.deep {
            c.env("RDY_QA_DEEP", "1");
        }
        let out = step_out(&step_def.id, c, step_def.deadline);
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
            let out = step_out(
                &lib.id,
                with_require_all(
                    cargo(&["test", "-p", "rdy-qa", "--release", "--lib"]),
                    &args,
                ),
                lib.deadline,
            );
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
        let (guarded, problems) = list_guarded(&exes, &dir, |c| {
            run_stage("list guarded tests", c, deadline(StageKind::Listing))
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
    let pass = report::overall(&gates, &areas, perf_ok);
    let rep = Report {
        revision: git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into()),
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
    };

    let dir = root().join("target/qa");
    std::fs::create_dir_all(&dir).expect("create target/qa");
    std::fs::write(dir.join("report.md"), report::markdown(&rep)).expect("write report.md");
    std::fs::write(dir.join("report.json"), report::json(&rep)).expect("write report.json");
    println!("{}", dir.join("report.md").display());
    println!("{}", if pass { "PASS" } else { "FAIL" });
    std::process::exit(if pass { 0 } else { 1 });
}
