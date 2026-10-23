//! rf5 (D-2WR0G): a write checks only the decision basis and time values it sets. The four time
//! fields take an RFC 3339 UTC time or a date; a delegated basis needs a non-blank attested_by;
//! an accepted decision's frozen basis still wins.
mod common;

use rdy_engine::decision_fields::{
    ATTESTED_BY_FIX, ATTESTED_BY_MESSAGE, TIME_FIELDS, TIME_FIX, is_utc_time_or_date, validate_set,
};
use rdy_engine::*;
use rdy_model::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn writer() -> Session {
    Session {
        actor: "agent:designer".into(),
        run: Some("run-1".into()),
        role: Role::Writer,
        kind: SessionKind::Human,
    }
}

/// An approver: the only role that may write decided_at.
fn founder() -> Session {
    Session {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
        kind: SessionKind::Human,
    }
}

fn write_as(r: &mut Rdy, s: &Session, ops: Value) -> Result<WriteResult, RdyError> {
    r.write(
        s,
        WriteRequest {
            view: "work".into(),
            ops: parse_ops(serde_json::from_value(ops).unwrap()).unwrap(),
            justification: None,
            message: String::new(),
        },
    )
}

/// A project with a decisions doc and its section; returns the section id.
fn setup() -> (tempfile::TempDir, Rdy, String) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write_as(
        &mut r,
        &writer(),
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Decisions", "doc_type": "decisions-log"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Decisions"}}
        ]),
    )
    .unwrap();
    (dir, r, w.created["$s"].clone())
}

/// The issues of a refused write as (code, message, fix).
fn issues(e: &RdyError) -> Vec<(&'static str, String, String)> {
    e.issues
        .iter()
        .map(|i| (i.code, i.message.clone(), i.fix.clone()))
        .collect()
}

fn time_issue(name: &str) -> (&'static str, String, String) {
    (
        "E_FIELD_KIND",
        format!("field \"{name}\" must be an RFC 3339 UTC time or a date"),
        TIME_FIX.to_string(),
    )
}

fn attest_issue() -> (&'static str, String, String) {
    (
        "E_FIELD_REQUIRED",
        ATTESTED_BY_MESSAGE.to_string(),
        ATTESTED_BY_FIX.to_string(),
    )
}

