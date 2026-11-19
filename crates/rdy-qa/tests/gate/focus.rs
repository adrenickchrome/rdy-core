//! `cargo qa --focus`: one crate or one area runs without the workspace build, clippy or perf.
use rdy_qa::focus::{self, Focus, FocusStep};
use rdy_qa::gate_steps::ChildOutput;
use std::process::Command;

fn areas() -> Vec<String> {
    ["daemon", "engine", "engine_read", "mcp"]
        .map(String::from)
        .to_vec()
}

fn crates() -> Vec<String> {
    ["rdy-cli", "rdy-engine", "rdy-qa", "rdy-store"]
        .map(String::from)
        .to_vec()
}

/// No crate has a test target named like the filter.
fn no_targets(_: &str, _: &str) -> bool {
    false
}

fn parse(arg: &str) -> Focus {
    Focus::parse(arg, &areas(), &crates(), no_targets).expect(arg)
}

fn lines(steps: &[FocusStep]) -> Vec<String> {
    steps.iter().map(|s| s.args.join(" ")).collect()
}

/// A libtest run of `n` passing tests (0: the filter matched nothing).
fn libtest(n: usize) -> ChildOutput {
    ChildOutput {
        ok: true,
        code: Some(0),
        stdout: format!(
            "running {n} tests\ntest result: ok. {n} passed; 0 failed; 0 ignored; 0 measured; 7 filtered out\n"
        ),
        ..ChildOutput::default()
    }
}

#[test]
fn focus_engine_crate_builds_no_cli() {
    let f = parse("rdy-engine");
    assert_eq!(
        f,
        Focus::Crate {
            name: "rdy-engine".into(),
            test: None,
            filter: None
        }
    );
    let steps = lines(&focus::focus_steps(&f));
    assert_eq!(steps, ["test -p rdy-engine --no-fail-fast"]);
    for s in &steps {
        assert!(!s.contains("rdy-cli"), "{s}");
        assert!(!s.contains("clippy"), "{s}");
        assert!(!s.contains("--workspace"), "{s}");
        assert!(!s.contains("qa-perf"), "{s}");
    }
    // a filter that names a test target runs that target alone; the rest filters by name
    let f = Focus::parse("rdy-engine::scopes::sc1", &areas(), &crates(), |c, t| {
        c == "rdy-engine" && t == "scopes"
    })
    .unwrap();
    assert_eq!(
        lines(&focus::focus_steps(&f)),
        ["test -p rdy-engine --test scopes -- sc1"]
    );
    let f = parse("rdy-engine::scope_share");
    assert_eq!(
        lines(&focus::focus_steps(&f)),
        ["test -p rdy-engine --no-fail-fast -- scope_share"]
    );
    // the gate crate is run by area
    let e = Focus::parse("rdy-qa", &areas(), &crates(), no_targets).unwrap_err();
    assert!(e.contains("area"), "{e}");
    let e = Focus::parse("nope", &areas(), &crates(), no_targets).unwrap_err();
    assert!(
        e.contains("nope") && e.contains("rdy-engine") && e.contains("daemon"),
        "{e}"
    );
}

#[test]
fn focus_daemon_area_builds_cli() {
    let f = parse("daemon::ls5");
    let steps = focus::focus_steps(&f);
    assert_eq!(
        lines(&steps),
        [
            "build --release -p rdy-cli",
            "test -p rdy-qa --release --test daemon -- ls5",
        ]
    );
    assert!(!steps[0].tests && steps[1].tests);
    let all = lines(&steps).join("\n");
    assert!(!all.contains("clippy") && !all.contains("qa-perf") && !all.contains("--workspace"));
    // an area of two binaries runs both
    assert_eq!(
        lines(&focus::focus_steps(&parse("engine"))),
        [
            "build --release -p rdy-cli",
            "test -p rdy-qa --release --test engine",
            "test -p rdy-qa --release --test engine_read",
        ]
    );
}

#[test]
fn focus_empty_selection_fails() {
    let f = parse("rdy-engine::no_such_test_zzz");
    let out = focus::run(&f, |_: &str, _: Command| libtest(0));
    assert_ne!(out.exit_code(), 0);
    let text = out.lines.join("\n");
    assert!(text.contains("no test matched"), "{text}");
    assert!(!text.contains("PASS"), "{text}");
    // a matching run passes
    let out = focus::run(&f, |_: &str, _: Command| libtest(3));
    assert_eq!(out.exit_code(), 0, "{:?}", out.lines);
    // a failed build runs no tests and fails
    let mut seen = vec![];
    let out = focus::run(&parse("daemon"), |label: &str, _: Command| {
        seen.push(label.to_string());
        ChildOutput {
            stderr: "error: could not compile".into(),
            ..ChildOutput::default()
        }
    });
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_ne!(out.exit_code(), 0);
    assert!(!out.lines.join("\n").contains("PASS"));
}

#[test]
fn focus_prints_replay() {
    for arg in [
        "rdy-engine",
        "rdy-engine::no_such_test_zzz",
        "daemon::ls5_a_proposal",
        "engine",
    ] {
        let f = parse(arg);
        assert_eq!(focus::replay_command(&f), format!("cargo qa --focus {arg}"));
        for n in [0, 2] {
            let out = focus::run(&f, |_: &str, _: Command| libtest(n));
            let last = out.lines.last().expect("output");
            assert!(last.starts_with("replay: cargo qa --focus"), "{last}");
            assert_eq!(*last, format!("replay: cargo qa --focus {arg}"));
        }
    }
    let f = Focus::parse("rdy-engine::scopes::sc1", &areas(), &crates(), |_, t| {
        t == "scopes"
    })
    .unwrap();
    assert_eq!(
        focus::replay_command(&f),
        "cargo qa --focus rdy-engine::scopes::sc1"
    );
}
