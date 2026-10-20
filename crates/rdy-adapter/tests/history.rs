//! ha10 (D-D2NPN, D-DAGNB, D-WDKWQ): recorded landing facts, tag histories, signatures and
//! object-key presence through the adapter. A consumer names every type from `rdy_adapter`; the
//! project is built with core calls in a closed store first. Reads that need a write while a
//! snapshot is live are in the crate's own unit tests.

use rdy_adapter::history::{
    BlockOp, BlockWrite, LandingFacts, ObjectKey, ObjectPresence, Signature, SignatureFact,
};
use rdy_adapter::{BlockId, Hash, RdyError, StoreHandle};
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest};
use rdy_model::Role;
use rdy_store::kinds;
use serde_json::{Value, json};

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

fn write(r: &mut Rdy, ops: Value) -> rdy_engine::WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "ha10".into(),
        },
    )
    .unwrap()
}

struct Fixture {
    dir: tempfile::TempDir,
    landing: Hash,
    tag: Hash,
    orphan: Hash,
    doc: BlockId,
    signatures: Vec<Hash>,
    tag_signature: Hash,
}

fn signature(subject: Hash, statement: &[u8], at_ms: i64) -> Signature {
    Signature {
        subject,
        namespace: "rdy-approval-v1".into(),
        statement: statement.to_vec(),
        sig: b"not checked here".to_vec(),
        signed_by: rdy_model::Provenance {
            actor: "agent:s".into(),
            run: None,
            role: Role::Approver,
            at_ms,
        },
    }
}

/// A closed project: one landing tagged v1.0.0, an object nothing refers to, and signatures of
/// the landing (one whose statement does not parse) and of the tag.
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([{"op": "create", "type": "doc", "id": "$d", "fields": {"name": "a", "title": "A", "doc_type": "page"}}]),
    );
    let p = r
        .propose(&session(Role::Writer), "work", "first", None)
        .unwrap();
    let landing: Hash = r
        .land(&session(Role::Approver), &p.id)
        .unwrap()
        .parse()
        .unwrap();
    r.tag(&session(Role::Approver), "v1.0.0", None, None)
        .unwrap();
    let tag = r.store().tags().unwrap()[0].1;
    let orphan = rdy_model::Block {
        id: BlockId::parse("d-00009").unwrap(),
        block_type: "doc".into(),
        parent: None,
        order: "a0".into(),
        fields: Default::default(),
        body: "nobody points here".into(),
        links: Default::default(),
        pins: Default::default(),
        read_identity: None,
    };
    let orphan = r.store_mut().put(kinds::BLOCK, &orphan).unwrap();
    let mut signatures = vec![
        r.store_mut()
            .put_signature(&signature(landing, b"x", 1))
            .unwrap(),
        r.store_mut()
            .put_signature(&signature(landing, b"y", 2))
            .unwrap(),
    ];
    signatures.sort();
    let tag_signature = r
        .store_mut()
        .put_signature(&signature(tag, b"z", 3))
        .unwrap();
    r.seal_open().unwrap();
    r.close().unwrap();
    Fixture {
        dir,
        landing,
        tag,
        orphan,
        doc: BlockId::parse(&w.created["$d"]).unwrap(),
        signatures,
        tag_signature,
    }
}

fn key(kind: u8, hash: Hash) -> ObjectKey {
    ObjectKey { kind, hash }
}

#[test]
fn ha10_a_consumer_names_every_fact_type_from_the_adapter() {
    let f = fixture();
    let handle = StoreHandle::open(f.dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let facts: LandingFacts = snap.landing_facts(f.landing).unwrap();
    assert_eq!(facts.hash, f.landing);
    assert_eq!(facts.record.approved_by.actor, "human:founder");
    let writes: &Vec<BlockWrite> = &facts.blocks;
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].id, f.doc);
    assert_eq!(writes[0].op, BlockOp::Create);
    assert!(writes[0].base.is_none() && writes[0].version.is_some() && !writes[0].redacted);
    // the landing's signatures: hash order, statements that do not parse have no kind
    let sigs: &Vec<SignatureFact> = &facts.signatures;
    assert_eq!(
        sigs.iter().map(|s| s.hash).collect::<Vec<_>>(),
        f.signatures
    );
    assert!(sigs.iter().all(|s| s.kind.is_none()));
    assert_eq!(sigs[0].record.subject, f.landing);
    // the tag's own signatures, apart from the landing's
    let tagged = snap.tag_signatures("1.0.0").unwrap();
    assert_eq!(tagged.len(), 1);
    assert_eq!(tagged[0].hash, f.tag_signature);
    assert_eq!(tagged[0].record.subject, f.tag);
    let e: RdyError = snap.tag_signatures("v9.9.9").unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND");
    // the same facts through the chain reads
    assert_eq!(snap.canon_chain().unwrap(), std::slice::from_ref(&facts));
    assert_eq!(snap.tag_history("v1.0.0").unwrap(), [facts]);
    handle.close().unwrap();
}

