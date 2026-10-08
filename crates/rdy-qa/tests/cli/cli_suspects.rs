//! scli1: rdy suspects lists the suspects in a scope, as rdy_suspects does.
use rdy_engine::*;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use serde_json::json;

/// A claim citing a term, landed; then the term edited and landed: the claim is suspect in canon.
fn seeded(s: &Served) -> (String, String) {
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let res = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Uses the {{$t}}."}
    ]))
    .unwrap();
    land(&mut r, "seed").unwrap();
    let (t, c) = (res.created["$t"].clone(), res.created["$c"].clone());
    write(
        &mut r,
        "edit",
        json!([{"op": "update", "id": t, "body": "Balances, per wallet."}]),
    )
    .unwrap();
    land(&mut r, "edit").unwrap();
    r.close().unwrap();
    (c, t)
}

#[test]
fn scli1_rdy_suspects_lists_canon_suspects_in_text_and_json() {
    let s = Served::new();
    let (c, t) = seeded(&s);
    let (ok, v) = s.json(&["--json", "suspects"]);
    assert!(ok, "{v}");
    assert_eq!(
        v,
        json!({"items": [{"block": c, "target": t, "reason": "changed"}]})
    );
    let o = s.rdy(&["suspects"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        String::from_utf8_lossy(&o.stdout).trim_end(),
        format!("{c} -> {t}  changed")
    );
    let (ok, v) = s.json(&["--json", "suspects", "--scope", "canon"]);
    assert!(ok && v["items"].as_array().unwrap().len() == 1, "{v}");
    let (ok, v) = s.json(&["--json", "suspects", "--scope", "nope"]);
    assert!(!ok);
    assert_eq!(v["code"], "E_NOT_FOUND", "{v}");
}

#[test]
fn scli1_no_suspects_prints_one_line() {
    let s = Served::new();
    let o = s.rdy(&["suspects"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim_end(), "no suspects");
    let (ok, v) = s.json(&["--json", "suspects"]);
    assert!(ok);
    assert_eq!(v, json!({"items": []}));
}
