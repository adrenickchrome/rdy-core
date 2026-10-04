use rdy_engine::*;
use rdy_model::*;
use serde_json::json;

pub fn agent() -> Session {
    Session {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
    }
}
pub fn founder() -> Session {
    Session {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
    }
}
pub fn write(r: &mut Rdy, view: &str, ops: serde_json::Value) -> Result<WriteResult, RdyError> {
    r.write(
        &agent(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: String::new(),
        },
    )
}
pub fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}
fn name(r: &mut Rdy, id: &str) -> String {
    r.block(&Scope::View("work".into()), &BlockId::parse(id).unwrap())
        .unwrap()
        .field_str("name")
        .unwrap()
        .to_string()
}

#[test]
fn docs_get_a_name_at_creation() {
    let (_d, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$a", "fields": {"title": "Wallet Design", "doc_type": "overview"}},
        {"op": "create", "type": "doc", "id": "$b", "fields": {"title": "Wallet design", "doc_type": "overview"}},
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "Specs"}}
    ])).unwrap();
    let (a, b) = (res.created["$a"].clone(), res.created["$b"].clone());
    assert_eq!(name(&mut r, &a), "wallet-design");
    assert_eq!(name(&mut r, &b), "wallet-design-2");
    assert_eq!(res.names[&a], "wallet-design");
    assert!(
        !res.names.contains_key(&res.created["$f"]),
        "a given name is not 'assigned'"
    );
    // later writes de-duplicate against what exists
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "fields": {"title": "Wallet design", "doc_type": "overview"}}
    ])).unwrap();
    assert_eq!(name(&mut r, &res.created["op0"]), "wallet-design-3");
    // a title edit never renames
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": a, "fields": {"title": "Ledger"}}]),
    )
    .unwrap();
    assert_eq!(name(&mut r, &a), "wallet-design");
}

#[test]
fn assigned_names_fall_back_to_untitled_when_the_slug_is_invalid() {
    let (_d, mut r) = project();
    for title in ["Index", "CON", "notes.yaml", "日本語"] {
        let res = write(
            &mut r,
            "work",
            json!([
                {"op": "create", "type": "doc", "fields": {"title": title, "doc_type": "overview"}}
            ]),
        )
        .unwrap();
        let id = res.created["op0"].clone();
        let expected = format!("untitled-{}", id.split_once('-').unwrap().1.to_lowercase());
        assert_eq!(name(&mut r, &id), expected, "{title}");
    }
}

#[test]
fn doc_summaries_show_the_title() {
    let (_d, mut r) = project();
    let res = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "fields": {"title": "Wallet Design", "doc_type": "overview"}}
    ])).unwrap();
    let doc = r
        .block(
            &Scope::View("work".into()),
            &BlockId::parse(&res.created["op0"]).unwrap(),
        )
        .unwrap();
    assert_eq!(summary(&doc), "Wallet Design");
}

/// A nameless doc written straight to the store, as M1 wrote docs.
pub fn legacy_doc(r: &mut Rdy, view: &str, id: &str, title: &str, parent: Option<&str>) -> BlockId {
    let bid = BlockId::parse(id).unwrap();
    let mut fields = std::collections::BTreeMap::new();
    fields.insert("title".to_string(), FieldValue::Str(title.into()));
    fields.insert("doc_type".to_string(), FieldValue::Str("glossary".into()));
    let block = Block {
        id: bid.clone(),
        block_type: "doc".into(),
        parent: parent.map(|p| BlockId::parse(p).unwrap()),
        order: "V".into(),
        fields,
        body: String::new(),
        links: Default::default(),
        pins: Default::default(),
    };
    r.store_mut()
        .commit_to_view(
            view,
            Change {
                view: view.into(),
                ops: vec![Op::Create { block }],
                deps: vec![],
                provenance: agent().provenance(),
                justification: None,
                message: "m1".into(),
            },
        )
        .unwrap();
    r.invalidate();
    bid
}

#[test]
fn a_legacy_view_with_a_nameless_doc_still_lands() {
    let (_d, mut r) = project();
    let id = legacy_doc(&mut r, "old", "d-AAAAB", "Legacy notes", None);
    let p = r.propose(&agent(), "old", "legacy").unwrap();
    r.land(&founder(), &p.id).unwrap();
    assert!(
        r.block(&Scope::Canon, &id)
            .unwrap()
            .field_str("name")
            .is_none(),
        "landing doesn't rewrite it"
    );
}

