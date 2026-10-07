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

/// An export directory holding `files` (path under `docs/` → text).
fn export_of(files: &[(&str, &str)]) -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(
        t.path().join("rdy-export.yaml"),
        "format: 2\npack: rdy-standard\nscope: canon\n",
    )
    .unwrap();
    for (path, text) in files {
        let p = t.path().join("docs").join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    t
}

/// The block id `w` assigned key `k` to.
fn keyed(w: &rdy_engine::WriteResult, k: &str) -> String {
    w.keys
        .iter()
        .find(|(_, v)| v.as_str() == k)
        .map(|(id, _)| id.clone())
        .unwrap_or_else(|| panic!("no block got key {k:?}: {:?}", w.keys))
}

#[test]
fn akey1_a_second_doc_whose_path_gives_p_gets_a_valid_key() {
    // the leaf "p" is the key; "p-2" would read as a proposal id
    let (_d, mut r) = project();
    let a = r
        .new_doc(
            &writer(),
            "work",
            "glossary-of-terms-and-acronyms-v1/p",
            None,
            Some("P"),
        )
        .unwrap();
    let b = r
        .new_doc(
            &writer(),
            "work",
            "glossary-of-terms-and-acronyms-v2/p",
            None,
            Some("P"),
        )
        .unwrap();
    let pa = keyed(&a, "p");
    let kb: Vec<&String> = b
        .keys
        .iter()
        .filter(|(id, _)| id.starts_with("d-"))
        .map(|(_, k)| k)
        .collect();
    assert_eq!(kb.len(), 1, "{:?}", b.keys);
    assert!(rdy_model::key::problem(kb[0]).is_none(), "{kb:?}");
    assert_ne!(kb[0].to_ascii_lowercase(), "p");
    assert_eq!(key(&r, "work", &pa).as_deref(), Some("p"));
    // a top-level doc "p" made again after a rename
    let (_d, mut r) = project();
    let w = r.new_doc(&writer(), "work", "p", None, Some("P")).unwrap();
    let first = keyed(&w, "p");
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": first, "fields": {"name": "old-p"}}]),
    )
    .unwrap();
    let w = r.new_doc(&writer(), "work", "p", None, Some("P")).unwrap();
    let k = w.keys.values().next().unwrap();
    assert!(rdy_model::key::problem(k).is_none(), "{k:?}");
}

#[test]
fn akey1_an_import_with_a_doc_p_and_a_requirement_keyed_p_is_not_refused() {
    let t = export_of(&[(
        "p.yaml",
        "- id: d-AAAAA\n  type: doc\n  order: a1\n  fields:\n    title: P\n    doc_type: overview\n- id: s-AAAAA\n  type: section\n  parent: d-AAAAA\n  order: a1\n  fields:\n    title: Rules\n- id: r-AAAAA\n  type: requirement\n  parent: s-AAAAA\n  order: a1\n  fields:\n    key: P\n  body: It must.\n",
    )]);
    let (_d, mut r) = project();
    let w = r.import(&approver(), "work", t.path()).unwrap();
    let k = key(&r, "work", "d-AAAAA").unwrap();
    assert!(rdy_model::key::problem(&k).is_none(), "{k:?}");
    assert_ne!(k.to_ascii_lowercase(), "p");
    assert_eq!(key(&r, "work", "r-AAAAA").as_deref(), Some("P"));
    assert_eq!(w.keys.len(), 1, "{:?}", w.keys);
}

