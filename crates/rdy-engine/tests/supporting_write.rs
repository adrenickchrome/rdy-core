//! rf5 (D-Z8P09): findings and sources are written through the ordinary write path. A create of
//! either type with no parent goes to the decision home, before the briefing check; evidence
//! pins what it cites; a finding is never deleted, a source may be.
use rdy_engine::brief::Coverage;
use rdy_engine::*;
use rdy_model::*;
use rdy_schema::supporting::{EVIDENCE_FIX, EVIDENCE_MESSAGE};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn writer() -> Session {
    Session {
        actor: "agent:designer".into(),
        run: Some("run-1".into()),
        role: Role::Writer,
        kind: SessionKind::Human,
    }
}

fn work() -> Scope {
    Scope::View("work".into())
}

fn write(r: &mut Rdy, ops: Value, brief: Option<String>) -> Result<WriteResult, RdyError> {
    r.write_briefed(
        &writer(),
        WriteRequest {
            view: "work".into(),
            ops: parse_ops(serde_json::from_value(ops).unwrap()).unwrap(),
            justification: None,
            message: String::new(),
        },
        brief,
    )
}

/// A project with a stable top-level doc `register` and a draft doc `notes` holding section `s`
/// and claim `c`. Returns (register, s, c).
fn setup() -> (tempfile::TempDir, Rdy, String, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$reg", "fields": {"name": "register", "title": "Register", "doc_type": "decisions-log", "lifecycle": "stable"}},
            {"op": "create", "type": "doc", "id": "$notes", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$notes", "fields": {"title": "Retention"}},
            {"op": "create", "type": "claim", "id": "$c", "parent": "$s", "body": "Retention is thirty days."}
        ]),
        None,
    )
    .unwrap();
    (
        dir,
        r,
        w.created["$reg"].clone(),
        w.created["$s"].clone(),
        w.created["$c"].clone(),
    )
}

/// Sets `decisions.home` in the project config and reloads it.
fn configure(r: &mut Rdy, home: &str) {
    let cfg = r.root().join(".rdy.yaml");
    let text = std::fs::read_to_string(&cfg).unwrap() + &format!("decisions:\n  home: {home}\n");
    std::fs::write(&cfg, text).unwrap();
    r.reload_pack().unwrap();
}

fn finding_op(parent: Option<&str>) -> Value {
    let mut op = json!({"op": "create", "type": "finding", "id": "$f",
        "fields": {"statement": "The release changed retention.", "evidence": [{"quote": "Retention is now seven days."}]}});
    if let Some(p) = parent {
        op["parent"] = json!(p);
    }
    op
}

fn source_op(parent: Option<&str>) -> Value {
    let mut op = json!({"op": "create", "type": "source", "id": "$src",
        "fields": {"title": "Release 2", "kind": "release", "locator": "v2"}});
    if let Some(p) = parent {
        op["parent"] = json!(p);
    }
    op
}

fn block(r: &Rdy, id: &str) -> Block {
    r.read()
        .block(&work(), &BlockId::parse(id).unwrap())
        .unwrap()
}

fn same(a: &RdyError, b: &RdyError) {
    assert_eq!(
        (a.code, &a.message, &a.fix),
        (b.code, &b.message, &b.fix),
        "{a} / {b}"
    );
}