fn tree(r: &mut Rdy) -> WriteResult {
    write(r, "work", json!([
        {"op": "create", "type": "folder", "id": "$w", "fields": {"name": "wallets"}},
        {"op": "create", "type": "doc", "id": "$t", "parent": "$w", "fields": {"title": "Transactions", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$t", "fields": {"title": "Summary"}},
        {"op": "create", "type": "folder", "id": "$c", "parent": "$w", "fields": {"name": "cards"}}
    ])).unwrap()
}

#[test]
fn an_explicit_invalid_name_is_refused_with_a_corrected_name() {
    let (_d, mut r) = project();
    let e = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "fields": {"name": "has space"}}
        ]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_NAME");
    assert_eq!(
        e.corrected,
        Some(json!({"op": 0, "fields": {"name": "has-space"}}))
    );
}

#[test]
fn names_clash_ignoring_case_across_docs_and_folders() {
    let (_d, mut r) = project();
    let res = tree(&mut r);
    let e = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "fields": {"name": "Wallets"}}
        ]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_PATH_CLASH");
    assert!(
        e.message.contains(&res.created["$w"]),
        "names the other block: {e}"
    );
    assert_eq!(
        e.corrected,
        Some(json!({"op": 0, "fields": {"name": "Wallets-2"}}))
    );
    // a doc may not share a folder's name either (wallets.yaml next to wallets/)
    let e = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "fields": {"name": "wallets", "title": "W", "doc_type": "overview"}}
    ])).unwrap_err();
    assert_eq!(e.code, "E_PATH_CLASH");
    // renaming onto a sibling
    let e = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": res.created["$c"], "fields": {"name": "TRANSACTIONS"}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_PATH_CLASH");
    // moving into a folder that holds the same name
    let other = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "fields": {"name": "transactions", "title": "Other", "doc_type": "overview"}}
    ])).unwrap();
    let e = write(
        &mut r,
        "work",
        json!([{"op": "move", "id": other.created["op0"], "parent": "wallets"}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_PATH_CLASH");
    // a move takes no fields: no corrected payload; the fix says to rename first
    assert_eq!(e.corrected, None);
    assert!(
        e.fix.contains("rename") && e.fix.contains("update"),
        "{}",
        e.fix
    );
}

#[test]
fn ops_take_paths_for_ids_parents_and_links() {
    let (_d, mut r) = project();
    let res = tree(&mut r);
    let made = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "parent": "wallets/cards", "fields": {"title": "Limits", "doc_type": "overview"}},
        {"op": "update", "id": "wallets/transactions", "fields": {"lifecycle": "stable"}}
    ])).unwrap();
    let limits = r
        .block(
            &Scope::View("work".into()),
            &BlockId::parse(&made.created["op0"]).unwrap(),
        )
        .unwrap();
    assert_eq!(
        limits.parent.as_ref().map(|p| p.to_string()),
        Some(res.created["$c"].clone())
    );
    let e = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": "wallets/nope", "fields": {"lifecycle": "stable"}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_UNKNOWN_BLOCK");
    assert!(e.fix.contains("transactions"), "{e}");
}

