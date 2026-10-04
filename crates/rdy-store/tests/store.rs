use rdy_model::*;
use rdy_store::{kinds, Store, StoreError};
use std::collections::BTreeMap;

fn block(id: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
    }
}

fn change(ops: Vec<Op>) -> Change {
    Change {
        view: "work".into(),
        ops,
        deps: vec![],
        provenance: Provenance { actor: "agent:a".into(), run: None, role: Role::Writer, at_ms: 1 },
        justification: None,
        message: String::new(),
    }
}

#[test]
fn second_open_is_locked() {
    let dir = tempfile::tempdir().unwrap();
    let _s = Store::open(dir.path()).unwrap();
    assert!(matches!(Store::open(dir.path()), Err(StoreError::Locked(_))));
}

#[test]
fn objects_persist_and_dedupe() {
    let dir = tempfile::tempdir().unwrap();
    let b = block("b-AAAAA", "x");
    let h = {
        let mut s = Store::open(dir.path()).unwrap();
        let h1 = s.put(kinds::BLOCK, &b).unwrap();
        let h2 = s.put(kinds::BLOCK, &b).unwrap();
        assert_eq!(h1, h2);
        assert_eq!(h1, b.version_hash());
        h1
    };
    let s = Store::open(dir.path()).unwrap();
    assert_eq!(s.get_block(&h).unwrap(), b);
    assert!(matches!(s.get::<Block>(&Hash::of(b"missing")), Err(StoreError::NotFound(_))));
}

#[test]
fn blobs_chunk_and_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    let data: Vec<u8> = (0..(5 * 1024 * 1024 / 2)).map(|i| (i % 251) as u8).collect();
    let h = s.put_blob(&data).unwrap();
    assert_eq!(s.get_blob(&h).unwrap(), data);
    assert_eq!(s.put_blob(&data).unwrap(), h);
}

#[test]
fn commit_change_computes_deps_from_producers() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    let v1 = block("b-AAAAA", "one");
    let c1 = s.commit_change(change(vec![Op::Create { block: v1.clone() }]), vec![]).unwrap();
    let v2 = block("b-AAAAA", "two");
    let c2 = s.commit_change(change(vec![Op::Update { base: v1.version_hash(), block: v2.clone() }]), vec![]).unwrap();
    let stored: Change = s.get(&c2).unwrap();
    assert_eq!(stored.deps, vec![c1]);
    assert_eq!(s.get_block(&v2.version_hash()).unwrap(), v2);
    let bad = change(vec![Op::Update { base: Hash::of(b"nope"), block: block("b-AAAAA", "x") }]);
    assert!(matches!(s.commit_change(bad, vec![]), Err(StoreError::NotFound(_))));
}

#[test]
fn reindex_rebuilds_everything_including_refs() {
    let dir = tempfile::tempdir().unwrap();
    let v1 = block("b-AAAAA", "one");
    {
        let mut s = Store::open(dir.path()).unwrap();
        s.commit_change(change(vec![Op::Create { block: v1.clone() }]), vec![("view/work".into(), vec![1])]).unwrap();
        s.set_refs(vec![("canon".into(), b"A".to_vec())]).unwrap();
        s.set_refs(vec![("canon".into(), b"B".to_vec())]).unwrap();
        s.set_refs(vec![("canon".into(), b"A".to_vec())]).unwrap(); // same value again
    }
    std::fs::remove_file(dir.path().join("index.redb")).unwrap();
    let s = Store::open(dir.path()).unwrap();
    assert_eq!(s.get_ref("canon").unwrap(), Some(b"A".to_vec()));
    assert_eq!(s.get_ref("view/work").unwrap(), Some(vec![1]));
    assert_eq!(s.get_block(&v1.version_hash()).unwrap(), v1);
    let mut s = s;
    s.reindex().unwrap();
    assert_eq!(s.get_ref("canon").unwrap(), Some(b"A".to_vec()));
}
