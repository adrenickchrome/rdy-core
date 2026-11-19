use rdy_model::*;
use rdy_store::{Store, StoreError, kinds};

fn prov() -> Provenance {
    Provenance {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
        at_ms: 1,
    }
}

fn tag(name: &str, landing: Hash) -> Tag {
    Tag {
        name: name.into(),
        landing,
        message: Some("first cut".into()),
        tagged_by: prov(),
    }
}

#[test]
fn a_tag_round_trips_with_its_ref_in_one_write() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(dir.path()).unwrap();
    let t = tag("v1.0.0", Hash([3; 32]));
    let frames_before = s.verify().unwrap();
    let h = s.put_tag(&t).unwrap();
    assert_eq!(h, t.id());
    assert_eq!(s.get_raw(&h).unwrap().0, kinds::TAG);
    assert_eq!(s.read_tag(&h).unwrap(), t);
    assert_eq!(s.tags().unwrap(), vec![("v1.0.0".to_string(), h, t)]);
    assert_eq!(s.get_ref("tag/v1.0.0").unwrap(), Some(h.0.to_vec()));
    // the tag object and its ref update: two frames, one write
    assert_eq!(s.verify().unwrap(), frames_before + 2);
}

#[test]
fn tag_refs_survive_reindex_and_a_damaged_index() {
    let dir = tempfile::tempdir().unwrap();
    let t = tag("v1.0.0", Hash([3; 32]));
    {
        let mut s = Store::create(dir.path()).unwrap();
        s.put_tag(&t).unwrap();
        s.reindex().unwrap();
        assert_eq!(s.tags().unwrap().len(), 1);
    }
    std::fs::write(dir.path().join("index.redb"), b"not a database").unwrap();
    let s = Store::create(dir.path()).unwrap();
    assert_eq!(s.tags().unwrap()[0].2, t);
}

#[test]
fn a_failed_tag_write_leaves_no_ref() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(dir.path()).unwrap();
    s.fail_next_index_apply();
    assert!(s.put_tag(&tag("v1.0.0", Hash([3; 32]))).is_err());
    assert!(s.tags().unwrap().is_empty());
    drop(s);
    let s = Store::create(dir.path()).unwrap();
    assert!(
        s.tags().unwrap().is_empty(),
        "nothing unacknowledged survives a reopen"
    );
}

#[test]
fn reading_a_tag_checks_the_object_kind() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(dir.path()).unwrap();
    let snap = s.put(kinds::SNAPSHOT, &StateSnapshot::default()).unwrap();
    assert!(matches!(s.read_tag(&snap), Err(StoreError::Decode(_))));
    assert!(matches!(
        s.read_tag(&Hash([9; 32])),
        Err(StoreError::NotFound(_))
    ));
    s.set_refs(vec![("tag/v1.0.0".into(), b"short".to_vec())])
        .unwrap();
    assert!(matches!(s.tags(), Err(StoreError::Decode(_))));
}