#[test]
fn ha10_presence_accepts_keys_and_counts_unreferenced_objects() {
    let f = fixture();
    let handle = StoreHandle::open(f.dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let facts = snap.landing_facts(f.landing).unwrap();
    let doc_version = facts.blocks[0].version.unwrap();
    let want = [
        key(kinds::LANDING, f.landing),
        key(kinds::TAG, f.tag),
        key(kinds::BLOCK, doc_version),
        key(kinds::BLOCK, f.orphan),
        key(kinds::SIGNATURE, f.signatures[0]),
        key(kinds::SIGNATURE, f.tag_signature),
    ];
    let absent = key(kinds::BLOCK, Hash([9; 32]));
    // unsorted, repeated, and with an absent key
    let mut input = vec![absent, want[3], want[0], want[5], want[1], want[0]];
    input.extend([want[2], want[4]]);
    let got: Vec<ObjectPresence> = snap.has_objects(&input).unwrap();
    let mut keys: Vec<ObjectKey> = want.to_vec();
    keys.push(absent);
    keys.sort();
    assert_eq!(got.iter().map(|p| p.key).collect::<Vec<_>>(), keys);
    for p in &got {
        assert_eq!(p.present, p.key != absent, "{}", p.key);
        assert!(!p.redacted);
    }
    // an unreferenced object and a signature (kind 11) count like any other stored object
    assert!(got.iter().any(|p| p.key == want[3] && p.present));
    assert!(got.iter().any(|p| p.key == want[4] && p.present));
    assert!(snap.has_objects(&[]).unwrap().is_empty());
    handle.close().unwrap();
}

/// The hashes of the store's local records: ref updates, batch markers and transitions.
fn local_hashes(project: &std::path::Path) -> Vec<Hash> {
    let r = Rdy::open(project).unwrap();
    let mut local = vec![];
    r.store()
        .for_each_frame_from(
            rdy_store::segment::Location {
                segment: 0,
                offset: 0,
            },
            r.store().log_end(),
            |_, kind, hash, _, _| {
                if [kinds::REF_UPDATE, kinds::BATCH_COMMIT, kinds::TRANSITION].contains(&kind) {
                    local.push(hash);
                }
                Ok(())
            },
        )
        .unwrap();
    r.close().unwrap();
    local
}

#[test]
fn ha10_same_hash_with_another_kind_is_absent() {
    let f = fixture();
    let local = local_hashes(f.dir.path());
    assert!(!local.is_empty());
    let handle = StoreHandle::open(f.dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let got = snap
        .has_objects(&[
            key(kinds::LANDING, f.landing),
            key(kinds::TAG, f.landing),
            key(kinds::CHANGE, f.landing),
            key(kinds::BLOCK, f.landing),
            key(kinds::SIGNATURE, f.tag),
            key(kinds::TAG, f.tag),
        ])
        .unwrap();
    for p in got {
        let exact = p.key == key(kinds::LANDING, f.landing) || p.key == key(kinds::TAG, f.tag);
        assert_eq!(p.present, exact, "{}", p.key);
    }
    // the hash of a local record is no object under any portable kind
    for hash in local {
        let keys: Vec<ObjectKey> = rdy_store::sync_stream::SYNC_KINDS
            .iter()
            .map(|k| key(*k, hash))
            .collect();
        assert!(snap.has_objects(&keys).unwrap().iter().all(|p| !p.present));
    }
    handle.close().unwrap();
}

/// A closed project with three landings, oldest first; the first is tagged v1.0.0.
fn three_landings() -> (tempfile::TempDir, [Hash; 3]) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let mut out = [Hash([0; 32]); 3];
    for (i, title) in ["one", "two", "three"].into_iter().enumerate() {
        write(
            &mut r,
            json!([{"op": "create", "type": "doc", "fields": {"title": title, "doc_type": "page"}}]),
        );
        let p = r
            .propose(&session(Role::Writer), "work", title, None)
            .unwrap();
        out[i] = r
            .land(&session(Role::Approver), &p.id)
            .unwrap()
            .parse()
            .unwrap();
        if i == 0 {
            r.tag(&session(Role::Approver), "v1.0.0", None, None)
                .unwrap();
        }
    }
    r.close().unwrap();
    (dir, out)
}

#[test]
fn sdc2_batch_equals_per_hash_results() {
    let (dir, [a, b, c]) = three_landings();
    let handle = StoreHandle::open(dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let hashes = |got: &[LandingFacts]| got.iter().map(|f| f.hash).collect::<Vec<_>>();
    // repeats collapse to the first occurrence; neither order is a sort by hash
    assert_eq!(hashes(&snap.landings(&[b, a, b]).unwrap()), [b, a]);
    assert_eq!(hashes(&snap.landings(&[a, b, a]).unwrap()), [a, b]);
    // unsorted with repeats: one fact per distinct hash, each equal to the single read
    let input = [c, a, b, a, c, b];
    let got = snap.landings(&input).unwrap();
    assert_eq!(hashes(&got), [c, a, b]);
    for f in &got {
        assert_eq!(f, &snap.landing_facts(f.hash).unwrap());
    }
    // the handle captures its own snapshot and answers the same
    assert_eq!(handle.landings(&input).unwrap(), got);
    assert!(snap.landings(&[]).unwrap().is_empty());
    handle.close().unwrap();
}
