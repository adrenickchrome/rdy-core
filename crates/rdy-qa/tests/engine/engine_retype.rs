//! rtyp1: a content block changes its type in place and keeps its id (FW-F5); docs, folders and
//! supporting types keep theirs, and no type change adds or drops the presentational trait. A write
//! checks E_PREFIX when it creates a block; import and `rdy check` refuse a prefix the block could
//! not have been retyped from. wcmv1: an update moves its block in the same op.
use rdy_engine::*;
use rdy_model::{BlockId, Op};
use rdy_qa::fixtures::*;
use serde_json::json;

fn view() -> Scope {
    Scope::View("work".into())
}

fn block(r: &Rdy, id: &str) -> rdy_model::Block {
    r.read()
        .block(&view(), &BlockId::parse(id).unwrap())
        .unwrap()
}

#[test]
fn rtyp1_a_claim_becomes_a_requirement_with_the_same_id() {
    let (_d, mut r) = project();
    let (_doc, _s, c) = seed_doc(&mut r, "work");
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": c, "type": "requirement", "fields": {"priority": "must"}}]),
    )
    .unwrap();
    let b = block(&r, &c);
    assert_eq!(b.block_type, "requirement");
    assert_eq!(b.id.to_string(), c, "same id");
    assert_eq!(b.field_str("priority"), Some("must"));
    // the store, export and import keep it: a retyped block re-imports
    let out = tempfile::tempdir().unwrap();
    r.read()
        .export(&Scope::View("work".into()), &out.path().join("x"))
        .unwrap();
    let (_d2, mut r2) = project();
    r2.import(&approver(), "work", &out.path().join("x"))
        .unwrap();
    assert_eq!(block(&r2, &c).block_type, "requirement");
}

#[test]
fn rtyp1_docs_folders_and_supporting_types_keep_their_type() {
    let (_d, mut r) = project();
    let (doc, section, c) = seed_doc(&mut r, "work");
    let e = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": doc, "type": "folder"}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert!(
        e.message.contains("docs and folders keep their type"),
        "{e}"
    );
    let e = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": c, "type": "rationale"}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert!(
        e.message.contains("supporting types keep their type"),
        "{e}"
    );
    let d = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: section,
                statement: "S".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let dec = d.created["$decision"].clone();
    let e = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": dec, "type": "claim"}]),
    )
    .unwrap_err();
    assert!(e.code == E_BAD_INPUT || e.code == "E_FROZEN", "{e}");
    let e = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": c, "type": "no-such"}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_VALIDATION, "{e}");
}

#[test]
fn rtyp1_a_type_that_cannot_hold_the_children_is_refused() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "work");
    let e = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": section, "type": "prose", "body": "x"}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_VALIDATION, "{e}");
    assert!(e.issues.iter().any(|i| i.code == "E_CHILD_TYPE"), "{e:?}");
}

#[test]
fn rtyp1_a_field_the_new_type_lacks_is_refused_unless_unset() {
    let (_d, mut r) = project();
    let (_doc, s, _c) = seed_doc(&mut r, "work");
    let w = write(&mut r, "work", json!([{"op": "create", "type": "term", "id": "$t", "parent": s, "fields": {"name": "Ledger"}, "body": "Books."}])).unwrap();
    let t = w.created["$t"].clone();
    let e = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": t, "type": "claim"}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_VALIDATION, "{e}");
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": t, "type": "claim", "unset": ["name"]}]),
    )
    .unwrap();
    assert_eq!(block(&r, &t).block_type, "claim");
}

#[test]
fn rtyp1_e_prefix_is_a_create_time_check_only() {
    let (_d, mut r) = project();
    let (_doc, s, _c) = seed_doc(&mut r, "work");
    let e = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "id": "r-AAAAA", "parent": s, "body": "x"}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_VALIDATION, "{e}");
    assert!(e.issues.iter().any(|i| i.code == "E_PREFIX"), "{e:?}");
    let pack = rdy_schema::PackSet::with_builtin()
        .resolve("rdy-standard")
        .unwrap();
    let b = rdy_model::Block {
        id: BlockId::parse("r-AAAAA").unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "a1".into(),
        fields: Default::default(),
        body: "x".into(),
        links: Default::default(),
        pins: Default::default(),
    };
    assert!(
        rdy_schema::validate_block(&pack, &b, None)
            .iter()
            .all(|i| i.code != "E_PREFIX")
    );
}