#[test]
fn moves_reach_the_top_level_and_refuse_cycles() {
    let (_d, mut r) = project();
    let res = tree(&mut r);
    write(
        &mut r,
        "work",
        json!([{"op": "move", "id": res.created["$t"]}]),
    )
    .unwrap();
    assert_eq!(
        r.path_of(
            &Scope::View("work".into()),
            &BlockId::parse(&res.created["$t"]).unwrap()
        )
        .unwrap()
        .as_deref(),
        Some("transactions")
    );
    let e = write(
        &mut r,
        "work",
        json!([{"op": "move", "id": res.created["$s"]}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_VALIDATION");
    assert!(e.issues.iter().any(|i| i.code == "E_ORPHAN"));
    let e = write(
        &mut r,
        "work",
        json!([{"op": "move", "id": res.created["$w"], "parent": res.created["$c"]}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "a folder can't move inside itself");
    let e = write(
        &mut r,
        "work",
        json!([{"op": "delete", "id": res.created["$w"]}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_HAS_CHILDREN");
}

#[test]
fn a_nameless_doc_gets_its_effective_name_on_first_write() {
    let (_d, mut r) = project();
    let id = legacy_doc(&mut r, "work", "d-AAAAB", "Legacy Notes", None);
    let path = r.path_of(&Scope::View("work".into()), &id).unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": id.to_string(), "fields": {"lifecycle": "draft"}}]),
    )
    .unwrap();
    let b = r.block(&Scope::View("work".into()), &id).unwrap();
    assert_eq!(b.field_str("name"), Some("legacy-notes"));
    assert_eq!(
        r.path_of(&Scope::View("work".into()), &id).unwrap(),
        path,
        "the path didn't change"
    );
}

/// Ops creating a chain of `n` nested folders f0/f1/…, the first under `parent`.
fn chain(n: usize, parent: Option<&str>) -> serde_json::Value {
    json!((0..n)
        .map(|i| {
            let p = if i == 0 {
                parent.map(String::from)
            } else {
                Some(format!("$f{}", i - 1))
            };
            json!({"op": "create", "type": "folder", "id": format!("$f{i}"), "parent": p, "fields": {"name": format!("f{i}")}})
        })
        .collect::<Vec<_>>())
}

#[test]
fn folders_nest_at_most_32_deep() {
    assert_eq!(tree::MAX_FOLDER_DEPTH, 32);
    let (_d, mut r) = project();
    let e = write(&mut r, "work", chain(33, None)).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(e.fix.contains("at most 32 deep"), "{e}");
    let res = write(&mut r, "work", chain(32, None)).unwrap();
    let deepest = res.created["$f31"].clone();
    // a doc in the deepest folder is fine; another folder is not
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "parent": deepest, "fields": {"title": "Leaf", "doc_type": "overview"}}
    ]))
    .unwrap();
    let e = write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "folder", "parent": deepest, "fields": {"name": "x"}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    // moving a two-deep subtree under f30 would put its lower folder at depth 33
    let pair = write(&mut r, "work", chain(2, None)).unwrap_err();
    assert_eq!(pair.code, "E_PATH_CLASH", "f0 exists at the top level");
    let pair = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "folder", "id": "$a", "fields": {"name": "a"}},
            {"op": "create", "type": "folder", "id": "$b", "parent": "$a", "fields": {"name": "b"}}
        ]),
    )
    .unwrap();
    let e = write(
        &mut r,
        "work",
        json!([{"op": "move", "id": pair.created["$a"], "parent": res.created["$f30"]}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    write(
        &mut r,
        "work",
        json!([{"op": "move", "id": pair.created["$a"], "parent": res.created["$f29"]}]),
    )
    .unwrap();
}

#[test]
fn tree_walks_survive_a_damaged_store_with_a_very_deep_chain() {
    let (_d, mut r) = project();
    let mut ops = vec![];
    let mut parent: Option<BlockId> = None;
    for i in 0..5000 {
        let id = BlockId::parse(&format!("fd-{:05}", i)).unwrap();
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("name".to_string(), FieldValue::Str(format!("f{i}")));
        ops.push(Op::Create {
            block: Block {
                id: id.clone(),
                block_type: "folder".into(),
                parent: parent.clone(),
                order: "V".into(),
                fields,
                body: String::new(),
                links: Default::default(),
                pins: Default::default(),
            },
        });
        parent = Some(id);
    }
    r.store_mut()
        .commit_to_view(
            "work",
            Change {
                view: "work".into(),
                ops,
                deps: vec![],
                provenance: agent().provenance(),
                justification: None,
                message: "damaged".into(),
            },
        )
        .unwrap();
    r.invalidate();
    let work = Scope::View("work".into());
    let t = r.tree(&work, None, None).unwrap();
    assert_eq!(t.len(), 1);
    let out = tempfile::tempdir().unwrap();
    let e = r.export(&work, &out.path().join("x")).unwrap_err();
    assert_eq!(e.code, "E_IO", "{e}");
    assert!(e.message.contains("not inside"), "{e}");
}

#[test]
fn a_doc_in_the_deepest_folder_exports_and_round_trips() {
    let (_d, mut r) = project();
    let res = write(&mut r, "work", chain(32, None)).unwrap();
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "parent": res.created["$f31"], "fields": {"title": "Leaf", "doc_type": "overview"}}
    ]))
    .unwrap();
    let p = r.propose(&agent(), "work", "deep").unwrap();
    r.land(&founder(), &p.id).unwrap();
    let original = r
        .materialized(&Scope::Canon)
        .unwrap()
        .snapshot
        .blocks
        .clone();

    let out = tempfile::tempdir().unwrap();
    let summary = r.export(&Scope::Canon, out.path()).unwrap();
    assert_eq!(summary.folders, 32);
    assert_eq!(summary.docs, 1);

    let (_d2, mut b) = project();
    b.import(&founder(), "import", out.path()).unwrap();
    let p = b.propose(&founder(), "import", "migrate").unwrap();
    b.land(&founder(), &p.id).unwrap();
    let imported = b
        .materialized(&Scope::Canon)
        .unwrap()
        .snapshot
        .blocks
        .clone();
    assert_eq!(imported, original);
}
