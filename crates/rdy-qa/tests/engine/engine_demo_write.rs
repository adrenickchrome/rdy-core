//! df1: the demo findings on the write path: implements applies its decision (F2), two repins
//! in one write (F5).
use rdy_engine::consequence::propose_and_report;
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_model::{ActorRun, BlockId, Justification, RelayEvidence, Role};
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use rdy_qa::tensions::{ag, j_implements, put_j, try_briefed, try_decide_in, try_put_j};
use rdy_store::Relay;
use rdy_store::view::ViewOpKind;
use serde_json::json;

/// `rdy_qa::fixtures::write` with an editorial justification: the fixture writes stable normative
/// content, and an unjustified change to stable content blocks its landing (D-ME2HD).
fn clarified(
    r: &mut rdy_engine::Rdy,
    view: &str,
    ops: serde_json::Value,
) -> Result<rdy_engine::WriteResult, rdy_engine::RdyError> {
    r.write(
        &writer(),
        rdy_engine::WriteRequest {
            view: view.into(),
            ops: parse_test_ops(ops)?,
            justification: Some(rdy_model::Justification::Editorial {
                class: rdy_model::EditorialClass::Clarify,
                reason: "seed the test world".into(),
            }),
            message: String::new(),
        },
    )
}

/// A stable Rules section with claim `$open` (cites nothing) and a decision home. Landed.
fn wallets(r: &mut Rdy) -> (BlockId, BlockId) {
    let w = clarified(
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
    let p = propose_and_report(&e, None, |r| r.propose(&writer(), "cap", "Cap", None)).unwrap();
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
    // a second proposed decision, and a claim q that cites a claim p which then changes, so q
    // has a pin to repin
    let dx = try_decide_in(&mut r, "cap", &s, "Verified members hold at most 1000 EUR.").unwrap();
    let pq = put_j(
        &mut r,
        "cap",
        json!([
            {"op": "create", "type": "claim", "id": "$p", "parent": s.to_string(), "body": "Limits are in EUR."},
            {"op": "create", "type": "claim", "id": "$q", "parent": s.to_string(), "body": "See {{$p}}."}
        ]),
        None,
    );
    let (p, q) = (
        BlockId::parse(&pq.created["$p"]).unwrap(),
        BlockId::parse(&pq.created["$q"]).unwrap(),
    );
    put_j(
        &mut r,
        "cap",
        json!([{"op": "update", "id": p.to_string(), "body": "Limits are in EUR, per wallet."}]),
        None,
    );
    let w = put_j(
        &mut r,
        "cap",
        json!([
            {"op": "create", "type": "claim", "id": "$c", "parent": s.to_string(), "body": "Capped at 150 EUR.",
             "links": {"implements": [{"target": d.to_string()}]}},
            {"op": "update", "id": open.to_string(), "body": "A wallet opens for any member."},
            {"op": "update", "id": dx.to_string(), "fields": {"statement": "Verified members hold at most 2000 EUR."}},
            {"op": "create", "type": "rationale", "id": "$ra", "parent": dx.to_string(), "body": "Fraud stays rare."},
            {"op": "repin", "id": q.to_string()}
        ]),
        j_implements(&d),
    );
    let scope = Scope::View("cap".into());
    let c = BlockId::parse(&w.created["$c"]).unwrap();
    let ra = BlockId::parse(&w.created["$ra"]).unwrap();
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
    assert!(
        links(&r, &dx).is_empty(),
        "never on a decision the write edits"
    );
    assert!(links(&r, &ra).is_empty(), "never on a supporting block");
    assert!(links(&r, &q).is_empty(), "never on a repinned block");
    let read = r.read();
    assert_eq!(
        read.block(&scope, &q).unwrap().pins[&p],
        read.block(&scope, &p).unwrap().content_hash(),
        "the repin applied"
    );
    drop(read);
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
    let p = r.propose(&writer(), "w", "W", None).unwrap();
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
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "page"}},
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

const SUS3_FIX: &str = "repin in a separate write, after the write that changes the target";

fn sus3_refusal(e: &RdyError, t: &BlockId) {
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert_eq!(
        e.message,
        format!("{t} changes in this write, so a repin to it would pin an old version")
    );
    assert_eq!(e.fix, SUS3_FIX);
}

