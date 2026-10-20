//! sig4: detached signatures (kind SIGNATURE, D-1Z951). A signature is one new record that names
//! its subject, a landing or a tag; the subject's bytes never change (D-F0904). The index maps each
//! subject to its signatures, derived from the frames alone, and a store format 14 project moves in
//! the dump pair (2, 14) while every released pair keeps its own kind set (D-69AY3).
use rdy_model::signature::{Signature, StatementSource, statement};
use rdy_model::*;
use rdy_store::dump::{
    DUMP_FORMAT, DumpBatch, DumpHeader, DumpItem, DumpReader, DumpWriter, Frame, READS,
    RemovalFacts, frame_kinds,
};
use rdy_store::dump_chain::{BatchMarker, DumpChainHeader};
use rdy_store::format::STORE_FORMAT;
use rdy_store::kinds::{self, batch_digest};
use rdy_store::segment::Location;
use rdy_store::{Store, StoreError, signature};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn store() -> (tempfile::TempDir, PathBuf, Store) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let s = Store::create(&path).unwrap();
    (dir, path, s)
}

fn prov(actor: &str, run: Option<&str>, role: Role) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: run.map(str::to_string),
        role,
        at_ms: 5,
    }
}

fn tag(name: &str, landing: Hash) -> Tag {
    Tag {
        name: name.into(),
        landing,
        message: None,
        tagged_by: prov("human:a", None, Role::Approver),
    }
}

fn sig(subject: Hash, namespace: &str, stmt: &[u8], blob: &[u8]) -> Signature {
    Signature {
        subject,
        namespace: namespace.into(),
        statement: stmt.to_vec(),
        sig: blob.to_vec(),
        signed_by: prov("human:a", Some("r1"), Role::Approver),
    }
}

fn enc<T: serde::Serialize>(v: &T) -> Vec<u8> {
    postcard::to_allocvec(v).unwrap()
}

/// A landing record that names no change, written raw as a batch's subject.
fn landing_record(n: u8) -> LandingRecord {
    LandingRecord {
        proposal: Hash([n; 32]),
        prev: None,
        state: Hash([n.wrapping_add(1); 32]),
        approved_by: prov("human:a", None, Role::Approver),
        changes: vec![],
        chosen: None,
        checks: vec![],
        assurance: Assurance::Local,
        relayed_by: None,
        relay_evidence: None,
        approved_version: None,
        rebased_from: vec![],
        evidence: None,
    }
}

/// A real landing: one block committed to `work`, proposed and landed.
fn land(s: &mut Store, body: &str) -> Hash {
    let w = prov("agent:w", None, Role::Writer);
    let id = format!("b-{:0>5}", body.len());
    let block = Block {
        id: BlockId::parse(&id).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    };
    let change = Change {
        view: "work".into(),
        ops: vec![Op::Create { block }],
        deps: vec![],
        provenance: w.clone(),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    };
    s.commit_to_view("work", change).unwrap();
    let p = s.propose("work", body, w).unwrap();
    s.land(&p, prov("human:a", None, Role::Approver)).unwrap()
}

fn refused(r: Result<impl std::fmt::Debug, StoreError>, want: &str) {
    match r {
        Err(StoreError::Refused(m)) => assert_eq!(m, want),
        other => panic!("expected the refusal {want:?}, got {other:?}"),
    }
}

fn not_present(subject: &Hash) -> String {
    format!("signature subject {subject} is not a present landing or tag")
}

/// Every signature `visit_signatures` streams, in its order.
fn visited(s: &Store) -> Vec<(Hash, Signature)> {
    let mut out = vec![];
    s.visit_signatures(&mut |h, g| {
        out.push((h, g.clone()));
        Ok(())
    })
    .unwrap();
    out
}

/// The batch markers written in `[from, s.log_end())`.
fn markers_since(s: &Store, from: Location) -> usize {
    let mut n = 0;
    s.for_each_frame_from(from, s.log_end(), |_, kind, _, _, _| {
        n += usize::from(kind == kinds::BATCH_COMMIT);
        Ok(())
    })
    .unwrap();
    n
}

