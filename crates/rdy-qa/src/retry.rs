//! `cargo qa --retry-failed <report.json>`: one retry of the failed test binaries of a gate run,
//! on the same tree, inside the time that is left on the run's budget. The retry runs only those
//! binaries, one at a time. The report keeps both results: the first run's rows stay as they
//! were, and a `retried` list holds the second results. After a good retry the run is
//! `PASS (retried: <names>)`.
use crate::gate_steps::{BUDGET_ROW, BUILD_STEP};
use serde::Serialize;
use serde_json::Value;

/// The most failed binaries a retry takes. More is not a flake: the run stays failed.
pub const MAX_RETRIED: usize = 3;

/// The rows of the build stages. When one of them failed there is nothing to retry.
pub const BUILD_ROWS: [&str; 4] = [
    "cargo fetch --locked",
    BUILD_STEP,
    "test environment",
    "rdy-qa test binaries",
];

/// The workspace row: it fails when a workspace binary fails, so a retry of that binary covers it.
pub const WORKSPACE_ROW: &str = "cargo test --workspace --exclude rdy-qa";

/// The second result of one retried binary.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Retried {
    /// The binary key, `<package>/<kind>/<name>`.
    pub binary: String,
    pub ok: bool,
    /// The exit status as text (`exit 0`, `exit 101`, ...).
    pub exit: String,
    pub passed: usize,
    pub failed: usize,
    pub secs: f64,
}

