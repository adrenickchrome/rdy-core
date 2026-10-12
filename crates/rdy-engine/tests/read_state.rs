//! snap1: an immutable published state, built lazily, seeded incrementally, held safely.
use rdy_engine::{Rdy, ReadState, Scope, Session, WriteRequest};
use rdy_model::Role;

fn assert_send_sync<T: Send + Sync>() {}
fn assert_send<T: Send>() {}

fn writer() -> Session {
    Session {
        actor: "a".into(),
        run: None,
        role: Role::Writer,
        kind: rdy_engine::SessionKind::Human,
    }
}

fn doc(r: &mut Rdy, view: &str, name: &str) -> String {
    let ops = serde_json::from_value(serde_json::json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": name, "title": "D", "doc_type": "glossary"}}
    ]))
    .unwrap();
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops,
            justification: None,
            message: String::new(),
        },
    )
    .unwrap()
    .created["$d"]
        .clone()
}

fn land(r: &mut Rdy, view: &str) -> rdy_model::Hash {
    let p = r.propose(&writer(), view, "p").unwrap();
    r.land(
        &Session {
            actor: "h".into(),
            run: None,
            role: Role::Approver,
            kind: rdy_engine::SessionKind::Human,
        },
        &p.id,
    )
    .unwrap()
    .parse()
    .unwrap()
}

#[test]
fn snap1_types_are_send_sync() {
    assert_send_sync::<ReadState>();
    assert_send::<Rdy>();
}

#[test]
fn snap1_a_read_after_a_write_sees_it_and_the_states_seq_advances() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let s0 = r.read();
    assert_eq!(s0.seq(), r.store().log_end());
    assert!(s0.view_names().is_empty());
    let id = doc(&mut r, "v", "one");
    let s1 = r.read();
    assert!(s1.seq() > s0.seq());
    assert_eq!(s1.view_names(), vec!["v".to_string()]);
    assert_eq!(s1.included("v").len(), 1);
    let m = s1.materialized(&Scope::View("v".into())).unwrap();
    assert!(m.get(&rdy_model::BlockId::parse(&id).unwrap()).is_some());
    assert!(
        s0.materialized(&Scope::View("v".into()))
            .unwrap()
            .is_empty(),
        "the old state floats on an empty canon"
    );
}

#[test]
fn snap1_a_held_state_is_unchanged_by_later_writes() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    doc(&mut r, "v", "one");
    let held = r.read();
    let held_m = held.materialized(&Scope::View("v".into())).unwrap();
    doc(&mut r, "v", "two");
    let l = land(&mut r, "v");
    doc(&mut r, "w", "three");
    assert_eq!(held.canon_head(), None);
    assert_eq!(held.included("v").len(), 1);
    assert_eq!(held.view_names(), vec!["v".to_string()]);
    assert!(!held.has_view("w"));
    assert_eq!(
        held.materialized(&Scope::View("v".into())).unwrap().len(),
        1
    );
    assert!(std::sync::Arc::ptr_eq(
        &held_m,
        &held.materialized(&Scope::View("v".into())).unwrap()
    ));
    assert!(held.materialized(&Scope::Canon).unwrap().is_empty());
    let now = r.read();
    assert_eq!(now.canon_head(), Some(l));
    assert_eq!(now.materialized(&Scope::Canon).unwrap().len(), 2);
}

#[test]
fn snap1_an_empty_view_floats_to_the_new_canon_head() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    assert!(
        r.read()
            .materialized(&Scope::View("never-written".into()))
            .unwrap()
            .is_empty()
    );
    doc(&mut r, "v", "one");
    land(&mut r, "v");
    let s = r.read();
    assert!(!s.has_view("never-written"));
    assert_eq!(
        s.materialized(&Scope::View("never-written".into()))
            .unwrap()
            .len(),
        1
    );
    assert!(
        std::sync::Arc::ptr_eq(
            &s.materialized(&Scope::View("never-written".into()))
                .unwrap(),
            &s.materialized(&Scope::Canon).unwrap()
        ),
        "shares canon's build"
    );
}

#[test]
fn snap1_a_commit_seeds_the_views_entry_only_when_it_was_built() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    doc(&mut r, "v", "one");
    // the write itself needed the view's state, so the entry it publishes is seeded
    assert_eq!(r.read().is_built(&Scope::View("v".into())), (true, false));
    r.invalidate();
    assert_eq!(r.read().is_built(&Scope::View("v".into())), (false, false));
    doc(&mut r, "v", "two");
    let s = r.read();
    assert_eq!(s.is_built(&Scope::View("v".into())), (true, false));
    assert_eq!(s.materialized(&Scope::View("v".into())).unwrap().len(), 2);
    // plan M2e2: the write logs its moves, and the tension set it builds reads canon
    assert_eq!(
        s.is_built(&Scope::Canon),
        (true, true),
        "the write's moves built canon"
    );
}

#[test]
fn snap1_a_refused_write_publishes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let id = doc(&mut r, "v", "one");
    let before = r.read();
    let stale = serde_json::from_value(serde_json::json!([
        {"op": "update", "id": id, "base": rdy_model::Hash([3; 32]).to_string(), "fields": {"title": "X"}}
    ]))
    .unwrap();
    let e = r
        .write(
            &writer(),
            WriteRequest {
                view: "v".into(),
                ops: stale,
                justification: None,
                message: String::new(),
            },
        )
        .unwrap_err();
    assert_eq!(e.code, rdy_engine::E_STALE_BASE);
    let after = r.read();
    assert!(std::sync::Arc::ptr_eq(&before, &after), "no publish");
    assert_eq!(after.seq(), before.seq());
}

