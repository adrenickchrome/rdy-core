//! intn1: intents are advisory, versioned and expire at read time; iwr1: writes record an
//! intent and are told about other sessions' intents on what they wrote.
use rdy_engine::intent::*;
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::fixtures::*;
use rdy_qa::packs::qa_project;
use serde_json::json;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

#[test]
fn intn1_ttl_text() {
    assert_eq!(parse_ttl("30m").unwrap(), 1_800_000);
    assert_eq!(parse_ttl("2h").unwrap(), TTL_DEFAULT_MS);
    assert_eq!(parse_ttl("7d").unwrap(), TTL_MAX_MS);
    for bad in ["8d", "0h", "1s", "h", "2 h", "-1h", ""] {
        assert_eq!(parse_ttl(bad).unwrap_err().code, E_BAD_INPUT, "{bad}");
    }
}

#[test]
fn intn1_post_list_renew_withdraw() {
    let (_d, mut r) = qa_project();
    let (_doc, _s, claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = agent("agent:a", Some("ra"));
    let _clock = pin_clock(1_000_000);
    let i = r
        .intent_post(
            &a,
            None,
            Some("payments"),
            std::slice::from_ref(&claim),
            "rework fees",
            None,
        )
        .unwrap();
    assert!(i.id.starts_with("in-") && i.id.len() == 15);
    assert_eq!(i.view, "ra", "the session's view by default");
    assert_eq!(i.status, "open");
    assert_eq!(i.expires_at_ms - i.posted_at_ms, TTL_DEFAULT_MS);
    let listed = r
        .read()
        .intents(&IntentFilter::default(), i.posted_at_ms + 1)
        .unwrap();
    assert_eq!(listed, vec![i.clone()]);
    let by_block = IntentFilter {
        block: Some(id(&claim)),
        ..Default::default()
    };
    assert_eq!(
        r.read()
            .intents(&by_block, i.posted_at_ms + 1)
            .unwrap()
            .len(),
        1
    );
    let by_actor = IntentFilter {
        actor: Some("agent:b".into()),
        ..Default::default()
    };
    assert!(
        r.read()
            .intents(&by_actor, i.posted_at_ms + 1)
            .unwrap()
            .is_empty()
    );
    // renewing by a prefix writes a new version and keeps the id
    let prefix = &i.intent[..8];
    let renewed = r
        .intent_renew(
            &agent("agent:a", Some("other-run")),
            prefix,
            Some(parse_ttl("1d").unwrap()),
        )
        .unwrap();
    assert_eq!(renewed.id, i.id);
    assert_eq!(renewed.prev.as_deref(), Some(i.version.as_str()));
    assert_eq!(
        renewed.posted_at_ms, i.posted_at_ms,
        "posted_at is the first version's"
    );
    let e = r
        .intent_renew(&agent("agent:b", Some("rb")), &i.id, None)
        .unwrap_err();
    assert_eq!(e.code, E_PERMISSION);
    let w = r.intent_withdraw(&writer(), &i.id).unwrap();
    assert_eq!(w.status, "withdrawn");
    let now = w.posted_at_ms + 2;
    assert!(
        r.read()
            .intents(&IntentFilter::default(), now)
            .unwrap()
            .is_empty()
    );
    let all = IntentFilter {
        include_expired: true,
        ..Default::default()
    };
    assert_eq!(r.read().intents(&all, now).unwrap()[0].status, "withdrawn");
    assert_eq!(
        r.intent_renew(&a, &i.id, None).unwrap_err().code,
        E_BAD_INPUT
    );
}

#[test]
fn intn1_post_is_validated() {
    let (_d, mut r) = qa_project();
    let (_doc, _s, claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = agent("agent:a", Some("ra"));
    let e = r.intent_post(&a, None, None, &[], "x", None).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert!(e.fix.contains("payments"), "{}", e.fix);
    assert_eq!(
        r.intent_post(&a, None, Some("nowhere"), &[], "x", None)
            .unwrap_err()
            .code,
        E_BAD_INPUT
    );
    assert_eq!(
        r.intent_post(&a, None, Some("payments"), &[], "", None)
            .unwrap_err()
            .code,
        E_BAD_INPUT
    );
    assert_eq!(
        r.intent_post(&a, None, Some("payments"), &[], &"x".repeat(501), None)
            .unwrap_err()
            .code,
        E_BAD_INPUT
    );
    let many: Vec<String> = (0..1001).map(|_| claim.clone()).collect();
    assert_eq!(
        r.intent_post(&a, None, None, &many, "x", None)
            .unwrap_err()
            .code,
        E_LIMIT
    );
    assert_eq!(
        r.intent_post(&a, Some("work"), None, &["c-ZZZZZ".into()], "x", None)
            .unwrap_err()
            .code,
        E_UNKNOWN_BLOCK
    );
    assert_eq!(
        r.intent_post(&reader(), None, Some("payments"), &[], "x", None)
            .unwrap_err()
            .code,
        E_PERMISSION
    );
}

#[test]
fn intn1_expiry_is_read_time_and_inclusive_of_the_bound() {
    let (_d, mut r) = qa_project();
    let a = agent("agent:a", Some("ra"));
    let i = {
        let _clock = pin_clock(5_000_000);
        r.intent_post(&a, None, Some("payments"), &[], "x", Some(TTL_MIN_MS))
            .unwrap()
    };
    let st = r.read();
    let f = IntentFilter::default();
    assert_eq!(st.intents(&f, i.expires_at_ms - 1).unwrap().len(), 1);
    assert!(
        st.intents(&f, i.expires_at_ms).unwrap().is_empty(),
        "expired at expires_at_ms"
    );
    let all = IntentFilter {
        include_expired: true,
        ..Default::default()
    };
    assert_eq!(
        st.intents(&all, i.expires_at_ms).unwrap()[0].status,
        "expired"
    );
    drop(st);
    let before = r.read().seq();
    let _clock = pin_clock(i.expires_at_ms);
    let e = r.intent_renew(&a, &i.id, None).unwrap_err();
    assert_eq!(e.code, E_INTENT_EXPIRED);
    assert_eq!(
        r.intent_withdraw(&a, &i.id).unwrap_err().code,
        E_INTENT_EXPIRED
    );
    assert_eq!(
        r.read().seq(),
        before,
        "a refusal writes nothing; expiry writes nothing"
    );
}

#[test]
fn iwr1_a_write_records_its_intent_and_hears_of_other_sessions_intents() {
    let (_d, mut r) = qa_project();
    r.new_doc(
        &writer(),
        "seed",
        "payments/fees",
        Some("glossary"),
        Some("Fees"),
    )
    .unwrap();
    let doc = r
        .read()
        .resolve_path(&Scope::View("seed".into()), "payments/fees")
        .unwrap()
        .to_string();
    let res = write(&mut r, "seed", json!([
        {"op": "create", "type": "section", "id": "$s", "parent": doc, "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "fee"}
    ])).unwrap();
    land(&mut r, "seed").unwrap();
    let claim = res.created["$c"].clone();
    let a = agent("agent:a", Some("ra"));
    let b = agent("agent:b", Some("rb"));
    let ia = r
        .intent_post(&a, None, Some("payments"), &[], "rework fees", None)
        .unwrap();
    let ib = r
        .intent_post(
            &b,
            None,
            None,
            std::slice::from_ref(&claim),
            "b's angle",
            None,
        )
        .unwrap();
    // b writes the claim under its own intent: the change links it
    let v = version_of(&r, &Scope::Canon, &claim);
    let req = WriteRequest {
        view: "rb".into(),
        ops: serde_json::from_value(json!([{"op": "update", "id": claim, "base": v, "body": "b"}]))
            .unwrap(),
        justification: None,
        message: String::new(),
    };
    let w = r.write_with_intent(&b, req, None, Some(&ib.id)).unwrap();
    let ch = r
        .read()
        .reader()
        .change(&w.change.parse().unwrap())
        .unwrap();
    assert_eq!(
        ch.links.intent.map(|h| h.to_string()),
        Some(ib.intent.clone())
    );
    // the notes b's write gets: a's intent (area), never b's own
    let st = r.read();
    let touched = [id(&claim)].into_iter().collect();
    let notes = st
        .intent_notes(
            &Scope::View("rb".into()),
            "agent:b",
            Some("rb"),
            &touched,
            now_ms(),
        )
        .unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].id, ia.id);
    assert_eq!(notes[0].blocks, vec![claim.clone()]);
    // an unknown intent refuses the write and writes nothing
    let req = WriteRequest {
        view: "rb".into(),
        ops: serde_json::from_value(json!([{"op": "update", "id": claim, "base": version_of(&r, &Scope::View("rb".into()), &claim), "body": "c"}])).unwrap(),
        justification: None,
        message: String::new(),
    };
    assert_eq!(
        r.write_with_intent(&b, req, None, Some("in-ffffffffffff"))
            .unwrap_err()
            .code,
        E_NOT_FOUND
    );
}

#[test]
fn iwr1_write_and_report_carries_intents_only_when_there_are_some() {
    use rdy_engine::consequence::write_and_report;
    use rdy_engine::engine::{Engine, Local};
    let (_d, mut r) = qa_project();
    let (_doc, section, claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    r.intent_post(
        &agent("agent:a", Some("ra")),
        None,
        None,
        std::slice::from_ref(&claim),
        "mine",
        None,
    )
    .unwrap();
    let v = version_of(&r, &Scope::Canon, &claim);
    let engine = Local::new(r);
    let b = agent("agent:b", Some("rb"));
    let ops: Vec<OpInput> =
        serde_json::from_value(json!([{"op": "update", "id": claim, "base": v, "body": "b"}]))
            .unwrap();
    let out = write_and_report(&engine, "rb", None, move |r| {
        r.write(
            &b,
            WriteRequest {
                view: "rb".into(),
                ops,
                justification: None,
                message: String::new(),
            },
        )
    })
    .unwrap();
    assert_eq!(out["intents"].as_array().unwrap().len(), 1, "{out}");
    assert_eq!(out["intents"][0]["actor"], "agent:a");
    // a write that touches nothing an intent covers has no intents key
    let a = agent("agent:a", Some("ra"));
    let ops: Vec<OpInput> = serde_json::from_value(
        json!([{"op": "create", "type": "claim", "parent": section, "body": "other"}]),
    )
    .unwrap();
    let out = write_and_report(&engine, "ra", None, move |r| {
        r.write(
            &a,
            WriteRequest {
                view: "ra".into(),
                ops,
                justification: None,
                message: String::new(),
            },
        )
    })
    .unwrap();
    assert!(out.get("intents").is_none(), "{out}");
    let _ = engine.read();
}

#[test]
fn iwr1_a_delete_or_a_move_out_of_an_area_still_hears_of_the_area_intent() {
    use rdy_engine::consequence::write_and_report;
    use rdy_engine::engine::Local;
    let (_d, mut r) = qa_project();
    r.new_doc(
        &writer(),
        "seed",
        "payments/fees",
        Some("glossary"),
        Some("Fees"),
    )
    .unwrap();
    r.new_doc(
        &writer(),
        "seed",
        "notes/other",
        Some("glossary"),
        Some("Other"),
    )
    .unwrap();
    let doc = r
        .read()
        .resolve_path(&Scope::View("seed".into()), "payments/fees")
        .unwrap()
        .to_string();
    let other = r
        .read()
        .resolve_path(&Scope::View("seed".into()), "notes/other")
        .unwrap()
        .to_string();
    let res = write(&mut r, "seed", json!([
        {"op": "create", "type": "section", "id": "$s", "parent": doc, "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "fee"}
    ])).unwrap();
    land(&mut r, "seed").unwrap();
    let section = res.created["$s"].clone();
    let claim = res.created["$c"].clone();
    let ia = r
        .intent_post(
            &agent("agent:a", Some("ra")),
            None,
            Some("payments"),
            &[],
            "rework fees",
            None,
        )
        .unwrap();
    let vc = version_of(&r, &Scope::Canon, &claim);
    let vs = version_of(&r, &Scope::Canon, &section);
    let engine = Local::new(r);
    let b = agent("agent:b", Some("rb"));
    let report = |ops: serde_json::Value| {
        let b = b.clone();
        let ops: Vec<OpInput> = serde_json::from_value(ops).unwrap();
        write_and_report(&engine, "rb", None, move |r| {
            r.write(
                &b,
                WriteRequest {
                    view: "rb".into(),
                    ops,
                    justification: None,
                    message: String::new(),
                },
            )
        })
        .unwrap()
    };
    // a delete: the claim is gone after the write, but it was in payments before it
    let out = report(json!([{"op": "delete", "id": claim, "base": vc}]));
    assert_eq!(out["intents"].as_array().map(Vec::len), Some(1), "{out}");
    assert_eq!(out["intents"][0]["id"], ia.id.as_str());
    assert_eq!(out["intents"][0]["blocks"], json!([claim]));
    // a move out of payments: the section's new path is in no area, its old one was in payments
    let out = report(json!([{"op": "move", "id": section, "base": vs, "parent": other}]));
    assert_eq!(out["intents"].as_array().map(Vec::len), Some(1), "{out}");
    assert_eq!(out["intents"][0]["id"], ia.id.as_str());
    assert_eq!(out["intents"][0]["blocks"], json!([section]));
}

/// Final-review fix round 1: the open-intent index that writes, briefings, the list and the
/// coordination graph read follows every intent change (post, renew, withdraw, a landing and a
/// full re-read). It holds each intent whose latest version is open, expired ones included
/// (expiry is read-time), with its latest version, that version and when it was first posted.
#[test]
fn intn1_the_open_intent_index_follows_every_intent_change() {
    let (_d, mut r) = qa_project();
    let (_doc, _s, claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = agent("agent:a", Some("ra"));
    let index = |r: &Rdy| -> Vec<(String, String, String, i64)> {
        r.read()
            .coord_heads()
            .open_intents
            .iter()
            .map(|(stable, o)| {
                (
                    stable.to_string(),
                    o.version.to_string(),
                    o.intent.angle.clone(),
                    o.posted_at_ms,
                )
            })
            .collect()
    };
    let (i1, i2) = {
        let _clock = pin_clock(1_000_000);
        let i1 = r
            .intent_post(&a, None, None, std::slice::from_ref(&claim), "one", None)
            .unwrap();
        let i2 = r
            .intent_post(&a, None, Some("payments"), &[], "two", Some(60_000))
            .unwrap();
        (i1, i2)
    };
    let row = |i: &IntentInfo| {
        (
            i.intent.clone(),
            i.version.clone(),
            i.angle.clone(),
            i.posted_at_ms,
        )
    };
    let mut want = vec![row(&i1), row(&i2)];
    want.sort();
    assert_eq!(index(&r), want);
    // a renew moves the version and keeps the time of the first post
    let renewed = {
        let _clock = pin_clock(2_000_000);
        r.intent_renew(&a, &i1.id, None).unwrap()
    };
    assert_ne!(renewed.version, i1.version);
    assert_eq!(renewed.posted_at_ms, 1_000_000);
    let mut want = vec![row(&renewed), row(&i2)];
    want.sort();
    assert_eq!(index(&r), want);
    // i2 has expired by now: still in the index (expiry is read-time), not in the list
    assert!(
        r.read()
            .intents(&IntentFilter::default(), 3_000_000)
            .unwrap()
            .iter()
            .all(|i| i.id != i2.id)
    );
    // a withdrawn intent leaves the index, and stays in the heads
    {
        let _clock = pin_clock(3_000_000);
        r.intent_withdraw(&a, &renewed.id).unwrap();
    }
    assert_eq!(index(&r), vec![row(&i2)]);
    assert_eq!(r.read().coord_heads().intents.len(), 2);
    let listed = r
        .read()
        .intents(
            &IntentFilter {
                include_expired: true,
                ..Default::default()
            },
            3_000_000,
        )
        .unwrap();
    let mut statuses: Vec<(&str, &str)> = listed
        .iter()
        .map(|i| (i.angle.as_str(), i.status.as_str()))
        .collect();
    statuses.sort();
    assert_eq!(statuses, [("one", "withdrawn"), ("two", "expired")]);
    // a full re-read and a landing keep the same index
    r.invalidate();
    assert_eq!(index(&r), vec![row(&i2)]);
    let v = version_of(&r, &Scope::Canon, &claim);
    write(
        &mut r,
        "seed",
        json!([{"op": "update", "id": claim, "base": v, "body": "two"}]),
    )
    .unwrap();
    land(&mut r, "seed").unwrap();
    assert_eq!(index(&r), vec![row(&i2)]);
}

#[test]
fn msw13_intent_post_refuses_a_view_that_cannot_be_written() {
    let (_d, mut r) = qa_project();
    let (_doc, _s, claim) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = agent("agent:a", Some("ra"));
    for v in ["canon", "inflight", "tag:v1", "landing:x", ""] {
        let e = r
            .intent_post(&a, Some(v), Some("payments"), &[], "x", None)
            .unwrap_err();
        assert!(
            [E_RESERVED_VIEW, E_BAD_INPUT].contains(&e.code),
            "{v:?}: {e}"
        );
        let e = r
            .intent_post(&a, Some(v), None, std::slice::from_ref(&claim), "x", None)
            .unwrap_err();
        assert!(
            [E_RESERVED_VIEW, E_BAD_INPUT].contains(&e.code),
            "{v:?} with blocks: {e}"
        );
    }
    // a valid view that does not exist yet stays allowed (a session's default often doesn't)
    r.intent_post(&a, Some("not-yet"), Some("payments"), &[], "x", None)
        .unwrap();
}

#[test]
fn msw13_intent_post_caps_the_view_name() {
    let (_d, mut r) = qa_project();
    let a = agent("agent:a", Some("ra"));
    let e = r
        .intent_post(
            &a,
            Some(&"v".repeat(2_000_000)),
            Some("payments"),
            &[],
            "x",
            None,
        )
        .unwrap_err();
    assert_eq!(
        e.code,
        E_LIMIT,
        "{}",
        &e.message[..e.message.len().min(200)]
    );
    assert!(e.message.len() < 1_000, "the message clips the name");
}

#[test]
fn msw13_intent_post_strips_a_view_prefix_as_the_write_ops_do() {
    let (_d, mut r) = qa_project();
    let a = agent("agent:a", Some("ra"));
    let it = r
        .intent_post(&a, Some("view:foo"), Some("payments"), &[], "x", None)
        .unwrap();
    assert_eq!(it.view, "foo", "{it:?}");
    let e = r
        .intent_post(&a, Some("view:"), Some("payments"), &[], "x", None)
        .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
}
