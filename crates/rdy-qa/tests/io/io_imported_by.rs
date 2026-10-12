//! ib13 (FW-F7b, D-JCVPD): a block's history names the importer of an imported change.
use rdy_engine::*;
use rdy_model::*;
use serde_json::json;
use std::path::Path;

fn sess(actor: &str, run: Option<&str>, role: Role) -> Session {
    Session {
        actor: actor.into(),
        run: run.map(str::to_string),
        role,
        kind: SessionKind::Human,
    }
}

fn founder() -> Session {
    sess("human:founder", None, Role::Approver)
}

fn put(r: &mut Rdy, s: &Session, view: &str, v: serde_json::Value) -> WriteResult {
    r.write(
        s,
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(v).unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap()
}

/// Canon: doc `d` by agent:a (run r1), then prose `p` under it by agent:b (run r2).
fn two_authors(dir: &Path) -> (Rdy, String, String) {
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let a = sess("agent:a", Some("r1"), Role::Writer);
    let w = put(
        &mut r,
        &a,
        "va",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "intro", "title": "Intro", "doc_type": "page"}}
        ]),
    );
    let d = w.created["$d"].clone();
    let p = r.propose(&a, "va", "a").unwrap();
    r.land(&founder(), &p.id).unwrap();
    let b = sess("agent:b", Some("r2"), Role::Writer);
    let w = put(
        &mut r,
        &b,
        "vb",
        json!([
            {"op": "create", "type": "prose", "id": "$p", "parent": d, "body": "Hello."}
        ]),
    );
    let prose = w.created["$p"].clone();
    let p = r.propose(&b, "vb", "b").unwrap();
    r.land(&founder(), &p.id).unwrap();
    (r, d, prose)
}

#[test]
fn ib13_an_imported_change_names_its_importer_in_the_history() {
    let a_dir = tempfile::tempdir().unwrap();
    let (r, d, _p) = two_authors(a_dir.path());
    let out = tempfile::tempdir().unwrap();
    let out = out.path().join("x");
    r.read().export(&Scope::Canon, &out).unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    let imp = sess("human:imp", None, Role::Approver);
    b.import(&imp, "import", &out).unwrap();
    let id: BlockId = d.parse().unwrap();
    let h = b
        .read()
        .history(&Scope::View("import".into()), &id, 1)
        .unwrap();
    assert_eq!(h.entries.len(), 1, "{h:?}");
    assert_eq!(
        h.entries[0].imported_by,
        Some(ActorRun {
            actor: "human:imp".into(),
            run: None
        })
    );
    let json = serde_json::to_value(&h.entries[0]).unwrap();
    assert_eq!(
        json["imported_by"],
        json!({"actor": "human:imp", "run": null}),
        "{json}"
    );
}

#[test]
fn ib13_a_written_change_has_no_importer() {
    let a_dir = tempfile::tempdir().unwrap();
    let (r, d, _p) = two_authors(a_dir.path());
    let id: BlockId = d.parse().unwrap();
    let h = r.read().history(&Scope::Canon, &id, 1).unwrap();
    assert_eq!(h.entries.len(), 1, "{h:?}");
    assert_eq!(h.entries[0].imported_by, None);
    let json = serde_json::to_value(&h.entries[0]).unwrap();
    assert!(json.get("imported_by").is_none(), "{json}");
}