/// F5's guard: a repin of a conflicted block from a side's version writes the side's content,
/// so a repin to that block in the same write would pin a version that never lands.
#[test]
fn df1_a_repin_from_a_conflict_side_and_a_repin_to_it_stay_refused() {
    let (_d, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
            {"op": "create", "type": "term", "id": "$a", "parent": "$s", "fields": {"name": "Account"}, "body": "Verified by an ID."},
            {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Verified"}, "body": "See {{$a}}."},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Uses {{$t}} and {{$a}}."}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let id = |k: &str| BlockId::parse(&w.created[k]).unwrap();
    let (a, t, c) = (id("$a"), id("$t"), id("$c"));
    let v = version_of(&r, &Scope::Canon, &t.to_string());
    let sa = as_agent("agent:a", "r1");
    write_as(
        &mut r,
        &sa,
        "a",
        json!([{"op": "update", "id": t.to_string(), "base": v, "body": format!("A side: see {{{{{a}}}}}.")}]),
    )
    .unwrap();
    let cb = write_as(
        &mut r,
        &as_agent("agent:b", "r1"),
        "b",
        json!([{"op": "update", "id": t.to_string(), "base": v, "body": format!("B side: see {{{{{a}}}}} too.")}]),
    )
    .unwrap()
    .change;
    r.store_mut()
        .view_append(
            "a",
            vec![ViewOpKind::Include {
                changes: vec![cb.parse().unwrap()],
            }],
            &sa.provenance(),
        )
        .unwrap();
    r.invalidate();
    let scope = Scope::View("a".into());
    assert!(
        r.read().conflict_info(&scope, &t).unwrap().is_some(),
        "t is conflicted in view a"
    );
    write_as(
        &mut r,
        &sa,
        "a",
        json!([{"op": "update", "id": a.to_string(), "body": "Verified by an ID or a bank ID."}]),
    )
    .unwrap();
    let vb = version_of(&r, &Scope::View("b".into()), &t.to_string());
    let before = r.store().view("a").unwrap().changes.len();
    let e = write_as(
        &mut r,
        &sa,
        "a",
        json!([{"op": "repin", "id": c.to_string()}, {"op": "repin", "id": t.to_string(), "base": vb}]),
    )
    .unwrap_err();
    sus3_refusal(&e, &t);
    assert_eq!(r.store().view("a").unwrap().changes.len(), before);
    check(&r, &scope).unwrap();
}

/// F5's guards: a move of t, or a re-key of t that a create placed on an order-key tie makes,
/// keeps t's content (D-TX3KH: the order key is out of the content hash), so c's pin on t stays
/// current and a repin to t in that write stays refused, as a repin that changes no pin.
#[test]
fn df1_a_move_or_a_rekey_and_a_repin_to_it_stay_refused() {
    let (_d, mut r) = project();
    let (_doc, s, _one) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    // two views append a term each from one base: the terms tie on their order key in canon
    let t1 = write_as(
        &mut r,
        &as_agent("agent:a", "r"),
        "a",
        json!([{"op": "create", "type": "term", "id": "$t", "parent": s, "fields": {"name": "Ledger"}, "body": "Balances."}]),
    )
    .unwrap()
    .created["$t"]
        .clone();
    let t2 = write_as(
        &mut r,
        &as_agent("agent:b", "r"),
        "b",
        json!([{"op": "create", "type": "term", "id": "$t", "parent": s, "fields": {"name": "Wallet"}, "body": "Holds money."}]),
    )
    .unwrap()
    .created["$t"]
        .clone();
    land_with_a_tie(&mut r, "a");
    land_with_a_tie(&mut r, "b");
    let c = write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": s, "body": format!("Uses {{{{{t1}}}}} and {{{{{t2}}}}}.")}]),
    )
    .unwrap()
    .created["$c"]
        .clone();
    land(&mut r, "w").unwrap();
    // with two tied siblings, a create after the smaller id re-keys the larger one
    let (lo, hi) = if t1 < t2 { (t1, t2) } else { (t2, t1) };
    let hi_id = BlockId::parse(&hi).unwrap();
    let e = write(
        &mut r,
        "x",
        json!([
            {"op": "create", "type": "claim", "parent": s, "after": lo, "body": "Between."},
            {"op": "repin", "id": c, "targets": [hi]}
        ]),
    )
    .unwrap_err();
    // D-TX3KH (ok2): a re-key or a move keeps the target's content, so c's pin on it stays
    // current and the repin, which would change no pin, is refused as one
    already_pinned(&e, &c, &hi_id);
    // a move of the target, then a repin to it
    let e = write(
        &mut r,
        "x",
        json!([
            {"op": "move", "id": hi, "parent": s, "after": c},
            {"op": "repin", "id": c, "targets": [hi]}
        ]),
    )
    .unwrap_err();
    already_pinned(&e, &c, &hi_id);
}

