//! cgr1: the coordination graph of the in-flight state, and what rdy_context shows of it.
use rdy_engine::*;
use rdy_graph::coord::{CoordKind, Node};
use rdy_model::{BlockId, Hash};
use rdy_qa::fixtures::*;
use serde_json::json;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

#[test]
fn cgr1_in_flight_changes_are_nodes_with_authors_deps_and_proposals() {
    let (_d, mut r) = project();
    let (_doc, section, claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = agent("agent:a", Some("ra"));
    let v = version_of(&r, &Scope::Canon, &claim);
    let c1 = write_as(
        &mut r,
        &a,
        "ra",
        json!([{"op": "update", "id": claim, "base": v, "body": "a1"}]),
    )
    .unwrap()
    .change;
    let c2 = write_as(
        &mut r,
        &a,
        "ra",
        json!([{"op": "create", "type": "claim", "parent": section, "id": "c-NEWAA", "body": "n"}]),
    )
    .unwrap()
    .change;
    r.propose(&a, "ra", "a's work").unwrap();
    let st = r.read();
    let g = &st.coord().unwrap().graph;
    let h1: Hash = c1.parse().unwrap();
    let h2: Hash = c2.parse().unwrap();
    let authored: Vec<&Node> = g
        .incoming(&Node::Change(h1))
        .iter()
        .filter(|e| e.kind == CoordKind::AuthoredIn)
        .map(|e| &e.from)
        .collect();
    assert_eq!(authored, vec![&Node::Block(id(&claim))]);
    assert!(
        g.outgoing(&Node::Change(h1)).iter().any(
            |e| e.kind == CoordKind::MadeBy && e.to == Node::Run("agent:a".into(), "ra".into())
        )
    );
    assert!(
        g.outgoing(&Node::Run("agent:a".into(), "ra".into()))
            .iter()
            .any(|e| e.kind == CoordKind::RunOf && e.to == Node::Actor("agent:a".into()))
    );
    assert!(
        g.outgoing(&Node::Change(h2))
            .iter()
            .any(|e| e.kind == CoordKind::ProposedIn)
    );
    // the context of the claim in flight names the change, its view and its proposal
    let ctx = st.context(&Scope::Inflight, &id(&claim)).unwrap();
    assert_eq!(ctx.inflight.len(), 1);
    assert_eq!(ctx.inflight[0].change, c1);
    assert_eq!(ctx.inflight[0].views, vec!["ra".to_string()]);
    assert_eq!(ctx.inflight[0].run.as_deref(), Some("ra"));
    assert_eq!(ctx.inflight[0].proposals.len(), 1);
    assert!(ctx.inflight[0].proposals[0].starts_with("P-"));
    // other scopes have no in-flight list
    assert!(
        st.context(&Scope::Canon, &id(&claim))
            .unwrap()
            .inflight
            .is_empty()
    );
    let v = serde_json::to_value(st.context(&Scope::Canon, &id(&claim)).unwrap()).unwrap();
    assert!(v.get("inflight").is_none());
}

#[test]
fn cgr1_a_landing_removes_its_changes_from_the_graph() {
    let (_d, mut r) = project();
    let (_doc, _section, claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v = version_of(&r, &Scope::Canon, &claim);
    let c = write(
        &mut r,
        "w",
        json!([{"op": "update", "id": claim, "base": v, "body": "w"}]),
    )
    .unwrap()
    .change;
    assert!(r.read().coord().unwrap().graph.edge_count() > 0);
    land(&mut r, "w").unwrap();
    let st = r.read();
    let g = &st.coord().unwrap().graph;
    assert!(g.incoming(&Node::Change(c.parse().unwrap())).is_empty());
    assert!(
        st.context(&Scope::Inflight, &id(&claim))
            .unwrap()
            .inflight
            .is_empty()
    );
}

#[test]
fn cgr1_view_show_rows_carry_the_change_links() {
    use rdy_engine::coord_graph::{alt_label, intent_label};
    use rdy_model::{AltChoice, ChangeLinks};
    let (_d, mut r) = project();
    let (_doc, _s, claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let v0 = version_of(&r, &Scope::Canon, &claim);
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": claim, "base": v0, "body": "w"}]),
    )
    .unwrap();
    let (i, a) = (Hash([1; 32]), Hash([2; 32]));
    let req = WriteRequest {
        view: "w".into(),
        ops: serde_json::from_value(json!([{"op": "update", "id": claim, "base": version_of(&r, &Scope::View("w".into()), &claim), "body": "w2"}])).unwrap(),
        justification: None,
        message: String::new(),
    };
    let links = ChangeLinks {
        intent: Some(i),
        alternative: Some(AltChoice {
            set: a,
            member: "w".into(),
        }),
        client_nonce: None,
    };
    r.write_linked(&writer(), req, links, None).unwrap();
    let detail = serde_json::to_value(r.read().view_show("w").unwrap()).unwrap();
    let rows = detail["changes"].as_array().unwrap();
    let plain = rows
        .iter()
        .find(|x| x.get("intent").is_none())
        .expect("the unlinked change");
    assert!(
        plain.get("alternative").is_none(),
        "absent links are omitted: {plain}"
    );
    let linked = rows
        .iter()
        .find(|x| x.get("intent").is_some())
        .expect("the linked change");
    assert_eq!(linked["intent"], intent_label(&i));
    assert_eq!(
        linked["alternative"],
        json!({"set": alt_label(&a), "member": "w"})
    );
}

#[test]
fn cgr1_an_in_flight_decision_affects_its_own_docs_area_and_its_targets_areas() {
    use rdy_qa::packs::qa_project;
    let (_d, mut r) = qa_project();
    let seed = Scope::View("seed".into());
    for path in ["payments/fees", "identity/login"] {
        r.new_doc(&writer(), "seed", path, Some("glossary"), Some("T"))
            .unwrap();
    }
    let pdoc = r
        .read()
        .resolve_path(&seed, "payments/fees")
        .unwrap()
        .to_string();
    let idoc = r
        .read()
        .resolve_path(&seed, "identity/login")
        .unwrap()
        .to_string();
    let res = write(&mut r, "seed", json!([
        {"op": "create", "type": "section", "id": "$ps", "parent": pdoc, "fields": {"title": "S"}},
        {"op": "create", "type": "section", "id": "$is", "parent": idoc, "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$ic", "parent": "$is", "body": "login"}
    ])).unwrap();
    land(&mut r, "seed").unwrap();
    let (psec, iclaim) = (res.created["$ps"].clone(), res.created["$ic"].clone());
    // in flight: a decision in the payments doc that affects an identity claim
    let d = r
        .decide(
            &writer(),
            "w",
            DecisionInput {
                parent: psec,
                statement: "Login charges no fee".into(),
                affects: vec![iclaim],
                ..Default::default()
            },
        )
        .unwrap();
    let dec = Node::Block(id(&d.created["$decision"]));
    let st = r.read();
    let g = &st.coord().unwrap().graph;
    let areas: Vec<&Node> = g
        .outgoing(&dec)
        .iter()
        .filter(|e| e.kind == CoordKind::Affects)
        .map(|e| &e.to)
        .collect();
    assert_eq!(
        areas,
        vec![
            &Node::Area("identity".into()),
            &Node::Area("payments".into())
        ],
        "Q4: (a) payments, its own doc; (c) identity, its target"
    );
}
