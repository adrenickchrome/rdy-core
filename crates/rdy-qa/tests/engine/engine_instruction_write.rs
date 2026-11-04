//! ain1 (D-4W1ZR, D-2WY7Y, D-6CDCN): an instruction is written, checked and reviewed as any
//! normative block. Its selectors and reason are closed sets checked on the ordinary write path;
//! it needs the normal briefing on stable content, gets no coverage exemption, and turns on no
//! store feature.
use rdy_engine::brief::E_BRIEF_REQUIRED;
use rdy_engine::tensions::TensionKind;
use rdy_engine::{DecisionInput, Rdy, RdyError, Scope, WriteResult};
use rdy_model::{BlockId, FieldValue};
use rdy_qa::edit_loop::{brief_of, stable_spec, write_cited};
use rdy_qa::fixtures::*;
use rdy_qa::tensions::{j_implements, of_kind, put_j};
use serde_json::{Value, json};

const ID: &str = "ai-AAAAA";

/// A draft doc `rules` with section `s`, claim `c` and proposed decision `D`, in view `w`.
/// Returns the doc's, section's, claim's and decision's ids.
fn rules(r: &mut Rdy) -> (String, String, String, String) {
    let w = write(r, "w", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Tests"}},
        {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Tests guard the build."}
    ]))
    .unwrap();
    let s = w.created["$s"].clone();
    let d = r
        .decide(
            &writer(),
            "w",
            DecisionInput {
                parent: s.clone(),
                statement: "Run the tests before every proposal".into(),
                ..Default::default()
            },
        )
        .unwrap();
    (
        w.created["$d"].clone(),
        s,
        w.created["$c"].clone(),
        d.created["$decision"].clone(),
    )
}

/// One create of instruction `ai-AAAAA` under `parent` with `fields`, `body` and `links`.
fn create(parent: &str, fields: Value, body: &str, links: Value) -> Value {
    json!([{"op": "create", "type": "instruction", "id": ID, "parent": parent,
            "fields": fields, "body": body, "links": links}])
}

fn reason(d: &str) -> Value {
    json!({"decided_by": [{"target": d}]})
}

/// The code, message and fix of the refusal, and the codes of its issues.
fn refusal(res: Result<WriteResult, RdyError>) -> (String, String, String, Vec<&'static str>) {
    let e = res.expect_err("the write is refused");
    let codes = e.issues.iter().map(|i| i.code).collect();
    (e.code.to_string(), e.message, e.fix, codes)
}

fn issue_rows(e: &RdyError) -> Vec<(&'static str, String, String)> {
    e.issues
        .iter()
        .map(|i| (i.code, i.message.clone(), i.fix.clone()))
        .collect()
}

const SELECTOR_FIX: &str =
    "use all, area:<name>, type:<name>, or act:<write|decide|propose|relay|review|publish|redact>";
const REASON_FIX: &str = "link the instruction to its reason with decided_by: rdy_write / rdy new instruction --reason <decision>";
const LIMIT_FIX: &str = "write 1 to 1024 selectors in applies_to, each at most 256 bytes";

