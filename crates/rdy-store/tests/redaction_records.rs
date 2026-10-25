//! Redaction requests, residues and derived lookups (plan Redaction, Task 2; D-600CW, D-3M81W).
#[path = "support/redaction.rs"]
mod redaction_fixture;

use rdy_model::redaction::{
    CARRIER_MESSAGE, CONTENT_REMOVED, ChangeResidue, DropSpan, FramePlace, RedactTarget,
    RedactedBlockIdentity, RedactionResidue,
};
use rdy_model::*;
use rdy_store::kinds::{self, BLOB_CHUNK, BLOB_MANIFEST, BLOCK, CHANGE};
use rdy_store::redaction::{REMOVABLE_KINDS, StoredFrame, is_removable};
use rdy_store::segment::{Location, SegmentLog};
use rdy_store::transition::Transitions;
use rdy_store::{Store, StoreError};
use redaction_fixture::{
    Draft, FixtureOptions, ORIGIN, Proof, RequestSpec, authorized_batch_fixture, build_request,
    enc, relink, residue_frame, residue_id, segment_bytes,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const MIB: usize = 1024 * 1024;

fn block(id: &str, ty: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: ty.into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn writer() -> Provenance {
    Provenance {
        actor: "agent:w".into(),
        run: Some("w1".into()),
        role: Role::Writer,
        at_ms: 10,
    }
}

fn change(view: &str, ops: Vec<Op>) -> Change {
    Change {
        view: view.into(),
        ops,
        deps: vec![],
        provenance: writer(),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

fn version(b: &Block) -> RedactTarget {
    RedactTarget::Version {
        id: b.id.clone(),
        version: b.version_hash(),
    }
}

fn place(loc: Location) -> FramePlace {
    FramePlace {
        segment: loc.segment,
        offset: loc.offset,
    }
}

/// The refusal of removed content, naming `req`.
fn removed<T>(req: Hash, r: Result<T, StoreError>) -> bool {
    matches!(r, Err(StoreError::Redacted { request }) if request == req)
}

/// The refusal of a read projection offered to a writer.
fn projected<T>(r: Result<T, StoreError>) -> bool {
    matches!(r, Err(StoreError::ProjectedWrite))
}

/// The corrupt-span refusal at `at`.
fn corrupt_at(e: &StoreError, at: FramePlace) -> bool {
    matches!(e, StoreError::RedactionCorrupt { segment, offset }
        if *segment == at.segment && *offset == at.offset)
}

/// Appends `frames` raw to the closed store at `path` as one marked batch, as a dump of an old
/// store can hold them.
fn append_raw(path: &Path, frames: &[(u8, Vec<u8>)]) {
    let (mut log, _) =
        SegmentLog::open_marked(&path.join("segments"), 64 * 1024 * 1024, None).unwrap();
    log.append_batch(frames).unwrap();
    log.sync().unwrap();
}

/// Every frame of `s`'s log: (location, kind, hash).
fn raw_frames(s: &Store) -> Vec<(Location, u8, Hash)> {
    let mut out = vec![];
    s.for_each_frame_from(ORIGIN, s.log_end(), |loc, kind, hash, _, _| {
        out.push((loc, kind, hash));
        Ok(())
    })
    .unwrap();
    out
}

/// A three-op change: a create of A, an update of B and a delete of C, after a change that
/// created B and C. Returns (store path's store, a, b0, b1, c0, the change id, the base change id).
struct ThreeOps {
    a: Block,
    b0: Block,
    b1: Block,
    c0: Block,
    change: Hash,
    base: Hash,
}

fn three_ops(s: &mut Store) -> ThreeOps {
    let b0 = block("b-00002", "claim", "bravo before");
    let c0 = block("b-00003", "claim", "charlie");
    let base = s
        .commit_to_view(
            "work",
            change(
                "work",
                vec![
                    Op::Create { block: b0.clone() },
                    Op::Create { block: c0.clone() },
                ],
            ),
        )
        .unwrap();
    let a = block("b-00001", "claim", "canary alpha secret");
    let b1 = block("b-00002", "claim", "bravo after");
    let mut three = change(
        "work",
        vec![
            Op::Create { block: a.clone() },
            Op::Update {
                base: b0.version_hash(),
                block: b1.clone(),
            },
            Op::Delete {
                base: c0.version_hash(),
                id: c0.id.clone(),
            },
        ],
    );
    three.justification = Some(Justification::Editorial {
        class: EditorialClass::Clarify,
        reason: "tidy the claims".into(),
    });
    three.message = "rewrite the claims".into();
    three.links.client_nonce = Some("nonce-1".into());
    let change = s.commit_to_view("work", three).unwrap();
    ThreeOps {
        a,
        b0,
        b1,
        c0,
        change,
        base,
    }
}

#[test]
fn rdx2_a_request_is_one_batch_and_removes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let a = block("b-00001", "claim", "canary alpha secret");
    let c1 = s
        .commit_to_view(
            "work",
            change("work", vec![Op::Create { block: a.clone() }]),
        )
        .unwrap();
    let original_change = s.change(&c1).unwrap();
    let d = build_request(&s, &RequestSpec::new("work", vec![version(&a)]));
    assert_eq!(d.residues.len(), 2, "the block frame's and the change's");
    let before = segment_bytes(&path);
    let (end, writes) = (s.log_end(), s.write_count());

    let prepared = s
        .prepare_redaction(&d.carrier, &d.request, &d.residues)
        .unwrap();
    assert_eq!(s.log_end(), end, "preparation writes nothing");
    assert_eq!(s.write_count(), writes);
    assert_eq!(prepared.request(), d.request_id());
    assert_eq!(prepared.carrier(), d.carrier_id());

    let req = s.commit_redaction(prepared).unwrap();
    assert_eq!(req, d.request_id());
    assert_eq!(s.write_count(), writes + 1, "one write");
    let batch: Vec<u8> = s.last_batch().iter().map(|(k, _)| *k).collect();
    assert_eq!(
        batch,
        vec![
            residue_frame(&d.residues[0]).0,
            residue_frame(&d.residues[1]).0,
            CHANGE,
            kinds::REDACTION,
            kinds::VIEW_OP,
            kinds::REF_UPDATE,
        ],
        "residues, carrier, request and the view commit in one batch"
    );
    let residue_ids: Vec<Hash> = d.residues.iter().map(residue_id).collect();
    assert_eq!(s.last_batch()[0].1, residue_ids[0]);
    assert_eq!(s.last_batch()[1].1, residue_ids[1]);
    assert_eq!(s.last_batch()[2].1, d.carrier_id());
    assert_eq!(s.last_batch()[3].1, req);
    assert!(s.included("work").unwrap().contains(&d.carrier_id()));
    let carrier = s.change(&d.carrier_id()).unwrap();
    assert!(carrier.ops.is_empty());
    assert_eq!(carrier.message, CARRIER_MESSAGE);
    assert_eq!(s.redaction_for_carrier(&d.carrier_id()).unwrap(), Some(req));
    assert_eq!(s.redaction_request(&req).unwrap(), d.request);

    // nothing is removed: no active mask, every read and every byte as before
    assert_eq!(s.redaction_of(BLOCK, &a.version_hash()).unwrap(), None);
    assert_eq!(s.redaction_of(CHANGE, &c1).unwrap(), None);
    assert_eq!(s.get_block(&a.version_hash()).unwrap(), a);
    assert_eq!(s.change(&c1).unwrap(), original_change);
    s.get_raw(&a.version_hash()).unwrap();
    let after = segment_bytes(&path);
    for (name, bytes) in &before {
        assert!(after[name].starts_with(bytes), "{name} kept every byte");
    }
    s.verify().unwrap();
    s.check_objects().unwrap();
}

#[test]
fn rdx2_replay_finds_the_request_without_the_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let a = block("b-00001", "claim", "canary alpha");
    let b = block("b-00002", "claim", "canary bravo");
    s.commit_to_view(
        "hold",
        change("hold", vec![Op::Create { block: a.clone() }]),
    )
    .unwrap();
    s.commit_to_view(
        "work",
        change("work", vec![Op::Create { block: b.clone() }]),
    )
    .unwrap();
    // a recorded request that is not applied, and one the fixture applies
    let held = build_request(&s, &RequestSpec::new("hold", vec![version(&a)]));
    let p = s
        .prepare_redaction(&held.carrier, &held.request, &held.residues)
        .unwrap();
    let held_req = s.commit_redaction(p).unwrap();
    drop(s);
    let fx = authorized_batch_fixture(
        &path,
        &[RequestSpec::new("work", vec![version(&b)])],
        &FixtureOptions::approved(Proof::relayed_human()),
    );
    let applied = &fx.requests[0];

    let relations = |s: &Store| {
        (
            s.redaction_for_carrier(&held.carrier_id()).unwrap(),
            s.redaction_for_carrier(&applied.carrier).unwrap(),
            s.redaction_request(&held_req).unwrap(),
            s.redaction_request(&applied.request).unwrap(),
            s.redaction_of(BLOCK, &a.version_hash()).unwrap(),
            s.redaction_of(BLOCK, &b.version_hash()).unwrap(),
            s.get_block(&b.version_hash()).unwrap(),
        )
    };
    let mut s = Store::open(&path).unwrap();
    let live = relations(&s);
    assert_eq!(live.0, Some(held_req));
    assert_eq!(live.1, Some(applied.request));
    assert_eq!(live.2, held.request);
    assert_eq!(live.3, applied.draft.request);
    assert_eq!(live.4, None, "a request without an apply masks nothing");
    assert_eq!(live.5, Some(applied.request));
    assert!(live.6.is_redacted());
    let tables = s.index_dump_for_tests().unwrap();
    s.reindex().unwrap();
    assert_eq!(s.index_dump_for_tests().unwrap(), tables);
    assert_eq!(relations(&s), live);
    drop(s);

    // delete the derived tables: the open rebuilds them from the log
    std::fs::remove_file(path.join("index.redb")).unwrap();
    let s = Store::open(&path).unwrap();
    assert!(s.open_report().caught_up_frames > 0);
    assert_eq!(relations(&s), live);
    assert_eq!(s.index_dump_for_tests().unwrap(), tables);
}

#[test]
fn rdx2_every_embedded_block_occurrence_has_a_residue() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let x = block("b-00001", "claim", "canary twice");
    let v = x.version_hash();
    let ca = s
        .commit_to_view("a", change("a", vec![Op::Create { block: x.clone() }]))
        .unwrap();
    let cb = s
        .commit_to_view("b", change("b", vec![Op::Create { block: x.clone() }]))
        .unwrap();
    assert_ne!(ca, cb, "two changes embed the same version");
    drop(s);
    // a second BLOCK frame of the same version, as a dump of an old store can hold
    append_raw(&path, &[(BLOCK, enc(&x))]);
    let s = Store::open(&path).unwrap();

    // the occurrences, found independently of the request builder
    let frames = raw_frames(&s);
    let blocks: Vec<Location> = frames
        .iter()
        .filter(|(_, k, h)| *k == BLOCK && *h == v)
        .map(|(l, _, _)| *l)
        .collect();
    let changes: Vec<Location> = frames
        .iter()
        .filter(|(_, k, h)| *k == CHANGE && (*h == ca || *h == cb))
        .map(|(l, _, _)| *l)
        .collect();
    assert_eq!(blocks.len(), 2, "two BLOCK frames");
    assert_eq!(changes.len(), 2, "two CHANGE frames");
    let mut want: BTreeSet<FramePlace> = blocks.iter().copied().map(place).collect();
    want.extend(changes.iter().copied().map(place));

    let d = build_request(&s, &RequestSpec::new("a", vec![version(&x)]));
    let got: BTreeSet<FramePlace> = d.request.manifest.spans.iter().map(|s| s.place).collect();
    assert_eq!(got, want, "every occurrence is a span");
    assert!(d.request.manifest.spans.iter().all(|s| s.residue.is_some()));
    let block_residues: Vec<&RedactionResidue> = d
        .residues
        .iter()
        .filter(|r| matches!(r, RedactionResidue::Block(_)))
        .collect();
    assert_eq!(
        block_residues.len(),
        1,
        "both BLOCK frames share one residue"
    );
    let change_residues: BTreeSet<Hash> = d
        .residues
        .iter()
        .filter_map(|r| match r {
            RedactionResidue::Change(c) => Some(c.change),
            _ => None,
        })
        .collect();
    assert_eq!(change_residues, BTreeSet::from([ca, cb]));
    s.prepare_redaction(&d.carrier, &d.request, &d.residues)
        .unwrap();

    // leaving any one occurrence out is refused, naming it, and writes nothing
    let end = s.log_end();
    for i in 0..d.request.manifest.spans.len() {
        let mut bad = d.clone();
        let gone = bad.request.manifest.spans.remove(i);
        let named: BTreeSet<Hash> = bad
            .request
            .manifest
            .spans
            .iter()
            .filter_map(|s| s.residue)
            .collect();
        bad.residues.retain(|r| named.contains(&residue_id(r)));
        let e = s
            .prepare_redaction(&bad.carrier, &bad.request, &bad.residues)
            .unwrap_err();
        assert!(corrupt_at(&e, gone.place), "{e:?} for {:?}", gone.place);
    }
    assert_eq!(s.log_end(), end);
}

#[test]
fn rdx2_a_change_residue_keeps_unaffected_ops() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let t = three_ops(&mut s);
    let original = s.change(&t.change).unwrap();
    assert_eq!(original.deps, vec![t.base]);
    drop(s);
    let fx = authorized_batch_fixture(
        &path,
        &[RequestSpec::new("work", vec![version(&t.a)])],
        &FixtureOptions::approved(Proof::local_human()),
    );
    let req = fx.requests[0].request;
    assert!(
        fx.requests[0]
            .draft
            .residues
            .contains(&RedactionResidue::Change(ChangeResidue::of(&original)))
    );
    let s = Store::open(&path).unwrap();
    assert_eq!(s.redaction_of(CHANGE, &t.change).unwrap(), Some(req));

    let p = s.change(&t.change).unwrap();
    assert!(p.is_projection());
    assert_eq!(p.id(), t.change, "the original id");
    assert_eq!(p.view, original.view);
    assert_eq!(p.deps, original.deps, "dependencies");
    assert_eq!(p.provenance, original.provenance);
    assert_eq!(p.justification, original.justification);
    assert_eq!(p.message, original.message);
    assert_eq!(p.links, original.links);
    assert_eq!(p.ops.len(), 3, "op order");
    match &p.ops[0] {
        Op::Create { block } => {
            assert!(block.is_redacted());
            assert_eq!(block.id, t.a.id);
            assert_eq!(block.version_hash(), t.a.version_hash());
            assert_eq!(block.body, CONTENT_REMOVED);
            assert_eq!(block.redaction(), Some(req));
        }
        op => panic!("{op:?}"),
    }
    match &p.ops[1] {
        Op::Update { base, block } => {
            assert_eq!(*base, t.b0.version_hash());
            assert_eq!(block, &t.b1, "unaffected content reads in full");
            assert!(!block.is_redacted());
        }
        op => panic!("{op:?}"),
    }
    match &p.ops[2] {
        Op::Delete { base, id } => {
            assert_eq!(*base, t.c0.version_hash());
            assert_eq!(id, &t.c0.id);
        }
        op => panic!("{op:?}"),
    }
    // canon keeps the original version hash of the removed block
    let canon = s.canon_state().unwrap();
    assert_eq!(canon.blocks[&t.a.id], t.a.version_hash());
    assert_eq!(canon.blocks[&t.b1.id], t.b1.version_hash());
    let blocks = s.blocks(&canon).unwrap();
    assert!(blocks[&t.a.id].is_redacted());
    assert_eq!(blocks[&t.b1.id], t.b1);
}

