//! akey1: docs and folders created without a key get one from their path, on write and on import
//! (FW-PR3); an explicit key always wins; the key stays on move and rename.
use rdy_engine::{Rdy, Scope};
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::json;

fn key(r: &Rdy, view: &str, id: &str) -> Option<String> {
    r.read()
        .block(&Scope::View(view.into()), &BlockId::parse(id).unwrap())
        .unwrap()
        .field_str("key")
        .map(String::from)
}

#[test]
fn akey1_a_doc_and_a_folder_created_without_a_key_get_one_from_their_path() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "Guides", "title": "Guides"}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "Intro-Page", "title": "Intro", "doc_type": "overview"}}
    ]))
    .unwrap();
    let (f, d) = (w.created["$f"].clone(), w.created["$d"].clone());
    assert_eq!(key(&r, "work", &f).as_deref(), Some("guides"));
    assert_eq!(key(&r, "work", &d).as_deref(), Some("guides.intro-page"));
    assert_eq!(w.keys.len(), 2, "{:?}", w.keys);
    assert_eq!(w.keys[&f], "guides");
    assert_eq!(w.keys[&d], "guides.intro-page");
}

#[test]
fn akey1_an_explicit_key_wins_even_later_in_the_batch() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "One.", "fields": {"key": "SPEC"}},
        {"op": "create", "type": "doc", "id": "$e", "fields": {"name": "kept", "title": "Kept", "doc_type": "overview", "key": "KEEP-1"}}
    ]))
    .unwrap();
    assert_eq!(key(&r, "work", &w.created["$c"]).as_deref(), Some("SPEC"));
    assert_eq!(key(&r, "work", &w.created["$d"]).as_deref(), Some("spec-2"));
    assert_eq!(key(&r, "work", &w.created["$e"]).as_deref(), Some("KEEP-1"));
    assert_eq!(
        w.keys.len(),
        1,
        "an explicit key is not reported as assigned: {:?}",
        w.keys
    );
}

#[test]
fn akey1_the_key_stays_on_rename_and_move_and_a_new_doc_takes_the_next_suffix() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
        {"op": "create", "type": "folder", "id": "$a", "fields": {"name": "archive", "title": "Archive"}}
    ]))
    .unwrap();
    let (d, a) = (w.created["$d"].clone(), w.created["$a"].clone());
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": d, "fields": {"name": "old-notes"}}]),
    )
    .unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "move", "id": d, "parent": a}]),
    )
    .unwrap();
    assert_eq!(key(&r, "work", &d).as_deref(), Some("notes"));
    let w2 = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}}
    ]))
    .unwrap();
    assert_eq!(
        key(&r, "work", &w2.created["$n"]).as_deref(),
        Some("notes-2")
    );
}

#[test]
fn akey1_new_doc_creates_keyed_folders_and_docs() {
    let (_d, mut r) = project();
    let w = r
        .new_doc(&writer(), "work", "team/handbook", None, Some("Handbook"))
        .unwrap();
    let mut assigned: Vec<&str> = w.keys.values().map(String::as_str).collect();
    assigned.sort();
    assert_eq!(assigned, vec!["team", "team.handbook"]);
}

#[test]
fn akey1_import_assigns_keys_and_explicit_ones_win() {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(
        t.path().join("rdy-export.yaml"),
        "format: 2\npack: rdy-standard\nscope: canon\n",
    )
    .unwrap();
    std::fs::create_dir_all(t.path().join("docs")).unwrap();
    std::fs::write(
        t.path().join("docs/spec.yaml"),
        "- id: d-AAAAA\n  type: doc\n  order: a1\n  fields:\n    title: Spec\n    doc_type: overview\n- id: s-AAAAA\n  type: section\n  parent: d-AAAAA\n  order: a1\n  fields:\n    title: Rules\n- id: r-AAAAA\n  type: requirement\n  parent: s-AAAAA\n  order: a1\n  fields:\n    key: REQ-1\n  body: It must.\n",
    )
    .unwrap();
    std::fs::write(
        t.path().join("docs/req-1.yaml"),
        "- id: d-BBBBB\n  type: doc\n  order: a2\n  fields:\n    title: Req\n    doc_type: overview\n",
    )
    .unwrap();
    std::fs::write(
        t.path().join("docs/kept.yaml"),
        "- id: d-CCCCC\n  type: doc\n  order: a3\n  fields:\n    title: Kept\n    doc_type: overview\n    key: KEEP-1\n",
    )
    .unwrap();
    let (_d, mut r) = project();
    let w = r.import(&approver(), "work", t.path()).unwrap();
    assert_eq!(key(&r, "work", "d-AAAAA").as_deref(), Some("spec"));
    assert_eq!(
        key(&r, "work", "d-BBBBB").as_deref(),
        Some("req-1-2"),
        "REQ-1 is held: the requirement keeps it"
    );
    assert_eq!(key(&r, "work", "d-CCCCC").as_deref(), Some("KEEP-1"));
    assert_eq!(key(&r, "work", "r-AAAAA").as_deref(), Some("REQ-1"));
    assert_eq!(w.keys.len(), 2, "{:?}", w.keys);
}
