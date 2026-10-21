//! cp5 (D-EJBPD, p-0H3JT, p-0SH30, p-QMGB4, p-HF5W0): the `reaffirm` op records that a suspect
//! accepted decision still holds, as one metadata-only change, without touching its ruling.
use rdy_engine::*;
use rdy_model::engine_fields::{Against, REAFFIRMED, REFLECTS_AT, decode_reaffirmed};
use rdy_model::{Block, BlockId, FieldValue, Op};
use rdy_qa::fixtures::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The project: in canon, section `s` holds term `t`, claim `c`, decision `d0` (superseded by
/// `d0n`) and the accepted decisions `d1` ("Uses the {{t}}."), `d2` ("Per the {{t}}."), `d3`
/// ("Per {{c}}.") and `d5` ("Under {{d0}}."). View `w` changes `t` and deletes `c`, so in `w`
/// `d1` and `d2` are suspect on a changed `t`, `d3` on a deleted `c`, and `d5` on a superseded
/// `d0` (in canon too).
struct Fx {
    s: String,
    t: String,
    c: String,
    d0: String,
    d0n: String,
    d1: String,
    d2: String,
    d3: String,
    d5: String,
}

fn decide(r: &mut Rdy, view: &str, s: &str, statement: &str, body: &str) -> String {
    r.decide(
        &writer(),
        view,
        DecisionInput {
            parent: s.into(),
            statement: statement.into(),
            body: body.into(),
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("decide {statement}: {e}"))
    .created["$decision"]
        .clone()
}

fn fx(r: &mut Rdy) -> Fx {
    let seed = write(r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "led", "title": "Ledger", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Claims are kept."}
    ]))
    .unwrap();
    let (s, t, c) = (
        seed.created["$s"].clone(),
        seed.created["$t"].clone(),
        seed.created["$c"].clone(),
    );
    land(r, "seed").unwrap();
    let d0 = decide(r, "dv", &s, "Ledgers are kept", "");
    let d1 = decide(
        r,
        "dv",
        &s,
        "Balances use the ledger",
        &format!("Uses the {{{{{t}}}}}."),
    );
    let d2 = decide(
        r,
        "dv",
        &s,
        "Refunds use the ledger",
        &format!("Per the {{{{{t}}}}}."),
    );
    let d3 = decide(r, "dv", &s, "Claims hold", &format!("Per {{{{{c}}}}}."));
    let d5 = decide(
        r,
        "dv",
        &s,
        "Audits follow",
        &format!("Under {{{{{d0}}}}}."),
    );
    land(r, "dv").unwrap();
    let d0n = r
        .supersede(
            &writer(),
            "sv",
            &d0,
            None,
            DecisionInput {
                parent: s.clone(),
                statement: "Ledgers are archived".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(r, "sv").unwrap();
    write(
        r,
        "w",
        json!([
            {"op": "update", "id": t, "body": "Balances in pence."},
            {"op": "delete", "id": c}
        ]),
    )
    .unwrap();
    Fx {
        s,
        t,
        c,
        d0,
        d0n,
        d1,
        d2,
        d3,
        d5,
    }
}

fn w() -> Scope {
    Scope::View("w".into())
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn block(r: &Rdy, scope: &Scope, s: &str) -> Block {
    r.read().block(scope, &id(s)).unwrap()
}

/// The suspects of `s` in `scope`: (target, reason), sorted.
fn suspects(r: &Rdy, scope: &Scope, s: &str) -> Vec<(String, String)> {
    let mut v: Vec<_> = r
        .read()
        .context(scope, &id(s))
        .unwrap()
        .suspects
        .into_iter()
        .map(|x| (x.target.to_string(), x.reason))
        .collect();
    v.sort();
    v
}

fn record(b: &Block) -> BTreeMap<BlockId, (Against, String)> {
    decode_reaffirmed(&b.fields[REAFFIRMED])
        .unwrap()
        .into_iter()
        .map(|(t, r)| (t, (r.against, r.reason)))
        .collect()
}

fn changes(r: &Rdy, view: &str) -> usize {
    r.store().view(view).map_or(0, |v| v.changes.len())
}

fn reaffirm(r: &mut Rdy, op: Value) -> Result<WriteResult, RdyError> {
    write(r, "w", json!([op]))
}

fn rows(res: &WriteResult) -> Value {
    serde_json::to_value(&res.reaffirmed).unwrap()
}

#[test]
fn cp5_reaffirm_changed_retired_and_deleted_premises() {
    let (_d, mut r) = project();
    let f = fx(&mut r);
    assert_eq!(
        suspects(&r, &w(), &f.d1),
        vec![(f.t.clone(), "changed".into())]
    );
    assert_eq!(
        suspects(&r, &w(), &f.d3),
        vec![(f.c.clone(), "deleted".into())]
    );
    assert_eq!(
        suspects(&r, &w(), &f.d5),
        vec![(f.d0.clone(), "superseded".into())]
    );

    // changed: the pin moves to the target's current content, beside `against: []` (O18)
    let res = reaffirm(
        &mut r,
        json!({"op": "reaffirm", "id": f.d1, "reason": "  Still in pence.  "}),
    )
    .unwrap();
    assert_eq!(rows(&res), json!([{"id": f.d1, "causes": [f.t]}]));
    let b1 = block(&r, &w(), &f.d1);
    assert_eq!(b1.pins[&id(&f.t)], block(&r, &w(), &f.t).content_hash());
    assert_eq!(
        record(&b1),
        BTreeMap::from([(
            id(&f.t),
            (Against::Retiring(vec![]), "Still in pence.".into())
        )]),
        "the reason is stored trimmed"
    );
    assert!(suspects(&r, &w(), &f.d1).is_empty());

    // deleted: `against: deleted`, and the pin to the gone target stays
    let pin_c = block(&r, &w(), &f.d3).pins[&id(&f.c)];
    let res = reaffirm(
        &mut r,
        json!({"op": "reaffirm", "id": f.d3, "reason": "Claims still hold."}),
    )
    .unwrap();
    assert_eq!(rows(&res), json!([{"id": f.d3, "causes": [f.c]}]));
    let b3 = block(&r, &w(), &f.d3);
    assert_eq!(b3.pins[&id(&f.c)], pin_c);
    assert_eq!(
        record(&b3),
        BTreeMap::from([(id(&f.c), (Against::Deleted, "Claims still hold.".into()))])
    );
    assert!(suspects(&r, &w(), &f.d3).is_empty());

    // retired: the graph's sorted retiring set
    let res = reaffirm(
        &mut r,
        json!({"op": "reaffirm", "id": f.d5, "reason": "Audits still follow."}),
    )
    .unwrap();
    assert_eq!(rows(&res), json!([{"id": f.d5, "causes": [f.d0]}]));
    let b5 = block(&r, &w(), &f.d5);
    assert_eq!(
        record(&b5),
        BTreeMap::from([(
            id(&f.d0),
            (
                Against::Retiring(vec![id(&f.d0n)]),
                "Audits still follow.".into()
            )
        )])
    );
    assert!(suspects(&r, &w(), &f.d5).is_empty());
    assert!(
        r.read()
            .suspects(&w())
            .unwrap()
            .iter()
            .all(|s| s.block.to_string() != f.d1
                && s.block.to_string() != f.d3
                && s.block.to_string() != f.d5)
    );

    // a second re-affirmation keeps the record of the targets it does not name
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": f.t, "body": "Balances in cents."}]),
    )
    .unwrap();
    reaffirm(
        &mut r,
        json!({"op": "reaffirm", "id": f.d1, "targets": [f.t], "reason": "Cents too."}),
    )
    .unwrap();
    assert_eq!(
        record(&block(&r, &w(), &f.d1)),
        BTreeMap::from([(id(&f.t), (Against::Retiring(vec![]), "Cents too.".into()))])
    );
    assert!(suspects(&r, &w(), &f.d1).is_empty());
}

#[test]
fn cp5_reaffirm_preserves_every_frozen_field_and_downstream_pin() {
    let (_d, mut r) = project();
    let f = fx(&mut r);
    // a claim downstream of d1, pinned to d1's content
    let x = write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "claim", "parent": f.s, "body": format!("Per {{{{{}}}}}.", f.d1)}]),
    )
    .unwrap()
    .created["op0"]
        .clone();
    let before = block(&r, &w(), &f.d1);
    let x_before = block(&r, &w(), &x);
    let g = r.read().graph(&w()).unwrap();
    let (out_before, in_before) = (
        g.outgoing(&id(&f.d1)).to_vec(),
        g.incoming(&id(&f.d1)).to_vec(),
    );
    let status_before = g.status(&id(&f.d1));
    reaffirm(
        &mut r,
        json!({"op": "reaffirm", "id": f.d1, "reason": "Holds."}),
    )
    .unwrap();
    let after = block(&r, &w(), &f.d1);
    // the ruling, body, links, parent, order, type and every other field stay
    let mut fields = after.fields.clone();
    assert!(fields.remove(REAFFIRMED).is_some());
    assert_eq!(fields, before.fields);
    assert_eq!(after.body, before.body);
    assert_eq!(after.links, before.links);
    assert_eq!(after.parent, before.parent);
    assert_eq!(after.order, before.order);
    assert_eq!(after.block_type, before.block_type);
    assert_eq!(after.field_str("status"), Some("accepted"));
    assert_eq!(
        after.content_hash(),
        before.content_hash(),
        "the content hash leaves it out"
    );
    assert_ne!(after.version_hash(), before.version_hash());
    // only the cause's pin moved
    let mut pins = after.pins.clone();
    pins.remove(&id(&f.t));
    let mut old_pins = before.pins.clone();
    old_pins.remove(&id(&f.t));
    assert_eq!(pins, old_pins);
    // graph edges: the same edges, the cause's pin aside; the downstream pin holds
    let g = r.read().graph(&w()).unwrap();
    let strip = |v: &[rdy_graph::Edge]| -> Vec<rdy_graph::Edge> {
        v.iter()
            .cloned()
            .map(|mut e| {
                if e.to.to_string() == f.t {
                    e.pin = None;
                }
                e
            })
            .collect()
    };
    assert_eq!(strip(g.outgoing(&id(&f.d1))), strip(&out_before));
    assert_eq!(g.incoming(&id(&f.d1)), in_before.as_slice());
    assert_eq!(g.status(&id(&f.d1)), status_before);
    assert_eq!(
        block(&r, &w(), &x),
        x_before,
        "the downstream claim is untouched"
    );
    assert_eq!(block(&r, &w(), &x).pins[&id(&f.d1)], after.content_hash());
    assert!(
        suspects(&r, &w(), &x).is_empty(),
        "no new suspect downstream"
    );
}

