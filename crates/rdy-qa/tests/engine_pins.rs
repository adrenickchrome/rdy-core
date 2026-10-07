//! lpin1 (FW-LP1): a pin records the target's content hash: a cycle written in one batch is
//! pinned, a pin survives changes to pins and derived fields only, and an old export re-pins.
use rdy_engine::*;
use rdy_model::{BlockId, FieldValue, Hash};
use rdy_qa::fixtures::*;
use serde_json::json;

fn work() -> Scope {
    Scope::View("work".into())
}
fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}
fn suspect_pairs(r: &Rdy, scope: &Scope) -> Vec<(String, String, String)> {
    r.read()
        .suspects(scope)
        .unwrap()
        .into_iter()
        .map(|s| (s.block.to_string(), s.target.to_string(), s.reason))
        .collect()
}

#[test]
fn lpin1_content_hash_ignores_pins_and_derived_fields_and_nothing_else() {
    let b = raw("c-AAAAA", "claim", Some("s-AAAAA"), "a1", &[], "A holds.");
    let mut same = b.clone();
    same.pins.insert(id("c-BBBBB"), Hash([7; 32]));
    for (k, v) in [
        ("status", "accepted"),
        ("decided_at", "x"),
        ("lifecycle", "stable"),
    ] {
        same.fields.insert(k.into(), FieldValue::Str(v.into()));
    }
    assert_eq!(same.content_hash(), b.content_hash());
    assert_ne!(same.version_hash(), b.version_hash());
    let mut body = b.clone();
    body.body = "A holds, changed.".into();
    assert_ne!(body.content_hash(), b.content_hash());
    let mut moved = b.clone();
    moved.order = "a2".into();
    assert_ne!(moved.content_hash(), b.content_hash());
    assert_ne!(
        b.content_hash(),
        b.version_hash(),
        "the content hash is domain-separated"
    );
}

#[test]
fn lpin1_a_reference_cycle_written_in_one_batch_is_pinned_and_turns_suspect_on_drift() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "work");
    write(&mut r, "work", json!([
        {"op": "create", "type": "claim", "id": "c-AAAAA", "parent": section, "body": "See {{c-BBBBB}}."},
        {"op": "create", "type": "claim", "id": "c-BBBBB", "parent": section, "body": "See {{c-AAAAA}}."}
    ]))
    .unwrap();
    let a = r.read().block(&work(), &id("c-AAAAA")).unwrap();
    let b = r.read().block(&work(), &id("c-BBBBB")).unwrap();
    assert_eq!(a.pins[&b.id], b.content_hash());
    assert_eq!(b.pins[&a.id], a.content_hash());
    assert!(
        suspect_pairs(&r, &work()).is_empty(),
        "a fresh cycle is not suspect"
    );
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": "c-BBBBB", "body": "See {{c-AAAAA}}, changed."}]),
    )
    .unwrap();
    assert_eq!(
        suspect_pairs(&r, &work()),
        vec![(
            "c-AAAAA".to_string(),
            "c-BBBBB".to_string(),
            "changed".to_string()
        )],
        "drift inside the cycle is caught; B's own pin to A still holds"
    );
}

