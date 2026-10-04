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
    /// Run only this area's test files (model, store, schema, engine, io, mcp, cli, render, ...).
    #[arg(long, value_name = "AREA")]
    only: Option<String>,
    /// Skip the perf-regression tier.
    #[arg(long)]
    skip_perf: bool,
    /// Rewrite this machine class's perf baseline (after an intended performance change).
    #[arg(long)]
    rebaseline: bool,
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

fn test_files() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root().join("crates/rdy-qa/tests"))
        .expect("tests dir")
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            (p.extension()? == "rs").then(|| p.file_stem()?.to_str().map(str::to_string))?
        })
        .collect();
    v.sort();
    v
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
    machine: &str,
    notes: &mut Vec<String>,
) -> (Vec<perf::Verdict>, String, bool) {
    eprintln!("==> perf (qa-perf)");
    let out = cargo(&["run", "-q", "-p", "rdy-qa", "--release", "--bin", "qa-perf"])
        .current_dir(root())
        .stderr(Stdio::inherit())
        .output();
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
    if missing || rebaseline {
        verdicts = perf::compare(None, &measures);
        classes.insert(
            machine.to_string(),
            Baseline {
                machine: machine.to_string(),
                ops: measures.iter().map(|m| (m.op.clone(), m.clone())).collect(),
            },
        );
        if let Err(e) = write_baselines(&classes) {
            return (
                verdicts,
                format!("could not write the baseline: {e}"),
                false,
            );
        }
        note = if missing {
            format!(
                "No baseline for `{machine}`: recorded one in crates/rdy-qa/qa-baseline.json (commit it)."
            )
        } else {
            format!("Rebaselined `{machine}` in crates/rdy-qa/qa-baseline.json (commit the diff).")
        };
    } else {
        let base = classes.get(machine);
        if let Some(b) = base {
            notes.extend(report::baseline_drift(b, &measures));
        }
        verdicts = perf::compare(base, &measures);
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
    let files = test_files();
    let areas_known: std::collections::BTreeSet<&str> =
        files.iter().map(|f| report::area_of(f)).collect();
    if let Some(a) = &args.only
        && !areas_known.contains(a.as_str())
    {
        eprintln!("unknown area `{a}`; known: {areas_known:?}");
        std::process::exit(2);
    }

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
        cargo(&["test", "--workspace", "--exclude", "rdy-qa"]),
    );
    let run = report::parse_cargo_test(&out);
    seeds.extend(report::failing_seeds(&out));
    let mut detail = format!(
        "{} passed, {} failed, {} ignored",
        run.passed, run.failed, run.ignored
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

    // The hardening suite, one test binary at a time so each area reports on its own.
    for stem in files
        .iter()
        .filter(|f| args.only.as_deref().is_none_or(|a| report::area_of(f) == a))
    {
        let mut c = cargo(&["test", "-p", "rdy-qa", "--release", "--test", stem]);
        if args.deep {
            c.env("RDY_QA_DEEP", "1");
        }
        let (ok, out) = step(&format!("rdy-qa {stem}"), c);
        let run = report::parse_cargo_test(&out);
        seeds.extend(report::failing_seeds(&out));
        if !ok && run.failures.is_empty() {
            eprintln!("{out}");
        }
        let name = report::area_of(stem).to_string();
        match areas.iter_mut().find(|a| a.name == name) {
            Some(a) => {
                a.passed += run.passed;
                a.failed += run.failed;
                a.ignored += run.ignored;
                a.ok &= ok;
                a.failures.extend(run.failures);
            }
            None => areas.push(Area {
                name,
                passed: run.passed,
                failed: run.failed,
                ignored: run.ignored,
                ok,
                failures: run.failures,
            }),
        }
    }

    let (ok, out) = step(
        "rdy-qa unit tests",
        cargo(&["test", "-p", "rdy-qa", "--release", "--lib"]),
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
        ok,
        failures: run.failures,
    });

    let (perf_verdicts, perf_note, perf_ok) = if args.skip_perf {
        (vec![], "Skipped (`--skip-perf`).".to_string(), true)
    } else {
        perf_tier(args.rebaseline, &machine, &mut notes)
    };

    // The `Running ...` headers go to stderr: merge the streams in order for the area parse.
    let mut list_cmd = Command::new("sh");
    list_cmd.args(["-c", "cargo test -p rdy-qa --release -- --list 2>&1"]);
    let (_, list) = step("list guarded tests", list_cmd);

    let dirty = git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    let mut mode = if args.deep { "deep" } else { "standard" }.to_string();
    if let Some(a) = &args.only {
        mode += &format!(", only {a}");
    }
    let pass =
        gates.iter().all(|g| g.pass) && areas.iter().all(|a| a.ok && a.failed == 0) && perf_ok;
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
        guarded: report::guarded_ids(&list),
    };

    let dir = root().join("target/qa");
    std::fs::create_dir_all(&dir).expect("create target/qa");
    std::fs::write(dir.join("report.md"), report::markdown(&rep)).expect("write report.md");
    std::fs::write(dir.join("report.json"), report::json(&rep)).expect("write report.json");
    println!("{}", dir.join("report.md").display());
    println!("{}", if pass { "PASS" } else { "FAIL" });
    std::process::exit(if pass { 0 } else { 1 });
}