/// The refusal of a repin by `c` to `t` that changes no pin (D-TX3KH: a move or a re-key of `t`
/// keeps its content).
fn already_pinned(e: &RdyError, c: &str, t: &BlockId) {
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert_eq!(
        e.message,
        format!("{c} already pins the current version of {t}; nothing to repin")
    );
}

const NOT_A_DECISION_FIX: &str = "name the decision (D-…), or give an editorial reason";

/// The ruling on task 1 (D-YQCY9: the link goes "to that decision"): implements names a
/// decision; any other block is refused up front, and nothing is written.
#[test]
fn df1_implements_a_non_decision_is_refused() {
    let (_d, mut r) = project();
    let (s, open) = wallets(&mut r);
    let q = put_j(
        &mut r,
        "x",
        json!([{"op": "create", "type": "question", "id": "$q", "parent": s.to_string(), "fields": {"prompt": "Who approves a cap?"}}]),
        None,
    );
    let q = BlockId::parse(&q.created["$q"]).unwrap();
    let scope = Scope::View("x".into());
    // a doc named by its path resolves to the doc's id, as any implements text does
    let Some(Justification::Implements(by_path)) =
        parse_justification(&r.read(), &scope, Some(vec!["wallets".into()]), None, None)
            .unwrap()
            .stored
    else {
        panic!("implements")
    };
    let doc = by_path[0].clone();
    let before = r.store().view("x").unwrap().changes.len();
    for (target, ty) in [
        (&open, "claim"),
        (&q, "question"),
        (&s, "section"),
        (&doc, "doc"),
    ] {
        let e = try_put_j(
            &mut r,
            "x",
            json!([{"op": "create", "type": "claim", "parent": s.to_string(), "body": "Capped."}]),
            j_implements(target),
        )
        .unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{e}");
        assert_eq!(
            e.message,
            format!(
                "implements names {target}, a {ty}; implements names the decision this change carries out"
            )
        );
        assert_eq!(e.fix, NOT_A_DECISION_FIX);
    }
    assert_eq!(
        r.store().view("x").unwrap().changes.len(),
        before,
        "nothing written"
    );
}

fn not_held(e: &RdyError, d: &BlockId) {
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert_eq!(
        e.message,
        format!("implements names {d}, which this view does not hold")
    );
    assert_eq!(
        e.fix,
        format!("include the change that records {d} (rdy_view include), or wait until {d} lands")
    );
}

/// The ruling on task 1: implements names a decision the write's view holds. One agent decides
/// in view cap; another implements in view impl before the landing: refused with a text about
/// the justification, until impl includes the deciding change.
#[test]
fn df1_implements_a_decision_this_view_does_not_hold_is_refused() {
    let (_d, mut r) = project();
    let (s, open) = wallets(&mut r);
    let d = decide(&mut r, "cap", &s);
    let create = json!([{"op": "create", "type": "claim", "id": "$c", "parent": s.to_string(), "body": "Capped at 150 EUR."}]);
    let e = try_put_j(&mut r, "impl", create.clone(), j_implements(&d)).unwrap_err();
    not_held(&e, &d);
    // a typo'd id is not held either
    let typo = BlockId::parse("D-ZZZZZ").unwrap();
    let e = try_put_j(&mut r, "impl", create.clone(), j_implements(&typo)).unwrap_err();
    not_held(&e, &typo);
    // a structure-only write (a move) is checked the same way
    let x = put_j(
        &mut r,
        "impl",
        json!([{"op": "create", "type": "claim", "id": "$x", "parent": s.to_string(), "body": "Caps are per wallet."}]),
        None,
    );
    let mv = json!([{"op": "move", "id": open.to_string(), "parent": s.to_string(), "after": x.created["$x"]}]);
    let before = r.store().view("impl").unwrap().changes.len();
    let e = try_put_j(&mut r, "impl", mv.clone(), j_implements(&typo)).unwrap_err();
    not_held(&e, &typo);
    assert_eq!(r.store().view("impl").unwrap().changes.len(), before);
    // after impl includes the change that records d, the same write is accepted and links
    let changes: Vec<String> = r
        .store()
        .view("cap")
        .unwrap()
        .changes
        .iter()
        .map(|h| h.to_string())
        .collect();
    r.view_include(&ag("impl"), "impl", &changes).unwrap();
    let w = put_j(&mut r, "impl", create, j_implements(&d));
    let c = BlockId::parse(&w.created["$c"]).unwrap();
    let scope = Scope::View("impl".into());
    let links = r
        .read()
        .block(&scope, &c)
        .unwrap()
        .links
        .get("implements")
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        links.iter().map(|l| l.target.clone()).collect::<Vec<_>>(),
        vec![d]
    );
    put_j(&mut r, "impl", mv, None);
    check(&r, &scope).unwrap();
}

