//! akey1: docs and folders created without a key get one from their path, on write and on import
//! (FW-PR3); an explicit key always wins and stays on move and rename; a key rdy assigned follows
//! the path of the block a write renames or moves (FW-PR3a, kfol1; FW-PR3b, kroot1) and yields to
//! a top-level name equal to it.
use rdy_engine::{Rdy, Scope};
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use rdy_qa::intake::TEST_SUMMARY;
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
fn akey1_an_rdy_key_follows_rename_and_move_and_a_new_doc_takes_the_key_it_left() {
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
    assert_eq!(key(&r, "work", &d).as_deref(), Some("archive.old-notes"));
    let w2 = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}}
    ]))
    .unwrap();
    assert_eq!(key(&r, "work", &w2.created["$n"]).as_deref(), Some("notes"));
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
    write(&mut r, "c", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "intro", "title": "I", "doc_type": "overview", "key": "onboarding.overview"}}
    ]))
    .unwrap();
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
    // K1: the key rdy gave b's doc is re-derived when b is proposed, and the result lists it
    let p = r.propose(&writer(), "b", "b", Some(TEST_SUMMARY)).unwrap();
    assert_eq!(p.keys.len(), 1, "{:?}", p.keys);
    assert_eq!(p.keys[&db], "onboarding.overview-2");
    assert_eq!(key(&r, "b", &db).as_deref(), Some("onboarding.overview-2"));
    assert_eq!(
        r.read()
            .resolve_id(&vb, "onboarding.overview")
            .unwrap()
            .to_string(),
        da
    );
    r.land(&approver(), &p.id).unwrap();
    assert_eq!(
        r.read()
            .resolve_id(&Scope::Canon, "onboarding.overview")
            .unwrap()
            .to_string(),
        da
    );
    // a key an agent set is still refused: view c gave its doc the key before a landed
    let e = r.propose(&writer(), "c", "c", None).unwrap_err();
    assert_eq!(e.code, "E_VALIDATION", "{e}");
    assert!(
        e.issues.iter().any(|i| i.code == "E_KEY_CLASH"),
        "{:?}",
        e.issues
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

/// Whether `id` in `view` carries the marker of a key rdy derived (`fields.key_auto: true`).
fn key_auto(r: &Rdy, view: &str, id: &str) -> bool {
    r.read()
        .block(&Scope::View(view.into()), &BlockId::parse(id).unwrap())
        .unwrap()
        .fields
        .get("key_auto")
        == Some(&rdy_model::FieldValue::Bool(true))
}

#[test]
fn rkey1_two_views_that_derive_the_same_key_both_land() {
    // P4: each view's doc derives onboarding.overview; the second proposal re-derives its key
    let (_d, mut r) = project();
    let a = r
        .new_doc(
            &writer(),
            "a",
            "engineering-handbook/onboarding/overview",
            None,
            Some("O"),
        )
        .unwrap();
    let b = r
        .new_doc(
            &writer(),
            "b",
            "product-handbook/onboarding/overview",
            None,
            Some("O"),
        )
        .unwrap();
    let (da, db) = (
        keyed(&a, "onboarding.overview"),
        keyed(&b, "onboarding.overview"),
    );
    assert!(key_auto(&r, "a", &da) && key_auto(&r, "b", &db));
    let pa = r.propose(&writer(), "a", "a", Some(TEST_SUMMARY)).unwrap();
    assert!(pa.keys.is_empty(), "{:?}", pa.keys);
    r.land(&approver(), &pa.id).unwrap();
    let pb = r.propose(&writer(), "b", "b", Some(TEST_SUMMARY)).unwrap();
    assert_eq!(pb.keys.len(), 1, "{:?}", pb.keys);
    assert_eq!(pb.keys[&db], "onboarding.overview-2");
    let l = r.land_report(&approver(), &pb.id).unwrap();
    assert!(
        l.keys.is_empty(),
        "nothing clashed since the proposal: {:?}",
        l.keys
    );
    assert_eq!(key(&r, "b", &da).as_deref(), Some("onboarding.overview"));
    assert_eq!(key(&r, "b", &db).as_deref(), Some("onboarding.overview-2"));
    assert!(key_auto(&r, "b", &db), "a re-derived key is still rdy's");
    let c = Scope::Canon;
    assert_eq!(
        r.read()
            .resolve_id(&c, "onboarding.overview-2")
            .unwrap()
            .to_string(),
        db
    );
}

#[test]
fn rkey1_two_top_level_spec_docs_land_once_one_is_renamed() {
    // P5: both views create a top-level doc "spec"; the name clash is the agent's to fix, the
    // key clash is not, and renaming the doc is enough
    let (_d, mut r) = project();
    let doc = json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}}
    ]);
    let da = write(&mut r, "a", doc.clone()).unwrap().created["$d"].clone();
    let db = write(&mut r, "b", doc).unwrap().created["$d"].clone();
    land(&mut r, "a").unwrap();
    let e = r.propose(&writer(), "b", "b", None).unwrap_err();
    assert_eq!(e.code, "E_VALIDATION", "{e}");
    assert!(
        e.issues.iter().any(|i| i.code == "E_PATH_CLASH"),
        "{:?}",
        e.issues
    );
    assert!(
        !e.issues.iter().any(|i| i.code == "E_KEY_CLASH"),
        "no agent set the key: {:?}",
        e.issues
    );
    assert_eq!(
        key(&r, "b", &db).as_deref(),
        Some("spec"),
        "a refused proposal writes nothing"
    );
    let w = write(
        &mut r,
        "b",
        json!([{"op": "update", "id": db, "fields": {"name": "spec-b"}}]),
    )
    .unwrap();
    assert_eq!(
        w.keys[&db], "spec-b",
        "the key follows the rename (FW-PR3a)"
    );
    let p = r.propose(&writer(), "b", "b", Some(TEST_SUMMARY)).unwrap();
    assert!(p.keys.is_empty(), "nothing clashes now: {:?}", p.keys);
    r.land(&approver(), &p.id).unwrap();
    let c = Scope::Canon;
    assert_eq!(r.read().resolve_id(&c, "spec").unwrap().to_string(), da);
    assert_eq!(key(&r, "b", &db).as_deref(), Some("spec-b"));
}

