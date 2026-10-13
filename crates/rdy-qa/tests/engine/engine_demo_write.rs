//! df1: the demo findings on the write path: implements applies its decision (F2), two repins
//! in one write (F5).
use rdy_engine::consequence::propose_and_report;
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_model::{ActorRun, BlockId, RelayEvidence, Role};
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use rdy_qa::tensions::{j_implements, put_j, try_decide_in, try_put_j};
use rdy_store::Relay;
use serde_json::json;

/// A stable Rules section with claim `$open` (cites nothing) and a decision home. Landed.
fn wallets(r: &mut Rdy) -> (BlockId, BlockId) {
    let w = write(
        r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "wallets", "title": "Wallets", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules", "lifecycle": "stable"}},
            {"op": "create", "type": "claim", "id": "$open", "parent": "$s", "body": "A wallet opens only for a verified member."}
        ]),
    )
    .unwrap();
    land(r, "work").unwrap();
    (
        BlockId::parse(&w.created["$s"]).unwrap(),
        BlockId::parse(&w.created["$open"]).unwrap(),
    )
}

fn decide(r: &mut Rdy, view: &str, parent: &BlockId) -> BlockId {
    try_decide_in(r, view, parent, "Unverified members hold at most 150 EUR.").unwrap()
}

/// The repro case `unapplied-with-implements`: a claim written with implements: D only.
#[test]
fn df1_unapplied_with_implements() {
    let (_d, mut r) = project();
    let (s, _) = wallets(&mut r);
    let d = decide(&mut r, "cap", &s);
    let w = put_j(
        &mut r,
        "cap",
        json!([
            {"op": "create", "type": "claim", "id": "$c", "parent": s.to_string(),
             "body": "A member who has not been verified may hold at most 150 EUR."}
        ]),
        j_implements(&d),
    );
    let c = BlockId::parse(&w.created["$c"]).unwrap();
    let scope = Scope::View("cap".into());
    // `governing` lists live decisions only, and d is proposed in this view: the graph's
    // `governed_by` (FW-LP6) reads the same `implements` edge whatever d's status
    let governed = r.read().graph(&scope).unwrap().governed_by(&d);
    assert!(governed.contains(&c), "governed_by: {governed:?}");
    let e = Local::new(r);
    let p = propose_and_report(&e, None, |r| r.propose(&writer(), "cap", "Cap")).unwrap();
    assert!(
        p["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|w| w["code"] != "W_UNAPPLIED_DECISION"),
        "{p:#}"
    );
}

#[test]
fn df1_implements_links_only_the_content_the_write_rewrites() {
    let (_d, mut r) = project();
    let (s, open) = wallets(&mut r);
    let d = decide(&mut r, "cap", &s);
    let w = put_j(
        &mut r,
        "cap",
        json!([
            {"op": "create", "type": "claim", "id": "$c", "parent": s.to_string(), "body": "Capped at 150 EUR.",
             "links": {"implements": [{"target": d.to_string()}]}},
            {"op": "update", "id": open.to_string(), "body": "A wallet opens for any member."}
        ]),
        j_implements(&d),
    );
    let scope = Scope::View("cap".into());
    let c = BlockId::parse(&w.created["$c"]).unwrap();
    let links = |r: &Rdy, id: &BlockId| {
        r.read()
            .block(&scope, id)
            .unwrap()
            .links
            .get("implements")
            .cloned()
            .unwrap_or_default()
    };
    assert_eq!(links(&r, &c).len(), 1, "never twice to one decision");
    assert_eq!(
        links(&r, &open)
            .iter()
            .map(|l| l.target.clone())
            .collect::<Vec<_>>(),
        vec![d.clone()]
    );
    assert!(links(&r, &d).is_empty(), "never on a decision");
    // a move adds nothing: the claim moved after c keeps exactly one implements link, and a
    // second decision named on a move-only write links nothing
    let d2 = decide(&mut r, "cap", &s);
    try_put_j(
        &mut r,
        "cap",
        json!([{"op": "move", "id": open.to_string(), "parent": s.to_string(), "after": c.to_string()}]),
        j_implements(&d2),
    )
    .unwrap();
    assert_eq!(
        links(&r, &open)
            .iter()
            .map(|l| l.target.clone())
            .collect::<Vec<_>>(),
        vec![d.clone()]
    );
    check(&r, &scope).unwrap();
}

#[test]
fn df1_implements_skips_structure_and_an_explicit_implements_list() {
    let (_d, mut r) = project();
    let (s, open) = wallets(&mut r);
    let d = decide(&mut r, "cap", &s);
    let scope = Scope::View("cap".into());
    let w = put_j(
        &mut r,
        "cap",
        json!([
            {"op": "create", "type": "doc", "id": "$d2", "fields": {"name": "limits", "title": "Limits", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s2", "parent": "$d2", "fields": {"title": "Caps"}},
            {"op": "create", "type": "claim", "id": "$c2", "parent": "$s2", "body": "Caps apply per wallet."}
        ]),
        j_implements(&d),
    );
    let links = |r: &Rdy, id: &str| {
        r.read()
            .block(&scope, &BlockId::parse(id).unwrap())
            .unwrap()
            .links
            .get("implements")
            .cloned()
            .unwrap_or_default()
    };
    assert!(
        links(&r, &w.created["$d2"]).is_empty(),
        "a doc is structure"
    );
    assert!(
        links(&r, &w.created["$s2"]).is_empty(),
        "a section is structure"
    );
    assert_eq!(
        links(&r, &w.created["$c2"]).len(),
        1,
        "the claim is content"
    );
    // an update that names implements sets that list exactly (link1): rdy adds nothing
    put_j(
        &mut r,
        "cap",
        json!([{"op": "update", "id": open.to_string(), "body": "A wallet opens for any member.",
            "links": {"implements": []}}]),
        j_implements(&d),
    );
    assert!(links(&r, &open.to_string()).is_empty());
}

/// D-HA7YZ: the engine's own relay check points at the agent path.
#[test]
fn df1_the_engine_relay_check_names_the_agent_path() {
    let (_d, mut r) = project();
    write(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Relay", "doc_type": "overview"}},
            {"op": "create", "type": "claim", "parent": "$d", "body": "Relayed claim."}
        ]),
    )
    .unwrap();
    let p = r.propose(&writer(), "w", "W").unwrap();
    // `Session::agent` never holds the approver role, and `land_with` checks the role first; a
    // session built by hand reaches the engine's own relay check
    let agent = Session {
        actor: "agent:w".into(),
        run: Some("chat".into()),
        role: Role::Approver,
        kind: SessionKind::Agent,
    };
    let relay = Relay {
        by: ActorRun {
            actor: "agent:w".into(),
            run: Some("chat".into()),
        },
        evidence: RelayEvidence {
            quoted: "yes, land it".into(),
            session_ref: "chat-1".into(),
            question: Some("Land it?".into()),
        },
    };
    let e = r
        .land_with(
            &agent,
            &p.id,
            LandOptions {
                relay: Some(relay),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert_eq!(e.code, "E_PERMISSION", "{e}");
    assert_eq!(e.message, "only a human session records a relayed approval");
    assert_eq!(
        e.fix,
        "run rdy approve under the relaying agent's RDY_ACTOR, with --relayed-by naming that agent"
    );
}

/// The repro case `repin-two-in-one-write`: c cites t, t cites a; both turn suspect; one write
/// repins both.
#[test]
fn df1_repin_two_in_one_write() {
    let (_d, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "glossary"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
            {"op": "create", "type": "term", "id": "$a", "parent": "$s", "fields": {"name": "Account"}, "body": "Verified by an ID."},
            {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Verified"}, "body": "See {{$a}}."},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Uses {{$t}}."}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let id = |k: &str| BlockId::parse(&w.created[k]).unwrap();
    let (a, t, c) = (id("$a"), id("$t"), id("$c"));
    write(
        &mut r,
        "b",
        json!([
            {"op": "update", "id": a.to_string(), "body": "Verified by an ID or a bank ID."},
            {"op": "update", "id": t.to_string(), "body": format!("See {{{{{a}}}}} for how.")}
        ]),
    )
    .unwrap();
    let scope = Scope::View("b".into());
    assert!(
        !r.read().context(&scope, &c).unwrap().suspects.is_empty(),
        "c is suspect on t"
    );
    write(
        &mut r,
        "b",
        json!([{"op": "repin", "id": t.to_string()}, {"op": "repin", "id": c.to_string()}]),
    )
    .expect("two repins in one write");
    let read = r.read();
    assert!(read.context(&scope, &c).unwrap().suspects.is_empty());
    assert!(read.context(&scope, &t).unwrap().suspects.is_empty());
    assert_eq!(
        read.block(&scope, &c).unwrap().pins[&t],
        read.block(&scope, &t).unwrap().content_hash()
    );
    check(&r, &scope).unwrap();
}