#[test]
fn rf5_home_and_explicit_parent_match_decide() {
    let (_d, mut r, reg, s, _c) = setup();
    // no home: the decision home's own refusal, word for word, and nothing is written
    let seq = r.read().seq();
    let want = r.decision_parent("work", None).unwrap_err();
    assert_eq!(
        want.message,
        "no parent given, and the project names no decision home"
    );
    for op in [finding_op(None), source_op(None)] {
        let e = write(&mut r, json!([op]), None).unwrap_err();
        same(&e, &want);
    }
    assert_eq!(r.read().seq(), seq);
    // a home that names nothing: its E_CONFIG
    configure(&mut r, "nowhere/at-all");
    let want = r.decision_parent("work", None).unwrap_err();
    assert_eq!(want.code, E_CONFIG);
    for op in [finding_op(None), source_op(None)] {
        same(&write(&mut r, json!([op]), None).unwrap_err(), &want);
    }
    // the home: the parent rdy_decide uses; it is stable, so the briefing must cover it
    let cfg = r.root().join(".rdy.yaml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(&cfg, text.replace("nowhere/at-all", "register")).unwrap();
    r.reload_pack().unwrap();
    assert_eq!(r.decision_parent("work", None).unwrap(), reg);
    for (op, ty) in [(finding_op(None), "finding"), (source_op(None), "source")] {
        let e = write(&mut r, json!([op.clone()]), None).unwrap_err();
        assert_eq!(e.code, E_BRIEF_REQUIRED, "{e}");
        assert!(e.message.contains(&reg), "{e}");
        assert_eq!(
            e.briefing.as_ref().unwrap()["ids"],
            json!([reg.clone()]),
            "the normalized parent is the briefing subject"
        );
        // a briefing of another block does not cover it
        let other = r
            .read()
            .brief(
                &work(),
                &Coverage {
                    ids: vec![s.clone()],
                    path: None,
                },
                None,
            )
            .unwrap();
        other.register(&r.briefs());
        let e = write(&mut r, json!([op.clone()]), Some(other.base.token.clone())).unwrap_err();
        assert_eq!(e.code, E_BRIEF_REQUIRED, "{e}");
        assert!(
            e.message
                .contains(&format!("{reg} (the parent of the new {ty})")),
            "{e}"
        );
        let b = r
            .read()
            .brief(
                &work(),
                &Coverage {
                    ids: vec![reg.clone()],
                    path: None,
                },
                None,
            )
            .unwrap();
        b.register(&r.briefs());
        let w = write(&mut r, json!([op]), Some(b.base.token.clone())).unwrap();
        let id = w.created.values().next().unwrap().clone();
        let got = block(&r, &id);
        assert_eq!(got.block_type, ty);
        assert_eq!(got.parent.map(|p| p.to_string()), Some(reg.clone()));
    }
    // an explicit parent is kept, as rdy_decide keeps it
    assert_eq!(r.decision_parent("work", Some(&s)).unwrap(), s);
    let w = write(
        &mut r,
        json!([source_op(Some(&s)), finding_op(Some(&s))]),
        None,
    )
    .unwrap();
    for id in [&w.created["$src"], &w.created["$f"]] {
        assert_eq!(block(&r, id).parent.map(|p| p.to_string()), Some(s.clone()));
    }
}

fn suspects(r: &Rdy) -> BTreeSet<(String, String)> {
    r.read()
        .suspects(&work())
        .unwrap()
        .into_iter()
        .map(|s| (s.block.to_string(), s.target.to_string()))
        .collect()
}

fn issue_triples(e: &RdyError) -> Vec<(&'static str, String, String)> {
    e.issues
        .iter()
        .map(|i| (i.code, i.message.clone(), i.fix.clone()))
        .collect()
}

#[test]
fn rf5_finding_evidence_pins_and_lifecycle() {
    let (_d, mut r, _reg, s, c) = setup();
    // one write: a source, and a finding whose evidence quotes the claim and cites the source by
    // its placeholder
    let w = write(
        &mut r,
        json!([
            source_op(Some(&s)),
            {"op": "create", "type": "finding", "id": "$f", "parent": s, "fields": {
                "statement": "The release changed retention.",
                "evidence": [
                    {"quote": format!("{{{{{c}}}}} says thirty days."), "question": "How long is retention?", "session_ref": "session 4"},
                    {"source": "{{$src}}"}
                ]}}
        ]),
        None,
    )
    .unwrap();
    let (src, f) = (w.created["$src"].clone(), w.created["$f"].clone());
    let fb = block(&r, &f);
    assert_eq!(
        fb.fields["evidence"],
        FieldValue::List(vec![
            FieldValue::Map(
                [
                    ("question", "How long is retention?"),
                    ("quote", &format!("{{{{{c}}}}} says thirty days.")),
                    ("session_ref", "session 4"),
                ]
                .into_iter()
                .map(|(k, v)| (k.to_string(), FieldValue::Str(v.to_string())))
                .collect()
            ),
            FieldValue::Map(
                [(
                    "source".to_string(),
                    FieldValue::Str(format!("{{{{{src}}}}}"))
                )]
                .into_iter()
                .collect()
            ),
        ])
    );
    let pinned: BTreeSet<String> = fb.pins.keys().map(ToString::to_string).collect();
    assert_eq!(pinned, BTreeSet::from([src.clone(), c.clone()]));
    // a decision cites the finding with depends_on
    let w = write(
        &mut r,
        json!([{"op": "create", "type": "decision", "id": "$d", "parent": s,
            "fields": {"statement": "Retention follows the release.", "status": "proposed"},
            "links": {"depends_on": [{"target": f}]}}]),
        None,
    )
    .unwrap();
    let d = w.created["$d"].clone();
    assert!(
        block(&r, &d)
            .pins
            .contains_key(&BlockId::parse(&f).unwrap())
    );
    assert!(suspects(&r).is_empty(), "{:?}", suspects(&r));
    // a source change makes the finding suspect; a finding change makes the decision suspect
    write(
        &mut r,
        json!([{"op": "update", "id": src, "fields": {"version": "2.0.1"}}]),
        None,
    )
    .unwrap();
    assert!(
        suspects(&r).contains(&(f.clone(), src.clone())),
        "{:?}",
        suspects(&r)
    );
    write(
        &mut r,
        json!([{"op": "update", "id": f, "fields": {"statement": "The release cut retention to seven days."}}]),
        None,
    )
    .unwrap();
    assert!(
        suspects(&r).contains(&(d.clone(), f.clone())),
        "{:?}",
        suspects(&r)
    );
    // a finding is never deleted: refused before anything is appended
    let seq = r.read().seq();
    let e = write(&mut r, json!([{"op": "delete", "id": f}]), None).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(
        e.message,
        format!("{f} is a finding: findings keep their evidence and are never deleted")
    );
    assert_eq!(e.fix, "update the finding, or record a new finding");
    assert_eq!(r.read().seq(), seq);
    // a source may be deleted; the finding that pinned it is suspect on it
    write(&mut r, json!([{"op": "delete", "id": src}]), None).unwrap();
    assert!(block_absent(&r, &src));
    assert!(
        suspects(&r).contains(&(f.clone(), src.clone())),
        "{:?}",
        suspects(&r)
    );
}

fn block_absent(r: &Rdy, id: &str) -> bool {
    r.read()
        .block(&work(), &BlockId::parse(id).unwrap())
        .is_err()
}

#[test]
fn rf5_evidence_diagnostics_name_the_entry_and_the_fix() {
    let (_d, mut r, _reg, s, c) = setup();
    let finding = |evidence: Value| {
        json!([{"op": "create", "type": "finding", "parent": s,
            "fields": {"statement": "S", "evidence": evidence}}])
    };
    let refused = |r: &mut Rdy, evidence: Value| {
        let seq = r.read().seq();
        let e = write(r, finding(evidence), None).unwrap_err();
        assert_eq!(r.read().seq(), seq);
        e
    };
    // an unknown key: the entry, the key and the nearest known key, with the evidence fix
    let e = refused(
        &mut r,
        json!([{"quote": "a"}, {"quote": "b", "sesion_ref": "x"}]),
    );
    assert_eq!(e.code, E_VALIDATION);
    let m = "evidence entry 1: unknown key \"sesion_ref\"; did you mean \"session_ref\"?";
    assert_eq!(
        issue_triples(&e),
        vec![("E_FIELD_KIND", m.to_string(), EVIDENCE_FIX.to_string())]
    );
    assert_eq!(e.message, m);
    assert_eq!(e.fix, EVIDENCE_FIX);
    let e = refused(&mut r, json!([{"quote": "a", "zzzz": "x"}]));
    assert_eq!(
        issue_triples(&e),
        vec![(
            "E_FIELD_KIND",
            "evidence entry 0: unknown key \"zzzz\"".to_string(),
            EVIDENCE_FIX.to_string()
        )]
    );
    let e = refused(&mut r, json!([{"sorce": "release 2"}]));
    assert_eq!(
        e.issues[0].message,
        "evidence entry 0: unknown key \"sorce\"; did you mean \"source\"?"
    );
    // a bad shape: the schema's one evidence issue
    for bad in [json!([]), json!("quote"), json!([{"question": "q"}])] {
        let e = refused(&mut r, bad.clone());
        assert_eq!(
            issue_triples(&e),
            vec![(
                "E_FIELD_KIND",
                EVIDENCE_MESSAGE.to_string(),
                EVIDENCE_FIX.to_string()
            )],
            "{bad}"
        );
    }
    // a source entry naming a held block that is no source
    let e = refused(&mut r, json!([{"source": format!("{{{{{c}}}}}")}]));
    assert_eq!(e.code, E_VALIDATION);
    assert_eq!(e.issues[0].code, "E_FIELD_KIND");
    assert_eq!(e.issues[0].fix, EVIDENCE_FIX);
    // a source entry naming nothing: the ordinary unknown-reference refusal
    let e = refused(&mut r, json!([{"source": "{{S-ZZZZZ}}"}]));
    assert_eq!(e.code, E_UNKNOWN_REF, "{e}");
    // no evidence, or no statement: the generic required-field issue
    let e = write(
        &mut r,
        json!([{"op": "create", "type": "finding", "parent": s, "fields": {"statement": "S"}}]),
        None,
    )
    .unwrap_err();
    assert_eq!(e.issues[0].code, "E_FIELD_REQUIRED", "{e}");
    let e = write(
        &mut r,
        json!([{"op": "create", "type": "finding", "parent": s, "fields": {"evidence": [{"quote": "q"}]}}]),
        None,
    )
    .unwrap_err();
    assert_eq!(e.issues[0].code, "E_FIELD_REQUIRED", "{e}");
}
