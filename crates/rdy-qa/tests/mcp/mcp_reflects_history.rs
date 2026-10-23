//! rf4 (D-4WC6R): a change justified by `Reflects`, stored as a typed change, reads through
//! rdy_context (its justification and its history) and marks the compact rdy_read line with the
//! engine's one label: sources in stored order, then `code`.
use rdy_engine::engine::Local;
use rdy_engine::*;
use rdy_mcp::tools::*;
use rdy_model::{BlockId, Change, Justification, Op, Provenance, Role};
use rdy_qa::fixtures::*;
use serde_json::{Value, from_value, json};
use std::sync::Arc;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// Rewrites `b` in view `view` with `body`, justified by `j`, as one typed commit.
fn rewrite(r: &mut Rdy, view: &str, b: &str, body: &str, j: Justification) {
    let mut next = r.read().block(&Scope::View(view.into()), &id(b)).unwrap();
    let base = next.version_hash();
    next.body = body.into();
    let deps = r.read().included(view).to_vec();
    r.store_mut()
        .commit_to_view(
            view,
            Change {
                view: view.into(),
                ops: vec![Op::Update { base, block: next }],
                deps,
                provenance: Provenance {
                    actor: "agent:a".into(),
                    run: Some("ra".into()),
                    role: Role::Writer,
                    at_ms: 7,
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
fn rf4_history_and_compact_show_reflects() {
    let (_d, mut r) = project();
    let w = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Rx", "doc_type": "page"}},
            {"op": "create", "type": "claim", "id": "$a", "parent": "$d", "body": "One."},
            {"op": "create", "type": "claim", "id": "$b", "parent": "$d", "body": "Uno."}
        ]),
    )
    .unwrap();
    land(&mut r, "seed").unwrap();
    let (doc, a, b) = (
        w.created["$d"].clone(),
        w.created["$a"].clone(),
        w.created["$b"].clone(),
    );
    rewrite(
        &mut r,
        "ra",
        &a,
        "Two.",
        Justification::Reflects {
            sources: vec![id("S-0000B"), id("S-0000A")],
            code: false,
        },
    );
    rewrite(
        &mut r,
        "ra",
        &b,
        "Dos.",
        Justification::Reflects {
            sources: vec![],
            code: true,
        },
    );
    land(&mut r, "ra").unwrap();
    let mut st = McpState::new(Arc::new(Local::new(r)));
    rdy_session(
        &mut st,
        from_value(json!({"actor": "agent:qa", "role": "writer", "view": "work"})).unwrap(),
    )
    .unwrap();
    // rdy_context: the justification of the last change, and each history entry's
    for (block, want) in [
        (
            &a,
            json!({"reflects": {"sources": ["S-0000B", "S-0000A"], "code": false}}),
        ),
        (&b, json!({"reflects": {"sources": [], "code": true}})),
    ] {
        let v = rdy_context(
            &mut st,
            from_value(json!({"id": block, "scope": "canon"})).unwrap(),
        )
        .unwrap();
        assert_eq!(v["justification"], want, "{v}");
        let h = v["history"].as_array().unwrap();
        assert_eq!(h.len(), 2, "{v}");
        assert_eq!(h[0]["justification"], want);
        assert_eq!(h[0]["landing"]["approved_by"], "human:qa");
        assert_eq!(h[1]["justification"], Value::Null);
    }
    // rdy_read compact: each line carries its block's label
    let v = rdy_read(
        &mut st,
        from_value(json!({"id": doc, "scope": "canon", "max_tokens": 20000})).unwrap(),
    )
    .unwrap();
    let text = v["text"].as_str().unwrap().to_string();
    let line = |id: &str| -> String {
        text.lines()
            .find(|l| l.trim_start().starts_with(id))
            .unwrap_or_else(|| panic!("{id}: {text}"))
            .to_string()
    };
    assert!(line(&a).ends_with(" [reflects S-0000B, S-0000A]"), "{text}");
    assert!(line(&b).ends_with(" [reflects code]"), "{text}");
}
