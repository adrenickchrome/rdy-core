//! ib14: the TRANSITION record, held batches and the open-move tables.
use rdy_model::Hash;
use rdy_store::kinds;
use rdy_store::segment::Location;
use rdy_store::transition::*;
use rdy_store::{Store, StoreError};

fn fresh() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::create(&d.path().join("s")).unwrap();
    (d, s)
}

fn reopen(dir: &tempfile::TempDir) -> Store {
    Store::open(&dir.path().join("s")).unwrap()
}

fn why(cause: Cause, id: Option<Hash>) -> Why {
    Why {
        cause,
        id,
        actor: None,
        run: None,
    }
}
fn change_side(c: &[u8], actor: &str) -> Side {
    Side {
        change: Some(Hash::of(c)),
        actor: Some(actor.into()),
        run: Some("r".into()),
        landing: None,
    }
}
fn tension(id: &[u8], why_: Why) -> TensionMove {
    TensionMove {
        tension: Hash::of(id),
        kind: "edit_collision".into(),
        severity: "blocking".into(),
        sides: vec![change_side(b"c1", "agent:a"), change_side(b"c2", "agent:b")],
        rekey: None,
        why: why_,
    }
}
fn suspect(block: &str, target: &str) -> SuspectMove {
    SuspectMove {
        block: block.parse().unwrap(),
        target: target.parse().unwrap(),
        reason: "changed".into(),
        writers: vec![Author {
            actor: "agent:a".into(),
            run: Some("a".into()),
        }],
        why: why(Cause::Landed, Some(Hash::of(b"l1"))),
    }
}
fn rec(
    resolved: Vec<TensionMove>,
    opened: Vec<TensionMove>,
    gone: Vec<SuspectMove>,
    new: Vec<SuspectMove>,
) -> Transitions {
    Transitions {
        at_ms: 1,
        resolved,
        opened,
        gone,
        new,
    }
}
/// Counts the batch markers in `[from, end)`.
fn markers(st: &Store, from: Location) -> usize {
    let mut n = 0;
    st.reader()
        .for_each_frame_from(from, st.log_end(), |_, k, _, _, _| {
            if k == kinds::BATCH_COMMIT {
                n += 1;
            }
            Ok(())
        })
        .unwrap();
    n
}

#[test]
fn ib14_a_held_batch_is_one_batch_with_one_marker_and_one_sync() {
    let (dir, mut st) = fresh();
    let start = st.log_end();
    st.set_held(true).unwrap();
    st.set_refs(vec![("t/a".into(), vec![1])]).unwrap();
    st.set_refs(vec![("t/b".into(), vec![2])]).unwrap();
    assert_eq!(st.open_batch().unwrap().len(), 2);
    assert_eq!(
        st.reader().get_ref("t/a").unwrap(),
        Some(vec![1]),
        "indexed before the seal"
    );
    assert_eq!(markers(&st, start), 0);
    let r = rec(
        vec![],
        vec![tension(b"t1", why(Cause::Committed, Some(Hash::of(b"c1"))))],
        vec![],
        vec![],
    );
    st.seal(vec![r.frame()]).unwrap();
    assert!(st.open_batch().is_none());
    assert_eq!(markers(&st, start), 1);
    assert_eq!(st.last_batch().len(), 3);
    assert_eq!(st.last_batch()[2].0, kinds::TRANSITION);
    drop(st);
    let st = reopen(&dir);
    assert_eq!(st.reader().get_ref("t/b").unwrap(), Some(vec![2]));
    assert_eq!(st.reader().open_tensions().unwrap().len(), 1);
}

#[test]
fn ib14_the_tables_fold_the_records_and_reindex_rebuilds_them() {
    let (_dir, mut st) = fresh();
    st.set_held(true).unwrap();
    let t1 = tension(b"t1", why(Cause::Committed, Some(Hash::of(b"c1"))));
    let mut t2 = tension(b"t2", why(Cause::Committed, Some(Hash::of(b"c2"))));
    t2.rekey = None;
    let mut opened = vec![t1.clone(), t2.clone()];
    opened.sort_by_key(|t| t.tension);
    st.seal(vec![
        rec(vec![], opened, vec![], vec![suspect("c-AAAAA", "c-BBBBB")]).frame(),
    ])
    .unwrap();
    let mut gone = t1.clone();
    gone.why = why(Cause::Landed, Some(Hash::of(b"l1")));
    st.seal(vec![rec(vec![gone], vec![], vec![], vec![]).frame()])
        .unwrap();
    let ids: Vec<Hash> = st
        .reader()
        .open_tensions()
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(ids, vec![t2.tension]);
    assert_eq!(st.reader().open_suspects().unwrap().len(), 1);
    let before = st.index_dump_for_tests().unwrap();
    st.reindex().unwrap();
    assert_eq!(st.index_dump_for_tests().unwrap(), before);
    assert_eq!(st.check_objects().unwrap().transitions, 2);
}