#[test]
fn rdx2_residue_validation_rejects_forged_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let t = three_ops(&mut s);
    let d = build_request(&s, &RequestSpec::new("work", vec![version(&t.a)]));
    let block_span = |d: &Draft| {
        d.request
            .manifest
            .spans
            .iter()
            .position(|s| s.kind == BLOCK)
            .unwrap()
    };
    let change_span = |d: &Draft| {
        d.request
            .manifest
            .spans
            .iter()
            .position(|s| s.kind == CHANGE)
            .unwrap()
    };
    let block_residue = |d: &Draft| {
        d.residues
            .iter()
            .position(|r| matches!(r, RedactionResidue::Block(_)))
            .unwrap()
    };
    let change_residue = |d: &Draft| {
        d.residues
            .iter()
            .position(|r| matches!(r, RedactionResidue::Change(_)))
            .unwrap()
    };
    let end = s.log_end();
    s.prepare_redaction(&d.carrier, &d.request, &d.residues)
        .unwrap();

    // (case, the draft, the span the refusal names)
    let mut cases: Vec<(&str, Draft, usize)> = vec![];
    let mut x = d.clone();
    let r = block_residue(&x);
    if let RedactionResidue::Block(b) = &mut x.residues[r] {
        b.id = BlockId::parse("b-00009").unwrap();
    }
    relink(&mut x.request, &x.residues);
    let at = block_span(&x);
    cases.push(("block id", x, at));

    let mut x = d.clone();
    let r = block_residue(&x);
    if let RedactionResidue::Block(b) = &mut x.residues[r] {
        b.version = Hash::of(b"another version");
    }
    let at = block_span(&x);
    x.request.manifest.spans[at].residue = Some(residue_id(&x.residues[r]));
    cases.push(("block hash", x, at));

    let mut x = d.clone();
    let at = block_span(&x);
    let cr = change_residue(&x);
    x.request.manifest.spans[at].residue = Some(residue_id(&x.residues[cr]));
    cases.push(("residue kind", x, at));

    let mut x = d.clone();
    let at = block_span(&x);
    x.request.manifest.spans[at].kind = CHANGE;
    cases.push(("frame kind", x, at));

    let mut x = d.clone();
    let at = block_span(&x);
    x.request.manifest.spans[at].len += 1;
    cases.push(("frame length", x, at));

    let mut x = d.clone();
    let cr = change_residue(&x);
    if let RedactionResidue::Change(c) = &mut x.residues[cr]
        && let rdy_model::redaction::ResidualOp::Update { base, .. } = &mut c.ops[1]
    {
        *base = Hash::of(b"another base");
    }
    relink(&mut x.request, &x.residues);
    let at = change_span(&x);
    cases.push(("base", x, at));

    let mut x = d.clone();
    let cr = change_residue(&x);
    if let RedactionResidue::Change(c) = &mut x.residues[cr] {
        c.ops.swap(0, 2);
    }
    relink(&mut x.request, &x.residues);
    let at = change_span(&x);
    cases.push(("op order", x, at));

    for (case, x, at) in &cases {
        let want = x.request.manifest.spans[*at].place;
        let e = s
            .prepare_redaction(&x.carrier, &x.request, &x.residues)
            .unwrap_err();
        assert!(corrupt_at(&e, want), "{case}: {e:?}");
    }

    // a manifest version of another block, and a carrier the request does not name
    let mut x = d.clone();
    x.request.manifest.versions[0].0 = t.b0.id.clone();
    let e = s
        .prepare_redaction(&x.carrier, &x.request, &x.residues)
        .unwrap_err();
    assert!(
        matches!(&e, StoreError::Refused(m) if m.contains("belongs to another block")),
        "{e:?}"
    );
    let mut x = d.clone();
    x.carrier.message = "something else".into();
    let e = s
        .prepare_redaction(&x.carrier, &x.request, &x.residues)
        .unwrap_err();
    assert!(matches!(e, StoreError::Refused(_)), "{e:?}");
    assert_eq!(s.log_end(), end, "no refusal writes");

    // a duplicate request for one carrier
    let p = s
        .prepare_redaction(&d.carrier, &d.request, &d.residues)
        .unwrap();
    s.commit_redaction(p).unwrap();
    let end = s.log_end();
    let e = s
        .prepare_redaction(&d.carrier, &d.request, &d.residues)
        .unwrap_err();
    assert!(
        matches!(&e, StoreError::Refused(m) if m.contains("already carries")),
        "{e:?}"
    );
    let mut other = build_request(&s, &RequestSpec::new("work", vec![version(&t.b1)]));
    other.carrier = d.carrier.clone();
    other.request.carrier = d.carrier_id();
    let e = s
        .prepare_redaction(&other.carrier, &other.request, &other.residues)
        .unwrap_err();
    assert!(
        matches!(&e, StoreError::Refused(m) if m.contains("already carries")),
        "{e:?}"
    );
    assert_eq!(s.log_end(), end);

    // a change whose untargeted version has no BLOCK frame cannot keep its other ops
    let a2 = block("b-00011", "claim", "canary in a raw change");
    let d2 = block("b-00012", "claim", "no frame of its own");
    let raw = change(
        "raw",
        vec![
            Op::Create { block: a2.clone() },
            Op::Create { block: d2.clone() },
        ],
    );
    let raw_id = s
        .write(vec![(BLOCK, enc(&a2)), (CHANGE, enc(&raw))])
        .unwrap()[1];
    let x = build_request(&s, &RequestSpec::new("raw", vec![version(&a2)]));
    let at = x
        .request
        .manifest
        .spans
        .iter()
        .find(|s| s.hash == raw_id)
        .unwrap()
        .place;
    let end = s.log_end();
    let e = s
        .prepare_redaction(&x.carrier, &x.request, &x.residues)
        .unwrap_err();
    assert!(corrupt_at(&e, at), "missing untargeted version: {e:?}");
    assert_eq!(s.log_end(), end);
}

