use rdy_engine::*;
use rdy_model::*;
use serde_json::json;
use std::collections::BTreeMap;

fn agent() -> Session { Session { actor: "agent:a".into(), run: None, role: Role::Writer } }
fn founder() -> Session { Session { actor: "human:founder".into(), run: None, role: Role::Approver } }

fn rich_project(dir: &std::path::Path) -> Rdy {
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let blob = r.store_mut().put_blob(&[0u8, 159, 146, 150, 255]).unwrap().to_string();
    let res = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Ünïcødé — design", "doc_type": "components"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Components"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Line one.\nLine two with \"quotes\" and: colons\n\ttabbed\n"},
        {"op": "create", "type": "diagram", "parent": "$s", "fields": {"lang": "mermaid", "alt": "Flow"}, "body": "graph TD\n  A[{{$t}}] --> B\n"},
        {"op": "create", "type": "figure", "parent": "$s", "fields": {"blob": blob, "media_type": "application/octet-stream", "alt": "bytes"}},
        {"op": "create", "type": "table", "parent": "$s", "fields": {"columns": ["a", "b"], "rows": [[1, true], ["x", null]], "normative": false}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let s = res.created["$s"].clone();
    let t = res.created["$t"].clone();
    let p = r.propose(&agent(), "work", "seed").unwrap();
    r.land(&founder(), &p.id).unwrap();
    r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "claim", "parent": s, "body": format!("Uses {{{{{t}}}}}."), "links": {"describes": [{"target": t, "part": "the balance part"}]}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    r.decide(&agent(), "work", DecisionInput { parent: s, statement: "Ledger is append-only".into(), options: vec!["append-only".into(), "mutable".into()], ..Default::default() }).unwrap();
    let p = r.propose(&agent(), "work", "more").unwrap();
    r.land(&founder(), &p.id).unwrap();
    r
}

#[test]
fn export_import_round_trip_preserves_every_version_hash() {
    let a_dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let mut a = rich_project(a_dir.path());
    let summary = a.export(&Scope::Canon, out.path()).unwrap();
    assert_eq!(summary.docs, 1);
    assert_eq!(summary.blobs, 1);
    let original: BTreeMap<BlockId, Hash> = a.materialized(&Scope::Canon).unwrap().snapshot.blocks.clone();
    assert_eq!(summary.blocks, original.len());

    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    assert_eq!(b.import(&agent(), "import", out.path()).unwrap_err().code, "E_PERMISSION");
    b.import(&founder(), "import", out.path()).unwrap();
    let p = b.propose(&founder(), "import", "migrate").unwrap();
    b.land(&founder(), &p.id).unwrap();
    let imported = b.materialized(&Scope::Canon).unwrap().snapshot.blocks.clone();
    assert_eq!(imported, original);

    // the blob came across too
    let fig = b.materialized(&Scope::Canon).unwrap().blocks.values().find(|x| x.block_type == "figure").unwrap().clone();
    let h: Hash = fig.field_str("blob").unwrap().parse().unwrap();
    assert_eq!(b.store().get_blob(&h).unwrap(), vec![0u8, 159, 146, 150, 255]);

    // importing twice collides
    assert_eq!(b.import(&founder(), "again", out.path()).unwrap_err().code, "E_ALREADY_EXISTS");
}

#[test]
fn single_block_yaml_round_trip_and_edit() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rich_project(dir.path());
    let term = r.materialized(&Scope::Canon).unwrap().blocks.values().find(|x| x.block_type == "term").unwrap().clone();
    let text = block_to_yaml(&term);
    assert_eq!(block_from_yaml(&text).unwrap(), term);

    let mut edited = term.clone();
    edited.body = "Edited definition.".into();
    r.edit_block(&founder(), "work", edited.clone()).unwrap();
    assert_eq!(r.block(&Scope::View("work".into()), &term.id).unwrap().body, "Edited definition.");

    let mut moved = term.clone();
    moved.order = "zz".into();
    assert_eq!(r.edit_block(&founder(), "work", moved).unwrap_err().code, "E_BAD_INPUT");
    assert_eq!(block_from_yaml("id: [").unwrap_err().code, "E_BAD_INPUT");
}
