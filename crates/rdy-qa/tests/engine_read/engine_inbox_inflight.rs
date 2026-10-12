//! ib2: a run's in-flight changes (with their blocks, citations, the run's views and areas) and
//! the reader-level replay.
use rdy_engine::Scope;
use rdy_qa::fixtures::*;
use rdy_qa::packs::qa_project;
use rdy_qa::tensions::*;
use serde_json::json;

#[test]
fn ib2_a_run_has_its_in_flight_changes_blocks_citations_views_and_areas() {
    let (_d, mut r) = qa_project();
    r.new_doc(
        &writer(),
        "work",
        "payments/fees",
        Some("overview"),
        Some("Fees"),
    )
    .unwrap();
    let doc = r
        .read()
        .resolve_path(&Scope::View("work".into()), "payments/fees")
        .unwrap();
    let w = write(&mut r, "work", json!([
        {"op": "create", "type": "section", "id": "$s", "parent": doc.to_string(), "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$t", "parent": "$s", "body": "Target."}
    ])).unwrap();
    land(&mut r, "work").unwrap();
    let (s, t) = (created(&w, "$s"), created(&w, "$t"));
    let a = put(&mut r, "a", cite(&s, &t));
    let y = created(&a, "$y");
    put(&mut r, "b", upd(&t, "Other agent's edit."));
    let infl = r.read().run_in_flight("agent:a", Some("a")).unwrap();
    assert_eq!(
        infl.changes.into_iter().collect::<Vec<_>>(),
        vec![change_of(&a)]
    );
    assert_eq!(infl.blocks.into_iter().collect::<Vec<_>>(), vec![y]);
    assert_eq!(infl.cited.into_iter().collect::<Vec<_>>(), vec![t]);
    assert_eq!(
        infl.views.into_iter().collect::<Vec<_>>(),
        vec!["a".to_string()]
    );
    assert_eq!(infl.areas, vec!["payments".to_string()]);
    // another run of the same actor has no in-flight changes here
    assert_eq!(
        r.read()
            .run_in_flight("agent:a", Some("other"))
            .unwrap()
            .changes
            .len(),
        0
    );
}

#[test]
fn ib2_landed_changes_are_no_longer_in_flight() {
    let (_d, mut r) = project();
    let spec = rdy_qa::edit_loop::spec_doc(&mut r);
    put(&mut r, "a", upd(&spec.c, "C changed."));
    land(&mut r, "a").unwrap();
    let infl = r.read().run_in_flight("agent:a", Some("a")).unwrap();
    assert!(
        infl.changes.is_empty() && infl.blocks.is_empty(),
        "{infl:?}"
    );
    assert!(
        infl.views.contains("a"),
        "the view stays the run's own after it lands"
    );
}

#[test]
fn ib2_a_state_replays_up_to_its_own_seq() {
    let (_d, mut r) = project();
    let start = r.read().seq();
    put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "doc", "id": "$d",
        "fields": {"name": "x", "title": "X", "doc_type": "overview"}}]),
    );
    let st = r.read();
    put(
        &mut r,
        "a",
        json!([{"op": "create", "type": "doc", "id": "$d",
        "fields": {"name": "y", "title": "Y", "doc_type": "overview"}}]),
    );
    let old = st.replay(start).unwrap();
    let new = r.read().replay(start).unwrap();
    assert!(
        old.len() < new.len(),
        "an older state never sees later batches"
    );
    assert_eq!(
        new,
        r.replay_events(start).unwrap(),
        "Rdy::replay_events is the same walk"
    );
}