#[test]
fn sig4_kind_eleven_adds_one_record_without_reencoding() {
    assert_eq!(kinds::SIGNATURE, 11);
    let (_d, _p, mut s) = store();
    let t = s.put_tag(&tag("v1.0.0", Hash([3; 32]))).unwrap();
    let tag_bytes = s.get_kind(kinds::TAG, &t).unwrap();
    let frames = s.verify().unwrap();
    let g = sig(t, "rdy-tag-v1", b"stmt", &[9, 8, 7]);
    let id = s.put_signature(&g).unwrap();
    assert_eq!(id, g.id());
    // one record, no other frame
    assert_eq!(s.verify().unwrap(), frames + 1);
    assert_eq!(
        s.last_batch(),
        &[(kinds::SIGNATURE, id)],
        "one SIGNATURE frame, nothing else in its batch"
    );
    let (kind, payload) = s.get_raw(&id).unwrap();
    assert_eq!(kind, 11);
    // the postcard fields in their fixed order: subject, namespace, statement, sig, signed_by
    let mut want = t.0.to_vec();
    want.push(10);
    want.extend_from_slice(b"rdy-tag-v1");
    want.push(4);
    want.extend_from_slice(b"stmt");
    want.push(3);
    want.extend_from_slice(&[9, 8, 7]);
    want.push(7);
    want.extend_from_slice(b"human:a");
    want.extend_from_slice(&[1, 2]);
    want.extend_from_slice(b"r1");
    want.push(3); // Role::Approver
    want.push(10); // at_ms 5, zigzag
    assert_eq!(payload, want);
    assert_eq!(Hash::of(&payload), id);
    assert_eq!(
        signature::frames(std::slice::from_ref(&g)).unwrap(),
        vec![(kinds::SIGNATURE, want)]
    );
    // the subject is not re-encoded: its bytes and its hash are those it was written with
    assert_eq!(s.get_kind(kinds::TAG, &t).unwrap(), tag_bytes);
    assert_eq!(Hash::of(&tag_bytes), t);
    assert_eq!(s.read_tag(&t).unwrap(), tag("v1.0.0", Hash([3; 32])));
    assert_eq!(s.signature(&id).unwrap(), g);
    assert_eq!(s.signatures_of(&t).unwrap(), vec![id]);
    // writing it again adds nothing
    assert_eq!(s.put_signature(&g).unwrap(), id);
    assert_eq!(s.verify().unwrap(), frames + 1);
}

#[test]
fn sig4_signature_index_rebuilds_from_frames() {
    let (_d, path, mut s) = store();
    let l = land(&mut s, "one");
    let t1 = s.put_tag(&tag("v1.0.0", l)).unwrap();
    let t2 = s.put_tag(&tag("v2.0.0", l)).unwrap();
    let all = [
        sig(l, "rdy-approval-v1", b"approval by key a", &[1]),
        sig(l, "rdy-relay-v1", b"relay by key b", &[2]),
        sig(l, "rdy-approval-v1", b"approval by key b", &[3]),
        sig(t1, "rdy-tag-v1", b"tag by key a", &[4]),
        sig(t1, "rdy-relay-v1", b"relay of a tag by key c", &[5]),
        sig(t2, "rdy-tag-v1", b"tag by key b", &[6]),
    ];
    // two in one batch, the rest one by one
    s.write(signature::frames(&all[..2]).unwrap()).unwrap();
    for g in &all[2..] {
        s.put_signature(g).unwrap();
    }
    let ids: Vec<Hash> = all.iter().map(Signature::id).collect();
    // per subject, in byte order of the signature hash
    let of = |subject: Hash| {
        let mut v: Vec<Hash> = all
            .iter()
            .filter(|g| g.subject == subject)
            .map(Signature::id)
            .collect();
        v.sort();
        v
    };
    assert_eq!(s.signatures_of(&l).unwrap(), of(l));
    assert_eq!(s.signatures_of(&l).unwrap().len(), 3);
    assert_eq!(s.signatures_of(&t1).unwrap(), of(t1));
    assert_eq!(s.signatures_of(&t2).unwrap(), of(t2));
    assert!(s.signatures_of(&Hash([9; 32])).unwrap().is_empty());
    // the stream: subject, then hash
    let mut want: Vec<(Hash, Hash)> = all.iter().map(|g| (g.subject, g.id())).collect();
    want.sort();
    let streamed: Vec<(Hash, Hash)> = visited(&s)
        .into_iter()
        .map(|(h, g)| {
            assert_eq!(g.id(), h);
            (g.subject, h)
        })
        .collect();
    assert_eq!(streamed, want);
    // the table: subject || hash -> empty
    let before = s.index_dump_for_tests().unwrap();
    let rows: Vec<(Vec<u8>, Vec<u8>)> = want
        .iter()
        .map(|(subject, h)| ([subject.0, h.0].concat(), vec![]))
        .collect();
    assert_eq!(before.signatures.into_iter().collect::<Vec<_>>(), rows);
    let before = s.index_dump_for_tests().unwrap();
    for h in &ids {
        assert!(
            before
                .typed
                .contains_key(&[&h.0[..], &[kinds::SIGNATURE]].concat())
        );
    }
    s.check_objects().unwrap();
    // reindex
    s.reindex().unwrap();
    assert_eq!(s.index_dump_for_tests().unwrap(), before, "reindex");
    // a missing table
    s.drop_signature_table_for_tests().unwrap();
    s.close().unwrap();
    let s = Store::open(&path).unwrap();
    assert!(s.open_report().index_rebuilt, "a missing table is rebuilt");
    assert_eq!(s.index_dump_for_tests().unwrap(), before, "missing table");
    s.close().unwrap();
    // a lost index
    std::fs::remove_file(path.join("index.redb")).unwrap();
    let s = Store::open(&path).unwrap();
    assert_eq!(s.index_dump_for_tests().unwrap(), before, "lost index");
    assert_eq!(s.signatures_of(&l).unwrap(), of(l));
    s.check_objects().unwrap();
}

