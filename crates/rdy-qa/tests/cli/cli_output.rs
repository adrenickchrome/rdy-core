//! The human boundary's output: edit (edit1), show (show1), impact (imp1).
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

#[test]
fn edit1_an_edit_that_changes_pins_is_refused_and_writes_nothing() {
    let (_d, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Uses the {{$t}}."}
    ]))
    .unwrap();
    let scope = Scope::View("work".into());
    let c = r
        .read()
        .block(&scope, &BlockId::parse(&res.created["$c"]).unwrap())
        .unwrap();
    let t = BlockId::parse(&res.created["$t"]).unwrap();
    let before = r.store().view("work").unwrap().changes.len();
    let mut only_pins = c.clone();
    only_pins.pins.insert(t.clone(), rdy_model::Hash([7; 32]));
    let mut pins_and_body = only_pins.clone();
    pins_and_body.body = "Uses the ledger.".into();
    for edited in [only_pins, pins_and_body] {
        let e = r.edit_block(&writer(), "work", edited, None).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{e}");
        assert!(e.fix.contains("pins are set by rdy"), "{e}");
    }
    assert_eq!(
        r.store().view("work").unwrap().changes.len(),
        before,
        "no change committed"
    );
}

/// `rdy <args>` against the served project as the human, through its daemon.
fn rdy(s: &Served, args: &[&str]) -> (bool, String, String) {
    let out = s.rdy(args);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into(),
        String::from_utf8_lossy(&out.stderr).into(),
    )
}

/// A served project opened in this process for seeding; drop the `Rdy` before running `rdy`.
fn served() -> (Served, Rdy) {
    let s = Served::new();
    let r = Rdy::open(s.dir.path()).unwrap();
    (s, r)
}

fn decisions_section(r: &mut Rdy) -> String {
    let res = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Log", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Decisions"}}
    ]))
    .unwrap();
    res.created["$s"].clone()
}

#[test]
fn show1_text_show_prints_the_derived_status() {
    let (srv, mut r) = served();
    let sec = decisions_section(&mut r);
    let d1 = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: sec.clone(),
                statement: "Wallets move".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "work").unwrap();
    let d2 = r
        .supersede(
            &writer(),
            "work",
            &d1,
            None,
            DecisionInput {
                parent: sec.clone(),
                statement: "Wallets stay".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(&mut r, "work").unwrap();
    drop(r);
    let (ok, out, err) = rdy(&srv, &["show", &d1]);
    assert!(ok, "{err}");
    assert!(
        out.contains(&format!("status: superseded by {d2}")),
        "{out}"
    );
    assert!(!out.contains("\"status\": \"accepted\""), "{out}");
    let (_, out, _) = rdy(&srv, &["show", &d2]);
    assert!(out.contains("status: live"), "{out}");
    // JSON keeps the stored field and the derived status side by side
    let (_, out, _) = rdy(&srv, &["--json", "show", &d1]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["fields"]["status"], "accepted");
    assert_eq!(v["status"]["status"], "superseded");
}

#[test]
fn imp1_impact_items_say_which_edge_reached_them() {
    let (srv, mut r) = served();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Uses the {{$t}}."},
        {"op": "create", "type": "note", "id": "$n", "parent": "$s", "body": "See {{$c}}."}
    ]))
    .unwrap();
    land(&mut r, "work").unwrap();
    let (t, c, n) = (
        res.created["$t"].clone(),
        res.created["$c"].clone(),
        res.created["$n"].clone(),
    );
    let items = r
        .read()
        .impact(&Scope::Canon, &BlockId::parse(&t).unwrap(), 3)
        .unwrap();
    let got: Vec<(u32, String, String, String)> = items
        .iter()
        .map(|i| {
            (
                i.depth,
                i.id.clone(),
                i.via.kind.clone(),
                i.via.from.clone(),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            (1, c.clone(), "references".to_string(), t.clone()),
            (2, n.clone(), "references".to_string(), c.clone()),
        ]
    );
    drop(r);
    let (ok, out, err) = rdy(&srv, &["impact", &t]);
    assert!(ok, "{err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(
        lines.iter().all(|l| l.starts_with("depth ")),
        "no tree indentation: {out}"
    );
    assert!(
        lines[1].starts_with(&format!("depth 2  {n} note:")),
        "{out}"
    );
    assert!(lines[1].ends_with(&format!("[references {c}]")), "{out}");
    let (_, out, _) = rdy(&srv, &["--json", "impact", &t]);
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        v["items"][1]["via"],
        json!({"from": c, "kind": "references"})
    );
}

#[test]
fn relay1_propose_text_names_the_ref_the_approver_approves() {
    let (s, mut r) = served();
    let section = seed_doc(&mut r, "seed").1;
    land(&mut r, "seed").unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "x"}]),
    )
    .unwrap();
    r.close().unwrap();
    // a version that is not ready prints no approve line (rg10, O20): it has a summary
    let (ok, text, err) = rdy(
        &s,
        &["propose", "--title", "t", "--summary", "test summary"],
    );
    assert!(ok, "{err}");
    let lines: Vec<&str> = text.lines().collect();
    // cc16 (FW-CH3) prints the proposal's coverage between the two lines this test pins
    assert!(
        !lines.iter().any(|l| l.starts_with("blocking")),
        "no blocking line without a conflict: {text}"
    );
    let pref = lines[0]
        .strip_prefix("proposal ")
        .and_then(|l| l.strip_suffix(" (1 changes)"))
        .unwrap();
    assert!(pref.starts_with("P-") && pref.ends_with("@1"), "{text}");
    assert_eq!(
        lines.last().copied(),
        Some(format!("approve with: rdy approve {pref}").as_str()),
        "{text}"
    );
}
