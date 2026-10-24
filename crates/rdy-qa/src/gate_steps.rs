//! The gate's prerequisite steps: the locked dependency fetch, then the release build.
use crate::report::{self, Gate};
use std::process::Command;

pub fn cargo(args: &[&str]) -> Command {
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.args(args);
    c
}

pub fn fail_tail(ok: bool, out: &str) -> String {
    if ok {
        String::new()
    } else {
        report::tail(out, 20)
    }
}

/// Runs the prerequisite steps through `run` (label and command in, success and output back):
/// the locked fetch of every target's crates, then the release build. Both always run; a failed
/// step is a failed gate row, and its output goes to stderr.
pub fn build_steps(mut run: impl FnMut(&str, Command) -> (bool, String)) -> Vec<Gate> {
    let steps: [(&str, &str, &[&str]); 2] = [
        (
            "fetch locked dependencies",
            "cargo fetch --locked",
            &["fetch", "--locked"],
        ),
        (
            "build rdy-cli",
            "cargo build --release -p rdy-cli",
            &["build", "--release", "-p", "rdy-cli"],
        ),
    ];
    let mut gates = Vec::with_capacity(steps.len());
    for (label, name, args) in steps {
        let (ok, out) = run(label, cargo(args));
        if !ok {
            eprintln!("{out}");
        }
        gates.push(Gate {
            name: name.into(),
            pass: ok,
            detail: fail_tail(ok, &out),
        });
    }
    gates
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_of(c: &Command) -> Vec<String> {
        c.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn plz3_fetch_precedes_build() {
        let mut seen: Vec<(String, Vec<String>)> = vec![];
        let gates = build_steps(|label, cmd| {
            seen.push((label.to_string(), args_of(&cmd)));
            (true, String::new())
        });
        let want: Vec<(String, Vec<String>)> = vec![
            (
                "fetch locked dependencies".into(),
                vec!["fetch".into(), "--locked".into()],
            ),
            (
                "build rdy-cli".into(),
                ["build", "--release", "-p", "rdy-cli"]
                    .map(String::from)
                    .to_vec(),
            ),
        ];
        assert_eq!(seen, want);
        let names: Vec<&str> = gates.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(
            names,
            ["cargo fetch --locked", "cargo build --release -p rdy-cli"]
        );
        assert!(gates.iter().all(|g| g.pass));
    }

    #[test]
    fn plz3_fetch_failure_remains_a_failed_gate() {
        let mut labels: Vec<String> = vec![];
        let gates = build_steps(|label, _| {
            labels.push(label.to_string());
            if label == "fetch locked dependencies" {
                (false, "locked fetch failed".to_string())
            } else {
                (true, String::new())
            }
        });
        assert_eq!(labels, ["fetch locked dependencies", "build rdy-cli"]);
        assert_eq!(gates.len(), 2);
        assert!(!gates[0].pass);
        assert_eq!(gates[0].detail, "locked fetch failed");
        assert!(gates[1].pass);
        assert!(!report::overall(&gates, &[], true));
    }
}
