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
    let (_d, mut r, res) = tree();
    let work = Scope::View("work".into());
    assert_eq!(
        r.path_of(&work, &id(&res, "$w")).unwrap().as_deref(),
        Some("wallets")
    );
    assert_eq!(
        r.path_of(&work, &id(&res, "$t")).unwrap().as_deref(),
        Some("wallets/transactions")
    );
    assert_eq!(
        r.path_of(&work, &id(&res, "$s")).unwrap().as_deref(),
        Some("wallets/transactions"),
        "inner blocks have their doc's path"
    );
    assert_eq!(
        r.path_of(&work, &id(&res, "$c")).unwrap().as_deref(),
        Some("wallets/cards")
    );
    assert_eq!(
        r.path_of(&work, &BlockId::parse("c-ZZZZZ").unwrap())
            .unwrap_err()
            .code,
        "E_UNKNOWN_BLOCK"
    );
    assert_eq!(
        r.resolve_path(&work, "wallets/transactions").unwrap(),
        id(&res, "$t")
    );
    assert_eq!(
        r.resolve_path(&work, "Wallets/TRANSACTIONS").unwrap(),
        id(&res, "$t"),
        "unique case-insensitive match"
    );
    assert_eq!(r.resolve_id(&work, "glossary").unwrap(), id(&res, "$g"));
    assert_eq!(
        r.resolve_id(&work, &res.created["$s"]).unwrap(),
        id(&res, "$s"),
        "id-shaped text is an id"
    );
}

#[test]
fn paths_ignore_leading_and_trailing_slashes() {
    let (_d, mut r, res) = tree();
    let work = Scope::View("work".into());
    for p in [
        "/wallets/",
        "wallets/transactions/",
        "/wallets/transactions",
    ] {
        assert!(r.resolve_path(&work, p).is_ok(), "{p}");
    }
    assert_eq!(r.resolve_path(&work, "/wallets/").unwrap(), id(&res, "$w"));
    assert_eq!(
        r.resolve_path(&work, "/").unwrap_err().code,
        "E_UNKNOWN_BLOCK"
    );
}

#[test]
fn an_unknown_path_lists_the_nearest_folder() {
    let (_d, mut r, _) = tree();
    let e = r
        .resolve_path(&Scope::View("work".into()), "wallets/nope")
        .unwrap_err();
    assert_eq!(e.code, "E_UNKNOWN_BLOCK");
    assert!(
        e.fix.contains("transactions") && e.fix.contains("cards"),
        "{e}"
    );
    let e = r
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
        r.resolve_path(&work, "Specs").unwrap().as_str(),
        "fd-AAAAA",
        "exact match wins"
    );
    let e = r.resolve_path(&work, "SPECS").unwrap_err();
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
        r.path_of(&work, &id(&res, "$t")).unwrap().as_deref(),
        Some("purses/transactions")
    );
    write(
        &mut r,
        json!([{"op": "move", "id": res.created["$t"], "parent": res.created["$c"]}]),
    );
    assert_eq!(
        r.path_of(&work, &id(&res, "$t")).unwrap().as_deref(),
        Some("purses/cards/transactions")
    );
    assert_eq!(
        r.resolve_path(&work, "purses/cards/transactions").unwrap(),
        id(&res, "$t")
    );
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
        r.path_of(&work, &BlockId::parse("d-AAAAA").unwrap())
            .unwrap()
            .as_deref(),
        Some("notes")
    );
    assert_eq!(
        r.path_of(&work, &BlockId::parse("d-BBBBB").unwrap())
            .unwrap()
            .as_deref(),
        Some("notes-2"),
        "de-duplicated in id order"
    );
    assert_eq!(
        r.path_of(&work, &BlockId::parse("d-CCCCC").unwrap())
            .unwrap()
            .as_deref(),
        Some("untitled-ccccc")
    );
    assert_eq!(
        r.resolve_path(&work, "notes-2").unwrap().as_str(),
        "d-BBBBB"
    );
}