/// The failed binaries of the first run's report that a retry runs, or why it refuses: the run
/// passed, a build stage failed, the budget was spent, more than [`MAX_RETRIED`] binaries failed,
/// no binary failed, or a row that no binary explains failed too.
pub fn targets(report: &Value) -> Result<Vec<String>, String> {
    if report["pass"] == true {
        return Err("the run passed: there is nothing to retry".into());
    }
    let failed: Vec<String> = report["failed_binaries"]
        .as_array()
        .ok_or("the report names no failed binaries (`failed_binaries`)")?
        .iter()
        .filter_map(|b| b.as_str().map(String::from))
        .collect();
    let failed_rows: Vec<&str> = report["gates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|g| g["pass"] != true)
        .filter_map(|g| g["name"].as_str())
        .collect();
    if let Some(row) = failed_rows.iter().find(|r| BUILD_ROWS.contains(r)) {
        return Err(format!(
            "a build stage failed ({row}): there is nothing to retry"
        ));
    }
    if failed_rows.contains(&BUDGET_ROW) {
        return Err("the run's budget is spent: there is no time to retry".into());
    }
    if failed.len() > MAX_RETRIED {
        return Err(format!(
            "{} binaries failed, more than {MAX_RETRIED}: {}",
            failed.len(),
            failed.join(", ")
        ));
    }
    if failed.is_empty() {
        return Err(format!(
            "no test binary failed; the run failed on: {}",
            failed_rows.join(", ")
        ));
    }
    let explained = |row: &str| {
        row == WORKSPACE_ROW
            || ["OVER ALLOWANCE: ", "NO ALLOWANCE: "].iter().any(|p| {
                row.strip_prefix(p)
                    .is_some_and(|k| failed.iter().any(|f| f == k))
            })
    };
    let other: Vec<&str> = failed_rows.into_iter().filter(|r| !explained(r)).collect();
    if !other.is_empty() {
        return Err(format!(
            "the retry runs failed binaries only, and these rows failed too: {}",
            other.join(", ")
        ));
    }
    Ok(failed)
}

/// The seconds a retry has: the run's budget less the first run's duration.
pub fn time_left(budget_secs: u64, report: &Value) -> u64 {
    let spent = report["duration_secs"].as_f64().unwrap_or(f64::MAX).ceil();
    (budget_secs as f64 - spent).max(0.0) as u64
}

/// The verdict text of a run: `PASS`, `FAIL`, or with a retry `PASS (retried: <names>)`.
pub fn verdict(pass: bool, retried: &[Retried]) -> String {
    let mark = if pass { "PASS" } else { "FAIL" };
    if retried.is_empty() {
        mark.into()
    } else {
        let names: Vec<&str> = retried.iter().map(|r| r.binary.as_str()).collect();
        format!("{mark} (retried: {})", names.join(", "))
    }
}

/// Adds the retry's results to the first run's report and sets its verdict. The run passes when
/// every retried binary passed. Returns the new verdict's pass.
pub fn apply(report: &mut Value, retried: &[Retried]) -> bool {
    let pass = !retried.is_empty() && retried.iter().all(|r| r.ok);
    report["pass"] = pass.into();
    report["verdict"] = verdict(pass, retried).into();
    report["retried"] = serde_json::to_value(retried).expect("retried rows serialize");
    pass
}

/// The first run's Markdown report with the retry's verdict and a `## Retry` table.
pub fn markdown(first: &str, retried: &[Retried], pass: bool) -> String {
    let v = verdict(pass, retried);
    let mut out = String::with_capacity(first.len() + 512);
    for line in first.lines() {
        if line.starts_with("# rdy QA gate: ") {
            out.push_str(&format!("# rdy QA gate: {v}"));
        } else if line.starts_with("- verdict: ") {
            out.push_str(&format!("- verdict: **{v}**"));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out.push_str("\n## Retry\n\nThe failed binaries ran once more on the same tree, one at a time. The rows above are the first results.\n\n| Binary | Result | Exit | Passed | Failed | Seconds |\n|---|---|---|---|---|---|\n");
    for r in retried {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {:.1} |\n",
            r.binary,
            if r.ok { "PASS" } else { "FAIL" },
            r.exit,
            r.passed,
            r.failed,
            r.secs
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn failed_run(binaries: &[&str], rows: &[&str]) -> Value {
        let gates: Vec<Value> = rows
            .iter()
            .map(|r| json!({"name": r, "pass": false}))
            .chain([json!({"name": "unix seams", "pass": true})])
            .collect();
        json!({"pass": false, "duration_secs": 400.2, "failed_binaries": binaries, "gates": gates})
    }

    fn retried(binary: &str, ok: bool) -> Retried {
        Retried {
            binary: binary.into(),
            ok,
            exit: if ok { "exit 0" } else { "exit 101" }.into(),
            passed: 3,
            failed: usize::from(!ok),
            secs: 2.5,
        }
    }

    #[test]
    fn rtry1_a_retry_takes_up_to_three_failed_binaries() {
        let r = failed_run(
            &["rdy-qa/test/cli", "rdy-store/test/sync"],
            &[WORKSPACE_ROW],
        );
        assert_eq!(
            targets(&r).unwrap(),
            ["rdy-qa/test/cli", "rdy-store/test/sync"]
        );
        // an allowance row of a failed binary is that binary's
        let r = failed_run(&["rdy-qa/test/cli"], &["OVER ALLOWANCE: rdy-qa/test/cli"]);
        assert_eq!(targets(&r).unwrap(), ["rdy-qa/test/cli"]);
        let four = failed_run(&["a/test/1", "a/test/2", "a/test/3", "a/test/4"], &[]);
        assert!(
            targets(&four)
                .unwrap_err()
                .contains("4 binaries failed, more than 3")
        );
    }

    #[test]
    fn rtry1_a_retry_refuses_a_build_failure_a_spent_budget_and_other_rows() {
        let r = failed_run(&[], &[BUILD_STEP]);
        assert!(targets(&r).unwrap_err().starts_with("a build stage failed"));
        let r = failed_run(&["rdy-qa/test/cli"], &["test environment"]);
        assert!(targets(&r).unwrap_err().starts_with("a build stage failed"));
        let r = failed_run(&["rdy-qa/test/cli"], &[BUDGET_ROW]);
        assert!(targets(&r).unwrap_err().contains("budget is spent"));
        let r = failed_run(&["rdy-qa/test/cli"], &["cargo clippy -D warnings"]);
        assert!(
            targets(&r)
                .unwrap_err()
                .contains("cargo clippy -D warnings")
        );
        let r = failed_run(&[], &["unix seams"]);
        assert!(
            targets(&r)
                .unwrap_err()
                .starts_with("no test binary failed")
        );
        let mut r = failed_run(&[], &[]);
        r["pass"] = true.into();
        assert!(targets(&r).unwrap_err().contains("nothing to retry"));
    }

    #[test]
    fn rtry2_a_good_retry_passes_and_keeps_both_results() {
        let mut r = failed_run(&["rdy-qa/test/cli"], &[WORKSPACE_ROW]);
        assert!(apply(&mut r, &[retried("rdy-qa/test/cli", true)]));
        assert_eq!(r["pass"], true);
        assert_eq!(r["verdict"], "PASS (retried: rdy-qa/test/cli)");
        // the first results stay
        assert_eq!(r["failed_binaries"], json!(["rdy-qa/test/cli"]));
        assert_eq!(r["retried"][0]["exit"], "exit 0");
        let md = markdown(
            "# rdy QA gate: FAIL\n\n- verdict: **FAIL**\n\n| x | FAIL |\n",
            &[retried("rdy-qa/test/cli", true)],
            true,
        );
        assert!(
            md.starts_with("# rdy QA gate: PASS (retried: rdy-qa/test/cli)\n"),
            "{md}"
        );
        assert!(
            md.contains("- verdict: **PASS (retried: rdy-qa/test/cli)**"),
            "{md}"
        );
        assert!(md.contains("| x | FAIL |"), "the first rows stay: {md}");
        assert!(
            md.contains("| rdy-qa/test/cli | PASS | exit 0 | 3 | 0 | 2.5 |"),
            "{md}"
        );
    }

    #[test]
    fn rtry2_a_failed_retry_stays_failed() {
        let mut r = failed_run(&["a/test/1", "a/test/2"], &[]);
        assert!(!apply(
            &mut r,
            &[retried("a/test/1", true), retried("a/test/2", false)]
        ));
        assert_eq!(r["pass"], false);
        assert_eq!(r["verdict"], "FAIL (retried: a/test/1, a/test/2)");
    }

    #[test]
    fn rtry3_the_retry_has_the_time_the_first_run_left() {
        let r = failed_run(&["a/test/1"], &[]);
        assert_eq!(time_left(660, &r), 259);
        assert_eq!(time_left(300, &r), 0);
        assert_eq!(time_left(660, &json!({})), 0, "no duration: no time");
    }
}