#[test]
fn sig4_signatures_share_the_subject_batch() {
    let (_d, _p, mut s) = store();
    // a preceding TAG in the same write
    let t = tag("v1.0.0", Hash([3; 32]));
    let th = t.id();
    let g = sig(th, "rdy-tag-v1", b"tag", &[1]);
    let from = s.log_end();
    let writes = s.write_count();
    let mut items = vec![(kinds::TAG, enc(&t))];
    items.extend(signature::frames(std::slice::from_ref(&g)).unwrap());
    s.write(items).unwrap();
    assert_eq!(
        s.last_batch(),
        &[(kinds::TAG, th), (kinds::SIGNATURE, g.id())]
    );
    assert_eq!(s.write_count(), writes + 1);
    assert_eq!(markers_since(&s, from), 1, "the batch seals once");
    assert_eq!(s.signatures_of(&th).unwrap(), vec![g.id()]);
    // a preceding LANDING in the same write
    let l = landing_record(7);
    let lh = l.id();
    let gl = sig(lh, "rdy-approval-v1", b"approval", &[2]);
    let mut items = vec![(kinds::LANDING, enc(&l))];
    items.extend(signature::frames(std::slice::from_ref(&gl)).unwrap());
    s.write(items).unwrap();
    assert_eq!(s.signatures_of(&lh).unwrap(), vec![gl.id()]);
    // a held batch: the subject in an earlier write of the batch, one seal
    s.set_held(true).unwrap();
    let from = s.log_end();
    let t2 = tag("v2.0.0", Hash([3; 32]));
    let g2 = sig(t2.id(), "rdy-tag-v1", b"tag two", &[3]);
    s.write(vec![(kinds::TAG, enc(&t2))]).unwrap();
    s.put_signature(&g2).unwrap();
    // a missing subject in the open batch is refused, and the batch stays open
    refused(
        s.put_signature(&sig(Hash([8; 32]), "rdy-tag-v1", b"x", &[4])),
        &not_present(&Hash([8; 32])),
    );
    s.seal(vec![]).unwrap();
    s.set_held(false).unwrap();
    assert_eq!(markers_since(&s, from), 1, "the held batch seals once");
    assert_eq!(s.signatures_of(&t2.id()).unwrap(), vec![g2.id()]);
    // a subject that follows its signature in the batch
    let t3 = tag("v3.0.0", Hash([3; 32]));
    let g3 = sig(t3.id(), "rdy-tag-v1", b"tag three", &[5]);
    let end = s.log_end();
    let mut items = signature::frames(std::slice::from_ref(&g3)).unwrap();
    items.push((kinds::TAG, enc(&t3)));
    refused(s.write(items), &not_present(&t3.id()));
    assert_eq!(s.log_end(), end, "nothing of a refused batch is written");
    // a missing subject
    let ghost = Hash([7; 32]);
    refused(
        s.put_signature(&sig(ghost, "rdy-tag-v1", b"x", &[6])),
        &not_present(&ghost),
    );
    // a subject of the wrong kind: a blob, and a raw object with a tag's bytes under kind 8
    let blob = s.put_blob(b"not a subject").unwrap();
    refused(
        s.put_signature(&sig(blob, "rdy-tag-v1", b"x", &[7])),
        &not_present(&blob),
    );
    let mut items = vec![(kinds::BLOB_MANIFEST, enc(&t3))];
    items.extend(signature::frames(std::slice::from_ref(&g3)).unwrap());
    let end_blob = s.log_end();
    refused(s.write(items), &not_present(&t3.id()));
    assert_eq!(s.log_end(), end_blob);
    assert!(!s.has(&t3.id()).unwrap());
    assert!(s.signatures_of(&ghost).unwrap().is_empty());
    assert!(s.signatures_of(&blob).unwrap().is_empty());
    assert!(s.signatures_of(&t3.id()).unwrap().is_empty());
}

