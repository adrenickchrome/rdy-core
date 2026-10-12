//! ib4: the closed rules that put a log event in a session's inbox.
use rdy_engine::inbox::Entry;
use rdy_engine::inbox::matcher::{batch_items, match_event};
use rdy_engine::{EventKind, Rdy, Session};
use rdy_model::Role;
use rdy_qa::fixtures::*;
use rdy_qa::packs::qa_project;
use rdy_qa::tensions::*;
use rdy_store::inbox::{InboxCursor, InboxSub};
use serde_json::json;

/// The subscription Task 3's `subscribe` writes for a new session, held in memory: its cursor
/// at the head, the given areas (sorted), the session's role.
fn sub_at_head(r: &Rdy, s: &Session, areas: &[&str]) -> InboxSub {
    let mut areas: Vec<String> = areas.iter().map(|a| a.to_string()).collect();
    areas.sort();
    let role = match s.role {
        Role::Reviewer => "reviewer",
        Role::Reader => "reader",
        _ => "writer",
    };
    InboxSub {
        actor: s.actor.clone(),
        run: s.run.clone(),
        cursor: InboxCursor {
            at: r.read().seq(),
            skip: 0,
        },
        areas,
        role: role.into(),
    }
}

/// Every entry the session of `sub` would get from its cursor, in log order.
fn entries(r: &Rdy, sub: &InboxSub) -> Vec<Entry> {
    let st = r.read();
    let infl = st.run_in_flight(&sub.actor, sub.run.as_deref()).unwrap();
    st.replay(sub.cursor.at)
        .unwrap()
        .iter()
        .filter_map(|e| match_event(&st, sub, &infl, e).unwrap())
        .collect()
}
/// The entries of the batch that holds the first landing after the cursor of `sub`, from raw
/// item `skip` on, each with its raw index in the batch: what a read that resumes inside that
/// batch at `skip` gets from it.
fn landing_batch_from(r: &Rdy, sub: &InboxSub, skip: usize) -> Vec<(usize, Entry)> {
    let st = r.read();
    let infl = st.run_in_flight(&sub.actor, sub.run.as_deref()).unwrap();
    let evs = st.replay(sub.cursor.at).unwrap();
    let batch = evs
        .chunk_by(|x, y| x.seq == y.seq)
        .find(|b| b.iter().any(|e| matches!(e.kind, EventKind::Landed { .. })))
        .unwrap();
    batch_items(&st, sub, &infl, batch, skip)
        .filter_map(|x| {
            let (i, e) = x.unwrap();
            e.map(|e| (i, e))
        })
        .collect()
}
fn kinds(es: &[(usize, Entry)]) -> Vec<&str> {
    es.iter().map(|(_, e)| e.event).collect()
}
fn events(es: &[Entry]) -> Vec<&str> {
    es.iter().map(|e| e.event).collect()
}

/// A doc at `path` in canon with one claim; returns the claim.
fn landed_claim(r: &mut Rdy, path: &str) -> rdy_model::BlockId {
    r.new_doc(&writer(), "work", path, Some("overview"), Some("T"))
        .unwrap();
    let doc = r
        .read()
        .resolve_path(&rdy_engine::Scope::View("work".into()), path)
        .unwrap();
    let w = write(
        r,
        "work",
        json!([{"op": "create", "type": "claim", "id": "$c",
        "parent": doc.to_string(), "body": "x"}]),
    )
    .unwrap();
    land(r, "work").unwrap();
    created(&w, "$c")
}

/// Session `s` posts an intent on `blocks` in its view `view`.
fn post_intent_on(r: &mut Rdy, s: &Session, view: &str, blocks: &[&rdy_model::BlockId]) {
    let blocks: Vec<String> = blocks.iter().map(|b| b.to_string()).collect();
    r.intent_post(s, Some(view), None, &blocks, "edit", None)
        .unwrap();
}

#[test]
fn ib4_a_landing_in_a_declared_area_matches_and_one_outside_does_not() {
    let (_d, mut r) = qa_project();
    let pay = landed_claim(&mut r, "payments/fees");
    let ident = landed_claim(&mut r, "identity/login");
    let a = sub_at_head(&r, &ag("a"), &["payments"]);
    put(&mut r, "b", upd(&ident, "identity edit"));
    land(&mut r, "b").unwrap();
    assert!(
        events(&entries(&r, &a)).is_empty(),
        "identity is not a's area"
    );
    put(&mut r, "c", upd(&pay, "payments edit"));
    let l = land(&mut r, "c").unwrap();
    let es = entries(&r, &a);
    assert_eq!(events(&es), ["landed"]);
    assert!(es[0].line.starts_with("landing "), "{}", es[0].line);
    assert!(
        es[0]
            .line
            .ends_with("of view c changed 1 blocks in your areas (payments)"),
        "{}",
        es[0].line
    );
    let _ = l;
}

#[test]
fn ib4_with_no_declared_areas_the_areas_of_your_blocks_apply() {
    let (_d, mut r) = qa_project();
    let pay = landed_claim(&mut r, "payments/fees");
    let pay2 = landed_claim(&mut r, "payments/limits");
    put(&mut r, "a", upd(&pay, "a edits payments"));
    let a = sub_at_head(&r, &ag("a"), &[]);
    put(&mut r, "b", upd(&pay2, "b lands in payments"));
    land(&mut r, "b").unwrap();
    assert_eq!(events(&entries(&r, &a)), ["landed"]);
}