#[test]
fn rkey1_a_clash_that_appears_after_the_proposal_is_re_derived_at_landing() {
    let (_d, mut r) = project();
    let doc = json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "a-long-folder-name-for-team-b-docs", "title": "B"}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "guide", "title": "G", "doc_type": "overview"}}
    ]);
    write(&mut r, "a", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "G", "doc_type": "overview"}}
    ]))
    .unwrap();
    let w = write(&mut r, "b", doc).unwrap();
    let db = w.created["$d"].clone();
    assert_eq!(w.keys[&db], "guide");
    let pa = r.propose(&writer(), "a", "a", Some(TEST_SUMMARY)).unwrap();
    let pb = r.propose(&writer(), "b", "b", Some(TEST_SUMMARY)).unwrap();
    assert!(
        pb.keys.is_empty(),
        "no clash when b is proposed: {:?}",
        pb.keys
    );
    r.land(&approver(), &pa.id).unwrap();
    let l = r.land_report(&approver(), &pb.id).unwrap();
    assert_eq!(l.keys.len(), 1, "{:?}", l.keys);
    assert_eq!(l.keys[&db], "guide-2");
    let c = Scope::Canon;
    assert_eq!(r.read().resolve_id(&c, "guide-2").unwrap().to_string(), db);
}

#[test]
fn rkey1_an_explicit_key_still_refuses_and_setting_a_key_makes_it_explicit() {
    let (_d, mut r) = project();
    let da = write(&mut r, "a", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "G", "doc_type": "overview"}}
    ]))
    .unwrap()
    .created["$d"]
    .clone();
    // given on create
    let db = write(&mut r, "b", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "other", "title": "O", "doc_type": "overview", "key": "guide"}}
    ]))
    .unwrap()
    .created["$d"]
    .clone();
    assert!(!key_auto(&r, "b", &db), "an explicit key is not rdy's");
    // set later on a doc that had a derived key
    let dc = write(&mut r, "c", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "third", "title": "T", "doc_type": "overview"}}
    ]))
    .unwrap()
    .created["$d"]
    .clone();
    assert!(key_auto(&r, "c", &dc));
    write(
        &mut r,
        "c",
        json!([{"op": "update", "id": dc, "fields": {"key": "GUIDE"}}]),
    )
    .unwrap();
    assert!(!key_auto(&r, "c", &dc), "a key the agent sets is explicit");
    land(&mut r, "a").unwrap();
    for (view, id) in [("b", &db), ("c", &dc)] {
        let e = r.propose(&writer(), view, view, None).unwrap_err();
        assert_eq!(e.code, "E_VALIDATION", "{view}: {e}");
        assert!(
            e.issues.iter().any(|i| i.code == "E_KEY_CLASH"
                && i.block.as_ref().map(|b| b.to_string()).as_deref() == Some(id.as_str())),
            "{view}: {:?}",
            e.issues
        );
    }
    assert_eq!(
        r.read()
            .resolve_id(&Scope::Canon, "guide")
            .unwrap()
            .to_string(),
        da
    );
}

