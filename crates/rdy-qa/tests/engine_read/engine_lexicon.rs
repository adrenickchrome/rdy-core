//! lex1: the lexicon (FW-EN1, FW-EN3): entity keys, names and aliases by phrase, mentions in a
//! block's text, recurring code literals; carried across writes exactly as a rebuild.
use rdy_engine::lexicon::{
    Lexicon, Mention, NameKind, is_content, is_fact, link_kind_for, linked_from_outside, tied,
};
use rdy_engine::{DecisionInput, Rdy, Scope};
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// Doc `ops`, section Summary: entity Ledger service (key SVC-LEDGER, aliases "ledger" and "the
/// books", kind service), entity Auth (no kind), value Request timeout, term Snapshot, and a claim
/// with key REQ-1. In view work, not landed.
fn seed(r: &mut Rdy) -> BTreeMap<String, String> {
    write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "ops", "title": "Ops", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Summary"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "body": "Keeps the books.", "fields": {"name": "Ledger service", "kind": "service", "aliases": ["ledger", "the books"], "key": "SVC-LEDGER"}},
        {"op": "create", "type": "entity", "id": "$a", "parent": "$s", "fields": {"name": "Auth"}},
        {"op": "create", "type": "value", "id": "$v", "parent": "$s", "fields": {"name": "Request timeout", "value": "5", "unit": "ms"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "body": "A frozen state.", "fields": {"name": "Snapshot"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "One claim.", "fields": {"key": "REQ-1"}}
    ]))
    .unwrap()
    .created
}

fn lex(r: &Rdy) -> Arc<Lexicon> {
    r.read().lexicon(&Scope::View("work".into())).unwrap()
}

#[test]
fn lex1_keys_names_and_aliases_resolve_ignoring_case_and_kind() {
    let (_d, mut r) = project();
    let c = seed(&mut r);
    let l = lex(&r);
    assert_eq!(
        l.resolve("ledger service"),
        vec![(id(&c["$e"]), NameKind::Name)]
    );
    assert_eq!(
        l.resolve("THE  Books"),
        vec![(id(&c["$e"]), NameKind::Alias)]
    );
    assert_eq!(l.resolve("svc-ledger"), vec![(id(&c["$e"]), NameKind::Key)]);
    assert_eq!(
        l.resolve("auth"),
        vec![(id(&c["$a"]), NameKind::Name)],
        "an entity with no kind resolves too"
    );
    assert_eq!(
        l.resolve("Request timeout"),
        vec![(id(&c["$v"]), NameKind::Name)]
    );
    assert_eq!(l.resolve("snapshot"), vec![(id(&c["$t"]), NameKind::Name)]);
    assert!(l.resolve("REQ-1").is_empty(), "a claim is not an entity");
    assert!(l.resolve("").is_empty() && l.resolve("--").is_empty());
    assert_eq!(l.entity_count(), 4);
}

#[test]
fn lex1_mentions_name_entities_by_name_or_alias_never_by_key_or_in_their_own_text() {
    let (_d, mut r) = project();
    let c = seed(&mut r);
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "flow", "id": "$x", "parent": c["$s"], "body": "SVC-LEDGER writes nightly.", "fields": {"name": "Close", "steps": ["Write to the Ledger", "Wait out the request timeout"]}}
    ]))
    .unwrap();
    let work = Scope::View("work".into());
    let x = r.read().block(&work, &id(&w.created["$x"])).unwrap();
    assert_eq!(
        lex(&r).mentions(&x),
        vec![
            Mention {
                entity: c["$e"].clone(),
                field: "steps[0]".into(),
                text: "Ledger".into()
            },
            Mention {
                entity: c["$v"].clone(),
                field: "steps[1]".into(),
                text: "request timeout".into()
            },
        ]
    );
    let e = r.read().block(&work, &id(&c["$e"])).unwrap();
    assert!(
        lex(&r).mentions(&e).is_empty(),
        "the entity's own body names \"the books\""
    );
}

