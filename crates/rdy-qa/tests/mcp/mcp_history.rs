//! mhist1: rdy_context carries a block's history and justification; rdy_read marks each block's justification.
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_mcp::tools::*;
use rdy_model::{ActorRun, BlockId, EditorialClass, Justification, RelayEvidence};
use rdy_qa::fixtures::*;
use rdy_qa::intake::TEST_SUMMARY;
use serde_json::{Value, from_value, json};
use std::sync::Arc;

struct Seed {
    doc: String,
    claim: String,
    term: String,
    dec: String,
    change: String,
}

/// A doc with a term, a claim and a decision, landed; then agent:a/ra updates the claim
/// implementing the decision and the term as an editorial typo fix, landed.
fn seeded(r: &mut Rdy) -> Seed {
    let res = write(r, "seed", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$t", "parent": "$s", "fields": {"name": "Ledger"}, "body": "Balances."},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "One."}
    ]))
    .unwrap();
    let dec = r
        .decide(
            &writer(),
            "seed",
            DecisionInput {
                parent: res.created["$s"].clone(),
                statement: "Claims are short".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    land(r, "seed").unwrap();
    let (claim, term) = (res.created["$c"].clone(), res.created["$t"].clone());
    let a = agent("agent:a", Some("ra"));
    let w = r
        .write(
            &a,
            WriteRequest {
                view: "ra".into(),
                ops: from_value(json!([{"op": "update", "id": claim, "body": "Two."}])).unwrap(),
                justification: Some(Justification::Implements(vec![
                    BlockId::parse(&dec).unwrap(),
                ])),
                message: String::new(),
            },
        )
        .unwrap();
    r.write(
        &a,
        WriteRequest {
            view: "ra".into(),
            ops: from_value(json!([{"op": "update", "id": term, "body": "Balances, per wallet."}]))
                .unwrap(),
            justification: Some(Justification::Editorial {
                class: EditorialClass::Typo,
                reason: "wording".into(),
            }),
            message: String::new(),
        },
    )
    .unwrap();
    land(r, "ra").unwrap();
    Seed {
        doc: res.created["$d"].clone(),
        claim,
        term,
        dec,
        change: w.change,
    }
}

fn mcp(r: Rdy) -> McpState {
    let mut st = McpState::new(Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        from_value(json!({"actor": "agent:qa", "role": "writer", "view": "work"})).unwrap(),
    )
    .unwrap();
    st
}

#[test]
fn mhist1_rdy_context_carries_the_justification_and_the_history_newest_first() {
    let (_d, mut r) = project();
    let s = seeded(&mut r);
    let mut st = mcp(r);
    let v = rdy_context(
        &mut st,
        from_value(json!({"id": s.claim, "scope": "canon"})).unwrap(),
    )
    .unwrap();
    assert_eq!(v["justification"], json!({"implements": [s.dec]}));
    let h = v["history"].as_array().unwrap();
    assert_eq!(h.len(), 2, "{v}");
    assert_eq!(h[0]["change"], s.change);
    assert_eq!(h[0]["author"]["actor"], "agent:a");
    assert_eq!(h[0]["landing"]["approved_by"], "human:qa");
    assert_eq!(h[1]["op"], "create");
    assert_eq!(v["history_more"], false);
    let v = rdy_context(
        &mut st,
        from_value(json!({"id": s.doc, "scope": "canon"})).unwrap(),
    )
    .unwrap();
    assert_eq!(v["justification"], Value::Null);
}

#[test]
fn mhist1_rdy_context_keeps_the_newest_twenty_versions() {
    let (_d, mut r) = project();
    let s = seeded(&mut r);
    for i in 0..20 {
        write(
            &mut r,
            "work",
            json!([{"op": "update", "id": s.claim, "body": format!("V{i}.")}]),
        )
        .unwrap();
    }
    let mut st = mcp(r);
    let v = rdy_context(
        &mut st,
        from_value(json!({"id": s.claim, "scope": "work", "max_tokens": 20000})).unwrap(),
    )
    .unwrap();
    assert_eq!(v["history"].as_array().unwrap().len(), CONTEXT_HISTORY);
    assert_eq!(v["history_more"], true);
    assert_eq!(
        v["history"][0]["landing"],
        Value::Null,
        "the newest version is unlanded"
    );
}

#[test]
fn mhist1_rdy_read_compact_lines_carry_each_blocks_justification() {
    let (_d, mut r) = project();
    let s = seeded(&mut r);
    let mut st = mcp(r);
    let v = rdy_read(
        &mut st,
        from_value(json!({"id": s.doc, "scope": "canon", "max_tokens": 20000})).unwrap(),
    )
    .unwrap();
    let text = v["text"].as_str().unwrap().to_string();
    let line = |id: &str| -> String {
        text.lines()
            .find(|l| l.trim_start().starts_with(id))
            .unwrap_or_else(|| panic!("{id}: {text}"))
            .to_string()
    };
    assert!(
        line(&s.claim).ends_with(&format!(" [implements {}]", s.dec)),
        "{text}"
    );
    assert!(line(&s.term).ends_with(" [editorial typo]"), "{text}");
    let doc = line(&s.doc);
    assert!(
        !doc.contains("[implements") && !doc.contains("[editorial"),
        "{text}"
    );
    let y = rdy_read(
        &mut st,
        from_value(json!({"id": s.doc, "scope": "canon", "format": "yaml", "max_tokens": 20000}))
            .unwrap(),
    )
    .unwrap();
    assert!(
        !y["text"].as_str().unwrap().contains("[implements"),
        "yaml stays content only"
    );
}

#[test]
fn mhist1_rdy_read_survives_a_damaged_producer_index_without_markers() {
    // fix round 1 (M1): a compact read degrades to no justification markers and a warning
    let (d, mut r) = project();
    let s = seeded(&mut r);
    // a change in view work that does not write the claim's version, indexed as its producer
    let x = write(
        &mut r,
        "work",
        json!([{"op": "update", "id": s.term, "body": "Balances, by wallet."}]),
    )
    .unwrap();
    let claim = BlockId::parse(&s.claim).unwrap();
    let v = r
        .read()
        .materialized(&Scope::View("work".into()))
        .unwrap()
        .version_of(&claim)
        .unwrap();
    r.close().unwrap();
    {
        let idx = rdy_store::index::Index::open(&d.path().join(".rdy/store/index.redb")).unwrap();
        let upto = idx.indexed_upto().unwrap();
        idx.apply_durable(
            &[rdy_store::index::IndexEntry::Producer {
                version: v,
                change: x.change.parse::<rdy_model::Hash>().unwrap(),
            }],
            upto,
        )
        .unwrap();
    }
    let r = Rdy::open(d.path()).unwrap();
    assert_eq!(
        r.read()
            .last_changes(&Scope::View("work".into()), &[claim])
            .unwrap_err()
            .code,
        E_IO,
        "the damage is real"
    );
    let mut st = mcp(r);
    let v = rdy_read(
        &mut st,
        from_value(json!({"id": s.doc, "scope": "work", "max_tokens": 20000})).unwrap(),
    )
    .unwrap();
    let text = v["text"].as_str().unwrap();
    assert!(text.contains(&s.claim), "{v}");
    assert!(
        !text.contains("[implements") && !text.contains("[editorial"),
        "{v}"
    );
    let w = v["warnings"].as_array().expect("warnings");
    assert_eq!(w.len(), 1, "{v}");
    assert_eq!(w[0]["code"], E_IO);
    assert!(
        w[0]["message"]
            .as_str()
            .unwrap()
            .contains("does not write version"),
        "{v}"
    );
    assert!(w[0]["fix"].as_str().unwrap().contains("rdy fsck"), "{v}");
}

/// The repro case `blame-omits-relay`: a landing that carries a relayed approval says who relayed
/// it in the history entry, as `rdy log` does.
#[test]
fn df7_context_history_names_the_relay() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = agent("agent:writer-a", Some("r1"));
    let w = r
        .write(
            &a,
            WriteRequest {
                view: "r1".into(),
                ops: from_value(
                    json!([{"op": "create", "type": "claim", "parent": section, "body": "Relayed."}]),
                )
                .unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
    let claim = w.created.values().next().unwrap().clone();
    let p = r.propose(&a, "r1", "c", Some(TEST_SUMMARY)).unwrap();
    r.land_with(
        &approver(),
        &p.r#ref,
        LandOptions {
            relay: Some(Relay {
                by: ActorRun {
                    actor: "agent:writer-a".into(),
                    run: Some("r1".into()),
                },
                evidence: RelayEvidence {
                    quoted: "yes, land it".into(),
                    session_ref: "chat-1".into(),
                    question: None,
                },
            }),
            ..Default::default()
        },
    )
    .unwrap();
    let mut st = mcp(r);
    let v = rdy_context(
        &mut st,
        from_value(json!({"id": claim, "scope": "canon"})).unwrap(),
    )
    .unwrap();
    // final review M3: the same {actor, run} object as `author`, `imported_by` and `rdy_log`
    assert_eq!(
        v["history"][0]["landing"]["relayed_by"],
        json!({"actor": "agent:writer-a", "run": "r1"}),
        "{v}"
    );
    assert_eq!(v["history"][0]["landing"]["approved_by"], "human:qa");
}