#[test]
fn kamb1_a_key_that_is_another_docs_path_names_both_blocks_and_the_fix() {
    // P2 with an explicit key (FW-PR3a): notes, keyed notes by the agent, is renamed old-notes
    // and keeps its key; a new doc named notes takes notes-2
    let (_d, mut r) = project();
    let old = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview", "key": "notes"}}
    ]))
    .unwrap()
    .created["$d"]
    .clone();
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": old, "fields": {"name": "old-notes"}}]),
    )
    .unwrap();
    let new = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}}
    ]))
    .unwrap()
    .created["$d"]
    .clone();
    assert_eq!(key(&r, "work", &new).as_deref(), Some("notes-2"));
    let check = |e: &rdy_engine::RdyError| {
        assert_eq!(e.code, "E_BAD_INPUT", "{e}");
        assert!(
            e.message.contains(&format!("key of {old} (old-notes)"))
                && e.message.contains(&format!("path of {new}")),
            "{e}"
        );
        assert_eq!(
            e.fix,
            format!(
                "address it by id ({old} holds the key, {new} the path), or rename one: change the key of {old} or the name of {new}"
            )
        );
    };
    let work = Scope::View("work".into());
    check(&r.read().resolve_id(&work, "notes").unwrap_err());
    check(
        &write(
            &mut r,
            "work",
            json!([
                {"op": "create", "type": "section", "parent": "notes", "fields": {"title": "S"}}
            ]),
        )
        .unwrap_err(),
    );
    // the ids still work
    write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "section", "parent": new, "fields": {"title": "S"}}
        ]),
    )
    .unwrap();
}

/// The single issue code of a refused write, with the error's code.
fn refused(e: &rdy_engine::RdyError) -> (&str, Vec<&str>) {
    (e.code, e.issues.iter().map(|i| i.code).collect())
}

#[test]
fn rkey1_an_agent_cannot_set_key_auto_and_any_key_it_sets_is_explicit() {
    // final review 1 (Q1, Q2a, Q2b): only rdy marks a key as its own
    let (_d, mut r) = project();
    let e = write(&mut r, "b", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "other", "title": "O", "doc_type": "overview", "key": "guide", "key_auto": true}}
    ]))
    .unwrap_err();
    assert_eq!(
        refused(&e),
        ("E_VALIDATION", vec!["E_FIELD_UNKNOWN"]),
        "{e}"
    );
    assert!(
        e.message.contains("key_auto") && e.message.contains("rdy sets it"),
        "{e}"
    );
    let w = write(&mut r, "b", json!([
        {"op": "create", "type": "doc", "id": "$x", "fields": {"name": "third", "title": "T", "doc_type": "overview", "key": "third-key"}},
        {"op": "create", "type": "doc", "id": "$y", "fields": {"name": "fourth", "title": "F", "doc_type": "overview"}}
    ]))
    .unwrap();
    let (x, y) = (w.created["$x"].clone(), w.created["$y"].clone());
    assert!(key_auto(&r, "b", &y));
    for id in [&x, &y] {
        let e = write(
            &mut r,
            "b",
            json!([{"op": "update", "id": id, "fields": {"key_auto": true}}]),
        )
        .unwrap_err();
        assert_eq!(
            refused(&e),
            ("E_VALIDATION", vec!["E_FIELD_UNKNOWN"]),
            "{id}: {e}"
        );
    }
    assert!(!key_auto(&r, "b", &x), "a refused update writes nothing");
    write(
        &mut r,
        "b",
        json!([{"op": "update", "id": y, "fields": {"key": "fourth"}}]),
    )
    .unwrap();
    assert_eq!(key(&r, "b", &y).as_deref(), Some("fourth"));
    assert!(
        !key_auto(&r, "b", &y),
        "a key the agent writes is explicit, even the same value"
    );
}

#[test]
fn rkey1_an_import_of_an_export_keeps_rdys_marker() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "G", "doc_type": "overview"}},
        {"op": "create", "type": "doc", "id": "$e", "fields": {"name": "mine", "title": "M", "doc_type": "overview", "key": "MINE-1"}}
    ]))
    .unwrap();
    let (d, e) = (w.created["$d"].clone(), w.created["$e"].clone());
    let out = tempfile::tempdir().unwrap();
    r.read()
        .export(&Scope::View("work".into()), &out.path().join("x"))
        .unwrap();
    let (_d2, mut r2) = project();
    let i = r2
        .import(&approver(), "work", &out.path().join("x"))
        .unwrap();
    assert!(
        i.keys.is_empty(),
        "every block came with its key: {:?}",
        i.keys
    );
    assert_eq!(key(&r2, "work", &d).as_deref(), Some("guide"));
    assert!(key_auto(&r2, "work", &d), "rdy's own export round-trips");
    assert!(!key_auto(&r2, "work", &e));
}

