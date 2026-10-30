//! dq7: `rdy q` through the daemon: the count line, one line per match, scope, JSON, parse errors
//! and the filter guide in its help.
use crate::cli_diff::{work, world};
use rdy_qa::daemon::Served;
use serde_json::Value;

fn stdout(s: &Served, args: &[&str]) -> String {
    let out = s.rdy(args);
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

/// The text a match prints (O17).
fn line(m: &Value) -> String {
    format!(
        "{}  {} ({})  {}",
        m["path"].as_str().unwrap_or("-"),
        m["id"].as_str().unwrap(),
        m["block_type"].as_str().unwrap(),
        m["summary"].as_str().unwrap()
    )
}

#[test]
fn dq7_rdy_q_prints_one_line_per_match() {
    let w = world();
    let (ok, v) = w.s.json(&["--json", "q", "type = claim"]);
    assert!(ok, "{v}");
    assert_eq!(v["total"], 2);
    let mut want = vec!["2 matching blocks".to_string()];
    want.extend(v["matches"].as_array().unwrap().iter().map(line));
    assert_eq!(stdout(&w.s, &["q", "type = claim"]), want.join("\n"));
    // a doc has a path; a claim has none of its own
    let docs = stdout(&w.s, &["q", "type = doc"]);
    assert!(docs.starts_with("1 matching block\nguide  "), "{docs}");
    w.s.stop();
}

#[test]
fn dq7_rdy_q_with_one_match_says_block() {
    let w = world();
    let text = stdout(&w.s, &["q", &format!("id = {}", w.old)]);
    assert!(text.starts_with("1 matching block\n"), "{text}");
    assert_eq!(text.lines().count(), 2);
    w.s.stop();
}

#[test]
fn dq7_rdy_q_with_no_match_prints_zero() {
    let w = world();
    assert_eq!(stdout(&w.s, &["q", "type = decision"]), "0 matching blocks");
    w.s.stop();
}

#[test]
fn dq7_rdy_q_scope_and_json() {
    let w = world();
    let n = work(&w);
    let canon = stdout(&w.s, &["q", "type = claim"]);
    assert!(canon.starts_with("2 matching blocks"), "{canon}");
    let (ok, v) =
        w.s.json(&["--json", "q", "type = claim", "--scope", "work"]);
    assert!(ok, "{v}");
    assert_eq!(v["total"], 3);
    assert!(
        v["matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == n.as_str())
    );
    // the JSON is the whole reply: no cut, no cursor
    assert!(v.get("next").is_none(), "{v}");
    let text = stdout(&w.s, &["q", "type = claim", "--scope", "work"]);
    assert!(text.starts_with("3 matching blocks"), "{text}");
    w.s.stop();
}

#[test]
fn dq7_rdy_q_parse_error_prints_code_column_and_fix() {
    let w = world();
    let out = w.s.rdy(&["q", "tpye = claim"]);
    assert!(!out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim_end(),
        "error E_BAD_INPUT: filter: unknown field tpye; did you mean type? at column 1\nfix: read the filter guide in rdy_schema, or run rdy q --help"
    );
    w.s.stop();
}

#[test]
fn dq7_rdy_q_help_holds_the_filter_guide() {
    let w = world();
    let help = stdout(&w.s, &["q", "--help"]);
    assert!(
        help.contains(
            "Filter blocks with an expression, such as type = decision and status = live. The chat path is rdy_query."
        ),
        "{help}"
    );
    assert!(help.contains(rdy_schema::filter::FILTER_GUIDE), "{help}");
    w.s.stop();
}