#[test]
fn akey1_a_key_two_blocks_hold_in_a_view_is_refused_not_resolved_to_the_first() {
    // two views derive the same key; once a lands, view b holds it twice
    let (_d, mut r) = project();
    let a = r
        .new_doc(
            &writer(),
            "a",
            "engineering-handbook/onboarding/overview",
            None,
            Some("Overview"),
        )
        .unwrap();
    let b = r
        .new_doc(
            &writer(),
            "b",
            "product-handbook/onboarding/overview",
            None,
            Some("Overview"),
        )
        .unwrap();
    let (da, db) = (
        keyed(&a, "onboarding.overview"),
        keyed(&b, "onboarding.overview"),
    );
    land(&mut r, "a").unwrap();
    let vb = Scope::View("b".into());
    let e = r.read().resolve_id(&vb, "onboarding.overview").unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(
        e.message.contains("is held by") && e.message.contains(&da) && e.message.contains(&db),
        "{e}"
    );
    assert_eq!(e.fix, "use the id");
    // a reference in text and an id-taking field are refused the same way
    let e = write(&mut r, "b", json!([
        {"op": "create", "type": "section", "id": "$s", "parent": db, "fields": {"title": "Links"}},
        {"op": "create", "type": "claim", "parent": "$s", "body": "See {{onboarding.overview}}."}
    ]))
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(e.message.contains("is held by"), "{e}");
    let e = write(&mut r, "b", json!([
        {"op": "create", "type": "section", "parent": "onboarding.overview", "fields": {"title": "Links"}}
    ]))
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(e.message.contains("is held by"), "{e}");
    // the id still works
    write(&mut r, "b", json!([
        {"op": "create", "type": "section", "id": "$s", "parent": db, "fields": {"title": "Links"}},
        {"op": "create", "type": "claim", "parent": "$s", "body": format!("See {{{{{db}}}}}.")}
    ]))
    .unwrap();
    // pending a plan ruling, the proposal is refused until b's doc gets another key
    let e = r.propose(&writer(), "b", "b").unwrap_err();
    assert_eq!(e.code, "E_VALIDATION", "{e}");
    assert!(
        e.issues.iter().any(|i| i.code == "E_KEY_CLASH"
            && i.block.as_ref().map(|b| b.to_string()) == Some(db.clone())),
        "{:?}",
        e.issues
    );
    write(
        &mut r,
        "b",
        json!([{"op": "update", "id": db, "fields": {"key": "product.onboarding"}}]),
    )
    .unwrap();
    land(&mut r, "b").unwrap();
    assert_eq!(
        r.read()
            .resolve_id(&Scope::Canon, "onboarding.overview")
            .unwrap()
            .to_string(),
        da
    );
}

#[test]
fn akey1_a_derived_key_that_is_another_blocks_path_is_taken() {
    // a top-level doc "overview" with another key (or none) keeps its path: a deep doc whose
    // leaf is "overview" gets the next suffix
    let (_d, mut r) = project();
    let top = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "overview", "title": "O", "doc_type": "overview", "key": "intro"}}
    ]))
    .unwrap()
    .created["$o"]
    .clone();
    let w = r
        .new_doc(
            &writer(),
            "work",
            "engineering-handbook/how-to-guides-for-everyone/overview",
            None,
            Some("O"),
        )
        .unwrap();
    keyed(&w, "overview-2");
    assert_eq!(
        r.read()
            .resolve_id(&Scope::View("work".into()), "overview")
            .unwrap()
            .to_string(),
        top
    );
    // in one batch, and for a doc with no key at all (made before keys were derived)
    let (_d, mut r) = project();
    raw_commit(
        &mut r,
        "work",
        vec![raw(
            "d-AAAAA",
            "doc",
            None,
            "a1",
            &[("name", "Guide"), ("title", "G"), ("doc_type", "overview")],
            "",
        )],
    );
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "summary", "title": "S", "doc_type": "overview", "key": "first"}},
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "a-very-long-folder-name-for-testing", "title": "F"}},
        {"op": "create", "type": "doc", "id": "$g", "parent": "$f", "fields": {"name": "guide", "title": "G", "doc_type": "overview"}},
        {"op": "create", "type": "doc", "id": "$s", "parent": "$f", "fields": {"name": "summary", "title": "S", "doc_type": "overview"}}
    ]))
    .unwrap();
    assert_eq!(
        key(&r, "work", &w.created["$g"]).as_deref(),
        Some("guide-2")
    );
    assert_eq!(
        key(&r, "work", &w.created["$s"]).as_deref(),
        Some("summary-2")
    );
    let work = Scope::View("work".into());
    assert_eq!(
        r.read().resolve_id(&work, "guide").unwrap().to_string(),
        "d-AAAAA"
    );
    assert_eq!(
        r.read().resolve_id(&work, "summary").unwrap().to_string(),
        w.created["$o"]
    );
}

#[test]
fn akey1_import_counts_a_top_level_path_as_taken() {
    let t = export_of(&[
        (
            "overview.yaml",
            "- id: d-AAAAA\n  type: doc\n  order: a1\n  fields:\n    title: O\n    doc_type: overview\n    key: intro\n",
        ),
        (
            "engineering-handbook/_folder.yaml",
            "id: fd-AAAAA\ntype: folder\norder: a2\n",
        ),
        (
            "engineering-handbook/how-to-guides-for-everyone/_folder.yaml",
            "id: fd-BBBBB\ntype: folder\norder: a1\n",
        ),
        (
            "engineering-handbook/how-to-guides-for-everyone/overview.yaml",
            "- id: d-BBBBB\n  type: doc\n  order: a1\n  fields:\n    title: O\n    doc_type: overview\n",
        ),
    ]);
    let (_d, mut r) = project();
    r.import(&approver(), "work", t.path()).unwrap();
    assert_eq!(key(&r, "work", "d-BBBBB").as_deref(), Some("overview-2"));
    assert_eq!(
        r.read()
            .resolve_id(&Scope::View("work".into()), "overview")
            .unwrap()
            .to_string(),
        "d-AAAAA"
    );
}