#[test]
fn ib14_two_equal_records_in_two_batches_are_both_kept() {
    let (_dir, mut st) = fresh();
    st.set_held(true).unwrap();
    let t = tension(b"t1", why(Cause::Committed, Some(Hash::of(b"c1"))));
    st.seal(vec![rec(vec![], vec![t.clone()], vec![], vec![]).frame()])
        .unwrap();
    st.seal(vec![rec(vec![t.clone()], vec![], vec![], vec![]).frame()])
        .unwrap();
    st.seal(vec![rec(vec![], vec![t], vec![], vec![]).frame()])
        .unwrap();
    assert_eq!(st.check_objects().unwrap().transitions, 3);
    assert_eq!(st.reader().open_tensions().unwrap().len(), 1);
}

#[test]
fn ib14_fsck_refuses_a_fold_that_breaks() {
    let (_dir, mut st) = fresh();
    st.set_held(true).unwrap();
    let t = tension(b"t1", why(Cause::Committed, Some(Hash::of(b"c1"))));
    st.seal(vec![rec(vec![t.clone()], vec![], vec![], vec![]).frame()])
        .unwrap();
    let e = st.check_objects().unwrap_err().to_string();
    assert!(
        e.contains(&format!(
            "tension {} resolves while it is not open",
            t.tension
        )),
        "{e}"
    );
}

#[test]
fn ib14_an_invalid_record_is_named() {
    let mut t = tension(b"t1", why(Cause::Landed, None));
    let r = rec(vec![], vec![t.clone()], vec![], vec![]);
    assert_eq!(
        r.invalid().as_deref(),
        Some("cause landed and its id do not agree")
    );
    t.why = why(Cause::Committed, Some(Hash::of(b"c")));
    t.sides[0].actor = None;
    let r = rec(vec![], vec![t], vec![], vec![]);
    assert_eq!(
        r.invalid().as_deref(),
        Some("a side is neither a change with its author nor canon")
    );
    assert_eq!(
        rec(vec![], vec![], vec![], vec![]).invalid().as_deref(),
        Some("it holds no move")
    );
}

#[test]
fn ib14_no_checkpoint_while_a_batch_is_open() {
    let (_dir, mut st) = fresh();
    st.set_held(true).unwrap();
    st.set_refs(vec![("t/a".into(), vec![1])]).unwrap();
    let e = st.checkpoint().unwrap_err();
    assert!(matches!(e, StoreError::Refused(_)), "{e:?}");
    assert!(
        !st.checkpoint_if_idle(std::time::Instant::now() + std::time::Duration::from_secs(60))
            .unwrap()
    );
    st.seal(vec![]).unwrap();
    st.checkpoint().unwrap();
}

#[test]
fn ib14_the_event_json_names_hashes_in_hex() {
    let t = tension(b"t1", why(Cause::Committed, Some(Hash::of(b"c1"))));
    let v = serde_json::to_value(&t).unwrap();
    assert_eq!(v["tension"], Hash::of(b"t1").to_string());
    assert_eq!(v["why"]["cause"], "committed");
    let back: TensionMove = postcard::from_bytes(&postcard::to_allocvec(&t).unwrap()).unwrap();
    assert_eq!(back, t);
}

/// A seal that fails after the batch's frames were indexed truncates the log and poisons the
/// store; the close must not make that index durable, so the reopen loses the whole batch and
/// opens clean.
#[test]
fn ib14_a_failed_seal_poisons_and_the_reopen_loses_the_whole_batch() {
    let (dir, mut st) = fresh();
    st.set_held(true).unwrap();
    st.set_refs(vec![("t/a".into(), vec![1])]).unwrap();
    st.fail_next_index_apply();
    assert!(st.seal(vec![]).is_err());
    assert!(
        st.set_refs(vec![("t/b".into(), vec![2])]).is_err(),
        "poisoned"
    );
    drop(st);
    let st = reopen(&dir);
    assert_eq!(st.reader().get_ref("t/a").unwrap(), None);
    st.verify().unwrap();
    st.check_objects().unwrap();
}
