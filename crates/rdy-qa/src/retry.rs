//! `cargo qa --retry-failed <report.json>`: one retry of the failed test binaries of a gate run,
//! on the same tree, inside the time that is left on the run's budget. The retry runs only those
//! binaries, one at a time. The report keeps both results: the first run's rows stay as they
//! were, and a `retried` list holds the second results. After a good retry the run is
//! `PASS (retried: <names>)`. Each retried row also holds the flake issue link that the run was
//! given for its binary (`--flake-links`), or none: the release counts a retried pass only with it.
use crate::gate_steps::{BUDGET_ROW, BUILD_STEP};
use crate::report::{BinaryFailure, binary_failure_sections};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

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
    /// The open flake issue that the run was given for this binary (`--flake-links`), or none.
    /// The release gate counts a retried pass only when every retried binary has one.
    pub flake_issue: Option<String>,
}

/// The most `<binary>=<issue id>` pairs of `--flake-links`.
pub const MAX_FLAKE_LINKS: usize = 64;

/// The longest `--flake-links` text, in bytes.
pub const MAX_FLAKE_LINKS_BYTES: usize = 4096;

/// A binary key: `<package>/<kind>/<name>`, three names of letters, digits, `.`, `_` and `-`.
fn is_binary_key(text: &str) -> bool {
    let mut parts = text.split('/');
    let names = [parts.next(), parts.next(), parts.next()];
    parts.next().is_none()
        && names.iter().all(|n| {
            n.is_some_and(|n| {
                !n.is_empty()
                    && n.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            })
        })
}

/// A flake issue id: `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`. The id is opaque text; this checks its form.
fn is_issue_id(text: &str) -> bool {
    let bytes = text.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Reads `--flake-links`: comma-separated `<binary>=<issue id>` pairs, by binary. An empty text is
/// no links. It holds at most [`MAX_FLAKE_LINKS`] pairs and [`MAX_FLAKE_LINKS_BYTES`] bytes, and
/// links a binary once.
pub fn flake_links(text: &str) -> Result<BTreeMap<String, String>, String> {
    if text.len() > MAX_FLAKE_LINKS_BYTES {
        return Err(format!(
            "--flake-links: the list is longer than {MAX_FLAKE_LINKS_BYTES} bytes"
        ));
    }
    let mut links = BTreeMap::new();
    if text.is_empty() {
        return Ok(links);
    }
    let pairs: Vec<&str> = text.split(',').collect();
    if pairs.len() > MAX_FLAKE_LINKS {
        return Err(format!("--flake-links: more than {MAX_FLAKE_LINKS} pairs"));
    }
    for pair in pairs {
        let (binary, id) = match pair.split_once('=') {
            Some((b, i)) if is_binary_key(b) && is_issue_id(i) => (b, i),
            _ => return Err(format!("--flake-links: {pair} is not <binary>=<issue id>")),
        };
        if links.insert(binary.to_string(), id.to_string()).is_some() {
            return Err(format!("--flake-links: {binary} is linked twice"));
        }
    }
    Ok(links)
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
    out.push_str("\n## Retry\n\nThe failed binaries ran once more on the same tree, one at a time. The rows above are the first results.\n\n| Binary | Result | Exit | Passed | Failed | Seconds | Flake issue |\n|---|---|---|---|---|---|---|\n");
    for r in retried {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {:.1} | {} |\n",
            r.binary,
            if r.ok { "PASS" } else { "FAIL" },
            r.exit,
            r.passed,
            r.failed,
            r.secs,
            r.flake_issue.as_deref().unwrap_or("none")
        ));
    }
    out
}

/// Adds the failures of the binaries that failed their retry to the report, as a
/// `retried_failures` list of `{binary, test, panic}`. A retry with no failure adds nothing.
pub fn apply_failures(report: &mut Value, failures: &[BinaryFailure]) {
    if !failures.is_empty() {
        report["retried_failures"] =
            serde_json::to_value(failures).expect("retry failures serialize");
    }
}