#[test]
fn ain1_instruction_selectors_and_reasons_are_closed() {
    let (_dir, mut r) = project();
    let (doc, s, c, d) = rules(&mut r);
    let scope = Scope::View("w".into());
    let held = |r: &Rdy| r.read().materialized(&scope).unwrap().len();
    let before = held(&r);

    // a bad selector, an unknown act or a malformed one: one issue per selector, in order
    for sel in [
        "",
        "any",
        "all ",
        "ALL",
        "area:",
        "area:Payments",
        "area:-x",
        "type:",
        "type:a b",
        "act:",
        "act:wirte",
        "act:land",
        "path:src/**",
        "area:x:y",
    ] {
        let (code, msg, fix, codes) = refusal(write(
            &mut r,
            "w",
            create(
                &doc,
                json!({"applies_to": [sel]}),
                "Run the tests.",
                reason(&d),
            ),
        ));
        assert_eq!(code, "E_VALIDATION", "{sel:?}");
        assert_eq!(msg, format!("instruction {ID} has invalid selector {sel}"));
        assert_eq!(fix, SELECTOR_FIX);
        assert_eq!(codes, vec!["E_VALIDATION"], "{sel:?}");
    }
    let e = write(
        &mut r,
        "w",
        create(
            &doc,
            json!({"applies_to": ["all", "act:wirte", "type:claim", "nope"]}),
            "Run the tests.",
            reason(&d),
        ),
    )
    .unwrap_err();
    assert_eq!(
        issue_rows(&e),
        vec![
            (
                "E_VALIDATION",
                format!("instruction {ID} has invalid selector act:wirte"),
                SELECTOR_FIX.to_string()
            ),
            (
                "E_VALIDATION",
                format!("instruction {ID} has invalid selector nope"),
                SELECTOR_FIX.to_string()
            ),
        ]
    );

    // the list's bounds and shape
    let many: Vec<Value> = (0..1025).map(|_| json!("all")).collect();
    for (fields, message, fix, code) in [
        (
            json!({"applies_to": []}),
            format!("instruction {ID} has 0 selectors; applies_to holds 1 to 1024"),
            LIMIT_FIX.to_string(),
            "E_LIMIT",
        ),
        (
            json!({"applies_to": many}),
            format!("instruction {ID} has 1025 selectors; applies_to holds 1 to 1024"),
            LIMIT_FIX.to_string(),
            "E_LIMIT",
        ),
        (
            json!({"applies_to": [format!("type:{}", "a".repeat(252))]}),
            format!("instruction {ID} has a selector longer than 256 bytes"),
            LIMIT_FIX.to_string(),
            "E_LIMIT",
        ),
        (
            json!({"applies_to": ["all", 1]}),
            "field \"applies_to\" must be a list of strings".to_string(),
            "write applies_to as a list of strings".to_string(),
            "E_FIELD_KIND",
        ),
        (
            json!({"applies_to": "all"}),
            "field \"applies_to\" must be List".to_string(),
            "write applies_to as a list value".to_string(),
            "E_FIELD_KIND",
        ),
        (
            json!({}),
            "type instruction requires field \"applies_to\"".to_string(),
            "add applies_to (list)".to_string(),
            "E_FIELD_REQUIRED",
        ),
        (
            json!({"applies_to": ["all"], "locked": "yes"}),
            "field \"locked\" must be Bool".to_string(),
            "write locked as a bool value".to_string(),
            "E_FIELD_KIND",
        ),
        // unknown fields, with the type's own fields as the near-key hint
        (
            json!({"applies-to": ["all"], "applies_to": ["all"]}),
            "type instruction has no field \"applies-to\"".to_string(),
            "remove it, or use one of: applies_to, locked".to_string(),
            "E_FIELD_UNKNOWN",
        ),
        (
            json!({"applies_to": ["all"], "lockd": true}),
            "type instruction has no field \"lockd\"".to_string(),
            "remove it, or use one of: applies_to, locked".to_string(),
            "E_FIELD_UNKNOWN",
        ),
        // a key, which an instruction never has
        (
            json!({"applies_to": ["all"], "key": "RULE-1"}),
            format!("instruction {ID} cannot have a key"),
            "remove key; refer to the instruction by its id".to_string(),
            "E_VALIDATION",
        ),
    ] {
        let e = write(
            &mut r,
            "w",
            create(&doc, fields.clone(), "Run the tests.", reason(&d)),
        )
        .unwrap_err();
        assert_eq!(e.code, "E_VALIDATION", "{fields}: {e}");
        assert_eq!(issue_rows(&e), vec![(code, message, fix)], "{fields}");
    }

    // the body, the parent and the reason
    let all = json!({"applies_to": ["all"]});
    let e = write(&mut r, "w", create(&doc, all.clone(), "  ", reason(&d))).unwrap_err();
    assert_eq!(
        issue_rows(&e),
        vec![(
            "E_BODY_REQUIRED",
            "type instruction requires a body".to_string(),
            "write the block's text in body".to_string()
        )]
    );
    let e = write(
        &mut r,
        "w",
        create(&c, all.clone(), "Run the tests.", reason(&d)),
    )
    .unwrap_err();
    assert_eq!(
        issue_rows(&e),
        vec![(
            "E_CHILD_TYPE",
            "a claim cannot contain a instruction".to_string(),
            "place it under a section (or another allowed parent type)".to_string()
        )]
    );
    let e = write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "instruction", "id": ID, "fields": all, "body": "Run the tests.", "links": reason(&d)}]),
    )
    .unwrap_err();
    assert_eq!(issue_rows(&e)[0].0, "E_ORPHAN", "{e}");
    for links in [json!({}), json!({"decided_by": []}), reason(&c), reason(&s)] {
        let (code, msg, fix, codes) = refusal(write(
            &mut r,
            "w",
            create(&doc, all.clone(), "Run the tests.", links.clone()),
        ));
        assert_eq!(
            (code.as_str(), msg.as_str(), fix.as_str()),
            (
                "E_VALIDATION",
                format!("instruction {ID} requires a decided_by decision").as_str(),
                REASON_FIX
            ),
            "{links}"
        );
        assert_eq!(codes, vec!["E_VALIDATION"], "{links}");
    }
    // a decision and any other block: still the missing reason
    let (code, msg, ..) = refusal(write(
        &mut r,
        "w",
        create(
            &doc,
            all.clone(),
            "Run the tests.",
            json!({"decided_by": [{"target": d}, {"target": c}]}),
        ),
    ));
    assert_eq!(
        (code.as_str(), msg),
        (
            "E_VALIDATION",
            format!("instruction {ID} requires a decided_by decision")
        )
    );
    // an unknown op key keeps the op's own did-you-mean
    let e = write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "instruction", "parent": doc, "fields": all, "boddy": "x", "links": reason(&d)}]),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert_eq!(e.message, "op 0: unknown field \"boddy\"", "{e}");
    assert!(e.fix.starts_with("did you mean \"body\"?"), "{e}");
    // nothing refused was written
    assert_eq!(held(&r), before);

    // valid selectors are kept as written, without matching anything at write time: an unknown
    // type name is a valid hint, and no warning comes with it
    let sels = json!([
        "all",
        "type:claim",
        "type:no-such-type",
        "act:review",
        "act:redact"
    ]);
    for (parent, id) in [(&doc, "ai-AAAAA"), (&s, "ai-BBBBB")] {
        let w = write(
            &mut r,
            "w",
            json!([{"op": "create", "type": "instruction", "id": id, "parent": parent,
                    "fields": {"applies_to": sels, "locked": true}, "body": "Run the tests.", "links": reason(&d)}]),
        )
        .unwrap();
        assert_eq!(w.warnings, vec![], "{parent}");
        let b = r
            .read()
            .block(&scope, &BlockId::parse(id).unwrap())
            .unwrap();
        assert_eq!(
            rdy_engine::json_to_field(&sels).unwrap(),
            b.fields["applies_to"]
        );
        assert_eq!(b.fields["locked"], FieldValue::Bool(true));
        // the read schema shares the checks
        let pack = r.read().pack().clone();
        let m = r.read().materialized(&scope).unwrap();
        assert_eq!(
            rdy_schema::validate_read_block(&pack, &b, m.get(&b.parent.clone().unwrap())),
            vec![]
        );
        let mut bad = b.clone();
        bad.fields.insert(
            "applies_to".into(),
            FieldValue::List(vec![FieldValue::Str("act:x".into())]),
        );
        assert_eq!(
            rdy_schema::validate_read_block(&pack, &bad, m.get(&b.parent.clone().unwrap()))
                .iter()
                .map(|i| i.message.clone())
                .collect::<Vec<_>>(),
            vec![format!("instruction {id} has invalid selector act:x")]
        );
    }
}