#[test]
fn cp5_because_selects_live_and_partly_superseded_only() {
    let (_d, mut r) = project();
    let seed = write(&mut r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "led", "title": "Ledger", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."}
    ]))
    .unwrap();
    let (s, t) = (seed.created["$s"].clone(), seed.created["$t"].clone());
    land(&mut r, "seed").unwrap();
    let cite = format!("Per the {{{{{t}}}}}.");
    let live1 = decide(&mut r, "dv", &s, "One", &cite);
    let live2 = decide(&mut r, "dv", &s, "Two", &cite);
    let part = decide(&mut r, "dv", &s, "Part", &cite);
    let whole = decide(&mut r, "dv", &s, "Whole", &cite);
    let revoked = decide(&mut r, "dv", &s, "Revoked", &cite);
    land(&mut r, "dv").unwrap();
    let dec = |st: &str| DecisionInput {
        parent: s.clone(),
        statement: st.into(),
        ..Default::default()
    };
    r.supersede(
        &writer(),
        "sv",
        &part,
        Some("Refunds".into()),
        dec("Part, refunds"),
    )
    .unwrap();
    r.supersede(&writer(), "sv", &whole, None, dec("Whole, again"))
        .unwrap();
    r.revoke(&writer(), "sv", &revoked, "no longer", &s, None)
        .unwrap();
    land(&mut r, "sv").unwrap();
    // a rejected decision, a proposed one and a claim that cite the term, in the view
    let more = r
        .write(
            &approver(),
            WriteRequest {
                view: "w".into(),
                ops: parse_test_ops(json!([
                    {"op": "create", "type": "decision", "parent": s, "fields": {"statement": "Rejected", "status": "rejected"}, "body": cite},
                    {"op": "create", "type": "decision", "parent": s, "fields": {"statement": "Proposed", "status": "proposed"}, "body": cite},
                    {"op": "create", "type": "claim", "parent": s, "body": cite}
                ]))
                .unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": t, "body": "Balances in pence."}]),
    )
    .unwrap();
    for x in more.created.values() {
        assert_eq!(
            suspects(&r, &w(), x),
            vec![(t.clone(), "changed".into())],
            "{x}"
        );
    }
    let mut expected = vec![live1.clone(), live2.clone(), part.clone()];
    expected.sort();
    assert_eq!(
        r.reaffirm_selection(
            &writer(),
            "w",
            &parse_test_ops(json!([{"op": "reaffirm", "because": t, "reason": "Holds."}])).unwrap()
        )
        .unwrap()
        .into_iter()
        .map(|b| b.to_string())
        .collect::<Vec<_>>(),
        expected
    );
    let res = reaffirm(
        &mut r,
        json!({"op": "reaffirm", "because": t, "reason": "Holds."}),
    )
    .unwrap();
    assert_eq!(
        rows(&res),
        Value::Array(
            expected
                .iter()
                .map(|d| json!({"id": d, "causes": [t]}))
                .collect()
        )
    );
    for d in &expected {
        assert!(suspects(&r, &w(), d).is_empty(), "{d}");
    }
    for x in more.created.values().chain([&whole, &revoked]) {
        assert!(!block(&r, &w(), x).fields.contains_key(REAFFIRMED), "{x}");
    }
    assert!(
        r.reaffirm_selection(
            &writer(),
            "w",
            &parse_test_ops(json!([{"op": "update", "id": t, "body": "x"}])).unwrap()
        )
        .unwrap()
        .is_empty(),
        "non-reaffirm ops select nothing"
    );
}