fn decision_block(fields: &[(&str, FieldValue)]) -> Block {
    let mut f: BTreeMap<String, FieldValue> = [
        ("statement", FieldValue::Str("S".into())),
        ("status", FieldValue::Str("proposed".into())),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    for (k, v) in fields {
        f.insert(k.to_string(), v.clone());
    }
    Block {
        id: BlockId::parse("D-AAAAA").unwrap(),
        block_type: "decision".into(),
        parent: Some(BlockId::parse("s-AAAAA").unwrap()),
        order: "V".into(),
        fields: f,
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn names(n: &[&str]) -> BTreeSet<String> {
    n.iter().map(|s| s.to_string()).collect()
}

const ACCEPTED: [&str; 9] = [
    "2026-10-05",
    "2026-10-21T15:10:58Z",
    "2026-10-21T15:10:58.123Z",
    "2026-10-21T15:10:58.1+00:00",
    "2026-10-21T15:10:58+00:00",
    "2024-02-29",
    "2000-02-29T00:00:00Z",
    "0001-01-01",
    "9999-12-31T23:59:59Z",
];

const REFUSED: [&str; 20] = [
    "",
    "next month",
    "2026-13-01",
    "2026-00-10",
    "2026-04-31",
    "2023-02-29",
    "1900-02-29",
    "0000-01-01",
    "2026-1-5",
    "2026-10-21t15:10:58Z",
    "2026-10-21T15:10:58z",
    "2026-10-21 15:10:58Z",
    "2026-10-21T24:00:00Z",
    "2026-10-21T15:60:00Z",
    "2026-10-21T15:10:60Z",
    "2026-10-21T15:10:58",
    "2026-10-21T15:10:58+01:00",
    "2026-10-21T15:10:58.Z",
    "2026-10-21T15:10Z",
    "2026-10-05Z",
];

#[test]
fn rf5_time_fields_validate_only_supplied_values() {
    // the closed spelling: accepted and refused values, and the 2000-character bound
    for v in ACCEPTED {
        assert!(is_utc_time_or_date(v), "{v}");
    }
    for v in REFUSED {
        assert!(!is_utc_time_or_date(v), "{v}");
    }
    let long = |n: usize| format!("2026-10-21T15:10:58.{}Z", "1".repeat(n - 21));
    assert_eq!(long(2000).chars().count(), 2000);
    assert!(is_utc_time_or_date(&long(2000)));
    assert!(!is_utc_time_or_date(&long(2001)));

    // validate_set reads only the supplied names, and a non-string value fails too
    let all = names(&TIME_FIELDS);
    for name in TIME_FIELDS {
        let b = decision_block(&[(name, FieldValue::Str("soon".into()))]);
        assert_eq!(
            validate_set(&b, &names(&[name]))
                .iter()
                .map(|i| (i.code, i.message.clone(), i.fix.clone()))
                .collect::<Vec<_>>(),
            vec![time_issue(name)]
        );
        assert!(validate_set(&b, &names(&["statement"])).is_empty());
        let b = decision_block(&[(name, FieldValue::Int(20261005))]);
        assert_eq!(validate_set(&b, &all).len(), 1, "{name}");
        for v in ACCEPTED {
            let b = decision_block(&[(name, FieldValue::Str(v.into()))]);
            assert!(validate_set(&b, &all).is_empty(), "{name} {v}");
        }
    }
    // a type change to decision supplies all four: every present bad one is reported, in order
    let bad: Vec<(&str, FieldValue)> = TIME_FIELDS
        .iter()
        .map(|n| (*n, FieldValue::Str("later".into())))
        .collect();
    let b = decision_block(&bad);
    assert_eq!(
        validate_set(&b, &all)
            .iter()
            .map(|i| i.message.clone())
            .collect::<Vec<_>>(),
        TIME_FIELDS
            .iter()
            .map(|n| time_issue(n).1)
            .collect::<Vec<_>>()
    );
    // another type is never checked
    let mut c = b.clone();
    c.block_type = "claim".into();
    assert!(validate_set(&c, &all).is_empty());

    // through the write: a create with each bad name is refused before anything is written
    let (_d, mut r, s) = setup();
    for name in TIME_FIELDS {
        let seq = r.read().seq();
        let e = write_as(
            &mut r,
            &founder(),
            json!([{"op": "create", "type": "decision", "parent": s,
                "fields": {"statement": "S", "status": "proposed", name: "soon"}}]),
        )
        .unwrap_err();
        assert_eq!(e.code, E_VALIDATION, "{name}: {e}");
        assert_eq!(issues(&e), vec![time_issue(name)], "{name}");
        assert_eq!(e.message, time_issue(name).1);
        assert_eq!(r.read().seq(), seq, "{name}: nothing written");
        // accepted spellings keep their bytes
        let v = "2026-10-21T15:10:58.5+00:00";
        let w = write_as(
            &mut r,
            &founder(),
            json!([{"op": "create", "type": "decision", "parent": s,
                "fields": {"statement": "S", "status": "proposed", name: v}}]),
        )
        .unwrap();
        let id = BlockId::parse(&w.created["op0"]).unwrap();
        let b = r.read().block(&Scope::View("work".into()), &id).unwrap();
        assert_eq!(b.field_str(name), Some(v), "{name}");
    }
    // rdy_decide writes recorded_at with the engine's own clock, and it passes
    let w = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: s.clone(),
                statement: "Times pass".into(),
                review_by: Some("2027-01-01".into()),
                effective_from: Some("2026-10-21T15:10:58Z".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let decided = w.created["$decision"].clone();
    let e = r
        .decide(
            &writer(),
            "work",
            DecisionInput {
                parent: s.clone(),
                statement: "Times fail".into(),
                review_by: Some("Q3".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert_eq!(issues(&e), vec![time_issue("review_by")]);

    // an untouched old bad value passes an unrelated update; setting it again (the same value)
    // fails; unsetting it passes
    let old = decision_block(&[
        ("review_by", FieldValue::Str("soon".into())),
        ("effective_from", FieldValue::Str("whenever".into())),
    ]);
    let mut old = old;
    old.id = BlockId::parse("D-XYZ23").unwrap();
    old.parent = Some(BlockId::parse(&s).unwrap());
    raw_commit(&mut r, old);
    write_as(
        &mut r,
        &writer(),
        json!([{"op": "update", "id": "D-XYZ23", "fields": {"statement": "S, again"}}]),
    )
    .unwrap();
    let seq = r.read().seq();
    let e = write_as(
        &mut r,
        &writer(),
        json!([{"op": "update", "id": "D-XYZ23", "fields": {"review_by": "soon"}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_VALIDATION);
    assert_eq!(issues(&e), vec![time_issue("review_by")]);
    assert_eq!(r.read().seq(), seq);
    write_as(
        &mut r,
        &writer(),
        json!([{"op": "update", "id": "D-XYZ23", "unset": ["review_by"]}]),
    )
    .unwrap();
    write_as(
        &mut r,
        &writer(),
        json!([{"op": "update", "id": decided, "fields": {"review_by": "2028-02-29"}}]),
    )
    .unwrap();
}

/// Writes `block` to view `work` straight through the store, bypassing every engine rule.
fn raw_commit(r: &mut Rdy, block: Block) {
    let by = writer().provenance();
    r.store_mut()
        .commit_to_view(
            "work",
            Change {
                view: "work".into(),
                ops: vec![Op::Create { block }],
                deps: vec![],
                provenance: by,
                justification: None,
                message: "raw".into(),
                links: ChangeLinks::default(),
                read_identity: None,
            },
        )
        .unwrap();
    r.invalidate();
}

#[test]
fn rf5_delegated_requires_attestation_and_keeps_freeze() {
    let (_d, mut r, s) = setup();
    let decide = |basis: Option<&str>, attested: Option<&str>| DecisionInput {
        parent: s.clone(),
        statement: "Delegated rulings name who ruled".into(),
        authority_basis: basis.map(String::from),
        attested_by: attested.map(String::from),
        ..Default::default()
    };
    // create: missing and blank attestations are refused; nothing is written
    for attested in [None, Some(""), Some("  \t")] {
        let seq = r.read().seq();
        let e = r
            .decide(&writer(), "work", decide(Some("delegated"), attested))
            .unwrap_err();
        assert_eq!(e.code, E_VALIDATION, "{attested:?}: {e}");
        assert_eq!(issues(&e), vec![attest_issue()], "{attested:?}");
        assert_eq!(e.message, ATTESTED_BY_MESSAGE);
        assert_eq!(r.read().seq(), seq);
    }
    // the same through rdy_write's create
    let e = write_as(
        &mut r,
        &writer(),
        json!([{"op": "create", "type": "decision", "parent": s,
            "fields": {"statement": "S", "status": "proposed", "authority_basis": "delegated"}}]),
    )
    .unwrap_err();
    assert_eq!(issues(&e), vec![attest_issue()]);
    let w = r
        .decide(
            &writer(),
            "work",
            decide(Some("delegated"), Some("Nick, under the M3 mandate")),
        )
        .unwrap();
    let delegated = w.created["$decision"].clone();
    // update guards: unsetting or blanking the attestation of a delegated decision fails, and so
    // does turning a decision delegated without one
    for op in [
        json!({"op": "update", "id": delegated, "unset": ["attested_by"]}),
        json!({"op": "update", "id": delegated, "fields": {"attested_by": " "}}),
    ] {
        let e = write_as(&mut r, &writer(), json!([op])).unwrap_err();
        assert_eq!(issues(&e), vec![attest_issue()], "{op}");
    }
    let w = r
        .decide(&writer(), "work", decide(Some("direct_ruling"), None))
        .unwrap();
    let direct = w.created["$decision"].clone();
    let e = write_as(
        &mut r,
        &writer(),
        json!([{"op": "update", "id": direct, "fields": {"authority_basis": "delegated"}}]),
    )
    .unwrap_err();
    assert_eq!(issues(&e), vec![attest_issue()]);
    // an unrelated update of a delegated decision that holds an attestation passes
    write_as(
        &mut r,
        &writer(),
        json!([{"op": "update", "id": delegated, "fields": {"statement": "Delegated rulings name their mandate"}}]),
    )
    .unwrap();

    // an accepted decision's basis stays frozen: the freeze is reported, not the attestation
    let p = r
        .propose(&writer(), "work", "basis", Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(&mut r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
    for fields in [
        json!({"authority_basis": "delegated"}),
        json!({"authority_basis": "delegated", "attested_by": "Nick"}),
    ] {
        let e = write_as(
            &mut r,
            &writer(),
            json!([{"op": "update", "id": direct, "fields": fields}]),
        )
        .unwrap_err();
        assert_eq!(e.code, E_FROZEN, "{e}");
    }

    // import: a delegated decision with no attestation is refused before anything is stored
    let (_d2, mut src, s2) = setup();
    let mut bad = decision_block(&[("authority_basis", FieldValue::Str("delegated".into()))]);
    bad.id = BlockId::parse("D-DGT23").unwrap();
    bad.parent = Some(BlockId::parse(&s2).unwrap());
    raw_commit(&mut src, bad);
    let out = tempfile::tempdir().unwrap();
    src.read()
        .export(&Scope::View("work".into()), out.path())
        .unwrap();
    let dst_dir = tempfile::tempdir().unwrap();
    let mut dst = Rdy::init(dst_dir.path(), "rdy-standard").unwrap();
    let seq = dst.read().seq();
    let e = dst.import(&founder(), "work", out.path()).unwrap_err();
    assert_eq!(e.code, E_VALIDATION, "{e}");
    assert_eq!(issues(&e), vec![attest_issue()]);
    assert_eq!(dst.read().seq(), seq, "a refused import writes nothing");
}
