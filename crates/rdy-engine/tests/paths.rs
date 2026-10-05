use rdy_engine::*;
use rdy_model::*;
use serde_json::json;

fn agent() -> Session {
    Session {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
    }
}
fn write(r: &mut Rdy, ops: serde_json::Value) -> WriteResult {
    r.write(
        &agent(),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap()
}
fn raw_create(r: &mut Rdy, block: Block) {
    r.store_mut()
        .commit_to_view(
            "work",
            Change {
                view: "work".into(),
                ops: vec![Op::Create { block }],
                deps: vec![],
                provenance: agent().provenance(),
                justification: None,
                message: "raw".into(),
                links: ChangeLinks::default(),
            },
        )
        .unwrap();
    r.invalidate();
}
fn raw(id: &str, ty: &str, parent: Option<&str>, fields: &[(&str, &str)]) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: ty.into(),
        parent: parent.map(|p| BlockId::parse(p).unwrap()),
        order: "V".into(),
        fields: fields
            .iter()
            .map(|(k, v)| (k.to_string(), FieldValue::Str(v.to_string())))
            .collect(),
        body: String::new(),
        links: Default::default(),
        pins: Default::default(),
    }
}

/// wallets/ { transactions (doc: section) , cards/ }, glossary (doc)
fn tree() -> (tempfile::TempDir, Rdy, WriteResult) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let res = write(
        &mut r,
        json!([
            {"op": "create", "type": "folder", "id": "$w", "fields": {"name": "wallets"}},
            {"op": "create", "type": "doc", "id": "$t", "parent": "$w", "fields": {"title": "Transactions", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$t", "fields": {"title": "Summary"}},
            {"op": "create", "type": "folder", "id": "$c", "parent": "$w", "fields": {"name": "cards"}},
            {"op": "create", "type": "doc", "id": "$g", "fields": {"title": "Glossary", "doc_type": "glossary"}}
        ]),
    );
    (dir, r, res)
}

fn id(res: &WriteResult, k: &str) -> BlockId {
    BlockId::parse(&res.created[k]).unwrap()
}

#[test]
fn path_of_and_resolve_path() {
    let (_d, r, res) = tree();
    let work = Scope::View("work".into());
    assert_eq!(
        r.read().path_of(&work, &id(&res, "$w")).unwrap().as_deref(),
        Some("wallets")
    );
    assert_eq!(
        r.read().path_of(&work, &id(&res, "$t")).unwrap().as_deref(),
        Some("wallets/transactions")
    );
    assert_eq!(
        r.read().path_of(&work, &id(&res, "$s")).unwrap().as_deref(),
        Some("wallets/transactions"),
        "inner blocks have their doc's path"
    );
    assert_eq!(
        r.read().path_of(&work, &id(&res, "$c")).unwrap().as_deref(),
        Some("wallets/cards")
    );
    assert_eq!(
        r.read()
            .path_of(&work, &BlockId::parse("c-ZZZZZ").unwrap())
            .unwrap_err()
            .code,
        "E_UNKNOWN_BLOCK"
    );
    assert_eq!(
        r.read()
            .resolve_path(&work, "wallets/transactions")
            .unwrap(),
        id(&res, "$t")
    );
    assert_eq!(
        r.read()
            .resolve_path(&work, "Wallets/TRANSACTIONS")
            .unwrap(),
        id(&res, "$t"),
        "unique case-insensitive match"
    );
    assert_eq!(
        r.read().resolve_id(&work, "glossary").unwrap(),
        id(&res, "$g")
    );
    assert_eq!(
        r.read().resolve_id(&work, &res.created["$s"]).unwrap(),
        id(&res, "$s"),
        "id-shaped text is an id"
    );
}

#[test]
fn paths_ignore_leading_and_trailing_slashes() {
    let (_d, r, res) = tree();
    let work = Scope::View("work".into());
    for p in [
        "/wallets/",
        "wallets/transactions/",
        "/wallets/transactions",
    ] {
        assert!(r.read().resolve_path(&work, p).is_ok(), "{p}");
    }
    assert_eq!(
        r.read().resolve_path(&work, "/wallets/").unwrap(),
        id(&res, "$w")
    );
    assert_eq!(
        r.read().resolve_path(&work, "/").unwrap_err().code,
        "E_UNKNOWN_BLOCK"
    );
}

#[test]
fn an_unknown_path_lists_the_nearest_folder() {
    let (_d, r, _) = tree();
    let e = r
        .read()
        .resolve_path(&Scope::View("work".into()), "wallets/nope")
        .unwrap_err();
    assert_eq!(e.code, "E_UNKNOWN_BLOCK");
    assert!(
        e.fix.contains("transactions") && e.fix.contains("cards"),
        "{e}"
    );
    let e = r
        .read()
        .resolve_path(&Scope::View("work".into()), "nowhere/x")
        .unwrap_err();
    assert!(
        e.fix.contains("the top level") && e.fix.contains("wallets"),
        "{e}"
    );
}

#[test]
fn an_ambiguous_case_insensitive_path_is_refused() {
    let (_d, mut r, _) = tree();
    // two siblings differing only in case can only come from outside the write path
    raw_create(
        &mut r,
        raw("fd-AAAAA", "folder", None, &[("name", "Specs")]),
    );
    raw_create(
        &mut r,
        raw("fd-BBBBB", "folder", None, &[("name", "specs")]),
    );
    let work = Scope::View("work".into());
    assert_eq!(
        r.read().resolve_path(&work, "Specs").unwrap().as_str(),
        "fd-AAAAA",
        "exact match wins"
    );
    let e = r.read().resolve_path(&work, "SPECS").unwrap_err();
    assert_eq!(e.code, "E_UNKNOWN_BLOCK");
    assert!(e.message.contains("more than one"), "{e}");
}

#[test]
fn paths_follow_renames_and_moves() {
    let (_d, mut r, res) = tree();
    let work = Scope::View("work".into());
    write(
        &mut r,
        json!([{"op": "update", "id": res.created["$w"], "fields": {"name": "purses"}}]),
    );
    assert_eq!(
        r.read().path_of(&work, &id(&res, "$t")).unwrap().as_deref(),
        Some("purses/transactions")
    );
    assert!(r.read().resolve_path(&work, "wallets").is_err());
    write(
        &mut r,
        json!([{"op": "move", "id": res.created["$t"], "parent": res.created["$c"]}]),
    );
    assert_eq!(
        r.read().path_of(&work, &id(&res, "$t")).unwrap().as_deref(),
        Some("purses/cards/transactions")
    );
    assert_eq!(
        r.read()
            .resolve_path(&work, "purses/cards/transactions")
            .unwrap(),
        id(&res, "$t")
    );
    assert!(r.read().resolve_path(&work, "purses/transactions").is_err());
}

#[test]
fn nameless_docs_have_effective_names() {
    let (_d, mut r, _) = tree();
    for (i, title) in [
        ("d-AAAAA", "Notes"),
        ("d-BBBBB", "Notes"),
        ("d-CCCCC", "日本語"),
    ] {
        raw_create(
            &mut r,
            raw(
                i,
                "doc",
                None,
                &[("title", title), ("doc_type", "glossary")],
            ),
        );
    }
    let work = Scope::View("work".into());
    assert_eq!(
        r.read()
            .path_of(&work, &BlockId::parse("d-AAAAA").unwrap())
            .unwrap()
            .as_deref(),
        Some("notes")
    );
    assert_eq!(
        r.read()
            .path_of(&work, &BlockId::parse("d-BBBBB").unwrap())
            .unwrap()
            .as_deref(),
        Some("notes-2"),
        "de-duplicated in id order"
    );
    assert_eq!(
        r.read()
            .path_of(&work, &BlockId::parse("d-CCCCC").unwrap())
            .unwrap()
            .as_deref(),
        Some("untitled-ccccc")
    );
    assert_eq!(
        r.read().resolve_path(&work, "notes-2").unwrap().as_str(),
        "d-BBBBB"
    );
    // the derived names are cached per parent: renaming d-AAAAA frees "notes" for d-BBBBB
    write(
        &mut r,
        json!([{"op": "update", "id": "d-AAAAA", "fields": {"name": "alpha"}}]),
    );
    assert_eq!(
        r.read()
            .path_of(&work, &BlockId::parse("d-BBBBB").unwrap())
            .unwrap()
            .as_deref(),
        Some("notes")
    );
    assert_eq!(
        r.read().resolve_path(&work, "notes").unwrap().as_str(),
        "d-BBBBB"
    );
    assert_eq!(
        r.read().resolve_path(&work, "alpha").unwrap().as_str(),
        "d-AAAAA"
    );
}

#[test]
fn new_folder_and_doc_create_missing_parents_in_one_change() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let before = r.store().view("work").unwrap().changes.len();
    let res = r
        .new_doc(
            &agent(),
            "work",
            "wallets/cards/limits",
            None,
            Some("Card limits"),
        )
        .unwrap();
    assert_eq!(
        r.store().view("work").unwrap().changes.len(),
        before + 1,
        "one change"
    );
    let work = Scope::View("work".into());
    let doc = r
        .read()
        .resolve_path(&work, "wallets/cards/limits")
        .unwrap();
    let b = r.read().block(&work, &doc).unwrap();
    assert_eq!(
        (b.field_str("title"), b.field_str("doc_type")),
        (Some("Card limits"), Some("overview"))
    );
    let sections: Vec<String> = r
        .read()
        .materialized(&work)
        .unwrap()
        .children(Some(&doc))
        .iter()
        .filter_map(|s| s.field_str("title").map(String::from))
        .collect();
    assert_eq!(sections, ["Summary", "Goals", "Non-goals", "Context"]);
    assert!(res.created.values().any(|v| v == doc.as_str()));
    r.new_folder(&agent(), "work", "wallets/archive").unwrap();
    assert_eq!(
        r.new_folder(&agent(), "work", "Wallets/Cards")
            .unwrap_err()
            .code,
        "E_PATH_CLASH"
    );
    assert_eq!(
        r.new_doc(&agent(), "work", "wallets/cards/limits/x", None, None)
            .unwrap_err()
            .code,
        "E_BAD_INPUT",
        "limits is a doc"
    );
    let g = r
        .new_doc(&agent(), "work", "glossary", Some("glossary"), None)
        .unwrap();
    let gid = BlockId::parse(
        &g.created
            .values()
            .find(|v| v.starts_with("d-"))
            .unwrap()
            .clone(),
    )
    .unwrap();
    assert_eq!(
        r.read().block(&work, &gid).unwrap().field_str("title"),
        Some("glossary")
    );
}