#[test]
fn rdx2_raw_reads_and_blob_reads_cannot_restore_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let a = block("b-00001", "claim", "canary alpha secret");
    let b = block("b-00002", "claim", "bravo stays");
    let ca = s
        .commit_to_view(
            "work",
            change(
                "work",
                vec![
                    Op::Create { block: a.clone() },
                    Op::Create { block: b.clone() },
                ],
            ),
        )
        .unwrap();
    let big: Vec<u8> = (0..MIB + 100).map(|i| (i % 251) as u8).collect();
    let x = s.put_blob(&big).unwrap();
    let mut shares = big[..MIB].to_vec();
    shares.extend_from_slice(b"another tail");
    let y = s.put_blob(&shares).unwrap();
    let z = s.put_blob(b"unrelated").unwrap();
    let empty = s.put_blob(b"").unwrap();
    let (k0, k1) = (Hash::of(&big[..MIB]), Hash::of(&big[MIB..]));
    drop(s);
    let fx = authorized_batch_fixture(
        &path,
        &[RequestSpec::new(
            "work",
            vec![
                version(&a),
                RedactTarget::Blob(x),
                RedactTarget::Blob(empty),
            ],
        )],
        &FixtureOptions::approved(Proof::local_human()),
    );
    let req = fx.requests[0].request;
    let manifest = &fx.requests[0].draft.request.manifest;
    assert_eq!(
        manifest.shared_blobs,
        vec![y],
        "the collateral blob is listed"
    );
    let s = Store::open(&path).unwrap();
    let av = a.version_hash();

    // raw reads refuse; typed block and change reads give stubs
    assert!(removed(req, s.get_raw(&av)));
    assert!(removed(req, s.get_kind(BLOCK, &av)));
    assert!(removed(req, s.get::<Block>(&av)));
    assert!(removed(req, s.get_raw(&ca)));
    assert!(removed(req, s.get_kind(BLOB_CHUNK, &k0)));
    let stub = s.get_block(&av).unwrap();
    assert!(stub.is_redacted());
    assert_eq!(stub.id, a.id);
    assert_eq!(stub.body, CONTENT_REMOVED);
    assert!(stub.fields.is_empty() && stub.links.is_empty() && stub.pins.is_empty());
    assert_eq!(stub.version_hash(), av);
    assert_eq!(stub.redaction(), Some(req));
    assert_eq!(s.get_block(&b.version_hash()).unwrap(), b);
    let p = s.change(&ca).unwrap();
    assert!(p.ops[0].block_id() == &a.id);
    match (&p.ops[0], &p.ops[1]) {
        (Op::Create { block: first }, Op::Create { block: second }) => {
            assert!(first.is_redacted());
            assert_eq!(second, &b);
        }
        ops => panic!("{ops:?}"),
    }

    // blob reads: the selected blob, the collateral blob and the empty blob are unavailable
    assert!(removed(req, s.get_blob(&x)));
    assert!(removed(req, s.get_blob(&y)));
    assert!(removed(req, s.get_blob(&empty)));
    assert_eq!(s.get_blob(&z).unwrap(), b"unrelated");
    assert!(s.has_blob(&x).unwrap() && s.has_blob(&empty).unwrap());
    assert!(
        s.has_kind(BLOCK, &av).unwrap(),
        "an authorized absent object is known"
    );
    assert!(s.has_kind(BLOB_CHUNK, &k0).unwrap());

    // the lookups
    assert_eq!(s.redaction_of(BLOCK, &av).unwrap(), Some(req));
    assert_eq!(s.redaction_of(CHANGE, &ca).unwrap(), Some(req));
    assert_eq!(s.redaction_of(BLOB_MANIFEST, &x).unwrap(), Some(req));
    assert_eq!(s.redaction_of(BLOB_MANIFEST, &empty).unwrap(), Some(req));
    assert_eq!(s.redaction_of(BLOB_CHUNK, &k0).unwrap(), Some(req));
    assert_eq!(s.redaction_of(BLOB_CHUNK, &k1).unwrap(), Some(req));
    assert_eq!(s.redaction_of(BLOB_MANIFEST, &y).unwrap(), None);
    assert_eq!(s.redaction_of(BLOCK, &b.version_hash()).unwrap(), None);

    // an unknown missing object is still an error of its own
    let never = Hash::of(b"never stored");
    assert!(matches!(s.get_raw(&never), Err(StoreError::NotFound(h)) if h == never));

    // the raw walker refuses a removed payload; the record walker names it as dropped
    let end = s.log_end();
    let raw = s.for_each_frame_from(ORIGIN, end, |_, _, _, _, _| Ok(()));
    assert!(
        removed(req, raw),
        "the raw walker never passes a removed payload"
    );
    let apply = fx.applies[0].0;
    let mut dropped = vec![];
    let mut markers = 0;
    s.for_each_record_from(ORIGIN, end, |loc, frame, _| {
        match frame {
            StoredFrame::Present { kind, payload, .. } => {
                assert!(
                    !payload.windows(6).any(|w| w == b"canary"),
                    "a present frame holds removed text"
                );
                if kind == kinds::BATCH_COMMIT {
                    markers += 1;
                }
            }
            StoredFrame::Dropped {
                kind,
                hash,
                len,
                apply: by,
                residue,
            } => {
                assert_eq!(by, apply);
                dropped.push(DropSpan {
                    place: place(loc),
                    kind,
                    hash,
                    len,
                    residue,
                });
            }
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(dropped, manifest.spans);
    assert!(markers > 0, "batch markers are walked");
    s.check_objects().unwrap();
}

#[test]
fn rdx2_projected_or_erased_objects_cannot_be_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let a = block("b-00001", "claim", "canary alpha secret");
    let b = block("b-00002", "claim", "bravo stays");
    let ca = s
        .commit_to_view(
            "work",
            change(
                "work",
                vec![
                    Op::Create { block: a.clone() },
                    Op::Create { block: b.clone() },
                ],
            ),
        )
        .unwrap();
    let ca_bytes = s.get_raw(&ca).unwrap().1;
    let big: Vec<u8> = (0..MIB + 100).map(|i| (i % 249) as u8).collect();
    let x = s.put_blob(&big).unwrap();
    drop(s);
    let fx = authorized_batch_fixture(
        &path,
        &[RequestSpec::new(
            "work",
            vec![version(&a), RedactTarget::Blob(x)],
        )],
        &FixtureOptions::approved(Proof::relayed_human()),
    );
    let req = fx.requests[0].request;
    let mut s = Store::open(&path).unwrap();
    let av = a.version_hash();
    let end = s.log_end();

    // a projected block, in a change written directly or through a view
    let stub = s.get_block(&av).unwrap();
    let with_stub = change(
        "work",
        vec![Op::Update {
            base: av,
            block: stub.clone(),
        }],
    );
    assert!(removed(req, s.commit_change(with_stub.clone(), vec![])));
    assert!(removed(req, s.commit_to_view("work", with_stub)));
    // a forged marker, and a real request's marker on a version it did not remove
    let forged = |request: Hash, version: Hash| {
        Block::redacted(
            b.id.clone(),
            "claim".into(),
            None,
            "V".into(),
            RedactedBlockIdentity {
                version,
                content_hash: b.content_hash(),
                frozen: false,
                request,
            },
        )
    };
    for marker in [
        forged(Hash::of(b"no such request"), av),
        forged(req, b.version_hash()),
    ] {
        let c = change("work", vec![Op::Create { block: marker }]);
        assert!(projected(s.commit_change(c, vec![])));
    }
    // a projected change resolves its request from its original id; a forged one cannot
    let p = s.change(&ca).unwrap();
    assert!(p.is_projection());
    assert!(removed(req, s.commit_change(p, vec![])));
    let mut fake = change("work", vec![]);
    fake.read_identity = Some(Hash::of(b"not a removed change"));
    assert!(projected(s.commit_change(fake, vec![])));

    // the erased hashes: the block, the change, the blob and its chunk, and a blob sharing it
    assert!(removed(req, s.write(vec![(BLOCK, enc(&a))])));
    assert!(removed(req, s.write(vec![(CHANGE, ca_bytes)])));
    assert!(removed(req, s.put_blob(&big)));
    assert!(removed(
        req,
        s.write(vec![(BLOB_CHUNK, big[..MIB].to_vec())])
    ));
    let mut shares = big[..MIB].to_vec();
    shares.extend_from_slice(b"a new tail");
    assert!(removed(req, s.put_blob(&shares)));

    // the raw writer takes no redaction record
    let request = fx.requests[0].draft.request.clone();
    let residue = fx.requests[0].draft.residues[0].clone();
    let apply = fx.applies[0].1.clone();
    for (kind, bytes) in [
        (kinds::REDACTION, enc(&request)),
        (kinds::REDACTION_RESIDUE, enc(&residue)),
        (kinds::REDACTION_APPLY, enc(&apply)),
    ] {
        let e = s.write(vec![(kind, bytes)]).unwrap_err();
        assert!(matches!(e, StoreError::Refused(_)), "kind {kind}: {e:?}");
    }
    assert!(matches!(
        s.put(kinds::REDACTION_APPLY, &apply),
        Err(StoreError::Refused(_))
    ));
    assert_eq!(s.log_end(), end, "no refused write left a frame");

    // an unrelated new version, and a new version of the removed block, still write
    let a2 = block("b-00001", "claim", "rewritten from source");
    let cid = s
        .commit_to_view(
            "work",
            change(
                "work",
                vec![Op::Update {
                    base: av,
                    block: a2.clone(),
                }],
            ),
        )
        .unwrap();
    assert_eq!(s.get_block(&a2.version_hash()).unwrap(), a2);
    assert!(!s.change(&cid).unwrap().is_projection());
    let d = block("b-00004", "claim", "delta");
    s.commit_to_view(
        "other",
        change("other", vec![Op::Create { block: d.clone() }]),
    )
    .unwrap();
    assert_eq!(s.get_block(&d.version_hash()).unwrap(), d);
    let fresh = s.put_blob(b"fresh bytes").unwrap();
    assert_eq!(s.get_blob(&fresh).unwrap(), b"fresh bytes");
}

#[test]
fn rdx2_metadata_and_history_records_are_not_targets() {
    // the removable kinds are exactly BLOCK, CHANGE and BLOB_CHUNK; every other code is not
    assert_eq!(REMOVABLE_KINDS, [BLOCK, CHANGE, BLOB_CHUNK]);
    for k in 0..=255u8 {
        assert_eq!(
            is_removable(k),
            k == BLOCK || k == CHANGE || k == BLOB_CHUNK,
            "kind {k}"
        );
    }

    // a store holding every record kind its public paths write, and the redaction kinds
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let a = block("b-00001", "claim", "canary alpha secret");
    let b = block("b-00002", "claim", "bravo");
    s.commit_to_view(
        "work",
        change("work", vec![Op::Create { block: a.clone() }]),
    )
    .unwrap();
    s.commit_to_view(
        "ship",
        change("ship", vec![Op::Create { block: b.clone() }]),
    )
    .unwrap();
    let approver = Provenance {
        actor: "human:alice".into(),
        run: None,
        role: Role::Approver,
        at_ms: 20,
    };
    let p = s.propose("ship", "ship it", writer()).unwrap();
    let landing = s.land(&p, approver.clone()).unwrap();
    let tag = s
        .put_tag(&Tag {
            name: "v1".into(),
            landing,
            message: None,
            tagged_by: approver,
        })
        .unwrap();
    s.put_blob(b"an asset").unwrap();
    s.set_held(true).unwrap();
    s.set_refs(vec![("t/held".into(), vec![1])]).unwrap();
    let t = Transitions {
        at_ms: 30,
        resolved: vec![],
        opened: vec![],
        gone: vec![],
        new: vec![],
    };
    s.seal(vec![t.frame()]).unwrap();
    s.set_held(false).unwrap();
    drop(s);
    let other = block("b-00003", "claim", "removed before");
    let mut s = Store::open(&path).unwrap();
    s.commit_to_view(
        "old",
        change(
            "old",
            vec![Op::Create {
                block: other.clone(),
            }],
        ),
    )
    .unwrap();
    drop(s);
    authorized_batch_fixture(
        &path,
        &[RequestSpec::new("old", vec![version(&other)])],
        &FixtureOptions::approved(Proof::local_human()),
    );
    let s = Store::open(&path).unwrap();

    let d = build_request(&s, &RequestSpec::new("work", vec![version(&a)]));
    s.prepare_redaction(&d.carrier, &d.request, &d.residues)
        .unwrap();
    // one frame of each record kind that is not content, named as a span
    let mut seen: BTreeMap<u8, (Location, Hash, u32)> = BTreeMap::new();
    s.for_each_record_from(ORIGIN, s.log_end(), |loc, frame, _| {
        if let StoredFrame::Present {
            kind,
            hash,
            payload,
        } = frame
            && !is_removable(kind)
        {
            seen.entry(kind)
                .or_insert((loc, hash, payload.len() as u32));
        }
        Ok(())
    })
    .unwrap();
    let kinds_seen: BTreeSet<u8> = seen.keys().copied().collect();
    let want: BTreeSet<u8> = [
        kinds::PROPOSAL,
        kinds::LANDING,
        kinds::SNAPSHOT,
        BLOB_MANIFEST,
        kinds::REF_UPDATE,
        kinds::TAG,
        kinds::VIEW_OP,
        kinds::BATCH_COMMIT,
        kinds::TRANSITION,
        kinds::REDACTION,
        residue_frame(&d.residues[0]).0,
        kinds::REDACTION_APPLY,
    ]
    .into();
    assert!(kinds_seen.is_superset(&want), "{kinds_seen:?}");
    let end = s.log_end();
    for (kind, (loc, hash, len)) in &seen {
        let mut x = d.clone();
        let span = DropSpan {
            place: place(*loc),
            kind: *kind,
            hash: *hash,
            len: *len,
            residue: None,
        };
        let spans = &mut x.request.manifest.spans;
        let at = spans.partition_point(|s| s.place < span.place);
        spans.insert(at, span);
        let e = s
            .prepare_redaction(&x.carrier, &x.request, &x.residues)
            .unwrap_err();
        assert!(corrupt_at(&e, place(*loc)), "kind {kind}: {e:?}");
    }

    // a selector naming a record rather than content is refused
    let mut x = d.clone();
    x.request.targets.push(RedactTarget::Version {
        id: a.id.clone(),
        version: landing,
    });
    x.request.manifest.versions.push((a.id.clone(), landing));
    x.request.manifest.versions.sort();
    let e = s
        .prepare_redaction(&x.carrier, &x.request, &x.residues)
        .unwrap_err();
    assert!(
        matches!(&e, StoreError::Refused(m) if m.contains("not a stored block version")),
        "{e:?}"
    );
    for h in [tag, landing] {
        let mut x = d.clone();
        x.request.targets.push(RedactTarget::Blob(h));
        x.request.manifest.blobs.push(h);
        let e = s
            .prepare_redaction(&x.carrier, &x.request, &x.residues)
            .unwrap_err();
        assert!(
            matches!(&e, StoreError::Refused(m) if m.contains("not a blob manifest")),
            "{e:?}"
        );
    }
    assert_eq!(s.log_end(), end);
}

#[test]
fn rdx2_readers_capture_the_redaction_epoch() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::create(&dir.path().join("s")).unwrap();
    let r = s.reader();
    assert_eq!(r.redaction_epoch(), 0, "a new open starts at zero");
    assert_eq!(r.captured_redaction_epoch(), 0);
    r.check_redaction_epoch().unwrap();
    let clone = r.clone();
    assert_eq!(clone.captured_redaction_epoch(), 0);
    clone.check_redaction_epoch().unwrap();
}