#[test]
fn lex1_code_literals_count_the_blocks_that_hold_them() {
    let (_d, mut r) = project();
    let c = seed(&mut r);
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "claim", "id": "$c1", "parent": c["$s"], "body": "Set `max_conn` low; `max_conn` is per pool."},
        {"op": "create", "type": "claim", "id": "$c2", "parent": c["$s"], "body": "Raise `max_conn` at peak."},
        {"op": "create", "type": "code", "parent": c["$s"], "body": "`max_conn` = 4", "fields": {"lang": "toml"}}
    ]))
    .unwrap();
    let mut want = vec![id(&w.created["$c1"]), id(&w.created["$c2"])];
    want.sort();
    assert_eq!(
        lex(&r).literal_blocks("max_conn"),
        want,
        "a code body holds no literal"
    );
    write(
        &mut r,
        "work",
        json!([{"op": "delete", "id": w.created["$c2"]}]),
    )
    .unwrap();
    assert_eq!(
        lex(&r).literal_blocks("max_conn"),
        vec![id(&w.created["$c1"])]
    );
}

#[test]
fn lex1_a_carried_lexicon_equals_a_rebuilt_one() {
    let (_d, mut r) = project();
    let c = seed(&mut r);
    let work = Scope::View("work".into());
    let steps = vec![
        json!([{"op": "update", "id": c["$e"], "fields": {"aliases": ["ledger", "journal"]}}]),
        json!([{"op": "create", "type": "entity", "parent": c["$s"], "fields": {"name": "Journal", "kind": "store"}}]),
        json!([{"op": "update", "id": c["$a"], "fields": {"name": "Authentication"}}]),
        json!([{"op": "create", "type": "claim", "parent": c["$s"], "body": "Use `cfg.path` and `cfg.path`."}]),
        json!([{"op": "delete", "id": c["$t"]}]),
        json!([{"op": "update", "id": c["$c"], "body": "Now `cfg.path` too."}]),
    ];
    for (i, ops) in steps.into_iter().enumerate() {
        lex(&r); // built now, so the next publish carries it
        write(&mut r, "work", ops).unwrap();
        assert!(
            r.read().lexicon_built(&work),
            "step {i}: the publish carried the lexicon"
        );
        let carried = lex(&r);
        let st = r.read();
        let rebuilt = Lexicon::build(st.pack(), st.materialized(&work).unwrap().iter());
        assert_eq!(*carried, rebuilt, "step {i}");
    }
}

#[test]
fn lex1_what_can_link_to_an_entity_and_how() {
    let (_d, mut r) = project();
    let c = seed(&mut r);
    let work = Scope::View("work".into());
    let st = r.read();
    let b = |k: &str| st.block(&work, &id(&c[k])).unwrap();
    assert_eq!(link_kind_for(st.pack(), &b("$c")), Some("describes"));
    assert_eq!(link_kind_for(st.pack(), &b("$e")), None, "an entity");
    assert_eq!(link_kind_for(st.pack(), &b("$s")), None, "a section");
    assert!(is_fact(st.pack(), &b("$c")));
    assert!(!is_fact(st.pack(), &b("$v")), "a value defines something");
    assert!(is_content(st.pack(), &b("$c")), "a fact is content");
    assert!(
        !is_content(st.pack(), &b("$e")) && !is_content(st.pack(), &b("$s")),
        "an entity and a section are not"
    );
    let d = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: c["$s"].clone(),
                statement: "Ledger first".into(),
                affects: vec![c["$e"].clone()],
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let st = r.read();
    assert_eq!(
        link_kind_for(st.pack(), &st.block(&work, &id(&d)).unwrap()),
        Some("affects")
    );
    assert!(
        !is_content(st.pack(), &st.block(&work, &id(&d)).unwrap()),
        "a decision is not content"
    );
    let g = st.graph(&work).unwrap();
    assert!(tied(&g, &id(&d), &id(&c["$e"])));
    assert!(!tied(&g, &id(&c["$c"]), &id(&c["$e"])));
    land(&mut r, "work").unwrap();
    let st = r.read();
    let frozen = st.block(&Scope::Canon, &id(&d)).unwrap();
    assert_eq!(
        link_kind_for(st.pack(), &frozen),
        None,
        "an accepted decision is frozen"
    );
    // a doc is linked from outside once a block in another doc points into it
    let m = st.materialized(&Scope::Canon).unwrap();
    let g = st.graph(&Scope::Canon).unwrap();
    assert!(!linked_from_outside(&g, &m, &id(&c["$d"])));
    let out = c["$c"].clone();
    write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "other", "title": "Other", "doc_type": "overview"}},
        {"op": "create", "type": "prose", "parent": "$o", "body": format!("See {{{{{out}}}}}.")}
    ]))
    .unwrap();
    let st = r.read();
    let (m, g) = (st.materialized(&work).unwrap(), st.graph(&work).unwrap());
    assert!(linked_from_outside(&g, &m, &id(&c["$d"])));
}