#[test]
fn sig4_failed_write_leaves_no_subject_or_signature() {
    let (_d, path, mut s) = store();
    let l = land(&mut s, "one");
    s.put_signature(&sig(l, "rdy-approval-v1", b"kept", &[1]))
        .unwrap();
    let snapshot = |s: &Store| {
        (
            s.log_end(),
            s.refs_with_prefix("").unwrap(),
            s.index_dump_for_tests().unwrap(),
        )
    };
    let before = snapshot(&s);
    let t = tag("v1.0.0", l);
    let g = sig(t.id(), "rdy-tag-v1", b"tag", &[2]);
    let batch = || {
        let mut items = vec![(kinds::TAG, enc(&t))];
        items.extend(signature::frames(std::slice::from_ref(&g)).unwrap());
        items
    };
    // the index fault of an ordinary write
    s.fail_next_index_apply();
    assert!(s.write(batch()).is_err());
    assert_eq!(snapshot(&s), before, "an ordinary write");
    assert!(!s.has(&t.id()).unwrap());
    assert!(s.signatures_of(&t.id()).unwrap().is_empty());
    // the seal fault of a held batch: the store is poisoned, and a reopen holds neither record
    s.set_held(true).unwrap();
    let items = batch();
    s.write(items[..1].to_vec()).unwrap();
    s.write(items[1..].to_vec()).unwrap();
    s.fail_next_index_apply();
    assert!(s.seal(vec![]).is_err());
    drop(s);
    let s = Store::open(&path).unwrap();
    assert_eq!(snapshot(&s), before, "a held batch");
    assert!(!s.has(&t.id()).unwrap());
    assert!(s.signatures_of(&t.id()).unwrap().is_empty());
    assert_eq!(s.signatures_of(&l).unwrap().len(), 1);
}

/// A dump of `s` whose header names `store_format`: the writer's verdict on its batches.
fn dump_as(s: &Store, store_format: u32) -> Result<Vec<u8>, StoreError> {
    let r = s.reader();
    let header = DumpHeader {
        store_format,
        log_end: s.log_end(),
        rdy: "test".into(),
        chain: Some(DumpChainHeader::full(
            r.repo_id()?.expect("a repo id"),
            r.batch_marker_at(s.log_end())?.expect("a batch"),
        )),
    };
    let mut w = DumpWriter::new(Vec::new(), &header)?;
    let mut failed = None;
    s.for_each_batch(s.log_end(), |b| {
        if failed.is_none()
            && let Err(e) = w.batch(&b)
        {
            failed = Some(e);
        }
        Ok(())
    })?;
    if let Some(e) = failed {
        return Err(e);
    }
    Ok(w.finish(RemovalFacts::default())?.0)
}