#[test]
fn rdx2_a_preview_reader_masks_only_the_named_requests() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let a = block("b-00001", "claim", "canary alpha");
    let b = block("b-00002", "claim", "canary bravo");
    s.commit_to_view(
        "work",
        change("work", vec![Op::Create { block: a.clone() }]),
    )
    .unwrap();
    s.commit_to_view(
        "more",
        change("more", vec![Op::Create { block: b.clone() }]),
    )
    .unwrap();
    let da = build_request(&s, &RequestSpec::new("work", vec![version(&a)]));
    let p = s
        .prepare_redaction(&da.carrier, &da.request, &da.residues)
        .unwrap();
    let ra = s.commit_redaction(p).unwrap();
    let db = build_request(&s, &RequestSpec::new("more", vec![version(&b)]));
    let p = s
        .prepare_redaction(&db.carrier, &db.request, &db.residues)
        .unwrap();
    s.commit_redaction(p).unwrap();
    let epoch = s.reader().redaction_epoch();

    let preview = s.redaction_preview_reader(&[ra, ra]).unwrap();
    let stub = preview.get_block(&a.version_hash()).unwrap();
    assert!(stub.is_redacted());
    assert_eq!(stub.redaction(), Some(ra));
    assert_eq!(
        preview.redaction_of(BLOCK, &a.version_hash()).unwrap(),
        Some(ra)
    );
    assert_eq!(
        preview.get_block(&b.version_hash()).unwrap(),
        b,
        "only the named request"
    );
    // the ordinary reader, the active index and the epoch are unchanged
    assert_eq!(s.get_block(&a.version_hash()).unwrap(), a);
    assert_eq!(s.redaction_of(BLOCK, &a.version_hash()).unwrap(), None);
    assert_eq!(s.reader().redaction_epoch(), epoch);
    assert_eq!(preview.captured_redaction_epoch(), epoch);
    let unknown = Hash::of(b"no such request");
    assert!(matches!(
        s.redaction_preview_reader(&[unknown]),
        Err(StoreError::NotFound(h)) if h == unknown
    ));
}

