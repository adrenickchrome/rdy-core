//! cc12: `rdy coverage` prints the report and `--json` is the report (FW-CV1, D-KE84J).
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use serde_json::json;
use std::collections::BTreeMap;

/// Doc `ops` holds a stable Summary with term Ledger and a claim that names the Ledger without a
/// link; doc `lone` holds one prose block. Landed. Returns the created ids.
fn world(s: &Served) -> BTreeMap<String, String> {
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Summary", "lifecycle": "stable"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "The Ledger is append-only."},
        {"op": "create", "type": "doc", "id": "$l", "fields": {"name": "lone", "title": "Lone", "doc_type": "page"}},
        {"op": "create", "type": "prose", "parent": "$l", "body": "Nothing here."}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    r.close().unwrap();
    w.created
}

#[test]
fn cc12_rdy_coverage_prints_the_report() {
    let s = Served::new();
    let ids = world(&s);
    let out = s.rdy(&["coverage"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let first = text.lines().next().unwrap();
    let n = first
        .strip_prefix("coverage ")
        .and_then(|r| r.strip_suffix(" (canon)"))
        .unwrap_or_else(|| panic!("the first line: {text}"));
    assert!(n.parse::<u32>().is_ok(), "a score: {text}");
    assert!(text.lines().any(|l| l.starts_with("  grounded ")), "{text}");
    assert!(text.contains("\nlowest pages:\n"), "{text}");
    assert!(text.contains("\nissues ("), "{text}");
    let fix = format!(
        "link this mention to Ledger ({}), or dismiss it as a passing mention",
        ids["$t"]
    );
    let line = text
        .lines()
        .find(|l| l.contains("unlinked_mention"))
        .unwrap_or_else(|| panic!("the issue line: {text}"));
    assert_eq!(
        line,
        format!("  {}  grounded  unlinked_mention  {fix}", ids["$c"]),
        "{text}"
    );
    s.stop();
}

#[test]
fn cc12_rdy_coverage_json_is_the_report() {
    let s = Served::new();
    world(&s);
    let (ok, v) = s.json(&["--json", "coverage"]);
    assert!(ok, "{v}");
    for k in [
        "scope",
        "score",
        "dimensions",
        "pages",
        "areas",
        "issues",
        "issues_total",
        "kinds",
        "similar_kinds",
    ] {
        assert!(v.get(k).is_some(), "key {k}: {v}");
    }
    assert_eq!(v["scope"], "canon", "{v}");
    assert!(v["issues_total"].as_u64().unwrap() >= 1, "{v}");
    s.stop();
}

#[test]
fn cc12_a_path_narrows_the_report() {
    let s = Served::new();
    world(&s);
    let (ok, whole) = s.json(&["--json", "coverage"]);
    assert!(ok, "{whole}");
    let (ok, v) = s.json(&["--json", "coverage", "ops"]);
    assert!(ok, "{v}");
    let pages = v["pages"].as_array().unwrap();
    assert!(!pages.is_empty(), "{v}");
    for p in pages {
        assert!(p["path"].as_str().unwrap().starts_with("ops"), "{v}");
    }
    assert!(
        whole["pages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["path"].as_str().unwrap().starts_with("lone")),
        "the whole report lists the other doc: {whole}"
    );
    let out = s.rdy(&["coverage", "nowhere"]);
    assert!(!out.status.success());
    s.stop();
}

/// A landed doc with `n` claims that each name the Ledger term without a link: `n` issues.
fn many_mentions(s: &Served, n: usize) {
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$o", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$o", "fields": {"title": "Summary"}}),
        json!({"op": "create", "type": "term", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Holds balances."}),
    ];
    for i in 0..n {
        ops.push(json!({"op": "create", "type": "claim", "parent": "$s",
            "body": format!("The Ledger is append-only, rule {i}.")}));
    }
    write(&mut r, "work", json!(ops)).unwrap();
    land(&mut r, "work").unwrap();
    r.close().unwrap();
}

const MORE: &str = "(pass --max-tokens for more)";

#[test]
fn cc12_max_tokens_shows_more_issues() {
    let s = Served::new();
    many_mentions(&s, 150);
    let cut = s.rdy(&["coverage"]);
    assert!(cut.status.success());
    let cut = String::from_utf8_lossy(&cut.stdout).into_owned();
    let line = cut
        .lines()
        .find(|l| l.contains("more"))
        .unwrap_or_else(|| panic!("a cut report says so: {cut}"));
    assert!(line.starts_with("  … ") && line.ends_with(MORE), "{line}");
    let shown = |t: &str| t.lines().filter(|l| l.contains("unlinked_mention")).count();
    let (ok, j) = s.json(&["--json", "coverage", "--max-tokens", "100000"]);
    assert!(ok, "{j}");
    let all = String::from_utf8_lossy(&s.rdy(&["coverage", "--max-tokens", "100000"]).stdout)
        .into_owned();
    assert!(shown(&all) > shown(&cut), "a larger budget shows more");
    assert!(!all.contains(MORE), "nothing is cut, no hint: {all}");
    assert_eq!(
        j["issues"].as_array().unwrap().len() as u64,
        j["issues_total"].as_u64().unwrap()
    );
    assert!(j.get("omitted").is_none(), "{j}");
    let (_, small) = s.json(&["--json", "coverage", "--max-tokens", "300"]);
    assert_eq!(small["truncated"], true, "{small}");
    s.stop();
}
