//! cp5 (p-0SH30): a re-affirmation needs a briefing of each decision it re-affirms, and a bulk
//! op's refusal carries a fresh briefing of exactly those decisions; it needs no justification.
use rdy_engine::brief::*;
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::edit_loop::*;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};

/// [`stable_spec`], then accepted decisions `d` (three, citing claim A) and `other` (citing
/// nothing) under the stable section, and view `w` changing A: in `w` the three are suspect on A.
fn suspect_decisions(r: &mut Rdy) -> (Spec, Vec<BlockId>, BlockId) {
    let s = stable_spec(r);
    let t = brief_of(r, &Scope::Canon, &[&s.section]).base.token;
    let decide = |r: &mut Rdy, statement: &str, body: String| {
        let res = r
            .decide(
                &writer(),
                "dv",
                DecisionInput {
                    parent: s.section.to_string(),
                    statement: statement.into(),
                    body,
                    brief: Some(t.clone()),
                    ..Default::default()
                },
            )
            .unwrap_or_else(|e| panic!("decide {statement}: {e}"));
        BlockId::parse(&res.created["$decision"]).unwrap()
    };
    let cite = format!("Per {{{{{}}}}}.", s.a);
    let mut d = vec![
        decide(r, "One", cite.clone()),
        decide(r, "Two", cite.clone()),
        decide(r, "Three", cite),
    ];
    let other = decide(r, "Other", "Stands alone.".into());
    land(r, "dv").unwrap();
    d.sort();
    let ta = brief_of(r, &Scope::Canon, &[&s.a]).base.token;
    write_cited(
        r,
        "w",
        json!([{"op": "update", "id": s.a.to_string(), "body": "A holds, revised."}]),
        Some(&ta),
    )
    .unwrap();
    (s, d, other)
}

fn ids(v: &[BlockId]) -> Value {
    Value::Array(v.iter().map(|b| json!(b.to_string())).collect())
}

#[test]
fn cp5_bulk_brief_refusal_covers_exactly_the_selected_decisions() {
    let (_dir, mut r) = project();
    let (s, d, _other) = suspect_decisions(&mut r);
    let op = json!([{"op": "reaffirm", "because": s.a.to_string(), "reason": "Holds."}]);
    let selected = r
        .reaffirm_selection(&writer(), "w", &parse_test_ops(op.clone()).unwrap())
        .unwrap();
    assert_eq!(
        selected.into_iter().collect::<Vec<_>>(),
        d,
        "the public selection"
    );
    let before = r.store().view("w").unwrap().changes.len();
    // no briefing: the refusal names exactly the selected decisions, never their parent
    let e = write_cited(&mut r, "w", op.clone(), None).unwrap_err();
    assert_eq!(e.code, E_BRIEF_REQUIRED, "{e}");
    assert_eq!(e.briefing.as_ref().unwrap()["ids"], ids(&d));
    assert!(!e.message.contains(&s.section.to_string()), "{e}");
    let (e, t) = token_in(&r, e, "w");
    assert_eq!(
        e.briefing.as_ref().unwrap()["covers"],
        d.len(),
        "exactly the selected decisions"
    );
    // a briefing of one of them covers only that one
    let one = brief_of(&r, &Scope::View("w".into()), &[&d[0]]).base.token;
    let e = write_cited(&mut r, "w", op.clone(), Some(&one)).unwrap_err();
    assert_eq!(e.code, E_BRIEF_REQUIRED, "{e}");
    assert_eq!(
        e.message,
        format!(
            "the cited briefing does not cover {}, {}, which this write touches on stable content",
            d[1], d[2]
        )
    );
    assert_eq!(
        e.issues
            .iter()
            .filter_map(|i| i.block.clone())
            .collect::<Vec<_>>(),
        d[1..].to_vec()
    );
    assert!(
        e.issues
            .iter()
            .all(|i| i.message == "not covered by the cited briefing")
    );
    assert_eq!(
        r.store().view("w").unwrap().changes.len(),
        before,
        "nothing written"
    );
    // the refusal's own token lands the write
    let res = write_cited(&mut r, "w", op, Some(&t)).unwrap();
    assert_eq!(
        res.reaffirmed
            .iter()
            .map(|x| x.id.clone())
            .collect::<Vec<_>>(),
        d.iter().map(ToString::to_string).collect::<Vec<_>>()
    );
}

#[test]
fn cp5_reaffirm_needs_no_justification() {
    let (_dir, mut r) = project();
    let (s, d, other) = suspect_decisions(&mut r);
    let op = json!([{"op": "reaffirm", "id": d[0].to_string(), "reason": "Holds."}]);
    let e = write_cited(&mut r, "w", op.clone(), None).unwrap_err();
    assert_eq!(e.code, E_BRIEF_REQUIRED, "{e}");
    assert_eq!(e.briefing.as_ref().unwrap()["ids"], ids(&d[..1]));
    // a briefing of something else does not cover it
    let wrong = brief_of(&r, &Scope::View("w".into()), &[&other]).base.token;
    let e = write_cited(&mut r, "w", op.clone(), Some(&wrong)).unwrap_err();
    assert_eq!(e.code, E_BRIEF_REQUIRED, "{e}");
    // with a valid briefing and no justification, the stable decision is re-affirmed
    let t = brief_of(&r, &Scope::View("w".into()), &[&d[0]]).base.token;
    let res = write_cited(&mut r, "w", op, Some(&t)).unwrap();
    assert_eq!(
        serde_json::to_value(&res.reaffirmed).unwrap(),
        json!([{"id": d[0].to_string(), "causes": [s.a.to_string()]}])
    );
    let change = r.store().change(&res.change.parse().unwrap()).unwrap();
    assert!(change.justification.is_none());
    assert!(
        r.read()
            .context(&Scope::View("w".into()), &d[0])
            .unwrap()
            .suspects
            .is_empty()
    );
}