#[test]
fn rdx2_commit_rechecks_the_spans() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let a = block("b-00001", "claim", "canary alpha");
    s.commit_to_view(
        "work",
        change("work", vec![Op::Create { block: a.clone() }]),
    )
    .unwrap();
    let d = build_request(&s, &RequestSpec::new("work", vec![version(&a)]));
    let p = s
        .prepare_redaction(&d.carrier, &d.request, &d.residues)
        .unwrap();
    // a new change embeds the selected version after preparation: a span is missing now
    s.commit_to_view(
        "copy",
        change("copy", vec![Op::Create { block: a.clone() }]),
    )
    .unwrap();
    let end = s.log_end();
    let e = s.commit_redaction(p).unwrap_err();
    assert!(matches!(e, StoreError::RedactionStale), "{e:?}");
    assert_eq!(s.log_end(), end);
    assert_eq!(s.redaction_for_carrier(&d.carrier_id()).unwrap(), None);
}

#[test]
fn rdx2_a_blob_read_names_the_earliest_apply() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![Op::Create {
                block: block("b-00001", "claim", "alpha"),
            }],
        ),
    )
    .unwrap();
    let big: Vec<u8> = (0..MIB + 100).map(|i| (i % 241) as u8).collect();
    let x = s.put_blob(&big).unwrap();
    let mut shares = b"another head".to_vec();
    shares.resize(MIB, 7);
    shares.extend_from_slice(&big[MIB..]);
    let y = s.put_blob(&shares).unwrap();
    drop(s);
    // the first apply removes y, which shares x's second chunk; the second removes x
    let first = authorized_batch_fixture(
        &path,
        &[RequestSpec::new("work", vec![RedactTarget::Blob(y)])],
        &FixtureOptions::approved(Proof::local_human()),
    );
    {
        // the store's own checks take a request that leaves the removed chunk out
        let s = Store::open(&path).unwrap();
        let d = build_request(
            &s,
            &RequestSpec::new("work", vec![RedactTarget::Blob(x)]).at(41),
        );
        s.prepare_redaction(&d.carrier, &d.request, &d.residues)
            .unwrap();
    }
    let second = authorized_batch_fixture(
        &path,
        &[RequestSpec::new("work", vec![RedactTarget::Blob(x)]).at(41)],
        &FixtureOptions::approved(Proof::relayed_human()),
    );
    let (r1, r2) = (first.requests[0].request, second.requests[0].request);
    let k1 = Hash::of(&big[MIB..]);
    let spans = &second.requests[0].draft.request.manifest.spans;
    assert_eq!(spans.len(), 1, "only x's remaining chunk is listed");
    let s = Store::open(&path).unwrap();
    assert_eq!(s.redaction_of(BLOB_MANIFEST, &x).unwrap(), Some(r2));
    assert_eq!(
        s.redaction_of(BLOB_CHUNK, &k1).unwrap(),
        Some(r1),
        "the first apply keeps it"
    );
    assert!(
        removed(r1, s.get_blob(&x)),
        "the earliest authorizing apply"
    );
    assert!(removed(r1, s.get_blob(&y)));
}