/// View `e`: c1 puts doc `manual` under a long folder (derived key `manual`); c2 creates doc
/// `other2` with the explicit key `manual-2`. View `a` creates the top-level doc `manual`.
/// Returns (c1's change, the doc of c1, the doc of c2).
fn manual_views(r: &mut Rdy) -> (String, String, String) {
    write(r, "a", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "manual", "title": "M", "doc_type": "overview"}}
    ]))
    .unwrap();
    let c1 = write(r, "e", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "a-very-long-folder-name-for-e-docs", "title": "E"}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "manual", "title": "M", "doc_type": "overview"}}
    ]))
    .unwrap();
    let m = c1.created["$d"].clone();
    assert_eq!(c1.keys[&m], "manual");
    let c2 = write(r, "e", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "other2", "title": "O", "doc_type": "overview", "key": "manual-2"}}
    ]))
    .unwrap();
    (c1.change, m, c2.created["$d"].clone())
}

/// After c1 landed with re-derived key `manual-3`, the view's own explicit `manual-2` still
/// resolves and proposes.
fn manual_2_stays_the_agents(r: &mut Rdy, m: &str, o: &str) {
    let e = Scope::View("e".into());
    assert_eq!(r.read().resolve_id(&e, "manual-2").unwrap().to_string(), o);
    assert_eq!(r.read().resolve_id(&e, "manual-3").unwrap().to_string(), m);
    let p = r
        .propose(&writer(), "e", "rest", Some(TEST_SUMMARY))
        .unwrap();
    r.land(&approver(), &p.id).unwrap();
    assert_eq!(
        r.read()
            .resolve_id(&Scope::Canon, "manual-2")
            .unwrap()
            .to_string(),
        o
    );
}

#[test]
fn rkey1_a_subset_proposal_does_not_re_key_onto_a_key_its_own_view_holds() {
    // final review 2 (Q3): the re-derived key skips manual-2, which the unproposed c2 holds
    let (_d, mut r) = project();
    let (c1, m, o) = manual_views(&mut r);
    land(&mut r, "a").unwrap();
    let p = r
        .propose_subset(
            &writer(),
            "e",
            "manual",
            Some(std::slice::from_ref(&c1)),
            Some(TEST_SUMMARY),
        )
        .unwrap();
    assert_eq!(p.keys.len(), 1, "{:?}", p.keys);
    assert_eq!(p.keys[&m], "manual-3");
    r.land(&approver(), &p.id).unwrap();
    manual_2_stays_the_agents(&mut r, &m, &o);
}

#[test]
fn rkey1_a_land_time_re_key_does_not_take_a_key_the_view_holds() {
    let (_d, mut r) = project();
    let (c1, m, o) = manual_views(&mut r);
    let pa = r.propose(&writer(), "a", "a", Some(TEST_SUMMARY)).unwrap();
    let p = r
        .propose_subset(
            &writer(),
            "e",
            "manual",
            Some(std::slice::from_ref(&c1)),
            Some(TEST_SUMMARY),
        )
        .unwrap();
    assert!(
        p.keys.is_empty(),
        "no clash when e is proposed: {:?}",
        p.keys
    );
    r.land(&approver(), &pa.id).unwrap();
    let l = r.land_report(&approver(), &p.id).unwrap();
    assert_eq!(l.keys.len(), 1, "{:?}", l.keys);
    assert_eq!(l.keys[&m], "manual-3");
    manual_2_stays_the_agents(&mut r, &m, &o);
}

// kfol1 (FW-PR3a, D-0KPYK; FW-PR3b): on a move or rename, a key rdy assigned to the block the
// write renames or moves follows the new path, re-derived with the usual suffix rule, and keeps
// its marker; an explicit key stays.

/// Creates the top-level doc `name` in `view` without a key and returns its id.
fn doc(r: &mut Rdy, view: &str, name: &str) -> String {
    write(r, view, json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": name, "title": "T", "doc_type": "overview"}}
    ]))
    .unwrap()
    .created["$d"]
    .clone()
}

#[test]
fn kfol1_a_renamed_doc_s_rdy_key_follows_its_path_and_the_result_lists_it() {
    let (_d, mut r) = project();
    let d = doc(&mut r, "work", "notes");
    assert_eq!(key(&r, "work", &d).as_deref(), Some("notes"));
    let w = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": d, "fields": {"name": "old-notes"}}]),
    )
    .unwrap();
    assert_eq!(key(&r, "work", &d).as_deref(), Some("old-notes"));
    assert!(key_auto(&r, "work", &d), "the key is still rdy's");
    assert_eq!(w.keys.len(), 1, "{:?}", w.keys);
    assert_eq!(w.keys[&d], "old-notes");
    // an edit that keeps the path keeps the key, and lists nothing
    let w = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": d, "fields": {"title": "Old notes"}}]),
    )
    .unwrap();
    assert!(w.keys.is_empty(), "{:?}", w.keys);
    assert_eq!(key(&r, "work", &d).as_deref(), Some("old-notes"));
}