#[test]
fn wcmv1_an_update_edits_and_moves_in_one_op() {
    let (_d, mut r) = project();
    let (doc, s1, c) = seed_doc(&mut r, "work");
    let w = write(&mut r, "work", json!([{"op": "create", "type": "section", "id": "$s2", "parent": doc, "fields": {"title": "T"}}])).unwrap();
    let s2 = w.created["$s2"].clone();
    let res = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": c, "body": "moved and edited", "move_to": {"parent": s2}}]),
    )
    .unwrap();
    let b = block(&r, &c);
    assert_eq!(b.parent.as_ref().map(|p| p.to_string()), Some(s2.clone()));
    assert_eq!(b.body, "moved and edited");
    let change = r
        .read()
        .reader()
        .change(&res.change.parse::<rdy_model::Hash>().unwrap())
        .unwrap();
    assert_eq!(change.ops.len(), 1, "{:?}", change.ops);
    assert!(matches!(change.ops[0], Op::Update { .. }));
    let e = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": s2, "move_to": {"parent": c}}]),
    )
    .unwrap_err();
    assert!(e.code == E_BAD_INPUT || e.code == E_VALIDATION, "{e}");
    let e = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": c, "body": "x"}, {"op": "move", "id": c, "parent": s1}]),
    )
    .unwrap_err();
    assert!(
        e.message.contains("more than once"),
        "a block is still touched once per write: {e}"
    );
}

#[test]
fn wcmv1_a_move_to_into_stable_content_needs_a_briefing() {
    let (_d, mut r) = project();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "page", "lifecycle": "stable"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}}
    ]))
    .unwrap();
    let stable_section = w.created["$s"].clone();
    let (_doc, _s, c) = seed_doc(&mut r, "work");
    let e = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": c, "move_to": {"parent": stable_section}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_BRIEF_REQUIRED, "{e}");
}

#[test]
fn wcmv1_move_to_rejects_unknown_keys() {
    let (_d, mut r) = project();
    let (_doc, s, c) = seed_doc(&mut r, "work");
    let e = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": c, "move_to": {"parent": s, "where": "up"}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
}

#[test]
fn wcmv1_a_move_only_update_keeps_its_pins() {
    let (_d, mut r) = project();
    let (doc, s1, c) = seed_doc(&mut r, "work");
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "prose", "id": "$p", "parent": s1, "body": format!("See {{{{{c}}}}}.")},
        {"op": "create", "type": "section", "id": "$s2", "parent": doc, "fields": {"title": "T"}}
    ]))
    .unwrap();
    let (p, s2) = (w.created["$p"].clone(), w.created["$s2"].clone());
    let cid = BlockId::parse(&c).unwrap();
    let pinned = block(&r, &p).pins[&cid];
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": c, "body": "two"}]),
    )
    .unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": p, "move_to": {"parent": s2}}]),
    )
    .unwrap();
    let b = block(&r, &p);
    assert_eq!(b.parent.as_ref().map(|x| x.to_string()), Some(s2));
    assert_eq!(
        b.pins[&cid], pinned,
        "a move keeps the pins, as a move op does"
    );
}

/// A project whose own pack has a presentational `aside` type (a section may hold one).
fn presentational_project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(
        dir.path().join(".rdy/packs/p.yaml"),
        rdy_qa::golden::PRESENTATIONAL_PACK,
    )
    .unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: p\n").unwrap();
    let r = Rdy::open(dir.path()).unwrap();
    (dir, r)
}

/// A doc named `name` with a section and an aside in it; returns (section, aside).
fn doc_with_aside(r: &mut Rdy, name: &str, lifecycle: &str) -> (String, String) {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": name, "title": "Spec", "doc_type": "page", "lifecycle": lifecycle}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
        {"op": "create", "type": "aside", "id": "$a", "parent": "$s", "body": "Decorative."}
    ]))
    .unwrap();
    (w.created["$s"].clone(), w.created["$a"].clone())
}

#[test]
fn rtyp1_a_retype_keeps_the_presentational_trait() {
    let (_d, mut r) = presentational_project();
    let (stable_section, aside) = doc_with_aside(&mut r, "spec", "stable");
    land(&mut r, "work").unwrap();
    // the control: normative content in stable content needs a briefing
    let e = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": stable_section, "body": "The system must hold."}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_BRIEF_REQUIRED, "{e}");
    // an aside does not become a claim in place, so it cannot skip the briefing
    let e = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": aside, "type": "claim", "body": "The system must hold."}]),
    )
    .unwrap_err();
    assert!(e.code == E_BRIEF_REQUIRED || e.code == E_BAD_INPUT, "{e}");
    assert_eq!(block(&r, &aside).block_type, "aside");
    // nor in a draft doc, moved into stable content in the same op
    let (draft_section, draft_aside) = doc_with_aside(&mut r, "draft", "draft");
    let e = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": draft_aside, "type": "requirement", "body": "The system must hold.",
                "move_to": {"parent": stable_section}}]),
    )
    .unwrap_err();
    assert!(e.code == E_BRIEF_REQUIRED || e.code == E_BAD_INPUT, "{e}");
    assert_eq!(block(&r, &draft_aside).block_type, "aside");
    // the other way: a claim does not become presentational
    let w = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": draft_section, "body": "x"}]),
    )
    .unwrap();
    let e = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": w.created["$c"], "type": "aside"}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert!(
        e.message.contains("presentational types keep their type"),
        "{e}"
    );
}