#[test]
fn ib4_your_own_landing_does_not_match() {
    let (_d, mut r) = qa_project();
    let pay = landed_claim(&mut r, "payments/fees");
    let a = sub_at_head(&r, &ag("a"), &["payments"]);
    put(&mut r, "a", upd(&pay, "mine"));
    land(&mut r, "a").unwrap();
    assert!(!events(&entries(&r, &a)).contains(&"landed"));
}

#[test]
fn ib4_your_views_rebased_with_conflicts_and_closed_match() {
    let (_d, mut r) = qa_project();
    let c = landed_claim(&mut r, "payments/fees");
    put(&mut r, "a", upd(&c, "a's text"));
    let a = sub_at_head(&r, &ag("a"), &[]);
    put(&mut r, "b", upd(&c, "b's text"));
    land(&mut r, "b").unwrap();
    r.view_close(&writer(), &["a".to_string()], "by hand")
        .unwrap();
    let es = entries(&r, &a);
    assert!(events(&es).contains(&"view_rebased"), "{es:?}");
    assert!(
        es.iter()
            .any(|e| e.line.starts_with("your view a rebased onto landing ")
                && e.line.ends_with("with 1 conflicts")),
        "{es:?}"
    );
    assert!(
        es.iter()
            .any(|e| e.event == "view_closed" && e.line.starts_with("your view a closed: ")),
        "{es:?}"
    );
}

#[test]
fn ib4_an_intent_on_your_blocks_by_another_session_matches() {
    let (_d, mut r) = qa_project();
    let c = landed_claim(&mut r, "payments/fees");
    put(&mut r, "a", upd(&c, "a edits c"));
    let a = sub_at_head(&r, &ag("a"), &[]);
    post_intent_on(&mut r, &ag("b"), "b", &[&c]);
    let es = entries(&r, &a);
    assert_eq!(events(&es), ["intent_posted"]);
    assert!(
        es[0].line.starts_with("agent:b posted intent ")
            && es[0].line.ends_with("on 1 blocks you are changing"),
        "{}",
        es[0].line
    );
}

#[test]
fn ib4_review_requests_reach_reviewers_only() {
    let (_d, mut r) = rdy_qa::fixtures::project();
    let spec = rdy_qa::edit_loop::spec_doc(&mut r);
    let rv = Session::agent("agent:rv".into(), Some("rv".into()), Role::Reviewer).unwrap();
    let rvs = sub_at_head(&r, &rv, &[]);
    let w = sub_at_head(&r, &ag("w"), &[]);
    put(&mut r, "a", upd(&spec.c, "C contradicts B."));
    let es = entries(&r, &rvs);
    assert_eq!(events(&es), ["review_requested"]);
    assert!(
        es[0].line.ends_with(": call rdy_review_next"),
        "{}",
        es[0].line
    );
    assert!(
        entries(&r, &w).is_empty(),
        "a writer gets no review requests"
    );
}

#[test]
fn ib4_other_kinds_never_match() {
    let (_d, mut r) = qa_project();
    let a = sub_at_head(&r, &ag("a"), &[]);
    put(
        &mut r,
        "b",
        json!([{"op": "create", "type": "doc", "id": "$d",
        "fields": {"name": "x", "title": "X", "doc_type": "overview"}}]),
    );
    let p = r.propose(&writer(), "b", "b").unwrap();
    let _ = p;
    assert!(entries(&r, &a).is_empty());
}

/// Ruling task-4.1: a cut inside a batch counts the batch's raw items, not its matched entries,
/// so a later close of the session's view (its effective areas become empty) loses no entry.
#[test]
fn ib4_a_cut_inside_a_batch_survives_a_later_close() {
    let (_d, mut r) = qa_project();
    let c = landed_claim(&mut r, "payments/fees");
    put(&mut r, "a", upd(&c, "a's text"));
    let a = sub_at_head(&r, &ag("a"), &[]);
    put(&mut r, "b", upd(&c, "b's text"));
    land(&mut r, "b").unwrap();
    let all = landing_batch_from(&r, &a, 0);
    assert_eq!(kinds(&all), ["landed", "view_rebased"], "{all:?}");
    // a page that holds only `landed` ends after its raw item
    let cut = all[0].0 + 1;
    r.view_close(&writer(), &["a".to_string()], "by hand")
        .unwrap();
    assert_eq!(
        kinds(&landing_batch_from(&r, &a, 0)),
        ["view_rebased"],
        "a has no in-flight blocks left"
    );
    let rest = landing_batch_from(&r, &a, cut);
    assert_eq!(kinds(&rest), ["view_rebased"], "the cut drops nothing");
}

/// Ruling task-4.1, the reverse: an area the session gains after a cut delivers nothing twice.
#[test]
fn ib4_a_cut_inside_a_batch_delivers_nothing_twice_after_an_area_is_gained() {
    let (_d, mut r) = qa_project();
    let c = landed_claim(&mut r, "payments/fees");
    put(&mut r, "a", upd(&c, "a's text"));
    let mut a = sub_at_head(&r, &ag("a"), &["identity"]);
    put(&mut r, "b", upd(&c, "b's text"));
    land(&mut r, "b").unwrap();
    let all = landing_batch_from(&r, &a, 0);
    assert_eq!(
        kinds(&all),
        ["view_rebased"],
        "payments is not a declared area"
    );
    // a page that holds `view_rebased` ends after its raw item
    let cut = all[0].0 + 1;
    a.areas = vec!["identity".into(), "payments".into()];
    assert_eq!(
        kinds(&landing_batch_from(&r, &a, 0)),
        ["landed", "view_rebased"],
        "payments is now an area"
    );
    let rest = landing_batch_from(&r, &a, cut);
    assert!(rest.is_empty(), "nothing is delivered twice: {rest:?}");
}
