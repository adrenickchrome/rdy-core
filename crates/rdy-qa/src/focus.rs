//! `cargo qa --focus <crate|area>[::<filter>]`: one crate's tests or one rdy-qa area, with only
//! the builds they need. No fmt, no clippy, no workspace tests, no perf and no report file. A
//! run that matches no test fails, and the last line is the command that replays the run. Before
//! any build, the test hygiene check (`crate::test_hygiene`) reads the lines the branch adds to
//! test code, and a new fixed wait or chosen port fails the run.
use crate::gate_steps::{ChildOutput, cargo};
use crate::report::{self, TestRun};
use std::path::Path;
use std::process::Command;

/// What `--focus` selects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Focus {
    /// The tests of one workspace crate (`cargo test -p <name>`), in the dev profile that the
    /// gate's workspace tests use. `test` is one test target (`tests/<test>.rs`) when the first
    /// filter segment names one.
    Crate {
        name: String,
        test: Option<String>,
        filter: Option<String>,
    },
    /// One rdy-qa area: its test binaries (`tests/<area>/main.rs`; the `engine` area is two), in
    /// the release profile, after the release CLI build that its tests start.
    Area {
        name: String,
        binaries: Vec<String>,
        filter: Option<String>,
    },
}

/// One step of a focused run: the cargo arguments, and whether it is a test run whose counts
/// make up the selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusStep {
    pub label: String,
    pub args: Vec<String>,
    pub tests: bool,
}

impl FocusStep {
    pub fn command(&self) -> Command {
        let args: Vec<&str> = self.args.iter().map(String::as_str).collect();
        cargo(&args)
    }
}

impl Focus {
    /// Reads `<crate|area>[::<filter>]`. `areas` are the rdy-qa test binaries, `crates` the
    /// workspace members, and `has_test(crate, name)` says whether a crate has the test target
    /// `name`. An area name wins over a crate name; `rdy-qa` itself is run by area.
    pub fn parse(
        arg: &str,
        areas: &[String],
        crates: &[String],
        has_test: impl Fn(&str, &str) -> bool,
    ) -> Result<Focus, String> {
        let (name, rest) = match arg.split_once("::") {
            Some((n, r)) => (n, Some(r)),
            None => (arg, None),
        };
        let rest = rest.filter(|r| !r.is_empty()).map(String::from);
        let binaries: Vec<String> = areas
            .iter()
            .filter(|b| report::area_of(b) == name)
            .cloned()
            .collect();
        if !binaries.is_empty() {
            return Ok(Focus::Area {
                name: name.into(),
                binaries,
                filter: rest,
            });
        }
        if name == "rdy-qa" {
            return Err(format!(
                "focus an rdy-qa area, not the crate: {}",
                known_areas(areas).join(", ")
            ));
        }
        if crates.iter().any(|c| c == name) {
            let (test, filter) = match rest {
                Some(r) => {
                    let (first, more) = match r.split_once("::") {
                        Some((f, m)) => (f.to_string(), Some(m.to_string())),
                        None => (r.clone(), None),
                    };
                    if has_test(name, &first) {
                        (Some(first), more.filter(|m| !m.is_empty()))
                    } else {
                        (None, Some(r))
                    }
                }
                None => (None, None),
            };
            return Ok(Focus::Crate {
                name: name.into(),
                test,
                filter,
            });
        }
        Err(format!(
            "no crate or area named `{name}`; crates: {}; areas: {}",
            crates.join(", "),
            known_areas(areas).join(", ")
        ))
    }

    fn filter(&self) -> Option<&str> {
        match self {
            Focus::Crate { filter, .. } | Focus::Area { filter, .. } => filter.as_deref(),
        }
    }
}

