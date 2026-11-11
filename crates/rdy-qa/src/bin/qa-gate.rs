//! `cargo qa`: the full regression and hardening gate, with a report in `target/qa/`.
use clap::Parser;
use rdy_qa::gate_steps::{
    ChildOutput, build_steps, cargo, discover_command, discovered, fail_tail, fmt_step,
    list_guarded, run_bounded,
};
use rdy_qa::perf::{self, Baseline, Measure};
use rdy_qa::report::{self, Area, Gate, Report};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

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
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

/// Runs a command with bounded output, keeping the exit status and the two streams apart.
fn step_out(name: &str, mut cmd: Command) -> ChildOutput {
    eprintln!("==> {name}");
    cmd.current_dir(root());
    run_bounded(cmd)
}

/// Runs a command, returning (success, stdout + stderr combined).
fn step(name: &str, cmd: Command) -> (bool, String) {
    let out = step_out(name, cmd);
    (out.ok, out.combined())
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
    eprintln!("==> perf (qa-perf)");
    let mut c = cargo(&["run", "-q", "-p", "rdy-qa", "--release", "--bin", "qa-perf"]);
    if deep {
        // the large tier (about 50k blocks, the `_50k` ops)
        c.env("RDY_QA_DEEP", "1");
    }
    let out = c.current_dir(root()).stderr(Stdio::inherit()).output();
    let out = match out {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            return (vec![], format!("qa-perf exited with {}", o.status), false);
        }
        Err(e) => return (vec![], format!("could not run qa-perf: {e}"), false),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
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
    let mut gates = build_steps(step);
    let mut areas: Vec<Area> = vec![];
    let mut seeds = vec![];

    let mut gate = |name: &str, ok: bool, detail: String| {
        gates.push(Gate {
            name: name.into(),
            pass: ok,
            detail,
        });
    };

    if plan.checks {
        // one check per workspace member: a single command line is too long for Windows
        let fmt = fmt_step(step);
        gate(&fmt.name, fmt.pass, fmt.detail);

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
        );
        gate("cargo clippy -D warnings", ok, fail_tail(ok, &out));
        if !ok {
            eprintln!("{out}");
        }

        let (ok, out) = step(
            "workspace tests",
            with_require_all(cargo(&report::WORKSPACE_TEST_ARGS), &args),
        );
        let run = report::parse_cargo_test(&out);
        seeds.extend(report::failing_seeds(&out));
        let detail = report::workspace_test_detail(ok, &run, &out);
        let empty = report::ran_nothing(&run);
        gate(
            "cargo test --workspace --exclude rdy-qa",
            ok && !empty,
            detail,
        );
    }

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
    if plan.guarded || !plan.files.is_empty() {
        let out = step_out("discover rdy-qa test binaries", discover_command());
        match discovered(&out) {
            Ok(found) => {
                exes = found;
                let names: Vec<String> = exes.iter().map(|e| e.name.clone()).collect();
                let problems = report::missing_binaries(&plan.files, &names);
                discovery_ok = problems.is_empty();
                gate("rdy-qa test binaries", discovery_ok, problems.join("\n"));
            }
            Err(e) => {
                discovery_ok = false;
                eprintln!("{e}");
                gate("rdy-qa test binaries", false, e);
            }
        }
        if out.dropped > 0 {
            notes.push(format!(
                "discovery output cut: {} bytes dropped from the front",
                out.dropped
            ));
        }
    }

    // The hardening suite, one test binary at a time; each area reports as one row (an area
    // of two binaries, `engine` and `engine_read`, sums them, and each binary keeps its own
    // evidence row).
    for binary in &plan.files {
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
        let out = step_out(&format!("rdy-qa {binary}"), c);
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
        report::add_run_with_exit(&mut areas, binary, out.ok, &out.status_text(), run);
    }
    if !discovery_ok {
        // a binary that Cargo did not name has no run above; name it in its area
        for binary in &plan.files {
            if !areas
                .iter()
                .any(|a| a.binaries.iter().any(|b| &b.name == binary))
            {
                report::add_missing(&mut areas, binary);
            }
        }
    }

    if plan.lib {
        let out = step_out(
            "rdy-qa unit tests",
            with_require_all(
                cargo(&["test", "-p", "rdy-qa", "--release", "--lib"]),
                &args,
            ),
        );
        let text = out.combined();
        let run = report::parse_cargo_test(&text);
        seeds.extend(report::failing_seeds(&text));
        if !out.ok && run.failures.is_empty() {
            eprintln!("{text}");
        }
        report::add_run_with_exit(&mut areas, "lib", out.ok, &out.status_text(), run);
    }

    let (perf_verdicts, perf_note, perf_ok) = if plan.perf {
        perf_tier(args.rebaseline, args.deep, &machine, &mut notes)
    } else {
        (vec![], "Skipped (`--skip-perf`).".to_string(), true)
    };

    // Each executable lists its own tests, run directly (no shell); its stdout is the listing.
    let guarded = if plan.guarded {
        eprintln!("==> list guarded tests");
        let dir = root().join("crates/rdy-qa");
        let (guarded, problems) = list_guarded(&exes, &dir, run_bounded);
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
