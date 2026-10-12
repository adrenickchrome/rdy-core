//! ib3: a subscription starts at the head; areas and role persist; nothing else writes.
use rdy_engine::inbox::sub::subscribe;
use rdy_engine::{E_BAD_INPUT, Engine, Local, Session};
use rdy_model::Role; // rdy_engine does not re-export Role at BASE
use rdy_qa::fixtures::*;
use rdy_qa::packs::qa_project;
use rdy_qa::tensions::*;
use serde_json::json;

fn rev(name: &str) -> Session {
    Session::agent(format!("agent:{name}"), Some(name.into()), Role::Reviewer).unwrap()
}

#[test]
fn ib3_a_new_subscription_starts_at_the_head() {
    let (_d, mut r) = qa_project();
    put(
        &mut r,
        "x",
        json!([{"op": "create", "type": "doc", "id": "$d",
        "fields": {"name": "x", "title": "X", "doc_type": "overview"}}]),
    );
    let head = r.read().seq();
    let e = Local::new(r);
    let sub = subscribe(&e, &ag("a"), None).unwrap();
    assert_eq!(sub.cursor.at, head);
    assert_eq!(sub.cursor.skip, 0);
    assert_eq!(sub.role, "writer");
    assert!(sub.areas.is_empty());
}

#[test]
fn ib3_areas_and_role_persist_and_an_unchanged_call_writes_nothing() {
    let (_d, r) = qa_project();
    let e = Local::new(r);
    subscribe(
        &e,
        &ag("a"),
        Some(vec!["payments".into(), "identity".into()]),
    )
    .unwrap();
    let end = e.read().seq();
    let again = subscribe(&e, &ag("a"), None).unwrap();
    assert_eq!(
        again.areas,
        vec!["identity".to_string(), "payments".to_string()]
    );
    assert_eq!(e.read().seq(), end, "no change: no write");
    let as_rev = subscribe(&e, &rev("a"), None);
    // rev("a") is agent:a/a with role reviewer: the same key, a new role
    assert_eq!(as_rev.unwrap().role, "reviewer");
    assert!(e.read().seq() > end);
    let cleared = subscribe(&e, &ag("a"), Some(vec![])).unwrap();
    assert!(cleared.areas.is_empty());
}

#[test]
fn ib3_an_unknown_area_is_refused_with_the_pack_areas() {
    let (_d, r) = qa_project();
    let e = Local::new(r);
    let err = subscribe(&e, &ag("a"), Some(vec!["paymnets".into()])).unwrap_err();
    assert_eq!(err.code, E_BAD_INPUT);
    assert_eq!(err.message, "area \"paymnets\" is not declared in the pack");
    assert_eq!(
        err.fix,
        "pass areas the pack declares: glossary, identity, payments"
    );
    let (_d2, r2) = project();
    let e2 = Local::new(r2);
    let err = subscribe(&e2, &ag("a"), Some(vec!["x".into()])).unwrap_err();
    assert_eq!(err.fix, "the pack declares no areas: pass none");
}

#[test]
fn ib3_a_store_refusal_is_an_internal_error_with_the_stores_text() {
    use rdy_engine::E_INTERNAL;
    use rdy_store::inbox::{InboxCursor, InboxSub};
    let (_d, mut r) = qa_project();
    let at = r.read().seq();
    let sub = InboxSub {
        actor: "agent:a".into(),
        run: Some("r1".into()),
        cursor: InboxCursor { at, skip: 0 },
        areas: vec![],
        role: "approver".into(),
    };
    let err = r.put_subscription(&sub).unwrap_err();
    assert_eq!(err.code, E_INTERNAL);
    assert_eq!(
        err.message,
        "inbox ref \"inbox/agent:a/r1\": role \"approver\" is not reader, writer or reviewer"
    );
    assert_eq!(err.fix, "report this as a bug in rdy");
}
