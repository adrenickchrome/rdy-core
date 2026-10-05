//! fsck and reindex for the new kinds (plan M2a1, fsck2).
use rdy_model::*;
use rdy_store::{Store, StoreError};
use std::collections::BTreeMap;

fn block(id: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: "x".into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
    }
}

fn prov(role: Role) -> Provenance {
    Provenance {
        actor: "someone".into(),
        run: None,
        role,
        at_ms: 1,
    }
}

fn create(view: &str, id: &str) -> Change {
    Change {
        view: view.into(),
        ops: vec![Op::Create { block: block(id) }],
        deps: vec![],
        provenance: prov(Role::Writer),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
    }
}

fn busy(s: &mut Store) {
    for (v, id) in [("a", "b-00001"), ("a", "b-00002"), ("b", "b-00003")] {
        s.commit_to_view(v, create(v, id)).unwrap();
    }
    let p = s.propose("a", "a", prov(Role::Writer)).unwrap();
    s.land(&p, prov(Role::Approver)).unwrap();
    s.commit_to_view("a", create("a", "b-00004")).unwrap();
}

#[test]
fn fsck2_a_healthy_store_checks_clean_and_counts() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(&dir.path().join("s")).unwrap();
    busy(&mut s);
    let c = s.check_objects().unwrap();
    assert_eq!((c.views, c.landed_changes), (2, 2));
    assert_eq!(
        c.view_ops, 7,
        "a: create, 2 commits, rebase, commit; b: create, commit"
    );
}

#[test]
fn fsck2_reindex_reproduces_every_table() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(&dir.path().join("s")).unwrap();
    busy(&mut s);
    let before = s.index_dump_for_tests().unwrap();
    s.reindex().unwrap();
    assert_eq!(s.index_dump_for_tests().unwrap(), before);
}

#[test]
fn fsck2_a_head_whose_count_is_wrong_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(&dir.path().join("s")).unwrap();
    busy(&mut s);
    let mut h = s.view_head("b").unwrap().unwrap();
    h.len = 9;
    s.set_refs(vec![("view/b".into(), postcard::to_allocvec(&h).unwrap())])
        .unwrap();
    match s.check_objects() {
        Err(StoreError::Decode(m)) => {
            assert_eq!(m, "view \"b\": the head says 9 changes, the chain gives 1")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn fsck2_a_chain_that_starts_with_a_commit_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(&dir.path().join("s")).unwrap();
    busy(&mut s);
    let h = s.view_head("b").unwrap().unwrap();
    let commit = s.view_op(&h.head).unwrap();
    let orphan = rdy_store::ViewOp {
        prev: None,
        ..commit
    };
    let oid = s
        .write(vec![(
            rdy_store::kinds::VIEW_OP,
            postcard::to_allocvec(&orphan).unwrap(),
        )])
        .unwrap()[0];
    let head = rdy_store::ViewHead { head: oid, ..h };
    s.set_refs(vec![(
        "view/b".into(),
        postcard::to_allocvec(&head).unwrap(),
    )])
    .unwrap();
    match s.check_objects() {
        Err(StoreError::Decode(m)) => assert!(
            m.starts_with("view \"b\": its chain starts with Commit"),
            "{m}"
        ),
        other => panic!("{other:?}"),
    }
}

/// Review Focus 6: a canon landing that carries every post-spec field (none of which M2 writes)
/// checks clean, and survives a reindex unchanged.
#[test]
fn fsck2_a_landing_with_every_new_field_set_checks_clean() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(&dir.path().join("s")).unwrap();
    busy(&mut s);
    let l = s.canon_head().unwrap().unwrap();
    let rec = s.landing(&l).unwrap();
    let relayed = LandingRecord {
        proposal: rec.proposal,
        prev: Some(l),
        state: rec.state,
        approved_by: rec.approved_by.clone(),
        changes: vec![],
        chosen: None,
        checks: vec![],
        assurance: Assurance::Relayed,
        relayed_by: Some(ActorRun {
            actor: "agent:relay".into(),
            run: Some("run-1".into()),
        }),
        relay_evidence: Some(RelayEvidence {
            quoted: "land it".into(),
            session_ref: "sess-1".into(),
            question: None,
        }),
        approved_version: Some(rec.proposal),
        rebased_from: vec![l],
        evidence: None,
    };
    let lid = s
        .write(vec![(
            rdy_store::kinds::LANDING,
            postcard::to_allocvec(&relayed).unwrap(),
        )])
        .unwrap()[0];
    s.set_refs(vec![("canon".into(), lid.0.to_vec())]).unwrap();
    assert_eq!(s.check_objects().unwrap().landed_changes, 2);
    let before = s.index_dump_for_tests().unwrap();
    s.reindex().unwrap();
    assert_eq!(s.index_dump_for_tests().unwrap(), before);
    assert_eq!(s.landing(&lid).unwrap(), relayed);
    s.check_objects().unwrap();
}