#[test]
fn kfol1_a_new_doc_takes_the_name_and_key_a_renamed_doc_left() {
    // P2, which K2 refused: the old key followed the rename, so `notes` names one block
    let (_d, mut r) = project();
    let old = doc(&mut r, "work", "notes");
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": old, "fields": {"name": "old-notes"}}]),
    )
    .unwrap();
    let new = doc(&mut r, "work", "notes");
    assert_eq!(key(&r, "work", &new).as_deref(), Some("notes"));
    let work = Scope::View("work".into());
    assert_eq!(
        r.read().resolve_id(&work, "notes").unwrap().to_string(),
        new
    );
    assert_eq!(
        r.read().resolve_id(&work, "old-notes").unwrap().to_string(),
        old
    );
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "section", "parent": "notes", "fields": {"title": "S"}}]),
    )
    .unwrap();
    // the rename and the new doc in one write: the new doc still gets `notes`
    let (_d2, mut r2) = project();
    let old2 = doc(&mut r2, "work", "notes");
    let w = write(&mut r2, "work", json!([
        {"op": "update", "id": old2, "fields": {"name": "old-notes"}},
        {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "T", "doc_type": "overview"}}
    ]))
    .unwrap();
    let new2 = w.created["$n"].clone();
    assert_eq!(w.keys[&old2], "old-notes");
    assert_eq!(w.keys[&new2], "notes");
}

#[test]
fn kfol1_an_explicit_key_stays_through_rename_and_move() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "N", "doc_type": "overview", "key": "NOTES-1"}},
        {"op": "create", "type": "folder", "id": "$a", "fields": {"name": "archive", "title": "A"}}
    ]))
    .unwrap();
    let (d, a) = (w.created["$d"].clone(), w.created["$a"].clone());
    let w1 = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": d, "fields": {"name": "old-notes"}}]),
    )
    .unwrap();
    let w2 = write(
        &mut r,
        "work",
        json!([{"op": "move", "id": d, "parent": a}]),
    )
    .unwrap();
    assert!(
        w1.keys.is_empty() && w2.keys.is_empty(),
        "{:?} {:?}",
        w1.keys,
        w2.keys
    );
    assert_eq!(key(&r, "work", &d).as_deref(), Some("NOTES-1"));
    assert!(!key_auto(&r, "work", &d));
}

#[test]
fn kfol1_moving_or_renaming_a_folder_re_derives_only_its_own_rdy_key() {
    // FW-PR3b: the blocks under a moved or renamed folder keep their keys
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$g", "fields": {"name": "guides", "title": "G"}},
        {"op": "create", "type": "doc", "id": "$i", "parent": "$g", "fields": {"name": "intro", "title": "I", "doc_type": "overview"}},
        {"op": "create", "type": "folder", "id": "$s", "parent": "$g", "fields": {"name": "sub", "title": "S"}},
        {"op": "create", "type": "doc", "id": "$e", "parent": "$s", "fields": {"name": "deep", "title": "D", "doc_type": "overview"}},
        {"op": "create", "type": "doc", "id": "$x", "parent": "$g", "fields": {"name": "mine", "title": "M", "doc_type": "overview", "key": "MINE-1"}},
        {"op": "create", "type": "folder", "id": "$a", "fields": {"name": "archive", "title": "A"}}
    ]))
    .unwrap();
    let id = |p: &str| w.created[p].clone();
    let (g, i, s, e, x, a) = (id("$g"), id("$i"), id("$s"), id("$e"), id("$x"), id("$a"));
    assert_eq!(key(&r, "work", &e).as_deref(), Some("guides.sub.deep"));
    let m = write(
        &mut r,
        "work",
        json!([{"op": "move", "id": g, "parent": a}]),
    )
    .unwrap();
    assert_eq!(m.keys.len(), 1, "{:?}", m.keys);
    assert_eq!(m.keys[&g], "archive.guides");
    assert!(key_auto(&r, "work", &g));
    let kept = [
        (&i, "guides.intro"),
        (&s, "guides.sub"),
        (&e, "guides.sub.deep"),
        (&x, "MINE-1"),
    ];
    for (b, k) in kept {
        assert_eq!(key(&r, "work", b).as_deref(), Some(k), "{b}");
    }
    assert_eq!(
        key(&r, "work", &a).as_deref(),
        Some("archive"),
        "the new parent keeps its key"
    );
    let n = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": s, "fields": {"name": "inner"}}]),
    )
    .unwrap();
    assert_eq!(n.keys.len(), 1, "{:?}", n.keys);
    assert_eq!(n.keys[&s], "archive.guides.inner");
    assert_eq!(key(&r, "work", &e).as_deref(), Some("guides.sub.deep"));
    assert_eq!(key(&r, "work", &i).as_deref(), Some("guides.intro"));
}

