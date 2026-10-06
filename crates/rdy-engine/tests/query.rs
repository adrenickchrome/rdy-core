use rdy_engine::*;
use rdy_model::*;
use serde_json::json;

fn agent() -> Session {
    Session {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
        kind: rdy_engine::SessionKind::Human,
    }
}
fn founder() -> Session {
    Session {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
        kind: rdy_engine::SessionKind::Human,
    }
}

#[test]
fn context_and_suspects_after_landing() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let res = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Wallets", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Decisions"}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let s = res.created["$s"].clone();
    let d = r
        .decide(
            &agent(),
            "work",
            DecisionInput {
                parent: s.clone(),
                statement: "Wallets don't move".into(),
                ..Default::default()
            },
        )
        .unwrap();
    let did = d.created["$decision"].clone();
    let p = r.propose(&agent(), "work", "seed").unwrap();
    r.land(&founder(), &p.id).unwrap();
    // a claim decided by the (now accepted) decision
    let c = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "claim", "parent": s, "body": format!("Leavers open a new wallet per {{{{{did}}}}}."), "links": {"decided_by": [{"target": did}]}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let cid = BlockId::parse(&c.created["op0"]).unwrap();
    let work = Scope::View("work".into());
    let ctx = r.read().context(&work, &cid).unwrap();
    assert_eq!(ctx.governing, vec![did.clone()]);
    assert!(ctx.suspects.is_empty());
    assert_eq!(ctx.doc.as_deref(), Some(res.created["$doc"].as_str()));

    let imp = r
        .read()
        .impact(&work, &BlockId::parse(&did).unwrap(), 2)
        .unwrap();
    assert_eq!(
        imp.iter().map(|i| i.id.clone()).collect::<Vec<_>>(),
        vec![cid.to_string()]
    );

    // supersede the decision and land: the claim becomes suspect
    r.supersede(
        &agent(),
        "work",
        &did,
        None,
        DecisionInput {
            parent: s.clone(),
            statement: "Wallets move".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let p = r.propose(&agent(), "work", "reverse").unwrap();
    r.land(&founder(), &p.id).unwrap();
    let sus = r.read().suspects(&Scope::Canon).unwrap();
    assert!(
        sus.iter()
            .any(|x| x.block == cid && x.reason == "superseded")
    );
    let st = r
        .read()
        .context(&Scope::Canon, &BlockId::parse(&did).unwrap())
        .unwrap()
        .status;
    assert!(matches!(
        st,
        Some(rdy_graph::DerivedStatus::Superseded { .. })
    ));
}

fn setup(r: &mut Rdy) -> (String, String) {
    let res = r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Wallets", "doc_type": "decisions-log"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Decisions"}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let s = res.created["$s"].clone();
    let d = r
        .decide(
            &agent(),
            "work",
            DecisionInput {
                parent: s.clone(),
                statement: "Wallets don't move".into(),
                ..Default::default()
            },
        )
        .unwrap();
    (s, d.created["$decision"].clone())
}

#[test]
fn graph_cache_is_refreshed_after_a_view_write() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let (s, did) = setup(&mut r);
    let work = Scope::View("work".into());
    let id = BlockId::parse(&did).unwrap();
    assert!(
        r.read()
            .context(&work, &id)
            .unwrap()
            .incoming
            .iter()
            .all(|e| e.kind != rdy_graph::EdgeKind::References)
    );
    r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "claim", "parent": s, "body": format!("See {{{{{did}}}}}.")}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let after = r.read().context(&work, &id).unwrap();
    assert!(
        after
            .incoming
            .iter()
            .any(|e| e.kind == rdy_graph::EdgeKind::References)
    );
}

#[test]
fn graph_cache_is_refreshed_after_landing() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let (_s, did) = setup(&mut r);
    let id = BlockId::parse(&did).unwrap();
    let p = r.propose(&agent(), "work", "seed").unwrap();
    // warm the canon caches before landing (decision absent from canon)
    assert!(r.read().context(&Scope::Canon, &id).is_err());
    r.land(&founder(), &p.id).unwrap();
    let ctx = r.read().context(&Scope::Canon, &id).unwrap();
    assert!(matches!(ctx.status, Some(rdy_graph::DerivedStatus::Live)));
}