// Fix round 1: the write, read and preparation gaps an adversarial review found.

/// A store at `path` whose block `a` (in view `work`) an approved request removed. Returns the
/// open store, the request and the change that created `a`.
fn removed_block(path: &Path, a: &Block) -> (Store, Hash, Hash) {
    let mut s = Store::create(path).unwrap();
    let ca = s
        .commit_to_view(
            "work",
            change("work", vec![Op::Create { block: a.clone() }]),
        )
        .unwrap();
    drop(s);
    let fx = authorized_batch_fixture(
        path,
        &[RequestSpec::new("work", vec![version(a)])],
        &FixtureOptions::approved(Proof::local_human()),
    );
    (Store::open(path).unwrap(), fx.requests[0].request, ca)
}

#[test]
fn rdx2_seal_cannot_activate_an_unapproved_request() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let a = block("b-00001", "claim", "seal bypass secret");
    s.commit_to_view(
        "work",
        change("work", vec![Op::Create { block: a.clone() }]),
    )
    .unwrap();
    let d = build_request(&s, &RequestSpec::new("work", vec![version(&a)]));
    let p = s
        .prepare_redaction(&d.carrier, &d.request, &d.residues)
        .unwrap();
    let req = s.commit_redaction(p).unwrap();
    let apply = rdy_model::redaction::RedactionApply {
        request: req,
        landing: Hash::of(b"there is no landing"),
        report_hash: Hash::of(b"there was no approved report"),
    };
    let records = [
        (kinds::REDACTION_APPLY, enc(&apply)),
        (kinds::REDACTION, enc(&d.request)),
        residue_frame(&d.residues[0]),
    ];
    let av = a.version_hash();

    // unheld: no batch is open, so a refused seal opens none and appends nothing
    for (kind, bytes) in &records {
        let end = s.log_end();
        let e = s.seal(vec![(*kind, bytes.clone())]).unwrap_err();
        assert!(matches!(e, StoreError::Refused(_)), "kind {kind}: {e:?}");
        assert_eq!(s.log_end(), end, "kind {kind}: the refused seal appended");
        assert!(s.open_batch().is_none());
    }
    assert_eq!(s.redaction_of(BLOCK, &av).unwrap(), None);
    assert_eq!(s.get_block(&av).unwrap(), a);

    // held: the open batch stays open and unpoisoned, and seals cleanly afterwards
    s.set_held(true).unwrap();
    let other = block("b-00002", "claim", "in the held batch");
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![Op::Create {
                block: other.clone(),
            }],
        ),
    )
    .unwrap();
    let open = s.open_batch().unwrap().to_vec();
    for (kind, bytes) in &records {
        let end = s.log_end();
        let e = s.seal(vec![(*kind, bytes.clone())]).unwrap_err();
        assert!(matches!(e, StoreError::Refused(_)), "kind {kind}: {e:?}");
        assert_eq!(s.log_end(), end, "kind {kind}: the refused seal appended");
        assert_eq!(s.open_batch().unwrap(), &open[..]);
    }
    s.seal(vec![]).unwrap();
    s.set_held(false).unwrap();
    assert_eq!(s.redaction_of(BLOCK, &av).unwrap(), None);
    assert_eq!(s.get_block(&av).unwrap(), a);
    assert_eq!(s.get_block(&other.version_hash()).unwrap(), other);
    let path = s.dir().to_path_buf();
    drop(s);
    let s = Store::open(&path).unwrap();
    assert_eq!(s.redaction_of(BLOCK, &av).unwrap(), None);
    assert_eq!(s.get_block(&av).unwrap(), a);
}