#[test]
fn kfol1_a_re_derived_key_that_is_taken_gets_the_next_suffix() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "other", "title": "O", "doc_type": "overview", "key": "old-notes"}},
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "N", "doc_type": "overview"}}
    ]))
    .unwrap();
    let (o, d) = (w.created["$o"].clone(), w.created["$d"].clone());
    let w = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": d, "fields": {"name": "old-notes"}}]),
    )
    .unwrap();
    assert_eq!(w.keys[&d], "old-notes-2");
    assert_eq!(key(&r, "work", &d).as_deref(), Some("old-notes-2"));
    assert_eq!(key(&r, "work", &o).as_deref(), Some("old-notes"));
    assert!(key_auto(&r, "work", &d));
}

/// The kfol1 scenario: (path, key) of every doc and folder in `work`, sorted.
fn kfol1_scenario() -> Vec<(String, String)> {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$g", "fields": {"name": "guides", "title": "G"}},
        {"op": "create", "type": "doc", "id": "$i", "parent": "$g", "fields": {"name": "intro", "title": "I", "doc_type": "overview"}},
        {"op": "create", "type": "doc", "id": "$j", "parent": "$g", "fields": {"name": "notes", "title": "J", "doc_type": "overview"}},
        {"op": "create", "type": "folder", "id": "$a", "fields": {"name": "archive", "title": "A"}},
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "taken", "title": "O", "doc_type": "overview", "key": "archive.guides"}}
    ]))
    .unwrap();
    let (g, a) = (w.created["$g"].clone(), w.created["$a"].clone());
    write(
        &mut r,
        "work",
        json!([{"op": "move", "id": g, "parent": a}]),
    )
    .unwrap();
    doc(&mut r, "work", "guides");
    let st = r.read();
    let m = st.materialized(&Scope::View("work".into())).unwrap();
    let mut out: Vec<(String, String)> = m
        .iter()
        .filter(|b| rdy_model::tree::is_tree_type(&b.block_type))
        .map(|b| {
            (
                m.path_of(&b.id).unwrap(),
                b.field_str("key").unwrap().to_string(),
            )
        })
        .collect();
    out.sort();
    out
}

#[test]
fn kfol1_re_derivation_is_deterministic() {
    let first = kfol1_scenario();
    assert!(
        first.contains(&("archive/guides".into(), "archive.guides-2".into())),
        "{first:?}"
    );
    assert!(
        first.contains(&("archive/guides/intro".into(), "guides.intro".into())),
        "{first:?}"
    );
    assert!(
        first.contains(&("guides".into(), "guides".into())),
        "{first:?}"
    );
    for _ in 0..3 {
        assert_eq!(kfol1_scenario(), first);
    }
}

#[test]
fn kfol1_two_views_whose_moves_derive_the_same_key_both_land() {
    // K1 stands: a key re-derived on a rename that another view landed first is re-derived again
    let (_d, mut r) = project();
    let d = doc(&mut r, "work", "draft");
    land(&mut r, "work").unwrap();
    let a = write(&mut r, "a", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "a-long-folder-name-for-team-a-docs", "title": "A"}},
        {"op": "create", "type": "doc", "id": "$g", "parent": "$f", "fields": {"name": "guide", "title": "G", "doc_type": "overview"}}
    ]))
    .unwrap();
    let ga = a.created["$g"].clone();
    assert_eq!(a.keys[&ga], "guide");
    let w = write(
        &mut r,
        "b",
        json!([{"op": "update", "id": d, "fields": {"name": "guide"}}]),
    )
    .unwrap();
    assert_eq!(w.keys[&d], "guide");
    land(&mut r, "a").unwrap();
    let p = r.propose(&writer(), "b", "b", Some(TEST_SUMMARY)).unwrap();
    assert_eq!(p.keys.len(), 1, "{:?}", p.keys);
    assert_eq!(p.keys[&d], "guide-2");
    r.land(&approver(), &p.id).unwrap();
    let c = Scope::Canon;
    assert_eq!(r.read().resolve_id(&c, "guide-2").unwrap().to_string(), d);
}

// kroot1 (FW-PR3b, refines D-0KPYK): on a move or rename only the block the write renames or
// moves gets its rdy key re-derived; the blocks under it keep theirs, so a folder move makes no
// suspects, no Edited marks and no conflicts. An rdy key yields to a new or renamed top-level
// name equal to it; an explicit key wins (K2 names both ids).