#[test]
fn sig4_dump_pairs_keep_their_own_kind_sets() {
    assert_eq!((DUMP_FORMAT, STORE_FORMAT), (3, 16));
    assert!(READS.contains(&(2, 14)));
    assert!(frame_kinds(2, 14).unwrap().contains(&kinds::SIGNATURE));
    for pair in [(1, 10), (1, 11), (1, 12), (2, 13)] {
        assert!(READS.contains(&pair), "{pair:?} stays readable");
        let set = frame_kinds(pair.0, pair.1).unwrap();
        assert!(!set.contains(&11), "{pair:?} refuses kind 11");
    }
    for (d, sf) in READS {
        assert!(
            !frame_kinds(*d, *sf).unwrap().contains(&12),
            "12 stays unassigned in ({d}, {sf})"
        );
    }
    // (2, 14) is (2, 13) plus 11
    let mut want = frame_kinds(2, 13).unwrap().to_vec();
    want.push(11);
    want.sort();
    assert_eq!(frame_kinds(2, 14).unwrap(), &want[..]);
    // (3, 16) keeps the signature: the kinds of (2, 15) and 23
    assert!(frame_kinds(3, 16).unwrap().contains(&kinds::SIGNATURE));
    // a store with a signature dumps as (3, 16) and reads back with its frame
    let (_d, _p, mut s) = store();
    let t = s.put_tag(&tag("v1.0.0", Hash([3; 32]))).unwrap();
    let g = sig(t, "rdy-tag-v1", b"tag", &[1]);
    let id = s.put_signature(&g).unwrap();
    let bytes = dump_as(&s, STORE_FORMAT).unwrap();
    let (mut r, format, h) = DumpReader::open(&bytes[..]).unwrap();
    assert_eq!((format, h.store_format), (3, 16));
    let mut found = false;
    while let Some(item) = r.next().unwrap() {
        if let DumpItem::Batch(b) = item {
            found |= b
                .frames
                .iter()
                .any(|f| f.kind() == kinds::SIGNATURE && *f.hash() == id);
        }
    }
    assert!(found, "the dump carries the SIGNATURE frame");
    // no writer of the (2, 13) set exists any more: its header is refused before a byte is
    // written, and the reader's (2, 13) set above still refuses kind 11
    refused(
        dump_as(&s, 13),
        "store format 13 in a dump of format 3 is not one this rdy reads",
    );
    // a hand-made batch with kind 11 under an old header is refused by the writer too
    let payload = signature::frames(std::slice::from_ref(&g))
        .unwrap()
        .remove(0)
        .1;
    let frame = Frame::Present {
        kind: kinds::SIGNATURE,
        hash: Hash::of(&payload),
        payload,
    };
    let b = DumpBatch {
        start: Location {
            segment: 0,
            offset: 0,
        },
        digest: batch_digest([frame.hash()]),
        frames: vec![frame],
    };
    let old = DumpHeader {
        store_format: 13,
        log_end: s.log_end(),
        rdy: "test".into(),
        chain: Some(DumpChainHeader::full(RepoId([1; 16]), BatchMarker::of(&b))),
    };
    refused(
        DumpWriter::new(Vec::new(), &old).map(|_| ()),
        "store format 13 in a dump of format 3 is not one this rdy reads",
    );
    // the current writer takes the same batch: (3, 16) assigns kind 11
    let current = DumpHeader {
        store_format: STORE_FORMAT,
        ..old
    };
    DumpWriter::new(Vec::new(), &current)
        .unwrap()
        .batch(&b)
        .unwrap();
}

/// The log end and frame count of `s`, which a refused write leaves as they were.
fn unchanged(s: &Store) -> (Location, usize) {
    (s.log_end(), s.verify().unwrap())
}