#[test]
fn rdx2_a_raw_change_cannot_restore_a_removed_block() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let a = block("b-00001", "claim", "embedded restore secret");
    let (mut s, req, _) = removed_block(&path, &a);
    let end = s.log_end();
    // a fresh change (no BLOCK frame) that embeds the removed original, created or updated
    let restore = change("restore", vec![Op::Create { block: a.clone() }]);
    let mut over = change(
        "restore",
        vec![Op::Update {
            base: Hash::of(b"some base"),
            block: a.clone(),
        }],
    );
    over.message = "an update embeds it too".into();
    for c in [&restore, &over] {
        assert!(removed(req, s.write(vec![(CHANGE, enc(c))])));
        assert!(removed(req, s.put(CHANGE, c)));
        assert!(removed(req, s.seal(vec![(CHANGE, enc(c))])));
        s.set_held(true).unwrap();
        assert!(removed(req, s.write(vec![(CHANGE, enc(c))])));
        s.set_held(false).unwrap();
    }
    assert_eq!(s.log_end(), end, "no refused write left a frame");
    // a change embedding an unrelated new version still writes
    let b = block("b-00002", "claim", "unrelated");
    let h = s
        .write(vec![(
            CHANGE,
            enc(&change("x", vec![Op::Create { block: b.clone() }])),
        )])
        .unwrap()[0];
    assert!(matches!(&s.change(&h).unwrap().ops[0], Op::Create { block } if *block == b));
}

#[test]
fn rdx2_block_reads_use_the_typed_mask() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let a = block("b-00001", "claim", "wrong kind original secret");
    // a blob chunk may hold a block's exact encoding; it is logged before the BLOCK frame
    s.write(vec![(BLOB_CHUNK, enc(&a))]).unwrap();
    s.commit_to_view(
        "work",
        change("work", vec![Op::Create { block: a.clone() }]),
    )
    .unwrap();
    drop(s);
    let fx = authorized_batch_fixture(
        &path,
        &[RequestSpec::new("work", vec![version(&a)])],
        &FixtureOptions::approved(Proof::local_human()),
    );
    let req = fx.requests[0].request;
    let s = Store::open(&path).unwrap();
    let av = a.version_hash();
    assert_eq!(s.redaction_of(BLOCK, &av).unwrap(), Some(req));
    assert!(removed(req, s.get_kind(BLOCK, &av)));
    let got = s.get_block(&av).unwrap();
    assert!(
        got.is_redacted(),
        "get_block read the earlier other-kind frame"
    );
    assert_eq!(got.redaction(), Some(req));
    assert_eq!(got.body, CONTENT_REMOVED);
    // the chunk itself is another object, which the request did not select
    assert_eq!(s.get_kind(BLOB_CHUNK, &av).unwrap(), enc(&a));
}

#[test]
fn rdx2_put_cannot_serialize_a_projection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let a = block("b-00001", "claim", "projected block secret");
    let (mut s, req, ca) = removed_block(&path, &a);
    let end = s.log_end();
    // a known projection names its request
    let stub = s.get_block(&a.version_hash()).unwrap();
    assert!(stub.is_redacted());
    assert!(removed(req, s.put(BLOCK, &stub)));
    let p = s.change(&ca).unwrap();
    assert!(p.is_projection());
    assert!(removed(req, s.put(CHANGE, &p)));
    // a change holding the stub as an op's block
    let with_stub = change(
        "work",
        vec![Op::Create {
            block: stub.clone(),
        }],
    );
    assert!(removed(req, s.put(CHANGE, &with_stub)));
    // a forged or unmatched marker
    let forged = Block::redacted(
        a.id.clone(),
        "claim".into(),
        None,
        "V".into(),
        RedactedBlockIdentity {
            version: a.version_hash(),
            content_hash: a.content_hash(),
            frozen: false,
            request: Hash::of(b"no such request"),
        },
    );
    assert!(projected(s.put(BLOCK, &forged)));
    let mut fake = change("work", vec![]);
    fake.read_identity = Some(Hash::of(b"not a removed change"));
    assert!(projected(s.put(CHANGE, &fake)));
    assert_eq!(s.log_end(), end, "no refused put left a frame");
    // an ordinary block still puts
    let b = block("b-00002", "claim", "ordinary");
    let h = s.put(BLOCK, &b).unwrap();
    assert_eq!(s.get_block(&h).unwrap(), b);
}

/// The refusal of a value `put` cannot check as a block or a change.
fn unchecked<T>(r: Result<T, StoreError>) -> bool {
    matches!(r, Err(StoreError::Refused(_)))
}