/// guides/{intro (rdy key), mine (explicit MINE-1)}, archive/, and z with claims citing both
/// docs. Returns (guides, intro, mine, archive, claim citing intro, claim citing mine).
fn kroot1_setup(r: &mut Rdy) -> (String, String, String, String, String, String) {
    let w = write(r, "work", json!([
        {"op": "create", "type": "folder", "id": "$g", "fields": {"name": "guides", "title": "G"}},
        {"op": "create", "type": "doc", "id": "$i", "parent": "$g", "fields": {"name": "intro", "title": "I", "doc_type": "overview"}},
        {"op": "create", "type": "doc", "id": "$x", "parent": "$g", "fields": {"name": "mine", "title": "M", "doc_type": "overview", "key": "MINE-1"}},
        {"op": "create", "type": "folder", "id": "$a", "fields": {"name": "archive", "title": "A"}},
        {"op": "create", "type": "doc", "id": "$z", "fields": {"name": "z", "title": "Z", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$z", "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "See {{$i}}."},
        {"op": "create", "type": "claim", "id": "$c2", "parent": "$s", "body": "See {{$x}}."}
    ]))
    .unwrap();
    let id = |p: &str| w.created[p].clone();
    (id("$g"), id("$i"), id("$x"), id("$a"), id("$c"), id("$c2"))
}

#[test]
fn kroot1_a_folder_move_makes_no_citer_of_a_doc_under_it_suspect() {
    let (_d, mut r) = project();
    let (g, i, _x, a, c, c2) = kroot1_setup(&mut r);
    let work = Scope::View("work".into());
    let before = r
        .read()
        .block(&work, &BlockId::parse(&i).unwrap())
        .unwrap()
        .content_hash();
    let m = write(
        &mut r,
        "work",
        json!([{"op": "move", "id": g, "parent": a}]),
    )
    .unwrap();
    assert_eq!(m.keys.len(), 1, "only the moved folder: {:?}", m.keys);
    assert_eq!(m.keys[&g], "archive.guides");
    assert_eq!(key(&r, "work", &i).as_deref(), Some("guides.intro"));
    assert!(key_auto(&r, "work", &i));
    let after = r
        .read()
        .block(&work, &BlockId::parse(&i).unwrap())
        .unwrap()
        .content_hash();
    assert_eq!(before, after, "the doc under the folder is unchanged");
    let sus = r.read().suspects(&work).unwrap();
    assert!(
        !sus.iter()
            .any(|s| s.block.to_string() == c || s.block.to_string() == c2),
        "{sus:?}"
    );
}

#[test]
fn kroot1_a_folder_move_gives_no_doc_under_it_an_edited_mark() {
    let (_d, mut r) = project();
    let (g, i, x, a, _c, _c2) = kroot1_setup(&mut r);
    land(&mut r, "work").unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "move", "id": g, "parent": a}]),
    )
    .unwrap();
    let cmp = r
        .read()
        .compare(&Scope::View("work".into()), &Scope::Canon)
        .unwrap();
    let changed: Vec<(&String, _)> = cmp.changed.iter().map(|e| (&e.id, &e.fields)).collect();
    assert!(
        !cmp.changed.iter().any(|e| e.id == i || e.id == x),
        "{changed:?}"
    );
    // the folder itself moved, and its own key changed with it
    assert!(
        cmp.moved.iter().any(|e| e.id == g) || cmp.changed.iter().any(|e| e.id == g),
        "{changed:?}"
    );
}

#[test]
fn kroot1_an_agent_that_read_a_doc_before_its_folder_moved_can_still_edit_it() {
    let (_d, mut r) = project();
    let (g, i, _x, a, _c, _c2) = kroot1_setup(&mut r);
    let base = version_of(&r, &Scope::View("work".into()), &i);
    write(
        &mut r,
        "work",
        json!([{"op": "move", "id": g, "parent": a}]),
    )
    .unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": i, "base": base, "fields": {"title": "I2"}}]),
    )
    .unwrap();
}

#[test]
fn kroot1_a_title_edit_of_a_doc_lands_after_another_view_moves_its_folder() {
    let (_d, mut r) = project();
    let (g, i, x, a, _c, _c2) = kroot1_setup(&mut r);
    land(&mut r, "work").unwrap();
    write(&mut r, "va", json!([{"op": "move", "id": g, "parent": a}])).unwrap();
    write(
        &mut r,
        "vb",
        json!([
            {"op": "update", "id": i, "fields": {"title": "Intro two"}},
            {"op": "update", "id": x, "fields": {"title": "Mine two"}}
        ]),
    )
    .unwrap();
    land(&mut r, "va").unwrap();
    land(&mut r, "vb").unwrap();
    let st = r.read();
    assert!(st.conflicts(&Scope::Canon).unwrap().is_empty());
    let b = st
        .block(&Scope::Canon, &BlockId::parse(&i).unwrap())
        .unwrap();
    assert_eq!(b.field_str("title"), Some("Intro two"));
    assert_eq!(b.field_str("key"), Some("guides.intro"));
    assert_eq!(
        st.materialized(&Scope::Canon)
            .unwrap()
            .path_of(&BlockId::parse(&i).unwrap())
            .as_deref(),
        Some("archive/guides/intro")
    );
}