/// An export of one glossary doc `d-AAAAA` with section `s-AAAAA` and `entry` after it.
fn export_with(entry: &str) -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(
        t.path().join("rdy-export.yaml"),
        "format: 2\npack: rdy-standard\nscope: canon\n",
    )
    .unwrap();
    std::fs::create_dir_all(t.path().join("docs")).unwrap();
    let doc = "- id: d-AAAAA\n  type: doc\n  order: a1\n  fields:\n    title: T\n    doc_type: glossary\n- id: s-AAAAA\n  type: section\n  parent: d-AAAAA\n  order: a1\n  fields:\n    title: Terms\n";
    std::fs::write(t.path().join("docs/t.yaml"), format!("{doc}{entry}")).unwrap();
    t
}

fn prefix_issue(e: &RdyError, id: &str) -> bool {
    e.issues
        .iter()
        .any(|i| i.code == "E_PREFIX" && i.block.as_ref().is_some_and(|b| b.to_string() == id))
}

#[test]
fn rtyp1_import_and_check_refuse_a_prefix_of_a_type_it_could_not_be_retyped_from() {
    let pack = rdy_schema::PackSet::with_builtin()
        .resolve("rdy-standard")
        .unwrap();
    // a claim's id on a supporting type, and a decision's id on a claim
    for (id, entry) in [
        (
            "c-AAAAA",
            "- id: c-AAAAA\n  type: rationale\n  parent: s-AAAAA\n  order: a2\n  body: Why.\n",
        ),
        (
            "D-AAAAA",
            "- id: D-AAAAA\n  type: claim\n  parent: s-AAAAA\n  order: a2\n  body: Holds.\n",
        ),
    ] {
        let t = export_with(entry);
        let (_d, mut r) = project();
        let e = r.import(&approver(), "work", t.path()).unwrap_err();
        assert_eq!(e.code, E_VALIDATION, "{e}");
        assert!(prefix_issue(&e, id), "{id}: {e:?}");
        let diags = check_path(&pack, None, t.path()).unwrap();
        assert!(
            diags
                .iter()
                .any(|d| d.code == "E_PREFIX" && d.line == 13 && d.message.contains(id)),
            "{id}: {diags:?}"
        );
    }
    // a claim retyped to a requirement keeps its id and imports
    let t = export_with(
        "- id: c-AAAAA\n  type: requirement\n  parent: s-AAAAA\n  order: a2\n  fields:\n    priority: must\n  body: Holds.\n",
    );
    let (_d, mut r) = project();
    r.import(&approver(), "work", t.path()).unwrap();
    assert_eq!(block(&r, "c-AAAAA").block_type, "requirement");
    assert!(
        check_path(&pack, None, t.path())
            .unwrap()
            .iter()
            .all(|d| d.code != "E_PREFIX")
    );
}

#[test]
fn rtyp1_import_refuses_a_presentational_prefix_on_a_normative_type() {
    let t = export_with(
        "- id: as-AAAAA\n  type: claim\n  parent: s-AAAAA\n  order: a2\n  body: Holds.\n",
    );
    std::fs::write(
        t.path().join("rdy-export.yaml"),
        "format: 2\npack: p\nscope: canon\n",
    )
    .unwrap();
    let (_d, mut r) = presentational_project();
    let e = r.import(&approver(), "work", t.path()).unwrap_err();
    assert_eq!(e.code, E_VALIDATION, "{e}");
    assert!(prefix_issue(&e, "as-AAAAA"), "{e:?}");
}

#[test]
fn hd3_a_move_with_parent_id_is_refused_and_names_parent() {
    let (_d, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "f", "title": "F"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$f",
             "fields": {"name": "d", "title": "D", "doc_type": "glossary"}}
        ]),
    )
    .unwrap();
    let (f, d) = (w.created["$f"].clone(), w.created["$d"].clone());
    let e = write(
        &mut r,
        "work",
        json!([{"op": "move", "id": d, "parent_id": f}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert_eq!(e.message, "op 0: unknown field \"parent_id\"");
    assert!(
        e.fix
            .starts_with("did you mean \"parent\"? the fields are "),
        "{e}"
    );
    assert_eq!(
        block(&r, &d).parent.map(|p| p.to_string()),
        Some(f),
        "nothing moved"
    );
}

#[test]
fn hd3_an_unknown_key_in_a_link_object_is_refused() {
    let (_d, mut r) = project();
    let (_doc, s, c) = seed_doc(&mut r, "work");
    let e = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "claim", "parent": s,
        "body": "Two.", "links": {"refines": [{"target": c, "prat": "x"}]}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    assert_eq!(
        e.message, "op 0: refines: unknown field \"prat\" in a link",
        "{e}"
    );
    assert_eq!(e.fix, "did you mean \"part\"? rename it", "{e}");
}
