//! m4bc1 (D-E4W0Y; D-2WR0G): `reconstructed` marks a decision inferred from an existing system.
//! It is a local basis like the six before it. It adds no required field and no landing
//! privilege: write, decide, supersede and format-3 import keep every check they run for other
//! bases, and an accepted decision still refuses a basis edit.
mod common;

use rdy_engine::*;
use rdy_model::*;
use serde_json::{Value, json};

const BASIS: &str = "authority_basis";

fn writer() -> Session {
    Session {
        actor: "agent:designer".into(),
        run: Some("run-1".into()),
        role: Role::Writer,
        kind: SessionKind::Human,
    }
}

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
fn setup(root: &std::path::Path) -> (Rdy, String) {
    std::fs::create_dir_all(root).unwrap();
    let mut r = Rdy::init(root, "rdy-standard").unwrap();
    let w = write_as(
        &mut r,
        &writer(),
        json!([
            {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Decisions", "doc_type": "decisions-log"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$doc", "fields": {"title": "Decisions"}}
        ]),
    )
    .unwrap();
    let s = w.created["$s"].clone();
    (r, s)
}

fn basis_of(r: &Rdy, id: &str) -> Option<String> {
    r.read()
        .block(&Scope::View("work".into()), &BlockId::parse(id).unwrap())
        .unwrap()
        .field_str(BASIS)
        .map(String::from)
}

fn input(parent: &str, statement: &str, basis: Option<&str>) -> DecisionInput {
    DecisionInput {
        parent: parent.into(),
        statement: statement.into(),
        authority_basis: basis.map(String::from),
        ..Default::default()
    }
}

#[test]
fn m4bc1_reconstructed_keeps_normal_write_decide_supersede_and_import_checks() {
    // a temporary Unicode path: the import case runs natively on it (Windows included)
    let tmp = tempfile::tempdir().unwrap();
    let (mut r, s) = setup(&tmp.path().join("projet-réconstruit-数据"));

    // write: create and update take the value; a near miss is still the enum refusal, and the
    // fix text lists the new value last
    let w = write_as(
        &mut r,
        &writer(),
        json!([{"op": "create", "type": "decision", "parent": s,
            "fields": {"statement": "Cache is write-through", "status": "proposed", BASIS: "reconstructed"}}]),
    )
    .unwrap();
    let written = w.created["op0"].clone();
    assert_eq!(basis_of(&r, &written).as_deref(), Some("reconstructed"));
    write_as(
        &mut r,
        &writer(),
        json!([{"op": "update", "id": written, "fields": {"statement": "Cache is write-through, per the code"}}]),
    )
    .unwrap();
    let seq = r.read().seq();
    let e = write_as(
        &mut r,
        &writer(),
        json!([{"op": "create", "type": "decision", "parent": s,
            "fields": {"statement": "S", "status": "proposed", BASIS: "Reconstructed"}}]),
    )
    .unwrap_err();
    assert_eq!(e.code, E_VALIDATION, "{e}");
    assert_eq!(
        e.issues
            .iter()
            .map(|i| (i.code, i.message.clone(), i.fix.clone()))
            .collect::<Vec<_>>(),
        vec![(
            "E_FIELD_ENUM",
            "field \"authority_basis\" has value \"Reconstructed\"".to_string(),
            "use one of: direct_ruling, recorded_ruling, delegated, precedent, elimination, none, reconstructed"
                .to_string()
        )]
    );
    assert_eq!(r.read().seq(), seq, "a refused write stores nothing");

    // decide: no attested_by is owed, unlike a delegated decision
    let w = r
        .decide(
            &writer(),
            "work",
            input(&s, "Retries are idempotent", Some("reconstructed")),
        )
        .unwrap();
    let decided = w.created["$decision"].clone();
    assert_eq!(basis_of(&r, &decided).as_deref(), Some("reconstructed"));
    let e = r
        .decide(
            &writer(),
            "work",
            input(&s, "Delegated needs a name", Some("delegated")),
        )
        .unwrap_err();
    assert_eq!(e.code, E_VALIDATION, "{e}");

    // an accepted decision keeps its basis: the ordinary freeze refuses the edit
    let p = r
        .propose(&writer(), "work", "basis", Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(&mut r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
    for to in ["direct_ruling", "reconstructed"] {
        let e = write_as(
            &mut r,
            &writer(),
            json!([{"op": "update", "id": decided, "fields": {BASIS: to}}]),
        )
        .unwrap_err();
        assert_eq!(e.code, E_FROZEN, "{to}: {e}");
    }
    assert_eq!(basis_of(&r, &decided).as_deref(), Some("reconstructed"));

    // supersede: the successor takes the value too
    let w = r
        .supersede(
            &writer(),
            "work",
            &decided,
            None,
            input(&s, "Retries are idempotent per key", Some("reconstructed")),
        )
        .unwrap();
    let successor = w.created["$decision"].clone();
    assert_eq!(basis_of(&r, &successor).as_deref(), Some("reconstructed"));

    // format-3 import: export the view, import it into a fresh project, same basis
    let out = tmp.path().join("export-référence");
    r.read().export(&Scope::View("work".into()), &out).unwrap();
    let mut dst = Rdy::init(
        &{
            let d = tmp.path().join("destination-目标");
            std::fs::create_dir_all(&d).unwrap();
            d
        },
        "rdy-standard",
    )
    .unwrap();
    dst.import(&founder(), "work", &out).unwrap();
    for id in [&written, &decided, &successor] {
        assert_eq!(basis_of(&dst, id).as_deref(), Some("reconstructed"), "{id}");
    }

    // an imported bad value is still refused before anything is stored
    let seq = dst.read().seq();
    let doc = std::fs::read_dir(out.join("docs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let text = std::fs::read_to_string(&doc).unwrap();
    assert!(text.contains("authority_basis: reconstructed"));
    std::fs::write(
        &doc,
        text.replace(
            "authority_basis: reconstructed",
            "authority_basis: inferred",
        ),
    )
    .unwrap();
    let mut other = Rdy::init(
        &{
            let d = tmp.path().join("other");
            std::fs::create_dir_all(&d).unwrap();
            d
        },
        "rdy-standard",
    )
    .unwrap();
    let e = other.import(&founder(), "work", &out).unwrap_err();
    assert_eq!(e.code, E_VALIDATION, "{e}");
    assert_eq!(dst.read().seq(), seq);
}