#[test]
fn ain1_instruction_uses_ordinary_review_and_coverage() {
    let (dir, mut r) = project();
    let spec = stable_spec(&mut r);
    let features = r.store().features().clone();
    let body = "Run the full test suite before you propose.\n\nWhy: a proposal that fails its tests wastes a review.\n\nHow to apply: run cargo test, then propose.";
    let op = json!([{"op": "create", "type": "instruction", "id": "$i", "parent": spec.section.to_string(),
        "fields": {"applies_to": ["all"]}, "body": body,
        "links": {"decided_by": [{"target": spec.decision.to_string()}]}}]);

    // stable content: the normal briefing rule
    let e = write_cited(&mut r, "a", op.clone(), None).unwrap_err();
    assert_eq!(e.code, E_BRIEF_REQUIRED, "{e}");
    let token = brief_of(&r, &Scope::View("a".into()), &[&spec.section])
        .base
        .token;
    let w = write_cited(&mut r, "a", op.clone(), Some(&token)).unwrap();
    let i = BlockId::parse(&w.created["$i"]).unwrap();
    // no implements or editorial: the normal unjustified_change, no exemption
    assert!(
        of_kind(&r, TensionKind::UnjustifiedChange)
            .iter()
            .any(|t| t.blocks().contains(&i)),
        "an unjustified stable instruction opens unjustified_change"
    );
    // with implements, it is justified as any block is
    let j = put_j(&mut r, "b", op.clone(), j_implements(&spec.decision));
    let ij = BlockId::parse(&j.created["$i"]).unwrap();
    assert!(
        !of_kind(&r, TensionKind::UnjustifiedChange)
            .iter()
            .any(|t| t.blocks().contains(&ij))
    );

    // a reason change makes the instruction suspect, as for any dependent
    let (_doc, s, _c, d) = rules(&mut r);
    let wi = write(
        &mut r,
        "w",
        json!([{"op": "create", "type": "instruction", "id": "$i", "parent": s,
                "fields": {"applies_to": ["act:propose"]}, "body": "Run the tests.",
                "links": {"decided_by": [{"target": d}]}}]),
    )
    .unwrap();
    let wi = BlockId::parse(&wi.created["$i"]).unwrap();
    let dv = version_of(&r, &Scope::View("w".into()), &d);
    write(
        &mut r,
        "w",
        json!([{"op": "update", "id": d, "base": dv, "fields": {"statement": "Run the tests before every landing"}}]),
    )
    .unwrap();
    let suspects = r.read().suspects(&Scope::View("w".into())).unwrap();
    assert!(
        suspects
            .iter()
            .any(|x| x.block == wi && x.target.as_str() == d),
        "{suspects:?}"
    );

    // the first instruction landing turns on no store feature (D-2WY7Y)
    land(&mut r, "b").unwrap();
    let landed = r.read().block(&Scope::Canon, &ij).unwrap();
    assert_eq!(landed.body, body, "every paragraph is kept");
    assert_eq!(r.store().features(), &features);
    assert!(
        !r.store()
            .features()
            .features
            .iter()
            .any(|f| f.name == "foreign_copy")
    );
    // a block YAML round trip keeps it
    let yaml = rdy_engine::block_to_yaml(&landed);
    assert_eq!(rdy_engine::block_from_yaml(&yaml).unwrap(), landed);

    // dump and load: the same block bytes and hashes; a reindex reproduces the derived state
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join("rules.rdydump");
    r.read().dump(&file).unwrap();
    r.close().unwrap();
    let to = scratch.path().join("loaded");
    rdy_engine::load::load(&file, &to).unwrap();
    let loaded = Rdy::open(&to).unwrap();
    let back = loaded.read().block(&Scope::Canon, &ij).unwrap();
    assert_eq!(back, landed);
    assert_eq!(back.version_hash(), landed.version_hash());
    assert_eq!(back.content_hash(), landed.content_hash());
    assert_eq!(loaded.store().features(), &features);
    loaded.close().unwrap();
    rdy_qa::reindex_check::reindex_reproduces(dir.path()).unwrap();
    rdy_qa::reindex_check::reindex_reproduces(&to).unwrap();
}