#[test]
fn cp5_reaffirm_reuses_stale_base_conflict_and_same_batch_guards() {
    let (_d, mut r) = project();
    let f = fx(&mut r);
    let before = changes(&r, "w");
    // stale base: the edit's own check
    let stale = version_of(&r, &w(), &f.t);
    let e = reaffirm(
        &mut r,
        json!({"op": "reaffirm", "id": f.d1, "base": stale, "reason": "Holds."}),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_STALE_BASE", "{e}");
    // the same batch changes the target: refused like a repin, nothing written
    let e = write(
        &mut r,
        "w",
        json!([
            {"op": "reaffirm", "id": f.d2, "reason": "Holds."},
            {"op": "update", "id": f.t, "body": "Balances in cents."}
        ]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert_eq!(
        e.message,
        format!(
            "{} changes in this write, so a repin to it would pin an old version",
            f.t
        )
    );
    // one decision twice in a batch
    let e = write(
        &mut r,
        "w",
        json!([
            {"op": "reaffirm", "id": f.d1, "reason": "Holds."},
            {"op": "reaffirm", "because": f.t, "reason": "Holds too."}
        ]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(e.message.contains("changed more than once"), "{e}");
    assert_eq!(changes(&r, "w"), before, "no partial write");
    assert!(!block(&r, &w(), &f.d1).fields.contains_key(REAFFIRMED));
    // two decisions against unchanged premises in one batch, with a current base
    let base = version_of(&r, &w(), &f.d1);
    let res = write(
        &mut r,
        "w",
        json!([
            {"op": "reaffirm", "id": f.d1, "base": base, "reason": "Holds."},
            {"op": "reaffirm", "id": f.d2, "reason": "Holds."}
        ]),
    )
    .unwrap();
    assert_eq!(res.reaffirmed.len(), 2);
    assert_eq!(changes(&r, "w"), before + 1);

    // a conflict on a frozen decision: view x re-affirms d1, view y re-affirms it too and lands
    // first; x now holds a conflict on d1 and a further re-affirmation of it is refused
    write(
        &mut r,
        "ch",
        json!([{"op": "update", "id": f.t, "body": "Balances in pence."}]),
    )
    .unwrap();
    land(&mut r, "ch").unwrap();
    assert_eq!(
        suspects(&r, &Scope::Canon, &f.d1),
        vec![(f.t.clone(), "changed".into())]
    );
    write(
        &mut r,
        "x",
        json!([{"op": "reaffirm", "id": f.d1, "reason": "X holds."}]),
    )
    .unwrap();
    write(
        &mut r,
        "y",
        json!([{"op": "reaffirm", "id": f.d1, "reason": "Y holds."}]),
    )
    .unwrap();
    // a re-affirmation of a frozen decision lands through the gate like any other change
    land(&mut r, "y").unwrap();
    let landed = block(&r, &Scope::Canon, &f.d1);
    assert_eq!(landed.field_str("status"), Some("accepted"));
    assert_eq!(record(&landed)[&id(&f.t)].1, "Y holds.");
    assert!(suspects(&r, &Scope::Canon, &f.d1).is_empty());
    let x = Scope::View("x".into());
    let conflicted = r
        .read()
        .conflicts(&x)
        .unwrap()
        .iter()
        .any(|c| c.block.to_string() == f.d1);
    assert!(conflicted, "x holds a conflict on d1");
    let before_x = changes(&r, "x");
    let e = write(
        &mut r,
        "x",
        json!([{"op": "reaffirm", "id": f.d1, "reason": "Again."}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_FROZEN", "{e}");
    assert!(e.fix.starts_with("take the landed side"), "{}", e.fix);
    assert!(
        e.fix.ends_with(
            ". For a suspect accepted decision that still holds, re-affirm it with a reaffirm op instead."
        ),
        "{}",
        e.fix
    );
    assert_eq!(changes(&r, "x"), before_x);
}

/// The closed refusal table (p-H2T9W, p-VNAVR): message and fix, all `E_BAD_INPUT`.
#[test]
fn cp5_reaffirm_refusals_have_exact_text() {
    let (_d, mut r) = project();
    let f = fx(&mut r);
    // a proposed decision, a rejected one and a revoked one in the view
    let proposed = decide(&mut r, "w", &f.s, "Proposed", "");
    let rejected = r
        .write(
            &approver(),
            WriteRequest {
                view: "w".into(),
                ops: parse_test_ops(json!([{"op": "create", "type": "decision", "parent": f.s,
                    "fields": {"statement": "Rejected", "status": "rejected"}}]))
                .unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap()
        .created["op0"]
        .clone();
    let revoked = decide(&mut r, "rv", &f.s, "Revoked", "");
    land(&mut r, "rv").unwrap();
    r.revoke(&writer(), "rv2", &revoked, "gone", &f.s, None)
        .unwrap();
    land(&mut r, "rv2").unwrap();
    let claim = write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "claim", "parent": f.s, "body": "Lone."}]),
    )
    .unwrap()
    .created["op0"]
        .clone();
    let before = changes(&r, "w");
    let cases: Vec<(Value, String, &str)> = vec![
        (
            json!({"op": "reaffirm", "id": f.d1, "because": f.t, "reason": "R"}),
            "reaffirm takes id or because, not both".into(),
            "pass id for one decision, or because for its suspect decisions",
        ),
        (
            json!({"op": "reaffirm", "reason": "R"}),
            "reaffirm takes id or because, not both".into(),
            "pass id for one decision, or because for its suspect decisions",
        ),
        (
            json!({"op": "reaffirm", "because": f.t, "base": version_of(&r, &w(), &f.d1), "reason": "R"}),
            "base goes with id only".into(),
            "drop base, or pass id instead of because",
        ),
        (
            json!({"op": "reaffirm", "id": claim, "reason": "R"}),
            format!("{claim} is not a decision"),
            "pass a live or partly superseded decision",
        ),
        (
            json!({"op": "reaffirm", "id": proposed, "reason": "R"}),
            format!("{proposed} is proposed; update it instead of re-affirming it"),
            "update the proposed decision",
        ),
        (
            json!({"op": "reaffirm", "id": rejected, "reason": "R"}),
            format!("{rejected} is rejected; it asserts nothing to re-affirm"),
            "pass a live or partly superseded decision",
        ),
        (
            json!({"op": "reaffirm", "id": revoked, "reason": "R"}),
            format!("{revoked} is revoked; it asserts nothing to re-affirm"),
            "pass a live or partly superseded decision",
        ),
        (
            json!({"op": "reaffirm", "id": f.d0, "reason": "R"}),
            format!("{} is superseded; it asserts nothing to re-affirm", f.d0),
            "pass a live or partly superseded decision",
        ),
        (
            json!({"op": "reaffirm", "id": f.d0n, "reason": "R"}),
            format!("{} has no suspect; there is nothing to re-affirm", f.d0n),
            "read its context and pass a suspect decision",
        ),
        (
            json!({"op": "reaffirm", "because": claim, "reason": "R"}),
            format!("no decision in this view is suspect on {claim}"),
            "read the suspects in this view",
        ),
        (
            json!({"op": "reaffirm", "id": f.d1, "reason": "   "}),
            "a re-affirmation needs a reason of 1 to 2000 characters".into(),
            "give a reason of 1 to 2000 characters",
        ),
        (
            json!({"op": "reaffirm", "id": f.d1}),
            "a re-affirmation needs a reason of 1 to 2000 characters".into(),
            "give a reason of 1 to 2000 characters",
        ),
        (
            json!({"op": "reaffirm", "id": f.d1, "reason": "ü".repeat(2001)}),
            "a re-affirmation needs a reason of 1 to 2000 characters".into(),
            "give a reason of 1 to 2000 characters",
        ),
        (
            json!({"op": "reaffirm", "id": f.d1, "targets": [claim], "reason": "R"}),
            format!("{claim} is not a selected suspect of {}", f.d1),
            "pass targets from the decision's current suspects",
        ),
        (
            json!({"op": "reaffirm", "id": f.d1, "targets": [], "reason": "R"}),
            "reaffirm targets is empty".into(),
            "omit targets, or pass a current suspect target",
        ),
    ];
    let by_cause = std::cmp::min(&f.d1, &f.d2);
    let mut cases = cases;
    cases.push((
        json!({"op": "reaffirm", "because": f.t, "targets": [f.c], "reason": "R"}),
        format!("{} is not a selected suspect of {by_cause}", f.c),
        "pass targets from the decision's current suspects",
    ));
    for (op, message, fix) in cases {
        let e = reaffirm(&mut r, op.clone()).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            ("E_BAD_INPUT", message.as_str(), fix),
            "{op}"
        );
    }
    // a 2000-character reason (scalar values, not bytes) is taken
    reaffirm(
        &mut r,
        json!({"op": "reaffirm", "id": f.d3, "reason": "ü".repeat(2000)}),
    )
    .unwrap();
    assert_eq!(changes(&r, "w"), before + 1);
    // unknown keys get did-you-mean
    let e = reaffirm(
        &mut r,
        json!({"op": "reaffirm", "id": f.d1, "reasons": "R"}),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(
        e.message.contains("reasons") && e.fix.contains("reason"),
        "{e}"
    );

    // the selection bound: 5001 decisions suspect on one superseded decision (distinct new ids:
    // 5001 random ones would often collide)
    let held = r.read().materialized(&w()).unwrap();
    let mut fresh = std::collections::BTreeSet::new();
    while fresh.len() < 5001 {
        let d = BlockId::new("D");
        if held.get(&d).is_none() {
            fresh.insert(d);
        }
    }
    let blocks: Vec<Block> = fresh
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let mut b = raw(
                d.as_str(),
                "decision",
                Some(&f.s),
                &format!("z{i:05}a"),
                &[("statement", "Bulk"), ("status", "accepted")],
                &format!("Under {{{{{}}}}}.", f.d0),
            );
            b.fields
                .insert("recorded_at".into(), FieldValue::Str("2026-10-21".into()));
            b
        })
        .collect();
    raw_commit(&mut r, "bulk", blocks);
    let e = write(
        &mut r,
        "bulk",
        json!([{"op": "reaffirm", "because": f.d0, "reason": "R"}]),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_BAD_INPUT",
            "reaffirm exceeds 5000 selected decisions or 100000 targets",
            "split the re-affirmation into smaller writes"
        )
    );
    assert!(e.briefing.is_none(), "refused before any briefing");
}

#[test]
fn cp5_generic_write_cannot_set_or_unset_metadata() {
    let (_d, mut r) = project();
    let f = fx(&mut r);
    let claim = write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "claim", "parent": f.s, "body": "Lone."}]),
    )
    .unwrap()
    .created["op0"]
        .clone();
    let before = changes(&r, "w");
    for field in [REAFFIRMED, REFLECTS_AT] {
        let cases = [
            (
                json!({"op": "create", "type": "claim", "parent": f.s, "body": "X.", "fields": {field: {}}}),
                "op 0".to_string(),
            ),
            (
                json!({"op": "update", "id": claim, "fields": {field: {}}}),
                claim.clone(),
            ),
            (
                json!({"op": "update", "id": claim, "unset": [field]}),
                claim.clone(),
            ),
        ];
        for (op, at) in cases {
            let e = write(&mut r, "w", json!([op])).unwrap_err();
            let message = format!("{at} sets \"{field}\", and rdy sets it");
            let fix = format!("remove {field}; use reaffirm or repin with code: true");
            assert_eq!(
                (e.code, e.message.as_str(), e.fix.as_str()),
                ("E_VALIDATION", message.as_str(), fix.as_str()),
                "{op}"
            );
            assert_eq!(e.issues.len(), 1, "{op}");
            assert_eq!(e.issues[0].code, "E_FIELD_UNKNOWN");
            assert_eq!(e.issues[0].message, message);
        }
    }
    // key_auto keeps its own text
    let e = write(
        &mut r,
        "w",
        json!([{"op": "update", "id": claim, "fields": {"key_auto": true}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_VALIDATION");
    assert!(
        e.message
            .ends_with("it marks a key rdy derived from the path"),
        "{e}"
    );
    assert_eq!(changes(&r, "w"), before);

    // a generic update or repin of a frozen decision stays frozen; its fix names the reaffirm
    // op only for a suspect one
    let hint = "For a suspect accepted decision that still holds, re-affirm it with a reaffirm op instead.";
    let e = write(
        &mut r,
        "w",
        json!([{"op": "update", "id": f.d1, "body": "Changed."}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_FROZEN");
    assert_eq!(
        e.fix,
        format!(
            "write a new decision that supersedes or revokes {}. {hint}",
            f.d1
        )
    );
    let e = write(&mut r, "w", json!([{"op": "repin", "id": f.d1}])).unwrap_err();
    assert_eq!(e.code, "E_FROZEN");
    assert!(e.fix.ends_with(hint), "{}", e.fix);
    let e = write(
        &mut r,
        "w",
        json!([{"op": "update", "id": f.d0n, "body": "Changed."}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_FROZEN");
    assert_eq!(
        e.fix,
        format!("write a new decision that supersedes or revokes {}", f.d0n)
    );
    assert_eq!(changes(&r, "w"), before);
}

/// p-0SH30: one write is one change, however many decisions it re-affirms.
#[test]
fn cp5_bulk_output_is_one_change_not_one_change_per_decision() {
    let (_d, mut r) = project();
    let f = fx(&mut r);
    let before = changes(&r, "w");
    let res = reaffirm(
        &mut r,
        json!({"op": "reaffirm", "because": f.t, "reason": "Holds."}),
    )
    .unwrap();
    let mut ids = vec![f.d1.clone(), f.d2.clone()];
    ids.sort();
    assert_eq!(
        rows(&res),
        json!([{"id": ids[0], "causes": [f.t]}, {"id": ids[1], "causes": [f.t]}])
    );
    assert!(res.changes.is_empty());
    assert_eq!(changes(&r, "w"), before + 1);
    let change = r.store().change(&res.change.parse().unwrap()).unwrap();
    let written: Vec<String> = change
        .ops
        .iter()
        .map(|op| match op {
            Op::Update { block, .. } => block.id.to_string(),
            other => panic!("only updates: {other:?}"),
        })
        .collect();
    assert_eq!(written, ids, "one update per decision, in one change");
    assert!(change.justification.is_none());
    let reply = serde_json::to_value(&res).unwrap();
    assert!(reply.get("reaffirmed").is_some());
    // a write with no re-affirmation omits the key
    let plain = write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "claim", "parent": f.s, "body": "P."}]),
    )
    .unwrap();
    assert!(
        serde_json::to_value(&plain)
            .unwrap()
            .get("reaffirmed")
            .is_none()
    );
}