fn blk(id: &str, block_type: &str, field: Option<(&str, &str)>, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: block_type.into(),
        parent: None,
        order: "a0".into(),
        fields: field
            .into_iter()
            .map(|(k, v)| (k.to_string(), FieldValue::Str(v.into())))
            .collect(),
        body: body.into(),
        links: Default::default(),
        pins: Default::default(),
    }
}

fn scope_of(blocks: Vec<Block>) -> Materialized {
    Materialized::build(
        StateSnapshot::default(),
        blocks.into_iter().map(|b| (b.id.clone(), b)).collect(),
    )
}

#[test]
fn summary_collapses_whitespace_to_single_spaces() {
    let b = blk("C-AAAAA", "claim", None, "line one\n\n  line two\tthree\n");
    assert_eq!(summary(&b), "line one line two three");
}

#[test]
fn summary_without_context_shows_references_as_bare_ids() {
    let b = blk("C-AAAAA", "claim", None, "Per {{D-98YRA}}.");
    assert_eq!(summary(&b), "Per D-98YRA.");
}

#[test]
fn summary_in_renders_a_reference_as_the_target_summary() {
    let d = blk(
        "D-98YRA",
        "decision",
        Some(("statement", "Wallets\ndon't move")),
        "",
    );
    let c = blk(
        "C-AAAAA",
        "claim",
        None,
        "Leavers open a new wallet per {{D-98YRA}}.",
    );
    let m = scope_of(vec![d, c.clone()]);
    assert_eq!(
        summary_in(&c, &m),
        "Leavers open a new wallet per Wallets don't move."
    );
}

#[test]
fn summary_in_renders_an_unknown_reference_as_its_bare_id() {
    let c = blk("C-AAAAA", "claim", None, "See {{D-ZZZZZ}} and {{$draft}}.");
    let m = scope_of(vec![c.clone()]);
    assert_eq!(summary_in(&c, &m), "See D-ZZZZZ and $draft.");
}

#[test]
fn summary_in_does_not_expand_references_inside_a_referenced_summary() {
    let a = blk("D-AAAAA", "decision", Some(("statement", "Base rule")), "");
    let b = blk(
        "D-BBBBB",
        "decision",
        Some(("statement", "Refines {{D-AAAAA}}")),
        "",
    );
    let c = blk("C-CCCCC", "claim", None, "Per {{D-BBBBB}}");
    let m = scope_of(vec![a, b, c.clone()]);
    assert_eq!(summary_in(&c, &m), "Per Refines D-AAAAA");
}

#[test]
fn summary_in_follows_ref_mode_rules() {
    let d = blk("D-98YRA", "decision", Some(("statement", "Ship it")), "");
    let code = blk("K-AAAAA", "code", None, "let x = {{D-98YRA}};");
    let diagram = blk(
        "G-AAAAA",
        "diagram",
        None,
        "flowchart TD\n  A{{Decide?}} --> B[{{D-98YRA}}]",
    );
    let escaped = blk("C-AAAAA", "claim", None, r"literal \{{D-98YRA}} here");
    let m = scope_of(vec![d, code.clone(), diagram.clone(), escaped.clone()]);
    assert_eq!(summary_in(&code, &m), "let x = {{D-98YRA}};");
    assert_eq!(
        summary_in(&diagram, &m),
        "flowchart TD A{{Decide?}} --> B[Ship it]"
    );
    assert_eq!(summary_in(&escaped, &m), "literal {{D-98YRA}} here");
}

#[test]
fn impact_items_render_references_in_their_summaries() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let (s, did) = setup(&mut r);
    r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "claim", "parent": s, "body": format!("Leavers open\na new wallet per {{{{{did}}}}}.")}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let imp = r
        .read()
        .impact(
            &Scope::View("work".into()),
            &BlockId::parse(&did).unwrap(),
            2,
        )
        .unwrap();
    assert_eq!(
        imp[0].summary,
        "Leavers open a new wallet per Wallets don't move."
    );
}
