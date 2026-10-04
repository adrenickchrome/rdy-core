use rdy_engine::*;
use rdy_model::*;
use serde_json::json;

fn session(role: Role) -> Session {
    Session {
        actor: "human:founder".into(),
        run: None,
        role,
    }
}
fn writer() -> Session {
    session(Role::Writer)
}
fn founder() -> Session {
    session(Role::Approver)
}

fn land_one(r: &mut Rdy, title: &str) -> String {
    r.write(
        &writer(),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(json!([
                {"op": "create", "type": "doc", "fields": {"title": title, "doc_type": "glossary"}}
            ]))
            .unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    let p = r.propose(&writer(), "work", title).unwrap();
    r.land(&founder(), &p.id).unwrap()
}

fn titles(r: &mut Rdy, scope: &Scope) -> Vec<String> {
    let mut t: Vec<String> = r
        .materialized(scope)
        .unwrap()
        .blocks
        .values()
        .filter_map(|b| b.field_str("title").map(String::from))
        .collect();
    t.sort();
    t
}

/// A view written straight to the store, as M1 allowed any name.
fn legacy_view(r: &mut Rdy, name: &str) -> BlockId {
    let id = BlockId::parse("d-AAAAB").unwrap();
    let mut fields = std::collections::BTreeMap::new();
    fields.insert("title".to_string(), FieldValue::Str("Legacy".into()));
    fields.insert("doc_type".to_string(), FieldValue::Str("glossary".into()));
    let block = Block {
        id: id.clone(),
        block_type: "doc".into(),
        parent: None,
        order: "V".into(),
        fields,
        body: String::new(),
        links: Default::default(),
        pins: Default::default(),
    };
    r.store_mut()
        .commit_to_view(
            name,
            Change {
                view: name.into(),
                ops: vec![Op::Create { block }],
                deps: vec![],
                provenance: writer().provenance(),
                justification: None,
                message: "m1".into(),
            },
        )
        .unwrap();
    r.invalidate();
    id
}

#[test]
fn tag_and_landing_scopes_read_the_frozen_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let l0 = land_one(&mut r, "A");
    r.tag(&founder(), "v1.0.0", None, None).unwrap();
    land_one(&mut r, "B");
    let at: Hash = l0.parse().unwrap();
    for text in [
        "tag:v1.0.0",
        "tag:1.0.0",
        &format!("landing:{}", &l0[..8]),
        &format!("landing:{l0}"),
    ] {
        assert_eq!(r.resolve_scope(text).unwrap(), Scope::At(at), "{text}");
    }
    let snap = r.store().snapshot_at(&at).unwrap();
    assert_eq!(r.materialized(&Scope::At(at)).unwrap().snapshot, snap);
    assert_eq!(titles(&mut r, &Scope::At(at)), ["A"]);
    assert_eq!(titles(&mut r, &Scope::Canon), ["A", "B"]);
    // a fresh process with a rebuilt index reads the same snapshot
    drop(r);
    std::fs::remove_file(dir.path().join(".rdy/store/index.redb")).unwrap();
    let mut r = Rdy::open(dir.path()).unwrap();
    let scope = r.resolve_scope("tag:v1.0.0").unwrap();
    assert_eq!(titles(&mut r, &scope), ["A"]);
}

#[test]
fn scope_text_resolves_by_kind() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    land_one(&mut r, "A");
    assert_eq!(r.resolve_scope("canon").unwrap(), Scope::Canon);
    assert_eq!(
        r.resolve_scope("run-1").unwrap(),
        Scope::View("run-1".into())
    );
    assert_eq!(
        r.resolve_scope("view:canon").unwrap(),
        Scope::View("canon".into())
    );
    assert_eq!(
        r.resolve_scope("view:a:b").unwrap(),
        Scope::View("a:b".into())
    );
    assert_eq!(
        r.resolve_scope("tag:v9.0.0").unwrap_err().code,
        "E_NOT_FOUND"
    );
    assert_eq!(
        r.resolve_scope("tag:nope").unwrap_err().code,
        "E_BAD_TAG_NAME"
    );
    assert_eq!(
        r.resolve_scope("landing:abc").unwrap_err().code,
        "E_NOT_FOUND"
    );
    // a hash that is not a landing never materializes
    assert_eq!(
        r.materialized(&Scope::At(Hash([7; 32])))
            .map(|_| ())
            .unwrap_err()
            .code,
        "E_NOT_FOUND"
    );
}

#[test]
fn legacy_colon_views_stay_readable_and_proposable_but_not_writable() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let id = legacy_view(&mut r, "tag:x");
    // tag:x is a version scope, never the view
    assert_eq!(r.resolve_scope("tag:x").unwrap_err().code, "E_BAD_TAG_NAME");
    let scope = r.resolve_scope("view:tag:x").unwrap();
    assert!(r.materialized(&scope).unwrap().get(&id).is_some());
    let write = |view: &str| WriteRequest {
        view: view.into(),
        ops: serde_json::from_value(json!([
            {"op": "create", "type": "doc", "fields": {"title": "New", "doc_type": "glossary"}}
        ]))
        .unwrap(),
        justification: None,
        message: String::new(),
    };
    assert_eq!(
        r.write(&writer(), write("tag:x")).unwrap_err().code,
        "E_RESERVED_VIEW"
    );
    assert_eq!(
        r.write(&writer(), write("new:view")).unwrap_err().code,
        "E_RESERVED_VIEW"
    );
    let out = tempfile::tempdir().unwrap();
    assert_eq!(
        r.import(&founder(), "a:b", out.path()).unwrap_err().code,
        "E_RESERVED_VIEW"
    );
    let p = r.propose(&writer(), "tag:x", "legacy").unwrap();
    r.land(&founder(), &p.id).unwrap();
    assert!(r.materialized(&Scope::Canon).unwrap().get(&id).is_some());
}

#[test]
fn as_of_snapshots_survive_landings() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let at: Hash = land_one(&mut r, "A").parse().unwrap();
    r.graph(&Scope::At(at)).unwrap();
    r.materialized(&Scope::Canon).unwrap();
    land_one(&mut r, "B");
    assert_eq!(
        r.is_cached(&Scope::At(at)),
        (true, true),
        "landings don't invalidate as-of reads"
    );
    assert_eq!(r.is_cached(&Scope::Canon), (false, false));
}

#[test]
fn the_fifth_as_of_scope_evicts_the_least_recently_used() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let l: Vec<Hash> = (0..6)
        .map(|i| land_one(&mut r, &format!("D{i}")).parse().unwrap())
        .collect();
    assert_eq!(AS_OF_CACHE_ENTRIES, 4);
    for h in &l[..4] {
        r.graph(&Scope::At(*h)).unwrap();
    }
    r.materialized(&Scope::At(l[0])).unwrap(); // l[0] is now the most recently used
    r.materialized(&Scope::At(l[4])).unwrap(); // evicts l[1]
    assert_eq!(
        r.is_cached(&Scope::At(l[1])),
        (false, false),
        "evicted with its graph"
    );
    for h in [l[0], l[2], l[3], l[4]] {
        assert!(r.is_cached(&Scope::At(h)).0);
    }
    r.graph(&Scope::At(l[2])).unwrap(); // a graph read counts as a use
    r.materialized(&Scope::At(l[5])).unwrap(); // evicts l[3]
    assert!(!r.is_cached(&Scope::At(l[3])).0);
    assert!(r.is_cached(&Scope::At(l[2])).0);
}