fn known_areas(areas: &[String]) -> Vec<&str> {
    let mut v: Vec<&str> = areas.iter().map(|a| report::area_of(a)).collect();
    v.sort();
    v.dedup();
    v
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

/// The steps of a focused run, in order. A crate builds only its own tests; an area builds the
/// release CLI (its tests start `rdy`), then its test binaries.
pub fn focus_steps(f: &Focus) -> Vec<FocusStep> {
    let filter = |args: &mut Vec<String>| {
        if let Some(x) = f.filter() {
            args.push("--".into());
            args.push(x.into());
        }
    };
    match f {
        Focus::Crate { name, test, .. } => {
            let mut args = strings(&["test", "-p", name]);
            match test {
                Some(t) => args.extend(strings(&["--test", t])),
                // every binary runs: cargo stops at the first failing one without this
                None => args.push("--no-fail-fast".into()),
            }
            filter(&mut args);
            vec![FocusStep {
                label: format!("test {name}"),
                args,
                tests: true,
            }]
        }
        Focus::Area { binaries, .. } => {
            let mut steps = vec![FocusStep {
                label: "build rdy-cli".into(),
                args: strings(&["build", "--release", "-p", "rdy-cli"]),
                tests: false,
            }];
            for b in binaries {
                let mut args = strings(&["test", "-p", "rdy-qa", "--release", "--test", b]);
                filter(&mut args);
                steps.push(FocusStep {
                    label: format!("rdy-qa {b}"),
                    args,
                    tests: true,
                });
            }
            steps
        }
    }
}

/// The command that runs the same selection again.
pub fn replay_command(f: &Focus) -> String {
    let mut s = String::from("cargo qa --focus ");
    match f {
        Focus::Crate { name, test, .. } => {
            s += name;
            if let Some(t) = test {
                s += "::";
                s += t;
            }
        }
        Focus::Area { name, .. } => s += name,
    }
    if let Some(x) = f.filter() {
        s += "::";
        s += x;
    }
    s
}

/// The result of a focused run: the lines to print (the last is the replay command).
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub pass: bool,
    pub lines: Vec<String>,
}

impl Outcome {
    pub fn exit_code(&self) -> i32 {
        if self.pass { 0 } else { 1 }
    }
}

/// Runs the steps through `run` (label and command in, output back). A failed build step stops
/// the run; every test step runs. The run passes only when every step passed and at least one
/// test was selected.
pub fn run(f: &Focus, mut run: impl FnMut(&str, Command) -> ChildOutput) -> Outcome {
    let mut lines = vec![];
    let mut pass = true;
    let mut total = TestRun::default();
    for step in focus_steps(f) {
        let out = run(&step.label, step.command());
        let text = out.combined();
        if !step.tests {
            if !out.ok {
                lines.push(format!(
                    "{}: `cargo {}` failed ({})",
                    step.label,
                    step.args.join(" "),
                    out.status_text()
                ));
                lines.push(report::tail(&text, 20));
                pass = false;
                break;
            }
            continue;
        }
        let r = report::parse_cargo_test(&text);
        lines.push(format!(
            "{}: {} passed, {} failed, {} ignored, {} skipped ({})",
            step.label,
            r.passed,
            r.failed,
            r.ignored,
            r.skipped.len(),
            out.status_text()
        ));
        for name in &r.failures {
            lines.push(format!("  failed: {name}"));
        }
        if !out.ok {
            pass = false;
            if r.failures.is_empty() {
                lines.push(report::tail(&text, 20));
            }
        }
        total.passed += r.passed;
        total.failed += r.failed;
        total.ignored += r.ignored;
    }
    let ran = !report::ran_nothing(&total);
    if pass && !ran {
        pass = false;
        lines.push(format!(
            "FAIL: no test matched `{}`",
            f.filter().unwrap_or("(no filter)")
        ));
    } else {
        lines.push(if pass { "PASS" } else { "FAIL" }.into());
    }
    lines.push(format!("replay: {}", replay_command(f)));
    Outcome { pass, lines }
}

/// The workspace members: the directories under `crates/` that hold a `Cargo.toml`.
pub fn workspace_crates(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root.join("crates"))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().join("Cargo.toml").is_file())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// `cargo qa --focus <arg>` from the workspace `root`: parses, runs each step with its output on
/// stderr as it runs, prints the outcome to stdout and returns the exit code (2: a bad focus).
pub fn main(
    arg: &str,
    root: &Path,
    env: &[(&str, &str)],
    mut run_step: impl FnMut(Command) -> ChildOutput,
) -> i32 {
    let areas = report::test_areas(&root.join("crates/rdy-qa/tests"));
    let crates = workspace_crates(root);
    let has_test = |c: &str, t: &str| {
        root.join("crates")
            .join(c)
            .join("tests")
            .join(format!("{t}.rs"))
            .is_file()
    };
    let f = match Focus::parse(arg, &areas, &crates, has_test) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    // first, and with no build: a new fixed wait or chosen port in changed test code fails the
    // run (crate::test_hygiene says why)
    match crate::test_hygiene::run(root) {
        Ok(true) => {}
        Ok(false) => {
            println!("FAIL");
            println!("replay: {}", replay_command(&f));
            return 1;
        }
        Err(e) => {
            println!(
                "test hygiene: could not read the changes ({e}); set {} to a base commit",
                crate::test_hygiene::BASE_VAR
            );
            println!("FAIL");
            return 1;
        }
    }
    let out = run(&f, |label, mut c| {
        eprintln!("==> {label}");
        c.current_dir(root);
        for (k, v) in env {
            c.env(k, v);
        }
        let o = run_step(c);
        eprint!("{}", o.combined());
        o
    });
    for l in &out.lines {
        println!("{l}");
    }
    out.exit_code()
}