#[test]
fn kroot1_a_top_level_name_takes_an_rdy_key_and_the_holder_yields() {
    // F5: top-level doc a.b while a/b holds the rdy key a.b
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "a", "title": "A"}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "b", "title": "B", "doc_type": "overview"}}
    ]))
    .unwrap();
    let d = w.created["$d"].clone();
    assert_eq!(key(&r, "work", &d).as_deref(), Some("a.b"));
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "a.b", "title": "AB", "doc_type": "overview"}}
    ]))
    .unwrap();
    let n = w.created["$n"].clone();
    assert_eq!(w.keys[&d], "a.b-2", "{:?}", w.keys);
    assert_eq!(key(&r, "work", &d).as_deref(), Some("a.b-2"));
    assert!(key_auto(&r, "work", &d), "the yielded key is still rdy's");
    let work = Scope::View("work".into());
    assert_eq!(r.read().resolve_id(&work, "a.b").unwrap().to_string(), n);
    assert_eq!(r.read().resolve_id(&work, "a.b-2").unwrap().to_string(), d);
    // a rename to a top-level name makes an rdy key yield too
    let o = doc(&mut r, "work", "other");
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$e", "parent": "a", "fields": {"name": "c", "title": "C", "doc_type": "overview"}}
    ]))
    .unwrap();
    let e = w.created["$e"].clone();
    assert_eq!(key(&r, "work", &e).as_deref(), Some("a.c"));
    let w = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": o, "fields": {"name": "a.c"}}]),
    )
    .unwrap();
    assert_eq!(w.keys[&e], "a.c-2", "{:?}", w.keys);
    assert_eq!(w.keys[&o], "a.c", "{:?}", w.keys);
    assert_eq!(r.read().resolve_id(&work, "a.c").unwrap().to_string(), o);
}

#[test]
fn kroot1_an_explicit_key_still_refuses_against_a_new_top_level_name() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "a", "title": "A"}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "b", "title": "B", "doc_type": "overview", "key": "a.b"}}
    ]))
    .unwrap();
    let d = w.created["$d"].clone();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "a.b", "title": "AB", "doc_type": "overview"}}
    ]))
    .unwrap();
    let n = w.created["$n"].clone();
    assert!(!w.keys.contains_key(&d), "{:?}", w.keys);
    assert_eq!(key(&r, "work", &d).as_deref(), Some("a.b"));
    assert!(!key_auto(&r, "work", &d));
    let e = r
        .read()
        .resolve_id(&Scope::View("work".into()), "a.b")
        .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(
        e.message.contains(&format!("key of {d}")) && e.message.contains(&format!("path of {n}")),
        "{e}"
    );
}

#[test]
fn kroot1_the_renamed_or_moved_block_s_own_rdy_key_follows_its_path() {
    let (_d, mut r) = project();
    let (g, i, _x, a, _c, _c2) = kroot1_setup(&mut r);
    let w = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": g, "fields": {"name": "manuals"}}]),
    )
    .unwrap();
    assert_eq!(w.keys.len(), 1, "{:?}", w.keys);
    assert_eq!(w.keys[&g], "manuals");
    assert_eq!(key(&r, "work", &i).as_deref(), Some("guides.intro"));
    let w = write(
        &mut r,
        "work",
        json!([{"op": "move", "id": i, "parent": a}]),
    )
    .unwrap();
    assert_eq!(w.keys.len(), 1, "{:?}", w.keys);
    assert_eq!(w.keys[&i], "archive.intro");
    assert_eq!(key(&r, "work", &i).as_deref(), Some("archive.intro"));
    assert!(key_auto(&r, "work", &i));
    // both moved in one write: each key follows its final path
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "move", "id": i, "parent": g},
            {"op": "move", "id": g, "parent": a}
        ]),
    )
    .unwrap();
    assert_eq!(w.keys[&i], "archive.manuals.intro", "{:?}", w.keys);
    assert_eq!(w.keys[&g], "archive.manuals", "{:?}", w.keys);
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "move", "id": g, "parent": null},
            {"op": "update", "id": i, "fields": {"title": "I3"}}
        ]),
    )
    .unwrap();
    assert_eq!(w.keys.len(), 1, "{:?}", w.keys);
    assert_eq!(w.keys[&g], "manuals");
    assert_eq!(
        key(&r, "work", &i).as_deref(),
        Some("archive.manuals.intro"),
        "a doc edited in the same write as its folder's move is not moved by it"
    );
}
