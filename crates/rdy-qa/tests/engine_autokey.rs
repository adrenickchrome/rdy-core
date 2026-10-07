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
    let p = r.propose(&writer(), "b", "b").unwrap();
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
    let e = r.propose(&writer(), "c", "c").unwrap_err();
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
    let pa = r.propose(&writer(), "a", "a").unwrap();
    assert!(pa.keys.is_empty(), "{:?}", pa.keys);
    r.land(&approver(), &pa.id).unwrap();
    let pb = r.propose(&writer(), "b", "b").unwrap();
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
    let e = r.propose(&writer(), "b", "b").unwrap_err();
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
    write(
        &mut r,
        "b",
        json!([{"op": "update", "id": db, "fields": {"name": "spec-b"}}]),
    )
    .unwrap();
    let p = r.propose(&writer(), "b", "b").unwrap();
    assert_eq!(p.keys.len(), 1, "{:?}", p.keys);
    assert_eq!(p.keys[&db], "spec-b", "re-derived from the doc's path now");
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
    let pa = r.propose(&writer(), "a", "a").unwrap();
    let pb = r.propose(&writer(), "b", "b").unwrap();
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
        let e = r.propose(&writer(), view, view).unwrap_err();
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
    // P2: notes is renamed old-notes and keeps its key; a new doc named notes takes notes-2
    let (_d, mut r) = project();
    let old = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}}
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
    let p = r.propose(&writer(), "e", "rest").unwrap();
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
            None,
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
    let pa = r.propose(&writer(), "a", "a").unwrap();
    let p = r
        .propose_subset(
            &writer(),
            "e",
            "manual",
            Some(std::slice::from_ref(&c1)),
            None,
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