/// [`markdown`], then a "Failing in" section for each binary that failed its retry, with the
/// failing tests and their panic text.
pub fn markdown_with_failures(
    first: &str,
    retried: &[Retried],
    failures: &[BinaryFailure],
    pass: bool,
) -> String {
    let mut out = markdown(first, retried, pass);
    out += &binary_failure_sections(failures, " on its retry");
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
            flake_issue: None,
        }
    }

    fn linked(binary: &str, issue: &str) -> Retried {
        Retried {
            flake_issue: Some(issue.into()),
            ..retried(binary, true)
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

    #[test]
    fn r59024_a_binary_that_fails_its_retry_names_its_tests() {
        let failures = vec![BinaryFailure {
            binary: "rdy-daemon/test/windows_lifecycle".into(),
            test: Some("lifecycle::stops".into()),
            panic: Some("thread 'lifecycle::stops' panicked at a.rs:1:1:\nboom".into()),
        }];
        let rows = [retried("rdy-daemon/test/windows_lifecycle", false)];
        let md = markdown_with_failures("# rdy QA gate: FAIL\n", &rows, &failures, false);
        assert!(
            md.contains("\nFailing in `rdy-daemon/test/windows_lifecycle` on its retry:\n- `lifecycle::stops`\n\n        thread 'lifecycle::stops' panicked at a.rs:1:1:\n        boom\n"),
            "{md}"
        );
        // the plain form has no such section
        assert!(!markdown("# rdy QA gate: FAIL\n", &rows, false).contains("Failing in"));
        let mut r = failed_run(&["rdy-daemon/test/windows_lifecycle"], &[WORKSPACE_ROW]);
        apply(&mut r, &rows);
        apply_failures(&mut r, &failures);
        assert_eq!(r["retried_failures"][0]["test"], "lifecycle::stops");
        assert_eq!(
            r["retried_failures"][0]["binary"],
            "rdy-daemon/test/windows_lifecycle"
        );
    }

    #[test]
    fn r59022_retry_rows_carry_their_flake_issue_links() {
        let mut r = failed_run(&["a/test/1", "a/test/2"], &[WORKSPACE_ROW]);
        let rows = [linked("a/test/1", "flake-7"), retried("a/test/2", true)];
        assert!(apply(&mut r, &rows));
        assert_eq!(r["retried"][0]["flake_issue"], "flake-7");
        assert!(r["retried"][1]["flake_issue"].is_null(), "{r}");
        let md = markdown("# rdy QA gate: FAIL\n", &rows, true);
        assert!(
            md.contains("| Seconds | Flake issue |\n|---|---|---|---|---|---|---|\n"),
            "{md}"
        );
        assert!(
            md.contains("| a/test/1 | PASS | exit 0 | 3 | 0 | 2.5 | flake-7 |\n"),
            "{md}"
        );
        assert!(
            md.contains("| a/test/2 | PASS | exit 0 | 3 | 0 | 2.5 | none |\n"),
            "{md}"
        );
        // the rtry2 row substring still matches
        assert!(
            md.contains("| a/test/2 | PASS | exit 0 | 3 | 0 | 2.5 |"),
            "{md}"
        );
    }

    #[test]
    fn r59022_flake_links_parse_and_refuse_bad_pairs() {
        assert!(flake_links("").unwrap().is_empty());
        let links = flake_links("rdy-qa/test/cli=flake-7,rdy-store/lib/rdy_store=9.a_b").unwrap();
        assert_eq!(links.len(), 2);
        assert_eq!(links["rdy-qa/test/cli"], "flake-7");
        assert_eq!(links["rdy-store/lib/rdy_store"], "9.a_b");
        let not_pair = |pair: &str| format!("--flake-links: {pair} is not <binary>=<issue id>");
        for bad in [
            "rdy-qa/test/cli",
            "rdy-qa/test/cli=",
            "=flake-7",
            "rdy-qa/test=flake-7",
            "rdy-qa/test/cli/x=flake-7",
            "rdy-qa//cli=flake-7",
            "rdy-qa/test/cli=-flake",
            "rdy-qa/test/cli=fl ake",
            "rdy-qa/test/cli=a=b",
            "rdy qa/test/cli=flake-7",
        ] {
            assert_eq!(flake_links(bad).unwrap_err(), not_pair(bad), "{bad}");
        }
        assert_eq!(
            flake_links("a/test/1=flake-1,,a/test/2=flake-2").unwrap_err(),
            not_pair("")
        );
        assert_eq!(
            flake_links("a/test/1=flake-1,a/test/1=flake-2").unwrap_err(),
            "--flake-links: a/test/1 is linked twice"
        );
        let id64 = format!("a/test/1={}", "x".repeat(64));
        assert!(flake_links(&id64).is_ok());
        let id65 = format!("a/test/1={}", "x".repeat(65));
        assert_eq!(flake_links(&id65).unwrap_err(), not_pair(&id65));
        let many = |n: usize| {
            (0..n)
                .map(|i| format!("a/test/b{i}=f{i}"))
                .collect::<Vec<_>>()
                .join(",")
        };
        assert_eq!(flake_links(&many(64)).unwrap().len(), 64);
        assert_eq!(
            flake_links(&many(65)).unwrap_err(),
            "--flake-links: more than 64 pairs"
        );
        let long = format!("a/test/1={}", "x".repeat(5000));
        assert_eq!(
            flake_links(&long).unwrap_err(),
            "--flake-links: the list is longer than 4096 bytes"
        );
        let edge = format!("a/test/1=f,{}", "y".repeat(4096 - "a/test/1=f,".len()));
        assert!(!flake_links(&edge).unwrap_err().contains("4096 bytes"));
    }
}