#[test]
fn sig4_invalid_record_lengths_and_subjects_fail_closed() {
    let (_d, _p, mut s) = store();
    let t = s.put_tag(&tag("v1.0.0", Hash([3; 32]))).unwrap();
    let before = unchanged(&s);
    // an oversized encoded payload, before it is decoded
    refused(
        s.write(vec![(kinds::SIGNATURE, vec![0; 65537])]),
        "signing input exceeds 65536 bytes",
    );
    assert_eq!(unchanged(&s), before);
    // each field over its bound, through the batch frames and the detached write
    let over = |g: Signature, limit: &str| {
        let want = format!("signing input exceeds {limit}");
        refused(signature::frames(std::slice::from_ref(&g)), &want);
        (g, want)
    };
    for (g, want) in [
        over(sig(t, &"n".repeat(65), b"x", &[1]), "64 bytes"),
        over(sig(t, "rdy-tag-v1", &[b'x'; 16385], &[1]), "16384 bytes"),
        over(sig(t, "rdy-tag-v1", b"x", &[1; 16385]), "16384 bytes"),
        over(
            Signature {
                signed_by: prov(&"a".repeat(4097), None, Role::Approver),
                ..sig(t, "rdy-tag-v1", b"x", &[1])
            },
            "4096 bytes",
        ),
        over(
            Signature {
                signed_by: prov("human:a", Some(&"r".repeat(4097)), Role::Approver),
                ..sig(t, "rdy-tag-v1", b"x", &[1])
            },
            "4096 bytes",
        ),
    ] {
        refused(s.put_signature(&g), &want);
        refused(s.write(vec![(kinds::SIGNATURE, enc(&g))]), &want);
        assert_eq!(unchanged(&s), before);
    }
    // every truncation of a valid record, and a length prefix past the record
    let good = enc(&sig(t, "rdy-tag-v1", b"statement", &[1, 2, 3]));
    for n in 0..good.len() {
        let r = s.write(vec![(kinds::SIGNATURE, good[..n].to_vec())]);
        assert!(r.is_err(), "a record cut at {n} bytes was taken");
        assert_eq!(unchanged(&s), before, "cut at {n}");
    }
    let mut long = t.0.to_vec();
    long.extend_from_slice(&[0xff, 0xff, 0x03]); // a namespace of 65535 bytes
    long.extend_from_slice(b"rdy");
    assert!(s.write(vec![(kinds::SIGNATURE, long)]).is_err());
    let mut past = t.0.to_vec();
    past.extend_from_slice(&[10, b'r']); // ten bytes promised, one given
    assert!(s.write(vec![(kinds::SIGNATURE, past)]).is_err());
    let mut trailing = good.clone();
    trailing.push(0);
    assert!(s.write(vec![(kinds::SIGNATURE, trailing)]).is_err());
    assert_eq!(unchanged(&s), before);
    // a key binding is never stored: by its namespace, or by its statement's kind
    let key = format!("SHA256:{}", "A".repeat(43));
    let kb = statement(
        StatementSource::Keybind {
            account: "acct",
            challenge: "c1",
        },
        None,
        &key,
    )
    .unwrap();
    for g in [
        sig(t, "rdy-keybind-v1", b"x", &[1]),
        sig(t, "rdy-tag-v1", &kb, &[1]),
    ] {
        refused(
            signature::frames(std::slice::from_ref(&g)),
            "key binding signatures are not stored",
        );
        refused(s.put_signature(&g), "key binding signatures are not stored");
        refused(
            s.write(vec![(kinds::SIGNATURE, enc(&g))]),
            "key binding signatures are not stored",
        );
    }
    assert_eq!(unchanged(&s), before);
    // an absent subject
    let ghost = Hash([7; 32]);
    refused(
        s.put_signature(&sig(ghost, "rdy-tag-v1", b"x", &[1])),
        &not_present(&ghost),
    );
    assert_eq!(unchanged(&s), before);
    assert!(s.signatures_of(&t).unwrap().is_empty());
    // a read never returns more than 4096 signatures of one subject; the stream reads them all
    let many: Vec<Signature> = (0..4097u32)
        .map(|i| sig(t, "rdy-tag-v1", format!("s{i}").as_bytes(), &[1]))
        .collect();
    s.write(signature::frames(&many[..4096]).unwrap()).unwrap();
    assert_eq!(s.signatures_of(&t).unwrap().len(), 4096);
    s.put_signature(&many[4096]).unwrap();
    refused(
        s.signatures_of(&t),
        "signing input exceeds 4096 signatures per subject",
    );
    let mut n = 0;
    s.visit_signatures(&mut |_, _| {
        n += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(n, 4097);
}

#[test]
fn sig4_a_missing_signature_table_never_means_unsigned() {
    let (_d, path, mut s) = store();
    let l = land(&mut s, "one");
    let t = s.put_tag(&tag("v1.0.0", l)).unwrap();
    let gl = s
        .put_signature(&sig(l, "rdy-approval-v1", b"approval", &[1]))
        .unwrap();
    let gt = s
        .put_signature(&sig(t, "rdy-tag-v1", b"tag", &[2]))
        .unwrap();
    s.drop_signature_table_for_tests().unwrap();
    // while it is gone, a read fails: it never answers "no signatures"
    assert!(s.signatures_of(&l).is_err());
    assert!(s.visit_signatures(&mut |_, _| Ok(())).is_err());
    s.close().unwrap();
    let s = Store::open(&path).unwrap();
    assert!(s.open_report().index_rebuilt);
    assert_eq!(s.signatures_of(&l).unwrap(), vec![gl]);
    assert_eq!(s.signatures_of(&t).unwrap(), vec![gt]);
    let mut seen: Vec<Hash> = visited(&s).into_iter().map(|(h, _)| h).collect();
    seen.sort();
    let mut want = vec![gl, gt];
    want.sort();
    assert_eq!(seen, want);
    // the index file set aside is kept, never deleted
    assert!(set_aside_exists(&path));
}

fn set_aside_exists(path: &Path) -> bool {
    std::fs::read_dir(path).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("index.redb.damaged-")
    })
}
