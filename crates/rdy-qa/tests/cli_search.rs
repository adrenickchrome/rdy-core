//! csrch1: `rdy search` through the daemon: text, flags, JSON, and the next-page line.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

#[test]
fn csrch1_rdy_search_prints_matches_and_takes_flags() {
    let s = Served::new();
    let (p, c) = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let w = write(&mut r, "work", json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Setup"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "Install the tool.\nRun rdy init to start."},
            {"op": "create", "type": "code", "id": "$c", "parent": "$s", "body": "rdy init", "fields": {"lang": "sh"}}
        ]))
        .unwrap();
        land(&mut r, "work").unwrap();
        let ids = (w.created["$p"].clone(), w.created["$c"].clone());
        r.close().unwrap();
        ids
    };
    let out = s.rdy(&["search", "rdy init"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim_end(),
        format!(
            "2 matching blocks\nguide  {p} (prose)\n  body:2: Run rdy init to start.\nguide  {c} (code)\n  body:1: rdy init"
        )
    );
    let (ok, v) = s.json(&["--json", "search", "RDY.INIT", "--regex", "-i"]);
    assert!(ok, "{v}");
    assert_eq!(v["total"], 2);
    let (ok, v) = s.json(&["--json", "search", "("]);
    assert!(ok, "a literal parenthesis is fine: {v}");
    assert_eq!(v["total"], 0);
    s.stop();
}

#[test]
fn csrch1_a_cut_search_prints_its_next_page() {
    let s = Served::new();
    {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let w = write(&mut r, "work", json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "many", "title": "Many", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}}
        ]))
        .unwrap();
        let sec = w.created["$s"].clone();
        let ops: Vec<Value> = (0..30)
            .map(|i| json!({"op": "create", "type": "claim", "parent": sec, "body": format!("needle {i}")}))
            .collect();
        write(&mut r, "work", Value::Array(ops)).unwrap();
        land(&mut r, "work").unwrap();
        r.close().unwrap();
    }
    let out = s.rdy(&["search", "needle", "--max-tokens", "200"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let last = text.trim_end().lines().last().unwrap().to_string();
    let cursor = last
        .strip_prefix("next page: --page ")
        .expect(&text)
        .to_string();
    let (ok, v) = s.json(&[
        "--json",
        "search",
        "needle",
        "--max-tokens",
        "200",
        "--page",
        &cursor,
    ]);
    assert!(ok, "{v}");
    assert_eq!(v["total"], 30);
    s.stop();
}
