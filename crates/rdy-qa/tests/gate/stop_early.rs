//! The gate stops at the first broken stage: a failed step skips the steps that need it, a child
//! past its deadline is ended (TERM, then KILL) and the report names the stage, and
//! `--all-stages` still runs every step.

#[path = "../../src/platform/child_fixture.rs"]
mod child_fixture;

use rdy_qa::gate_steps::{GateStep, run_stage, run_steps};
use rdy_qa::report::{self, Gate, RowState};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// This test executable started again as a child fixture in `mode` (`exit:<code>`, `hang`), so
/// the same bytes run on every platform.
fn child(mode: &str) -> Command {
    let exe = std::env::current_exe().expect("current_exe");
    let mut c = Command::new(exe);
    c.args(["--exact", &child_fixture::entry_selector(), "--nocapture"])
        .env(child_fixture::MODE_ENV, mode)
        .stdin(Stdio::null());
    c
}

fn step(id: &str, after: &[&str], deadline: Duration) -> GateStep {
    GateStep::new(id, after, deadline)
}

/// A plan whose build fails: two test steps need it, one of them through the other, and the
/// format check needs nothing.
fn failing_plan() -> Vec<GateStep> {
    let long = Duration::from_secs(120);
    vec![
        step("build", &[], long),
        step("fmt", &[], long),
        step("workspace tests", &["build"], long),
        step("rdy-qa engine", &["workspace tests"], long),
    ]
}

fn mode_of(id: &str) -> &'static str {
    match id {
        "build" => "exit:1",
        "fmt" => "exit:0",
        // a test step that would hold the gate for its whole deadline if it ran
        _ => "hang",
    }
}

#[test]
fn failed_build_skips_tests() {
    let mut failed_at = None;
    let mut ran = vec![];
    let gates = run_steps(&failing_plan(), false, |s| {
        ran.push(s.id.clone());
        let out = run_stage(&s.id, child(mode_of(&s.id)), s.deadline);
        if !out.ok && failed_at.is_none() {
            failed_at = Some(Instant::now());
        }
        (out.ok, out.combined())
    });
    let done = Instant::now();
    assert_eq!(
        ran,
        ["build", "fmt"],
        "the steps that need the build never start"
    );
    let states: Vec<(&str, bool, &RowState)> = gates
        .iter()
        .map(|g| (g.name.as_str(), g.pass, &g.state))
        .collect();
    let after_build = RowState::Skipped {
        after: "build".into(),
    };
    assert_eq!(
        states,
        [
            ("build", false, &RowState::Ran),
            ("fmt", true, &RowState::Ran),
            ("workspace tests", false, &after_build),
            // a step skipped through another names the step that failed
            ("rdy-qa engine", false, &after_build),
        ]
    );
    assert_eq!(
        report::gate_result(&gates[2]),
        "SKIPPED (after build failed)"
    );
    assert_eq!(report::gate_result(&gates[0]), "FAIL");
    assert_eq!(report::gate_result(&gates[1]), "PASS");
    // the gate's verdict is a failure, and it comes within a second of the failed build
    assert!(!report::overall(&gates, &[], true));
    let since = done - failed_at.expect("the build failed");
    assert!(
        since < Duration::from_secs(1),
        "{since:?} after the failure"
    );
}

#[test]
fn deadline_kills_child() {
    let start = Instant::now();
    let plan = [step("rdy-qa daemon", &[], Duration::from_secs(1))];
    let gates = run_steps(&plan, false, |s| {
        let out = run_stage(&s.id, child("hang"), s.deadline);
        assert!(out.timed_out, "the child passed its deadline");
        assert!(!out.ok);
        (out.ok, out.combined())
    });
    let took = start.elapsed();
    // TERM ends a child that does not refuse it, long before the 10 s grace and its own 30 s
    assert!(took < Duration::from_secs(9), "took {took:?}");
    let g: &Gate = &gates[0];
    assert!(!g.pass);
    assert_eq!(
        g.detail.lines().last(),
        Some("deadline 1s passed in stage rdy-qa daemon"),
        "{}",
        g.detail
    );
}

#[test]
fn all_stages_runs_everything() {
    let mut ran = vec![];
    let gates = run_steps(&failing_plan(), true, |s| {
        ran.push(s.id.clone());
        let mode = if s.id == "build" { "exit:1" } else { "exit:0" };
        let out = run_stage(&s.id, child(mode), s.deadline);
        (out.ok, out.combined())
    });
    assert_eq!(
        ran,
        ["build", "fmt", "workspace tests", "rdy-qa engine"],
        "--all-stages runs every step after a failure"
    );
    assert!(gates.iter().all(|g| g.state == RowState::Ran));
    let passed: Vec<bool> = gates.iter().map(|g| g.pass).collect();
    assert_eq!(passed, [false, true, true, true]);
    assert!(!report::overall(&gates, &[], true));
}
