//! cflt1: `rdy search` prints totals and collapsed hits, and takes the filter flags.
use rdy_engine::Rdy;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use serde_json::json;

#[test]
fn cflt1_rdy_search_prints_totals_and_takes_filter_flags() {
    let s = Served::new();
    let (ak, ap, bk) = {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let w = write(&mut r, "work", json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "code", "id": "$ak", "parent": "$a", "body": "port = 8080", "fields": {"lang": "sh"}},
            {"op": "create", "type": "prose", "id": "$ap", "parent": "$a", "body": "port = 8080"},
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "B", "doc_type": "page"}},
            {"op": "create", "type": "code", "id": "$bk", "parent": "$b", "body": "port = 8080", "fields": {"lang": "sh"}}
        ]))
        .unwrap();
        land(&mut r, "work").unwrap();
        let ids = (
            w.created["$ak"].clone(),
            w.created["$ap"].clone(),
            w.created["$bk"].clone(),
        );
        r.close().unwrap();
        ids
    };
    let out = s.rdy(&["search", "port = 8080"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim_end(),
        format!(
            "3 matching blocks\nby doc: a 2, b 1\nby type: code 2, prose 1\na  {ak} (code)\n  body:1: port = 8080\n  also: b {bk}\na  {ap} (prose)\n  body:1: port = 8080"
        )
    );
    let (ok, v) = s.json(&["--json", "search", "8080", "--type", "code", "--path", "b"]);
    assert!(ok, "{v}");
    assert_eq!(v["total"], 1);
    let (ok, v) = s.json(&[
        "--json", "search", "8080", "--type", "code", "--type", "prose",
    ]);
    assert!(ok, "{v}");
    assert_eq!(v["total"], 3);
    let (ok, v) = s.json(&["--json", "search", "8080", "--lifecycle", "final"]);
    assert!(!ok);
    assert_eq!(v["code"], "E_BAD_INPUT", "{v}");
    s.stop();
}