/// The orchestrator ruling after G1: `implements` names an accepted decision or one proposed in
/// this view. A rejected, superseded or revoked decision is refused up front, so rdy never writes
/// a store-only link that is suspect at once; nothing is written.
#[test]
fn df1_implements_a_replaced_decision_is_refused() {
    let (_d, mut r) = project();
    let (s, _) = wallets(&mut r);
    // rejected: an approver may write a decision with a non-proposed status
    let ops = rdy_qa::fixtures::parse_test_ops(
        json!([{"op": "create", "type": "decision", "id": "$x", "parent": s.to_string(),
        "fields": {"statement": "Caps are per member", "status": "rejected",
                   "decided_at": "2021-01-10T09:00:00Z", "recorded_at": "2021-01-10T09:00:00Z",
                   "options": ["yes", "no"]}}]),
    )
    .unwrap();
    let rejected = try_briefed(&mut r, "rej", |r, brief| {
        r.write_briefed(
            &approver(),
            WriteRequest {
                view: "rej".into(),
                ops: ops.clone(),
                justification: None,
                message: String::new(),
            },
            brief,
        )
    })
    .unwrap();
    let rejected = BlockId::parse(&rejected.created["$x"]).unwrap();
    land(&mut r, "rej").unwrap();
    // superseded and revoked: accepted, then replaced and landed
    let superseded = decide(&mut r, "d0", &s);
    land(&mut r, "d0").unwrap();
    let revoked = try_decide_in(&mut r, "dr", &s, "Caps reset monthly.").unwrap();
    land(&mut r, "dr").unwrap();
    try_briefed(&mut r, "x", |r, brief| {
        r.supersede(
            &ag("x"),
            "x",
            &superseded.to_string(),
            None,
            DecisionInput {
                parent: s.to_string(),
                statement: "Unverified members hold at most 100 EUR.".into(),
                brief,
                ..Default::default()
            },
        )
    })
    .unwrap();
    try_briefed(&mut r, "x", |r, brief| {
        r.revoke(
            &ag("x"),
            "x",
            &revoked.to_string(),
            "not needed",
            &s.to_string(),
            brief,
        )
    })
    .unwrap();
    land(&mut r, "x").unwrap();
    put_j(
        &mut r,
        "impl",
        json!([{"op": "create", "type": "claim", "parent": s.to_string(), "body": "Caps are per wallet."}]),
        None,
    );
    let before = r.store().view("impl").unwrap().changes.len();
    for (d, status) in [
        (&rejected, "rejected"),
        (&superseded, "superseded"),
        (&revoked, "revoked"),
    ] {
        let e = try_put_j(
            &mut r,
            "impl",
            json!([{"op": "create", "type": "claim", "parent": s.to_string(), "body": "Capped."}]),
            j_implements(d),
        )
        .unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{status}: {e}");
        assert_eq!(
            e.message,
            format!(
                "implements names {d}, which is {status}; implements names an accepted decision or one proposed in this view"
            ),
            "{status}"
        );
        assert_eq!(
            e.fix, "name the decision that replaced it, or give an editorial reason",
            "{status}"
        );
        assert_eq!(
            r.store().view("impl").unwrap().changes.len(),
            before,
            "{status}: nothing written"
        );
    }
}
