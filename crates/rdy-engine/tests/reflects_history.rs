//! rf4 (D-4WC6R): a stored change justified by `Reflects` reads through the history the engine
//! gives every reader: `JustificationInfo::Reflects`, its JSON and its one-line label. The
//! changes are typed store commits; no raw reflects input is involved.
use rdy_engine::{JustificationInfo, Rdy, Scope, Session, SessionKind, WriteRequest};
use rdy_model::{BlockId, Change, EditorialClass, Justification, Op, Provenance, Role};
use serde_json::json;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn writer() -> Session {
    Session {
        actor: "agent:a".into(),
        run: Some("ra".into()),
        role: Role::Writer,
        kind: SessionKind::Human,
    }
}

/// Rewrites block `b` in view `work` with a new body, justified by `j`, as one typed commit.
fn rewrite(r: &mut Rdy, b: &BlockId, body: &str, j: Justification) {
    let mut next = r.read().block(&Scope::View("work".into()), b).unwrap();
    let base = next.version_hash();
    next.body = body.into();
    let deps = r.read().included("work").to_vec();
    r.store_mut()
        .commit_to_view(
            "work",
            Change {
                view: "work".into(),
                ops: vec![Op::Update { base, block: next }],
                deps,
                provenance: Provenance {
                    actor: "agent:a".into(),
                    run: Some("ra".into()),
                    role: Role::Writer,
                    at_ms: 5,
                },
                justification: Some(j),
                message: String::new(),
                links: Default::default(),
                read_identity: None,
            },
        )
        .unwrap();
    r.invalidate();
}

#[test]
fn rf4_justification_info_converts_every_kind() {
    let cases = [
        (
            Justification::Implements(vec![id("D-00002"), id("D-00001")]),
            JustificationInfo::Implements(vec!["D-00002".into(), "D-00001".into()]),
            json!({"implements": ["D-00002", "D-00001"]}),
            "implements D-00002, D-00001",
        ),
        (
            Justification::Editorial {
                class: EditorialClass::Typo,
                reason: "wording".into(),
            },
            JustificationInfo::Editorial {
                class: "typo".into(),
                reason: "wording".into(),
            },
            json!({"editorial": {"class": "typo", "reason": "wording"}}),
            "editorial typo",
        ),
        (
            Justification::Reflects {
                sources: vec![id("S-0000B"), id("S-0000A")],
                code: true,
            },
            JustificationInfo::Reflects {
                sources: vec!["S-0000B".into(), "S-0000A".into()],
                code: true,
            },
            json!({"reflects": {"sources": ["S-0000B", "S-0000A"], "code": true}}),
            "reflects S-0000B, S-0000A, code",
        ),
        (
            Justification::Reflects {
                sources: vec![id("S-0000A")],
                code: false,
            },
            JustificationInfo::Reflects {
                sources: vec!["S-0000A".into()],
                code: false,
            },
            json!({"reflects": {"sources": ["S-0000A"], "code": false}}),
            "reflects S-0000A",
        ),
        (
            Justification::Reflects {
                sources: vec![],
                code: true,
            },
            JustificationInfo::Reflects {
                sources: vec![],
                code: true,
            },
            json!({"reflects": {"sources": [], "code": true}}),
            "reflects code",
        ),
    ];
    for (j, info, wire, label) in cases {
        let got = JustificationInfo::from(&j);
        assert_eq!(got, info);
        assert_eq!(serde_json::to_value(&got).unwrap(), wire);
        assert_eq!(got.label(), label);
    }
}

#[test]
fn rf4_history_reads_both_reflects_forms() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = r
        .write(
            &writer(),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
                    {"op": "create", "type": "claim", "id": "$c", "parent": "$d", "body": "One."}
                ]))
                .unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
    let claim = id(&w.created["$c"]);
    rewrite(
        &mut r,
        &claim,
        "Two.",
        Justification::Reflects {
            sources: vec![id("S-0000B"), id("S-0000A")],
            code: false,
        },
    );
    rewrite(
        &mut r,
        &claim,
        "Three.",
        Justification::Reflects {
            sources: vec![],
            code: true,
        },
    );
    let scope = Scope::View("work".into());
    let h = r.read().history(&scope, &claim, 10).unwrap();
    let got: Vec<Option<JustificationInfo>> =
        h.entries.iter().map(|e| e.justification.clone()).collect();
    assert_eq!(
        got,
        [
            Some(JustificationInfo::Reflects {
                sources: vec![],
                code: true
            }),
            Some(JustificationInfo::Reflects {
                sources: vec!["S-0000B".into(), "S-0000A".into()],
                code: false
            }),
            None,
        ]
    );
    let v = serde_json::to_value(&h.entries[1]).unwrap();
    assert_eq!(
        v["justification"],
        json!({"reflects": {"sources": ["S-0000B", "S-0000A"], "code": false}})
    );
    // the last change of each block, as the compact reader reads it
    let last = r.read().last_changes(&scope, &[claim.clone()]).unwrap();
    assert_eq!(
        last[&claim].justification.as_ref().map(|j| j.label()),
        Some("reflects code".into())
    );
}
