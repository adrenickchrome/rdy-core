//! own1: non-human sessions include, exclude, undo and close only in views they own; a human
//! does all of these anywhere; `own` and `close --stale` are the human's.
use rdy_engine::engine::{Engine, Local};
use rdy_engine::view_api::{ViewRequest, run_view};
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::invariants::check;
use rdy_store::view::Owner;
use rdy_store::view::ViewOpKind;
use serde_json::json;

fn claim(parent: &str, body: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "claim", "parent": parent, "body": body}])
}
fn head(r: &Rdy, v: &str) -> String {
    r.read().view_head(v).unwrap().head.to_string()
}

fn two_agents() -> (
    tempfile::TempDir,
    Rdy,
    String,
    Session,
    Session,
    String,
    String,
) {
    let (d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    let b = as_agent("agent:b", "r1");
    let ca = write_as(&mut r, &a, "a", claim(&section, "a"))
        .unwrap()
        .change;
    let cb = write_as(&mut r, &b, "b", claim(&section, "b"))
        .unwrap()
        .change;
    (d, r, section, a, b, ca, cb)
}

#[test]
fn own1_an_agent_cannot_include_exclude_undo_or_close_in_another_sessions_view() {
    let (_d, mut r, section, a, b, ca, cb) = two_agents();
    let e = r
        .view_include(&b, "a", std::slice::from_ref(&cb))
        .unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e}");
    assert!(
        e.message.contains("agent:a/r1") && e.fix.contains("fork"),
        "{e}"
    );
    let e = r
        .view_exclude(&b, "a", std::slice::from_ref(&ca), false)
        .unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e}");
    let e = r.undo(&b, "a", Some(&head(&r, "a")), false).unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e}");
    let e = r.view_close(&b, &["a".into()], "x").unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e}");
    let e = r
        .view_include(&as_agent("agent:a", "r2"), "a", std::slice::from_ref(&cb))
        .unwrap_err();
    assert_eq!(
        e.code, E_VIEW_OWNER,
        "another run of the same actor is another session: {e}"
    );
    let e = write_as(&mut r, &b, "a", claim(&section, "into a")).unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "M2b's commit rule is unchanged: {e}");
    assert_eq!(r.read().included("a").len(), 1, "nothing written into a");
    r.view_include(&b, "b", &[ca]).unwrap();
    r.view_include(&a, "a", &[cb]).unwrap();
    check(&r, &Scope::View("a".into())).unwrap();
}

#[test]
fn own1_a_human_does_everything_anywhere() {
    let (_d, mut r, section, _a, _b, ca, cb) = two_agents();
    let h = human_writer();
    r.view_include(&h, "a", std::slice::from_ref(&cb)).unwrap();
    r.view_exclude(&h, "a", &[ca], false).unwrap();
    let u = r.undo(&h, "a", Some(&head(&r, "a")), false).unwrap();
    assert_eq!(u.kind, "exclude");
    write_as(&mut r, &h, "a", claim(&section, "human's")).unwrap();
    assert_eq!(
        r.read()
            .view_head("a")
            .unwrap()
            .owner
            .as_ref()
            .unwrap()
            .actor,
        "agent:a",
        "a human's write does not take an owned view"
    );
    r.view_close(&h, &["a".into(), "b".into()], "human closes anything")
        .unwrap();
    assert!(r.read().view_head("b").unwrap().closed);
}

#[test]
fn own1_an_unowned_view_refuses_agents_ops_until_a_commit_claims_it() {
    let (_d, mut r, section, a, b, _ca, cb) = two_agents();
    let h = r.view_own(&human_writer(), "a", None).unwrap();
    assert_eq!(h.owner, None);
    let e = r
        .view_include(&b, "a", std::slice::from_ref(&cb))
        .unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e}");
    assert!(
        e.message.contains("unowned") && e.fix.contains("commit to it to claim it"),
        "{e}"
    );
    write_as(&mut r, &b, "a", claim(&section, "b claims")).unwrap();
    assert_eq!(
        r.read().view_head("a").unwrap().owner,
        Some(Owner {
            actor: "agent:b".into(),
            run: Some("r1".into())
        })
    );
    let e = r
        .view_include(&a, "a", std::slice::from_ref(&cb))
        .unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "a lost the view: {e}");
    r.view_include(&b, "a", &[cb]).unwrap();
}

