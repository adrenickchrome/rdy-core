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