#[test]
fn snap1_as_of_entries_are_shared_across_states_and_capped() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let mut landings = vec![];
    for i in 0..6 {
        doc(&mut r, "v", &format!("d{i}"));
        landings.push(land(&mut r, "v"));
    }
    let s = r.read();
    for l in &landings[..4] {
        s.materialized(&Scope::At(*l)).unwrap();
    }
    assert!(
        r.read().is_built(&Scope::At(landings[0])).0,
        "a later state sees the shared cache"
    );
    s.materialized(&Scope::At(landings[4])).unwrap();
    assert!(
        !s.is_built(&Scope::At(landings[0])).0,
        "the least recently used was evicted"
    );
    assert_eq!(s.materialized(&Scope::At(landings[2])).unwrap().len(), 3);
}

#[test]
fn snap1_a_conflicted_merge_is_materialized_from_its_state_not_by_replay() {
    use rdy_store::merge::{Conflict, Merged, Side};
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let id = rdy_model::BlockId::parse(&doc(&mut r, "v", "one")).unwrap();
    land(&mut r, "v");
    let s = r.read();
    let canon = s.materialized(&Scope::Canon).unwrap();
    let base_version = canon.version_of(&id).unwrap();
    // two sides edit the same block from the same base; the merge's state keeps the base
    let mut a = canon.get(&id).unwrap().clone();
    a.fields
        .insert("title".into(), rdy_model::FieldValue::Str("A".into()));
    let mut b = canon.get(&id).unwrap().clone();
    b.fields
        .insert("title".into(), rdy_model::FieldValue::Str("B".into()));
    let merged = Merged {
        state: canon.snapshot(),
        conflicts: vec![Conflict {
            id: rdy_model::Hash([5; 32]),
            block: id.clone(),
            base: Some(base_version),
            sides: vec![
                Side {
                    change: Some(rdy_model::Hash([1; 32])),
                    version: Some(a.version_hash()),
                },
                Side {
                    change: Some(rdy_model::Hash([2; 32])),
                    version: Some(b.version_hash()),
                },
            ],
        }],
        order: vec![rdy_model::Hash([1; 32]), rdy_model::Hash([2; 32])],
    };
    let built = rdy_engine::testkit::build_view_entry_for_tests(&s, "v", merged).unwrap();
    assert_eq!(
        built.0.version_of(&id),
        Some(base_version),
        "the base version, not a side"
    );
    assert_eq!(built.0.get(&id).unwrap().field_str("title"), Some("D"));
    assert_eq!(built.1.len(), 1);
}

#[test]
fn snap1_a_reopen_refused_by_a_held_state_names_this_process_not_another() {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let held = r.read();
    drop(r);
    let e = Rdy::open(dir.path())
        .err()
        .expect("a held state keeps the index");
    assert_eq!(e.code, rdy_engine::E_LOCKED, "{e}");
    assert!(
        e.message.contains("this process") && !e.message.contains("another"),
        "{e}"
    );
    assert!(e.fix.contains("read state"), "{e}");
    drop(held);
    Rdy::open(dir.path()).unwrap();
}

fn retitle(r: &mut Rdy, view: &str, id: &str, title: &str) {
    let ops = serde_json::from_value(serde_json::json!([
        {"op": "update", "id": id, "fields": {"title": title}}
    ]))
    .unwrap();
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops,
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
}

/// A damaged producer entry of canon's version makes a view's conflict count an error. Read as
/// "no producer", it would settle canon's side and report 0 conflicts.
#[test]
fn msw4_an_unreadable_producer_entry_fails_the_view_conflict_count_instead_of_reporting_none() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let id = doc(&mut r, "seed", "glossary");
    land(&mut r, "seed");
    // w1 then w2 (on w1): with canon's side read as "no producer", w2 would settle the conflict
    retitle(&mut r, "w", &id, "W1");
    retitle(&mut r, "w", &id, "W2");
    retitle(&mut r, "o", &id, "O");
    land(&mut r, "o");
    let count = |r: &Rdy| -> Result<usize, rdy_engine::RdyError> {
        let views = r.read().view_list(false, 30, 0)?;
        Ok(views.iter().find(|v| v.name == "w").unwrap().conflicts)
    };
    assert_eq!(count(&r).unwrap(), 1, "healthy index: one conflict");
    let canon = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .iter()
        .find(|b| b.id.as_str() == id)
        .unwrap()
        .version_hash();
    let mut key = canon.0.to_vec();
    key.extend_from_slice(&[0xAB; 3]); // a damaged producer entry: not 64 bytes
    r.store_mut().put_raw_producer_for_tests(&key).unwrap();
    drop(r);
    let r = Rdy::open(dir.path()).unwrap();
    assert!(
        r.store().producer(&canon).is_err(),
        "the damaged entry reads as an error"
    );
    let res = count(&r);
    assert!(res.is_err(), "must not report the count: {res:?}");
}