#[test]
fn lpin1_a_pin_is_unchanged_when_only_derived_fields_or_pins_change() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "work");
    let c_body = format!("C cites {{{{$t}}}} and its section {{{{{section}}}}}.");
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "claim", "id": "$u", "parent": section, "body": "U."},
        {"op": "create", "type": "claim", "id": "$t", "parent": section, "body": "T builds on {{$u}}."},
        {"op": "create", "type": "claim", "id": "$c", "parent": section, "body": c_body}
    ]))
    .unwrap();
    let (u, t, c) = (
        id(&w.created["$u"]),
        id(&w.created["$t"]),
        id(&w.created["$c"]),
    );
    let sid = id(&section);
    let (pin_t, pin_s) = {
        let cb = r.read().block(&work(), &c).unwrap();
        (cb.pins[&t], cb.pins[&sid])
    };
    // U changes: T is suspect; a repin of T changes only T's pins: C's pin to T holds
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": u.to_string(), "body": "U, changed."}]),
    )
    .unwrap();
    assert!(suspect_pairs(&r, &work()).contains(&(
        t.to_string(),
        u.to_string(),
        "changed".to_string()
    )));
    write(
        &mut r,
        "work",
        json!([{"op": "repin", "id": t.to_string()}]),
    )
    .unwrap();
    assert_eq!(
        r.read().block(&work(), &c).unwrap().pins[&t],
        pin_t,
        "C's pin is untouched"
    );
    assert_eq!(
        pin_t,
        r.read().block(&work(), &t).unwrap().content_hash(),
        "and still current"
    );
    // the section's lifecycle is a derived field: C's pin to the section holds (the write makes
    // the section stable, so it cites a briefing)
    let token = rdy_qa::edit_loop::brief_of(&r, &work(), &[&sid]).base.token;
    rdy_qa::edit_loop::write_cited(
        &mut r,
        "work",
        json!([{"op": "update", "id": section, "fields": {"lifecycle": "stable"}}]),
        Some(&token),
    )
    .unwrap();
    assert_eq!(r.read().block(&work(), &c).unwrap().pins[&sid], pin_s);
    assert_eq!(
        pin_s,
        r.read().block(&work(), &sid).unwrap().content_hash(),
        "and still current"
    );
    assert!(
        suspect_pairs(&r, &work()).is_empty(),
        "{:?}",
        suspect_pairs(&r, &work())
    );
}

const OLD_TREE: &str = "- id: d-AAAAA\n  type: doc\n  order: a1\n  fields:\n    title: Spec\n    doc_type: overview\n- id: s-AAAAA\n  type: section\n  parent: d-AAAAA\n  order: a1\n  fields:\n    title: Rules\n- id: r-AAAAA\n  type: requirement\n  parent: s-AAAAA\n  order: a1\n  body: It must.\n";
const TARGET: &str = "id: r-AAAAA\ntype: requirement\nparent: s-AAAAA\norder: a1\nbody: It must.\n";

fn old_export(format: u32) -> tempfile::TempDir {
    let current = block_from_yaml(TARGET).unwrap().version_hash();
    let stale = Hash([7; 32]);
    let tree = format!(
        "{OLD_TREE}- id: c-AAAAA\n  type: claim\n  parent: s-AAAAA\n  order: a2\n  body: See {{{{r-AAAAA}}}}.\n  pins:\n    r-AAAAA: {current}\n- id: c-BBBBB\n  type: claim\n  parent: s-AAAAA\n  order: a3\n  body: Also {{{{r-AAAAA}}}}.\n  pins:\n    r-AAAAA: {stale}\n    s-AAAAA: {stale}\n"
    );
    let t = tempfile::tempdir().unwrap();
    std::fs::write(
        t.path().join("rdy-export.yaml"),
        format!("format: {format}\npack: rdy-standard\nscope: canon\n"),
    )
    .unwrap();
    std::fs::create_dir_all(t.path().join("docs")).unwrap();
    std::fs::write(t.path().join("docs/spec.yaml"), tree).unwrap();
    t
}

#[test]
fn lpin1_import_recomputes_every_pin_from_the_imported_content() {
    for format in [2, 3] {
        let t = old_export(format);
        let (_d, mut r) = project();
        r.import(&approver(), "work", t.path()).unwrap();
        let target = r.read().block(&work(), &id("r-AAAAA")).unwrap();
        let want = std::collections::BTreeMap::from([(target.id.clone(), target.content_hash())]);
        for c in ["c-AAAAA", "c-BBBBB"] {
            assert_eq!(
                r.read().block(&work(), &id(c)).unwrap().pins,
                want,
                "format {format}, {c}: every cited target pinned to its content, nothing else"
            );
        }
        assert!(suspect_pairs(&r, &work()).is_empty(), "format {format}");
        let out = tempfile::tempdir().unwrap();
        let dir = out.path().join("x");
        r.read().export(&work(), &dir).unwrap();
        let manifest = std::fs::read_to_string(dir.join("rdy-export.yaml")).unwrap();
        assert!(manifest.contains("format: 3"), "{manifest}");
    }
    let (_d, mut r) = project();
    let e = r
        .import(&approver(), "work", old_export(1).path())
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert!(e.message.contains("unsupported export format 1"), "{e}");
}
