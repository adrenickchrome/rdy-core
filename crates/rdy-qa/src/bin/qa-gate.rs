//! `cargo qa`: the full regression and hardening gate, with a report in `target/qa/`.
use clap::Parser;
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
    /// Narrow the run to one area. `perf`: the release build and the perf tier only. Any other
    /// area (model, store, schema, engine, io, mcp, cli, render, ...): the release build, fmt,
    /// clippy, the workspace tests, that area's rdy-qa test binary (`tests/<area>/main.rs`), the
    /// rdy-qa unit tests and the perf tier (unless --skip-perf).
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

/// Runs a command, returning (success, stdout + stderr combined).
fn step(name: &str, mut cmd: Command) -> (bool, String) {
    eprintln!("==> {name}");
    cmd.current_dir(root());
    match cmd.output() {
        Ok(o) => {
            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.success(), text)
        }
        Err(e) => (false, format!("could not run: {e}")),
    }
}

fn cargo(args: &[&str]) -> Command {
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.args(args);
    c
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

fn fail_tail(ok: bool, out: &str) -> String {
    if ok {
        String::new()
    } else {
        report::tail(out, 20)
    }
}

fn main() {
    let args = Args::parse();
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
    let mut gates = vec![];
    let mut areas: Vec<Area> = vec![];
    let mut seeds = vec![];

    let mut gate = |name: &str, ok: bool, detail: String| {
        gates.push(Gate {
            name: name.into(),
            pass: ok,
            detail,
        });
    };

    let (ok, out) = step(
        "build rdy-cli",
        cargo(&["build", "--release", "-p", "rdy-cli"]),
    );
    gate("cargo build --release -p rdy-cli", ok, fail_tail(ok, &out));
    if !ok {
        eprintln!("{out}");
    }

    if plan.checks {
        let (ok, out) = step("fmt", cargo(&["fmt", "--all", "--check"]));
        gate("cargo fmt --all --check", ok, fail_tail(ok, &out));
        if !ok {
            eprintln!("{out}");
        }

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
            with_require_all(
                cargo(&["test", "--workspace", "--exclude", "rdy-qa"]),
                &args,
            ),
        );
        let run = report::parse_cargo_test(&out);
        seeds.extend(report::failing_seeds(&out));
        let mut detail = format!(
            "{} passed, {} failed, {} ignored, {} skipped",
            run.passed,
            run.failed,
            run.ignored,
            run.skipped.len()
        );
        if !run.failures.is_empty() {
            detail += &format!("; failing: {}", run.failures.join(", "));
        }
        if !ok {
            detail += &format!("\n{}", report::tail(&out, 20));
        }
        let empty = report::ran_nothing(&run);
        if empty {
            detail += " (no tests ran)";
        }
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

    // The hardening suite, one test binary at a time; each area reports as one row (an area
    // of two binaries, `engine` and `engine_read`, sums them).
    for binary in &plan.files {
        let mut c = with_require_all(
            cargo(&["test", "-p", "rdy-qa", "--release", "--test", binary]),
            &args,
        );
        if args.deep {
            c.env("RDY_QA_DEEP", "1");
        }
        let (ok, out) = step(&format!("rdy-qa {binary}"), c);
        let run = report::parse_cargo_test(&out);
        seeds.extend(report::failing_seeds(&out));
        if !ok && run.failures.is_empty() {
            eprintln!("{out}");
        }
        report::add_run(&mut areas, binary, ok, run);
    }

    if plan.lib {
        let (ok, out) = step(
            "rdy-qa unit tests",
            with_require_all(
                cargo(&["test", "-p", "rdy-qa", "--release", "--lib"]),
                &args,
            ),
        );
        let run = report::parse_cargo_test(&out);
        seeds.extend(report::failing_seeds(&out));
        if !ok && run.failures.is_empty() {
            eprintln!("{out}");
        }
        areas.push(Area {
            name: "lib".into(),
            passed: run.passed,
            failed: run.failed,
            ignored: run.ignored,
            skipped: run.skipped.len(),
            skip_lines: run.skipped,
            ok,
            failures: run.failures,
            panics: run.panics,
        });
    }

    let (perf_verdicts, perf_note, perf_ok) = if plan.perf {
        perf_tier(args.rebaseline, args.deep, &machine, &mut notes)
    } else {
        (vec![], "Skipped (`--skip-perf`).".to_string(), true)
    };

    let guarded = if plan.guarded {
        // The `Running ...` headers go to stderr: merge the streams in order for the area parse.
        let mut list_cmd = Command::new("sh");
        list_cmd.args([
            "-c",
            "${CARGO:-cargo} test -p rdy-qa --release -- --list 2>&1",
        ]);
        let (list_ok, list) = step("list guarded tests", list_cmd);
        if !list_ok {
            notes.push(format!(
                "guarded-id listing failed: {}",
                report::tail(&list, 5)
            ));
        }
        report::guarded_ids(&list)
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
    };

    let dir = root().join("target/qa");
    std::fs::create_dir_all(&dir).expect("create target/qa");
    std::fs::write(dir.join("report.md"), report::markdown(&rep)).expect("write report.md");
    std::fs::write(dir.join("report.json"), report::json(&rep)).expect("write report.json");
    println!("{}", dir.join("report.md").display());
    println!("{}", if pass { "PASS" } else { "FAIL" });
    std::process::exit(if pass { 0 } else { 1 });
}