#[test]
fn own1_own_is_human_only_and_reassigns_or_clears_and_undo_restores() {
    let (_d, mut r, section, a, _b, _ca, _cb) = two_agents();
    let e = r.view_own(&a, "a", Some("agent:b/r2")).unwrap_err();
    assert_eq!(e.code, E_PERMISSION, "{e}");
    let h = r
        .view_own(&human_writer(), "a", Some("agent:b/r2"))
        .unwrap();
    assert_eq!(
        h.owner,
        Some(OwnerInfo {
            actor: "agent:b".into(),
            run: Some("r2".into())
        })
    );
    let e = write_as(&mut r, &a, "a", claim(&section, "more")).unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e}");
    write_as(
        &mut r,
        &as_agent("agent:b", "r2"),
        "a",
        claim(&section, "b's"),
    )
    .unwrap();
    let own = r
        .read()
        .reader()
        .view_op(&r.read().view_head("a").unwrap().head)
        .unwrap()
        .prev
        .unwrap()
        .to_string();
    let u = r.undo(&human_writer(), "a", Some(&own), false).unwrap();
    assert_eq!(
        (u.kind.as_str(), u.effect.as_str()),
        ("own", "owner restored")
    );
    assert_eq!(
        r.read().view_head("a").unwrap().owner,
        Some(Owner {
            actor: "agent:a".into(),
            run: Some("r1".into())
        })
    );
    let e = r.view_own(&human_writer(), "a", Some("/r1")).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT, "{e}");
    let e = r.view_own(&human_writer(), "nope", None).unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND, "{e}");
    r.view_close(&human_writer(), &["a".into()], "x").unwrap();
    r.view_own(&human_writer(), "a", None).unwrap();
    assert!(
        r.read().view_head("a").unwrap().closed,
        "own works on a closed view and leaves it closed"
    );
}

#[test]
fn own1_close_stale_is_human_only_and_closes_idle_views() {
    let (_d, mut r, _section, a, _b, _ca, _cb) = two_agents();
    let e = r.view_close_stale(&a, 0, now_ms() + 1).unwrap_err();
    assert_eq!(e.code, E_PERMISSION, "{e}");
    assert!(
        r.view_close_stale(&human_writer(), 14, now_ms())
            .unwrap()
            .closed
            .is_empty()
    );
    let closed = r
        .view_close_stale(&human_writer(), 0, now_ms() + 86_400_000 * 2)
        .unwrap()
        .closed;
    assert_eq!(
        closed.iter().map(|c| c.view.as_str()).collect::<Vec<_>>(),
        vec!["a", "b", "seed"]
    );
    assert!(closed[0].reason.contains("idle"));
    assert!(r.read().view_list(false, 14, now_ms()).unwrap().is_empty());
}

#[test]
fn own1_an_agent_proposes_only_a_view_it_owns() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    let b = as_agent("agent:b", "r1");
    write_as(
        &mut r,
        &a,
        "a",
        json!([{"op": "create", "type": "claim", "parent": section, "body": "x"}]),
    )
    .unwrap();
    let e = r.propose(&b, "a", "t", None).unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e}");
    assert!(
        e.fix.contains("include its changes into your own view"),
        "{e}"
    );
    r.store_mut()
        .view_append(
            "a",
            vec![ViewOpKind::Own { owner: None }],
            &writer().provenance(),
        )
        .unwrap();
    r.invalidate();
    let e = r.propose(&b, "a", "t", None).unwrap_err();
    assert_eq!(
        e.code, E_VIEW_OWNER,
        "a commit claims an unowned view, a proposal does not: {e}"
    );
    r.view_own(&writer(), "a", Some("agent:a/r1")).unwrap();
    assert_eq!(r.propose(&a, "a", "t", None).unwrap().version, 1);
    assert_eq!(
        r.propose(&writer(), "a", "t", None).unwrap().version,
        2,
        "the human proposes any view"
    );
}

fn new_doc(name: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "doc", "fields": {"name": name, "title": name, "doc_type": "page"}}])
}