#[test]
fn new_doc_needs_a_type_when_the_pack_has_no_default() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".rdy/packs")).unwrap();
    std::fs::write(dir.path().join(".rdy/packs/plain.yaml"), "name: plain\nversion: \"1\"\nblock_types:\n  doc: { prefix: d, children: [section] }\n  section: { prefix: s, fields: { title: { kind: str, required: true } } }\ndoc_types:\n  memo: { skeleton: [{ title: Body, required: true }] }\n").unwrap();
    let mut r = Rdy::init(dir.path(), "plain").unwrap();
    let e = r
        .new_doc(&agent(), "work", "notes", None, None)
        .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert!(e.fix.contains("memo"), "{e}");
    r.new_doc(&agent(), "work", "notes", Some("memo"), None)
        .unwrap();
}

#[test]
fn tree_lists_folders_and_docs_with_depth() {
    let (_d, r, res) = tree();
    let work = Scope::View("work".into());
    let all = r.read().tree(&work, None, None).unwrap();
    assert_eq!(
        all.iter().map(|n| n.path.as_str()).collect::<Vec<_>>(),
        ["wallets", "glossary"]
    );
    assert_eq!(
        all[0]
            .children
            .iter()
            .map(|n| n.path.as_str())
            .collect::<Vec<_>>(),
        ["wallets/transactions", "wallets/cards"]
    );
    assert_eq!(all[0].children[0].doc_type.as_deref(), Some("overview"));
    let shallow = r.read().tree(&work, None, Some(0)).unwrap();
    assert!(shallow[0].children.is_empty());
    assert_eq!(shallow[0].collapsed, 2);
    let sub = r.read().tree(&work, Some("wallets/cards"), None).unwrap();
    assert_eq!(sub.len(), 1);
    assert_eq!(sub[0].id, res.created["$c"]);
}