#[test]
fn rdx2_put_checks_every_typed_block_and_change() {
    #[derive(serde::Serialize)]
    #[serde(transparent)]
    struct Wrapped<T>(T);

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let a = block("b-00001", "claim", "boxed projection secret");
    let (mut s, req, ca) = removed_block(&path, &a);
    let end = s.log_end();
    let stub = s.get_block(&a.version_hash()).unwrap();
    assert!(stub.is_redacted());
    let p = s.change(&ca).unwrap();
    assert!(p.is_projection());
    let with_stub = change(
        "work",
        vec![Op::Create {
            block: stub.clone(),
        }],
    );
    // a boxed known projection names its request, as the bare value does
    assert!(removed(req, s.put(BLOCK, &Box::new(stub.clone()))));
    assert!(removed(req, s.put(CHANGE, &Box::new(p.clone()))));
    assert!(removed(req, s.put(CHANGE, &Box::new(with_stub.clone()))));
    // a 'static reference is checked too
    let leaked: &'static Block = Box::leak(Box::new(stub.clone()));
    assert!(removed(req, s.put(BLOCK, &leaked)));
    // an unmatched marker: a real request that did not remove this version
    let other = block("b-00002", "claim", "never removed");
    let unmatched = Block::redacted(
        other.id.clone(),
        "claim".into(),
        None,
        "V".into(),
        RedactedBlockIdentity {
            version: other.version_hash(),
            content_hash: other.content_hash(),
            frozen: false,
            request: req,
        },
    );
    assert!(projected(s.put(BLOCK, &unmatched)));
    assert!(projected(s.put(BLOCK, &Box::new(unmatched.clone()))));
    let unmatched_op = change(
        "work",
        vec![Op::Create {
            block: unmatched.clone(),
        }],
    );
    assert!(projected(s.put(CHANGE, &Box::new(unmatched_op))));
    let mut fake = change("work", vec![]);
    fake.read_identity = Some(Hash::of(b"not a removed change"));
    assert!(projected(s.put(CHANGE, &Box::new(fake))));
    // a value put cannot see into is refused for BLOCK and CHANGE, even when it is ordinary
    assert!(unchecked(s.put(BLOCK, &Wrapped(stub.clone()))));
    assert!(unchecked(s.put(BLOCK, &Wrapped(other.clone()))));
    assert!(unchecked(s.put(CHANGE, &Wrapped(p.clone()))));
    assert!(unchecked(s.put(CHANGE, &Wrapped(change("work", vec![])))));
    let cow: std::borrow::Cow<'static, Block> = std::borrow::Cow::Owned(stub.clone());
    assert!(unchecked(s.put(BLOCK, &cow)));
    // a block offered as a change, or a change as a block, is not that kind's type
    assert!(unchecked(s.put(CHANGE, &other)));
    assert!(unchecked(s.put(BLOCK, &change("work", vec![]))));
    assert_eq!(s.log_end(), end, "no refused put left a frame");
    // an ordinary boxed block and change still put, under the bare value's hash
    let hb = s.put(BLOCK, &Box::new(other.clone())).unwrap();
    assert_eq!(hb, other.version_hash());
    assert_eq!(s.get_block(&hb).unwrap(), other);
    let c = change("work", vec![]);
    let hc = s.put(CHANGE, &Box::new(c.clone())).unwrap();
    assert_eq!(hc, Hash::of(&enc(&c)));
}

#[test]
fn rdx2_all_versions_rechecks_the_stored_versions() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let a = block("b-00001", "claim", "version one secret");
    s.commit_to_view(
        "work",
        change("work", vec![Op::Create { block: a.clone() }]),
    )
    .unwrap();
    let all = RequestSpec::new("work", vec![RedactTarget::AllVersions(a.id.clone())]);
    let d = build_request(&s, &all);
    let p = s
        .prepare_redaction(&d.carrier, &d.request, &d.residues)
        .unwrap();
    // a second version of the id, written after preparation, stales the request
    let a2 = block("b-00001", "claim", "version two secret");
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![Op::Update {
                base: a.version_hash(),
                block: a2.clone(),
            }],
        ),
    )
    .unwrap();
    let end = s.log_end();
    let r = s.commit_redaction(p);
    assert!(matches!(r, Err(StoreError::RedactionStale)), "{r:?}");
    assert_eq!(s.log_end(), end);
    // the same incomplete manifest, prepared now, is refused
    let r = s.prepare_redaction(&d.carrier, &d.request, &d.residues);
    assert!(matches!(r, Err(StoreError::Refused(_))), "{r:?}");
    // a manifest with every stored version of the id prepares and commits
    let d = build_request(&s, &all);
    assert_eq!(d.request.manifest.versions.len(), 2);
    let p = s
        .prepare_redaction(&d.carrier, &d.request, &d.residues)
        .unwrap();
    s.commit_redaction(p).unwrap();
}

#[test]
fn rdx2_a_span_must_be_a_real_frame_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let a = block("b-00001", "claim", "inner frame secret");
    s.commit_to_view(
        "work",
        change("work", vec![Op::Create { block: a.clone() }]),
    )
    .unwrap();
    // a blob whose bytes are a valid-looking BLOCK frame of a's encoding
    let payload = enc(&a);
    let mut decoy = Vec::new();
    decoy.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    decoy.push(BLOCK);
    decoy.extend_from_slice(&a.version_hash().0);
    decoy.extend_from_slice(&payload);
    let blob = s.put_blob(&decoy).unwrap();
    let chunk = Hash::of(&decoy);
    let loc = raw_frames(&s)
        .into_iter()
        .find(|(_, k, h)| *k == BLOB_CHUNK && *h == chunk)
        .unwrap()
        .0;
    let mut d = build_request(&s, &RequestSpec::new("work", vec![version(&a)]));
    let residue = d
        .request
        .manifest
        .spans
        .iter()
        .find(|sp| sp.kind == BLOCK)
        .unwrap()
        .residue;
    let inner = FramePlace {
        segment: loc.segment,
        offset: loc.offset + 37, // the frame header
    };
    d.request.manifest.spans.push(DropSpan {
        place: inner,
        kind: BLOCK,
        hash: a.version_hash(),
        len: payload.len() as u32,
        residue,
    });
    d.request.manifest.spans.sort_by_key(|sp| sp.place);
    let e = s
        .prepare_redaction(&d.carrier, &d.request, &d.residues)
        .unwrap_err();
    assert!(corrupt_at(&e, inner), "{e:?}");
    assert_eq!(s.get_blob(&blob).unwrap(), decoy);
}

#[test]
fn rdx2_a_residue_identity_matches_its_frame_hash() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    // a BLOCK payload that decodes but is not the block's canonical encoding
    let a = block("b-00001", "claim", "noncanonical block secret");
    let mut bytes = enc(&a);
    bytes.push(0);
    let h = s.write(vec![(BLOCK, bytes)]).unwrap()[0];
    assert_ne!(h, a.version_hash());
    assert_eq!(s.get_block(&h).unwrap().body, a.body);
    let target = RedactTarget::Version {
        id: a.id.clone(),
        version: h,
    };
    let d = build_request(&s, &RequestSpec::new("work", vec![target]));
    let span = d.request.manifest.spans[0].clone();
    assert_eq!(span.hash, h);
    let end = s.log_end();
    let e = s
        .prepare_redaction(&d.carrier, &d.request, &d.residues)
        .unwrap_err();
    assert!(corrupt_at(&e, span.place), "{e:?}");
    assert_eq!(s.log_end(), end);
    // a change payload with a trailing byte: its residue cannot name the frame hash either
    let b = block("b-00002", "claim", "change frame");
    let mut cbytes = enc(&change("work", vec![Op::Create { block: b.clone() }]));
    cbytes.push(0);
    s.write(vec![(BLOCK, enc(&b))]).unwrap();
    let ch = s.write(vec![(CHANGE, cbytes)]).unwrap()[0];
    let d = build_request(&s, &RequestSpec::new("work", vec![version(&b)]));
    let span = d
        .request
        .manifest
        .spans
        .iter()
        .find(|sp| sp.kind == CHANGE && sp.hash == ch)
        .unwrap()
        .clone();
    let e = s
        .prepare_redaction(&d.carrier, &d.request, &d.residues)
        .unwrap_err();
    assert!(corrupt_at(&e, span.place), "{e:?}");
}
