//! ib2: a run's in-flight changes (with their blocks, citations, the run's views and areas) and
//! the reader-level replay.
use rdy_engine::Scope;
use rdy_model::BlockId;
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
    assert!(new.starts_with(&old), "the older replay is a prefix");
    assert!(
        old.iter().all(|e| e.seq <= st.seq()),
        "an older state replays up to its own seq"
    );
    assert_eq!(
        new,
        r.replay_events(start).unwrap(),
        "Rdy::replay_events is the same walk"
    );
}

/// A payments doc in canon with section `$s` and claims `$t` and `$u`; returns (s, t, u).
fn payments_spec(r: &mut rdy_engine::Rdy) -> (BlockId, BlockId, BlockId) {
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
    let w = write(r, "work", json!([
        {"op": "create", "type": "section", "id": "$s", "parent": doc.to_string(), "fields": {"title": "S"}},
        {"op": "create", "type": "claim", "id": "$t", "parent": "$s", "body": "Target."},
        {"op": "create", "type": "claim", "id": "$u", "parent": "$s", "body": "Other target."}
    ])).unwrap();
    land(r, "work").unwrap();
    (created(&w, "$s"), created(&w, "$t"), created(&w, "$u"))
}

/// F3 (D-TPZ0W): a block the run writes is not among the blocks it cites, though its own
/// writes cite it.
#[test]
fn ib2_a_block_the_run_writes_is_not_among_its_citations() {
    let (_d, mut r) = qa_project();
    let (s, t, u) = payments_spec(&mut r);
    let a = put(&mut r, "a", cite(&s, &t));
    let y = created(&a, "$y");
    put(
        &mut r,
        "a",
        upd(&t, &format!("Target, now on {{{{{u}}}}}.")),
    );
    let infl = r.read().run_in_flight("agent:a", Some("a")).unwrap();
    let mut both = vec![t.clone(), y];
    both.sort();
    assert_eq!(infl.blocks.into_iter().collect::<Vec<_>>(), both);
    assert_eq!(
        infl.cited.into_iter().collect::<Vec<_>>(),
        vec![u],
        "t is the run's own block, so only u is cited"
    );
}

/// F3: only live writes that keep a block count. A delete writes no block, and an earlier write
/// that a later change of the run replaced cites nothing any more.
#[test]
fn ib2_deletes_and_replaced_writes_add_no_blocks_or_citations() {
    let (_d, mut r) = qa_project();
    let (s, t, u) = payments_spec(&mut r);
    let a = put(&mut r, "a", cite(&s, &t));
    let y = created(&a, "$y");
    // the run's later change makes y cite u instead of t
    put(&mut r, "a", upd(&y, &format!("Builds on {{{{{u}}}}} now.")));
    let infl = r.read().run_in_flight("agent:a", Some("a")).unwrap();
    assert_eq!(infl.changes.len(), 2, "{infl:?}");
    assert_eq!(infl.blocks.iter().cloned().collect::<Vec<_>>(), vec![y]);
    assert_eq!(
        infl.cited.into_iter().collect::<Vec<_>>(),
        vec![u.clone()],
        "the replaced write of y no longer cites t"
    );
    // a run that only deletes writes no block and is in no area
    put(&mut r, "d", json!([{"op": "delete", "id": u.to_string()}]));
    let infl = r.read().run_in_flight("agent:d", Some("d")).unwrap();
    assert_eq!(infl.changes.len(), 1, "{infl:?}");
    assert!(infl.blocks.is_empty() && infl.cited.is_empty(), "{infl:?}");
    assert!(infl.areas.is_empty(), "{infl:?}");
}

/// F4: a written block that the in-flight scope does not hold gives the run no area, and no
/// error. Run b creates payments claim y; the human forks b's view and deletes y there, so the
/// merged in-flight scope has no y while b's view still holds it.
#[test]
fn ib2_a_block_missing_from_the_in_flight_scope_gives_no_area() {
    let (_d, mut r) = qa_project();
    let (s, t, _u) = payments_spec(&mut r);
    let b = put(&mut r, "b", cite(&s, &t));
    let y = created(&b, "$y");
    r.view_fork(&writer(), "b", "w2").unwrap();
    write(&mut r, "w2", json!([{"op": "delete", "id": y.to_string()}])).unwrap();
    let missing = r.read().block(&Scope::Inflight, &y).unwrap_err();
    assert_eq!(missing.code, "E_UNKNOWN_BLOCK", "{missing:?}");
    let infl = r.read().run_in_flight("agent:b", Some("b")).unwrap();
    assert_eq!(infl.blocks.into_iter().collect::<Vec<_>>(), vec![y]);
    assert!(infl.areas.is_empty(), "{:?}", infl.areas);
}