#[test]
fn own1_work_is_the_humans_view() {
    let (_d, mut r) = project();
    // An agent's first write into `work` on a fresh project is refused, and creates nothing.
    let e = write_as(&mut r, &as_agent("agent:a", "r1"), "work", new_doc("w1")).unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e}");
    assert!(e.message.contains("the human's default view"), "{e}");
    assert!(
        e.fix.contains("r1"),
        "the fix names the agent's own view: {e}"
    );
    assert!(r.read().view_head("work").is_none(), "nothing was created");
    // Nor can an agent create `work` outright, or fork another view into it.
    let a = as_agent("agent:a", "r1");
    let e = r.view_create(&a, "work", None, false).unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e}");
    assert!(e.message.contains("the human's default view"), "{e}");
    assert!(e.fix.contains("r1"), "{e}");
    r.view_create(&a, "r1", None, false).unwrap();
    let e = r.view_fork(&a, "r1", "work").unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e}");
    assert!(e.message.contains("the human's default view"), "{e}");
    assert!(r.read().view_head("work").is_none(), "nothing was created");
    // An agent whose run is literally `work` gets the loop-breaking fix, not "write to work".
    let e = write_as(&mut r, &as_agent("agent:w", "work"), "work", new_doc("w2")).unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e}");
    assert!(e.fix.contains("another run name"), "{e}");
    // The human's first write creates and owns it.
    let c = write(&mut r, "work", new_doc("w3")).unwrap().change;
    assert_eq!(
        r.read().view_head("work").unwrap().owner,
        Some(Owner {
            actor: "human:qa".into(),
            run: Some("cli".into())
        })
    );
    // Every other agent action on `work` is refused the same way.
    let a = as_agent("agent:a", "r1");
    let e = r
        .view_include(&a, "work", std::slice::from_ref(&c))
        .unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e}");
    let e = r
        .view_exclude(&a, "work", std::slice::from_ref(&c), false)
        .unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e}");
    let e = r
        .undo(&a, "work", Some(&head(&r, "work")), false)
        .unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e}");
    let e = r.view_close(&a, &["work".into()], "x").unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e}");
    let e = r.propose(&a, "work", "t", None).unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e}");
    // Even an agent-owned (legacy) or unowned `work` stays the human's.
    r.view_own(&writer(), "work", Some("agent:a/r1")).unwrap();
    let e = write_as(&mut r, &a, "work", new_doc("w4")).unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e}");
    r.view_own(&writer(), "work", None).unwrap();
    let e = write_as(&mut r, &a, "work", new_doc("w5")).unwrap_err();
    assert_eq!(
        e.code, E_VIEW_OWNER,
        "an agent's commit does not claim an unowned work: {e}"
    );
    write(&mut r, "work", new_doc("w6")).unwrap();
    // The one-call MCP path (`run_view`) refuses an agent's create or fork of `work` too.
    let (_d2, r2) = project();
    let e = Local::new(r2);
    for v in [
        json!({"op": "create", "name": "work"}),
        json!({"op": "fork", "from": "r1", "name": "work"}),
    ] {
        if v["op"] == "fork" {
            run_view(
                &e,
                &a,
                "human:qa",
                "r1",
                req(json!({"op": "create", "name": "r1"})),
            )
            .unwrap();
        }
        let err = run_view(&e, &a, "human:qa", "r1", req(v)).unwrap_err();
        assert_eq!(err.code, E_VIEW_OWNER, "{err}");
        assert!(e.read().view_head("work").is_none(), "nothing was created");
    }
}

fn req(v: serde_json::Value) -> ViewRequest {
    serde_json::from_value(v).unwrap()
}

/// Final review fix round 1 (Minor 6): an import is a commit, so like every engine commit it
/// claims the existing unowned view it writes into.
#[test]
fn own1_an_import_into_an_unowned_view_claims_it() {
    let (_a, mut src) = project();
    seed_doc(&mut src, "seed");
    land(&mut src, "seed").unwrap();
    let out = tempfile::tempdir().unwrap();
    let dir = out.path().join("export");
    src.read().export(&Scope::Canon, &dir).unwrap();
    let (_d, mut r) = project();
    r.view_create(&human_writer(), "imp", None, false).unwrap();
    assert_eq!(r.view_own(&writer(), "imp", None).unwrap().owner, None);
    r.import(&approver(), "imp", &dir).unwrap();
    let st = r.read();
    let h = st.view_head("imp").unwrap();
    assert_eq!(
        h.owner.as_ref().map(|o| o.actor.as_str()),
        Some("human:qa"),
        "the import claimed the view"
    );
    let commit = st.reader().view_op(&h.head).unwrap();
    assert!(matches!(commit.op, ViewOpKind::Commit { .. }), "{commit:?}");
    let own = st.reader().view_op(&commit.prev.unwrap()).unwrap();
    assert!(
        matches!(own.op, ViewOpKind::Own { owner: Some(_) }),
        "an Own op before the Commit: {own:?}"
    );
}

/// Final review fix round 1 (Minor 8): `close --stale` never closes the session's own default
/// view (the human's `work`); it says it kept it.
#[test]
fn own1_close_stale_keeps_the_sessions_default_view_and_says_so() {
    let (_d, mut r, _section, _a, _b, _ca, _cb) = two_agents();
    write(&mut r, "work", new_doc("mine")).unwrap();
    let res = r
        .view_close_stale(&human_writer(), 0, now_ms() + 86_400_000 * 2)
        .unwrap();
    assert_eq!(
        res.closed
            .iter()
            .map(|c| c.view.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b", "seed"]
    );
    assert_eq!(res.kept.as_deref(), Some("work"));
    let st = r.read();
    assert!(!st.view_head("work").unwrap().closed, "work stays open");
    drop(st);
    write(&mut r, "work", new_doc("more")).unwrap();
    let none = r.view_close_stale(&human_writer(), 14, now_ms()).unwrap();
    assert!(none.closed.is_empty() && none.kept.is_none(), "{none:?}");
}

/// Final review fix round 1 (Minor 9): `own` takes the writer role, as creating a view does; a
/// human session with the reader role cannot reassign a view.
#[test]
fn own1_own_needs_the_writer_role() {
    let (_d, mut r, _section, _a, _b, _ca, _cb) = two_agents();
    let before = head(&r, "a");
    let e = r.view_own(&reader(), "a", None).unwrap_err();
    assert_eq!(e.code, E_PERMISSION, "{e}");
    assert_eq!(head(&r, "a"), before, "nothing was written");
}
