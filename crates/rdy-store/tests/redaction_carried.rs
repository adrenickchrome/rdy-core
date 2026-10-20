//! hc3: carried holes without erasure authority (D-QF2HQ: a carried record "never authorizes an
//! erasure"; D-CC90E: a removal needs a person's approval; D-0VG72; O5, O8, O13).
//!
//! A carried record (kind REDACTION_CARRIED) explains exact zero bytes a removal in another store
//! left in this log, and records who approved that removal there. The open, a reindex, a load
//! and the reads read it through the store's own removal bootstrap and mask. It is never an
//! approval here: nothing is erased for it, and a held payload under it hashes until an apply of
//! this store, or a later carried `held: false` span, accounts for its zeros.
//!
//! The source holds a kept block landed and tagged `v0`, then a secret block and another block,
//! landed and tagged `v1`. A stopped load's ingest (`carried_fixture::load`) copies it with the
//! secret's block and change frames named by one carried record of the source removal.
#[path = "support/carried.rs"]
mod carried_fixture;
#[path = "support/redaction.rs"]
mod redaction_fixture;

use carried_fixture::{
    Framed, HEADER, append_raw, approver, block_residue, carried_frames, carried_hash,
    change_residue, copy_store, find, framed, framed_at, ingest, load, log_bytes, poke, record,
    residue_hash, source_apply, source_landing, source_request, span, store_format, zero,
};
use rdy_model::redaction::{CONTENT_REMOVED, RedactTarget, RedactionCarried, RedactionResidue};
use rdy_model::*;
use rdy_store::closure::{END_OF_LOG, StoreSource, closure, write_batch};
use rdy_store::dump::{DumpBatch, DumpItem, DumpReader, DumpWriter, Frame, RemovalTally};
use rdy_store::dump_chain::{BatchMarker, DumpChainHeader, batch_end};
use rdy_store::ingest::Ingest;
use rdy_store::kinds::{self, batch_digest};
use rdy_store::object_refs::{ObjectKey, refs_of};
use rdy_store::redaction::limit;
use rdy_store::redaction_carried::{CarriedAt, HOLDER_FIX, PROJECT_FIX};
use rdy_store::segment::Location;
use rdy_store::sync_stream::{StreamHeader, StreamReader, StreamRecord};
use rdy_store::{Store, StoreError, StoreReader};
use redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const RAW_REFUSAL: &str =
    "object kind 23 is a redaction record: only the store's checked redaction writers write one";

fn prov(actor: &str, role: Role) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: None,
        role,
        at_ms: 1,
    }
}

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
        read_identity: None,
    }
}

fn change(view: &str, ops: Vec<Op>) -> Change {
    Change {
        view: view.into(),
        ops,
        deps: vec![],
        provenance: prov("agent:w", Role::Writer),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// The source project (closed) and what it holds.
struct Src {
    dir: tempfile::TempDir,
    path: PathBuf,
    kept: Block,
    secret: Block,
    other: Block,
    /// The change that wrote the secret and the other block.
    change: Change,
    change_id: Hash,
    v0: Hash,
    v1: Hash,
    frames: Vec<Framed>,
}

fn source() -> Src {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("src");
    let mut s = Store::create(&path).unwrap();
    let writer = prov("agent:w", Role::Writer);
    let human = prov("human:alice", Role::Approver);
    let kept = block("b-00003", "kept text");
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![Op::Create {
                block: kept.clone(),
            }],
        ),
    )
    .unwrap();
    let p0 = s.propose("work", "kept", writer.clone()).unwrap();
    let l0 = s.land(&p0, human.clone()).unwrap();
    let v0 = s
        .put_tag(&Tag {
            name: "v0".into(),
            landing: l0,
            message: None,
            tagged_by: human.clone(),
        })
        .unwrap();
    let secret = block("b-00001", "canary alpha secret");
    let other = block("b-00002", "other text");
    let c = change(
        "work",
        vec![
            Op::Create {
                block: secret.clone(),
            },
            Op::Create {
                block: other.clone(),
            },
        ],
    );
    let change_id = s.commit_to_view("work", c).unwrap();
    let stored = s.change(&change_id).unwrap();
    let p1 = s.propose("work", "secret", writer).unwrap();
    let l1 = s.land(&p1, human.clone()).unwrap();
    let v1 = s
        .put_tag(&Tag {
            name: "v1".into(),
            landing: l1,
            message: None,
            tagged_by: human,
        })
        .unwrap();
    drop(s);
    let frames = framed_at(&path);
    Src {
        dir,
        path,
        kept,
        secret,
        other,
        change: stored,
        change_id,
        v0,
        v1,
        frames,
    }
}

impl Src {
    fn secret_frame(&self) -> Framed {
        find(&self.frames, kinds::BLOCK, self.secret.version_hash())
    }

    fn other_frame(&self) -> Framed {
        find(&self.frames, kinds::BLOCK, self.other.version_hash())
    }

    fn change_frame(&self) -> Framed {
        find(&self.frames, kinds::CHANGE, self.change_id)
    }

    fn residues(&self) -> Vec<RedactionResidue> {
        vec![block_residue(&self.secret), change_residue(&self.change)]
    }

    fn targets(&self) -> Vec<RedactTarget> {
        vec![RedactTarget::Version {
            id: self.secret.id.clone(),
            version: self.secret.version_hash(),
        }]
    }

    /// The carried record of the secret's block and change frames, both `held`.
    fn record(&self, held: bool) -> RedactionCarried {
        let r = self.residues();
        let mut spans = vec![
            span(&self.secret_frame(), Some(&r[0]), held),
            span(&self.change_frame(), Some(&r[1]), held),
        ];
        spans.sort_by_key(|s| s.place);
        record(self.targets(), spans)
    }

    /// A new directory beside the source.
    fn at(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// The source loaded into `name` with `records` (the checked path).
    fn loaded(&self, name: &str, records: &[RedactionCarried]) -> PathBuf {
        let to = self.at(name);
        load(&self.path, &to, &self.residues(), records).unwrap();
        to
    }
}

fn corrupt_at(e: StoreError) -> Location {
    match e {
        StoreError::RedactionCorrupt { segment, offset } => Location { segment, offset },
        other => panic!("not a corrupt redaction refusal: {other:?}"),
    }
}

/// The proof the record walker names for the dropped frame at `loc`, if it is dropped.
fn proof_at(s: &Store, loc: Location) -> Option<Hash> {
    let mut out = None;
    s.reader()
        .for_each_record_from(loc, s.log_end(), |at, f, _| {
            if at == loc
                && let rdy_store::redaction::StoredFrame::Dropped { apply, .. } = f
            {
                out = Some(apply);
            }
            Ok(())
        })
        .unwrap();
    out
}

fn counts_of(kinds_of: &[Framed]) -> BTreeMap<u8, usize> {
    let mut out = BTreeMap::new();
    for f in kinds_of {
        *out.entry(f.kind).or_default() += 1;
    }
    out
}

fn carried_of(r: &StoreReader) -> Vec<CarriedAt> {
    let mut out = vec![];
    r.visit_carried(END_OF_LOG, &mut |c| {
        out.push(c);
        Ok(())
    })
    .unwrap();
    out
}

#[test]
fn hc3_only_ingest_can_append_carried() {
    let src = source();
    let rec = src.record(false);
    let bytes = postcard::to_allocvec(&rec).unwrap();
    // no raw writer takes kind 23, and no store writer exists for it
    {
        let mut s = Store::open(&src.path).unwrap();
        let end = s.log_end();
        for e in [
            s.write(vec![(kinds::REDACTION_CARRIED, bytes.clone())])
                .map(|_| ())
                .unwrap_err(),
            s.seal(vec![(kinds::REDACTION_CARRIED, bytes.clone())])
                .unwrap_err(),
            s.put(kinds::REDACTION_CARRIED, &rec)
                .map(|_| ())
                .unwrap_err(),
        ] {
            match e {
                StoreError::Refused(m) => assert_eq!(m, RAW_REFUSAL),
                other => panic!("not the raw-write refusal: {other:?}"),
            }
        }
        assert_eq!(s.log_end(), end);
    }
    // the checked path: a load's ingest appends one carried batch after the source's batches
    let to = src.at("to");
    let report = load(&src.path, &to, &src.residues(), &[rec.clone()]).unwrap();
    let source_batches = {
        let s = Store::open(&src.path).unwrap();
        let mut n = 0u64;
        let mut frames = 0u64;
        s.reader()
            .for_each_batch(s.log_end(), |b| {
                n += 1;
                frames += b.frames.len() as u64;
                Ok(())
            })
            .unwrap();
        (n, frames)
    };
    // the carried batch counts in no source total
    assert_eq!((report.batches, report.frames), source_batches);
    let s = Store::open(&to).unwrap();
    let r = s.reader();
    let all = framed(&r, s.log_end());
    let tail: Vec<(u8, Hash)> = all[all.len() - 3..]
        .iter()
        .map(|f| (f.kind, f.hash))
        .collect();
    let mut residues: Vec<Hash> = src.residues().iter().map(residue_hash).collect();
    residues.sort();
    assert_eq!(
        tail,
        vec![
            (kinds::REDACTION_RESIDUE, residues[0]),
            (kinds::REDACTION_RESIDUE, residues[1]),
            (kinds::REDACTION_CARRIED, carried_hash(&rec)),
        ]
    );
    // nothing new was approved: no request, apply or landing is made, and the source's are not
    // here
    let before = counts_of(&src.frames);
    let after = counts_of(&all);
    for k in [kinds::REDACTION, kinds::REDACTION_APPLY, kinds::LANDING] {
        assert_eq!(after.get(&k), before.get(&k), "kind {k}");
    }
    for h in [source_request(), source_apply(), source_landing()] {
        assert!(!r.has(&h).unwrap(), "{h} is not here");
    }
    // the record keeps the source's approver and time
    let carried = carried_of(&r);
    assert_eq!(carried.len(), 1);
    assert_eq!(carried[0].record, rec);
    assert_eq!(carried[0].record.approved_by, approver());
    assert_eq!(carried[0].record.approved_by.at_ms, 777);
    assert_eq!(carried[0].at, all[all.len() - 1].loc);
    drop(s);
    // the carried batch comes once, before the upgrade; an empty one writes nothing
    let batches = carried_fixture::batches_with_drops(&src.path, &[rec.clone()]);
    let mut i = Ingest::create(&src.at("twice")).unwrap();
    for (n, b) in batches.iter().enumerate() {
        i.batch(n as u64 + 1, b).unwrap();
    }
    let end = i.end();
    i.append_carried(&src.residues(), &[]).unwrap();
    assert_eq!(i.end(), end);
    assert!(matches!(
        i.append_carried(&src.residues(), &[rec.clone()]),
        Err(StoreError::Dump { .. })
    ));
    assert_eq!(i.end(), end);
    let mut i = Ingest::create(&src.at("late")).unwrap();
    for (n, b) in batches.iter().enumerate() {
        i.batch(n as u64 + 1, b).unwrap();
    }
    i.upgrade(store_format()).unwrap();
    let end = i.end();
    assert!(matches!(
        i.append_carried(&src.residues(), &[rec]),
        Err(StoreError::Dump { .. })
    ));
    assert_eq!(i.end(), end);
}

#[test]
fn hc3_carried_false_accepts_exact_zeros_and_never_erases() {
    let src = source();
    let to = src.loaded("to", &[src.record(false)]);
    let bytes = log_bytes(&to);
    {
        let mut s = Store::open(&to).unwrap();
        let counts = s.finish_redactions().unwrap();
        assert_eq!((counts.frames, counts.checked_payloads), (2, 0));
        let r = s.reader();
        assert_eq!(r.carried_counts(&src.record(false)).unwrap(), (0, 2));
        s.verify().unwrap();
    }
    // the open and the finish wrote no payload byte
    assert_eq!(log_bytes(&to), bytes);
    // one nonzero byte under a held=false span refuses the open, and is never erased
    let bad = src.at("bad");
    copy_store(&to, &bad);
    poke(&bad, &src.secret_frame(), 3, 1);
    let damaged = log_bytes(&bad);
    let e = Store::open(&bad).err().unwrap();
    assert_eq!(corrupt_at(e), src.secret_frame().loc);
    assert_eq!(log_bytes(&bad), damaged);
    // a held=false flag over intact data refuses the load before the store is made
    let rec = src.record(false);
    let intact = src.at("intact");
    let batches = carried_fixture::batches_with_drops(&src.path, &[]);
    let e = ingest(&intact, &batches, &src.residues(), &[rec]).unwrap_err();
    assert_eq!(
        corrupt_at(e),
        src.secret_frame().loc.min(src.change_frame().loc)
    );
    assert!(!intact.join("FORMAT").exists());
}

#[test]
fn hc3_carried_true_hashes_until_local_apply() {
    let src = source();
    let rec = src.record(true);
    let to = src.loaded("to", &[rec.clone()]);
    {
        let s = Store::open(&to).unwrap();
        let r = s.reader();
        // intact: the held payload reads, and it is still held
        assert_eq!(r.get_block(&src.secret.version_hash()).unwrap(), src.secret);
        assert_eq!(r.carried_counts(&rec).unwrap(), (2, 0));
        assert_eq!(
            r.redaction_of(kinds::BLOCK, &src.secret.version_hash())
                .unwrap(),
            None
        );
    }
    // an unexplained zero under a held span refuses the open
    let z = src.at("zeroed");
    copy_store(&to, &z);
    zero(&z, &src.secret_frame());
    let damaged = log_bytes(&z);
    assert_eq!(
        corrupt_at(Store::open(&z).err().unwrap()),
        src.secret_frame().loc
    );
    assert_eq!(log_bytes(&z), damaged);
    // a later apply of this store, approved by a person, removes it and is its proof
    let fixture = authorized_batch_fixture(
        &to,
        &[RequestSpec::new("work", src.targets()).at(60)],
        &FixtureOptions::approved(Proof::local_human()),
    );
    let s = Store::open(&to).unwrap();
    let r = s.reader();
    assert_eq!(
        r.get_block(&src.secret.version_hash()).unwrap().body,
        CONTENT_REMOVED
    );
    assert_eq!(r.carried_counts(&rec).unwrap(), (0, 2));
    assert_eq!(
        proof_at(&s, src.secret_frame().loc),
        Some(fixture.applies[0].0)
    );
    assert_eq!(
        r.redaction_of(kinds::BLOCK, &src.secret.version_hash())
            .unwrap(),
        Some(fixture.requests[0].request)
    );
}

#[test]
fn hc3_carried_residues_restore_reads_and_mask_embedded_versions() {
    let src = source();
    let to = src.loaded("to", &[src.record(false)]);
    let mut s = Store::open(&to).unwrap();
    let r = s.reader();
    let v = src.secret.version_hash();
    // the residue restores the block's identity; its content reads as removed
    let stub = r.get_block(&v).unwrap();
    assert_eq!(stub.body, CONTENT_REMOVED);
    assert_eq!(stub.id, src.secret.id);
    assert_eq!(stub.version_hash(), v);
    assert_eq!(
        r.redaction_of(kinds::BLOCK, &v).unwrap(),
        Some(source_request())
    );
    assert!(matches!(
        r.get_raw(&v),
        Err(StoreError::Redacted { request }) if request == source_request()
    ));
    // the change replays from its residue, its embedded secret masked, the other block intact
    let c = r.change(&src.change_id).unwrap();
    let written: Vec<Block> = c
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Create { block } => Some(block.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(written.len(), 2);
    assert_eq!(written[0].body, CONTENT_REMOVED);
    assert_eq!(written[1], src.other);
    assert_eq!(r.get_block(&src.other.version_hash()).unwrap(), src.other);
    assert_eq!(r.get_block(&src.kept.version_hash()).unwrap(), src.kept);
    // a new write never brings the removed version back, alone or embedded
    let block_bytes = postcard::to_allocvec(&src.secret).unwrap();
    assert!(matches!(
        s.write(vec![(kinds::BLOCK, block_bytes)]),
        Err(StoreError::Redacted { request }) if request == source_request()
    ));
    // a raw change that embeds the removed version, with no BLOCK frame of its own
    let again = change(
        "other",
        vec![Op::Create {
            block: src.secret.clone(),
        }],
    );
    let end = s.log_end();
    let got = s.write(vec![(
        kinds::CHANGE,
        postcard::to_allocvec(&again).unwrap(),
    )]);
    assert!(
        matches!(got, Err(StoreError::Redacted { request }) if request == source_request()),
        "{got:?}"
    );
    assert_eq!(s.log_end(), end);
}

#[test]
fn hc3_reindex_and_missing_index_keep_carried_authority() {
    let src = source();
    let to = src.loaded("to", &[src.record(false)]);
    let first = {
        let mut s = Store::open(&to).unwrap();
        let first = s.index_dump_for_tests().unwrap();
        s.reindex().unwrap();
        assert_eq!(s.index_dump_for_tests().unwrap(), first);
        s.check_redaction_index_for_tests().unwrap();
        s.reader().check_objects().unwrap();
        first
    };
    std::fs::remove_file(to.join("index.redb")).unwrap();
    let s = Store::open(&to).unwrap();
    assert_eq!(s.index_dump_for_tests().unwrap(), first);
    let r = s.reader();
    assert_eq!(
        r.get_block(&src.secret.version_hash()).unwrap().body,
        CONTENT_REMOVED
    );
    assert_eq!(
        proof_at(&s, src.secret_frame().loc),
        Some(carried_hash(&src.record(false)))
    );
    assert_eq!(
        r.redaction_of(kinds::BLOCK, &src.secret.version_hash())
            .unwrap(),
        Some(source_request())
    );
}

/// One damaged carried log: what it changes in the source (closed), and where the open refuses.
struct Case {
    name: &'static str,
    damage: Box<dyn Fn(&Src, &Path) -> Location>,
}

/// Appends `records` (with the source's residues) as one sealed batch to the closed store at
/// `dir`, then zeroes the secret's block and change frames when `zeros`. Returns where each
/// record went.
fn carry_raw(src: &Src, dir: &Path, records: &[RedactionCarried], zeros: bool) -> Vec<Location> {
    let frames = carried_frames(&src.residues(), records);
    let placed = append_raw(dir, &frames, true);
    if zeros {
        zero(dir, &src.secret_frame());
        zero(dir, &src.change_frame());
    }
    placed[placed.len() - records.len()..].to_vec()
}

fn with_span(src: &Src, f: impl Fn(&mut rdy_model::redaction::CarriedSpan)) -> RedactionCarried {
    let mut rec = src.record(false);
    let at = rec
        .spans
        .iter()
        .position(|s| s.kind == kinds::BLOCK)
        .unwrap();
    f(&mut rec.spans[at]);
    rec
}

#[test]
fn hc3_carried_corruption_table() {
    let src = source();
    let s_loc = src.secret_frame().loc;
    let place = |l: Location| rdy_model::redaction::FramePlace {
        segment: l.segment,
        offset: l.offset,
    };
    let cases: Vec<Case> = vec![
        Case {
            name: "wrong place",
            damage: Box::new(move |src, dir| {
                // the record explains no zero at the secret's frame, so its zeros are the
                // first invalid frame
                let rec = with_span(src, |s| s.place.offset += 1);
                carry_raw(src, dir, &[rec], true);
                s_loc
            }),
        },
        Case {
            name: "wrong kind",
            damage: Box::new(move |src, dir| {
                let rec = with_span(src, |s| s.kind = kinds::CHANGE);
                carry_raw(src, dir, &[rec], true);
                s_loc
            }),
        },
        Case {
            name: "kind that is never removed",
            damage: Box::new(|src, dir| {
                let landing = *src
                    .frames
                    .iter()
                    .find(|f| f.kind == kinds::LANDING)
                    .unwrap();
                let mut rec = src.record(false);
                rec.spans.push(span(&landing, None, true));
                rec.spans.sort_by_key(|s| s.place);
                carry_raw(src, dir, &[rec], true);
                landing.loc
            }),
        },
        Case {
            name: "wrong hash",
            damage: Box::new(move |src, dir| {
                let rec = with_span(src, |s| s.hash = Hash::of(b"another block"));
                carry_raw(src, dir, &[rec], true);
                s_loc
            }),
        },
        Case {
            name: "wrong length",
            damage: Box::new(move |src, dir| {
                let rec = with_span(src, |s| s.len += 1);
                carry_raw(src, dir, &[rec], true);
                s_loc
            }),
        },
        Case {
            name: "wrong residue",
            damage: Box::new(move |src, dir| {
                let rc = residue_hash(&change_residue(&src.change));
                let rec = with_span(src, |s| s.residue = Some(rc));
                carry_raw(src, dir, &[rec], true);
                s_loc
            }),
        },
        Case {
            name: "missing residue",
            damage: Box::new(move |src, dir| {
                let rec = with_span(src, |s| s.residue = None);
                carry_raw(src, dir, &[rec], true);
                s_loc
            }),
        },
        Case {
            name: "overlap",
            damage: Box::new(move |src, dir| {
                let mut rec = src.record(false);
                let inside = rdy_model::redaction::CarriedSpan {
                    place: place(Location {
                        segment: s_loc.segment,
                        offset: s_loc.offset + 1,
                    }),
                    kind: kinds::BLOB_CHUNK,
                    hash: Hash::of(b"inside"),
                    len: 1,
                    residue: None,
                    held: true,
                };
                rec.spans.push(inside);
                rec.spans.sort_by_key(|s| s.place);
                carry_raw(src, dir, &[rec], true);
                Location {
                    segment: s_loc.segment,
                    offset: s_loc.offset + 1,
                }
            }),
        },
        Case {
            name: "unsealed record",
            damage: Box::new(|src, dir| {
                let frames = carried_frames(&src.residues(), &[src.record(false)]);
                append_raw(dir, &frames, false);
                zero(dir, &src.secret_frame());
                zero(dir, &src.change_frame());
                src.secret_frame().loc.min(src.change_frame().loc)
            }),
        },
        Case {
            name: "false flag over intact data",
            damage: Box::new(|src, dir| {
                carry_raw(src, dir, &[src.record(false)], false);
                src.secret_frame().loc.min(src.change_frame().loc)
            }),
        },
        Case {
            name: "conflicting provenance",
            damage: Box::new(|src, dir| {
                let first = src.record(false);
                let mut second = first.clone();
                second.approved_by.at_ms += 1;
                let at = carry_raw(src, dir, &[first, second], true);
                at[1]
            }),
        },
        Case {
            name: "no person approved it",
            damage: Box::new(|src, dir| {
                let mut rec = src.record(false);
                rec.approved_by.role = Role::Writer;
                carry_raw(src, dir, &[rec], true)[0]
            }),
        },
    ];
    for case in &cases {
        let dir = src.at(&format!("case-{}", case.name.replace(' ', "-")));
        copy_store(&src.path, &dir);
        let want = (case.damage)(&src, &dir);
        let bytes = log_bytes(&dir);
        let got = Store::open(&dir)
            .err()
            .unwrap_or_else(|| panic!("{} opens", case.name));
        assert_eq!(corrupt_at(got), want, "{}", case.name);
        assert_eq!(log_bytes(&dir), bytes, "{} changed the log", case.name);
    }
    // the same records, valid, open; an identical duplicate is harmless provenance
    let dir = src.at("valid");
    copy_store(&src.path, &dir);
    carry_raw(&src, &dir, &[src.record(false), src.record(false)], true);
    let s = Store::open(&dir).unwrap();
    assert_eq!(carried_of(&s.reader()).len(), 1);
}

/// The full dump header of `batches`.
fn dump_header(repo: RepoId, batches: &[DumpBatch]) -> rdy_store::dump::DumpHeader {
    let last = batches.last().unwrap();
    rdy_store::dump::DumpHeader {
        store_format: store_format(),
        log_end: batch_end(last).unwrap(),
        rdy: "test".into(),
        chain: Some(DumpChainHeader::full(repo, BatchMarker::of(last))),
    }
}

#[test]
fn hc3_dump_of_carried_store_loads_again() {
    let src = source();
    let rec = src.record(false);
    let to = src.loaded("to", &[rec.clone()]);
    let (repo, batches, index) = {
        let s = Store::open(&to).unwrap();
        let mut batches = vec![];
        s.reader()
            .for_each_batch(s.log_end(), |b| {
                batches.push(b);
                Ok(())
            })
            .unwrap();
        (
            s.repo_id().unwrap().unwrap(),
            batches,
            s.index_dump_for_tests().unwrap(),
        )
    };
    // the dump names the carried record as the proof of each zero it explains
    let dropped: Vec<(u8, Hash)> = batches
        .iter()
        .flat_map(|b| &b.frames)
        .filter_map(|f| match f {
            Frame::Dropped { kind, apply, .. } => Some((*kind, *apply)),
            _ => None,
        })
        .collect();
    let c = carried_hash(&rec);
    assert_eq!(dropped.len(), 2);
    assert!(dropped.iter().all(|(_, a)| *a == c));
    let mut tally = RemovalTally::default();
    let mut w = DumpWriter::new(Vec::new(), &dump_header(repo, &batches)).unwrap();
    for b in &batches {
        tally.observe(b).unwrap();
        w.batch(b).unwrap();
    }
    let bytes = w.finish(tally.facts().unwrap()).unwrap().0;
    let (mut reader, _, _) = DumpReader::open(&bytes[..]).unwrap();
    let mut read = vec![];
    while let Some(item) = reader.next().unwrap() {
        if let DumpItem::Batch(b) = item {
            read.push(b);
        }
    }
    assert_eq!(read, batches);
    let again = src.at("again");
    let mut i = Ingest::create(&again).unwrap();
    for (n, b) in read.iter().enumerate() {
        i.batch(n as u64 + 1, b).unwrap();
    }
    i.upgrade(store_format()).unwrap();
    i.finish().unwrap();
    assert_eq!(log_bytes(&again), log_bytes(&to));
    let s = Store::open(&again).unwrap();
    assert_eq!(s.index_dump_for_tests().unwrap(), index);
    assert_eq!(
        s.reader()
            .get_block(&src.secret.version_hash())
            .unwrap()
            .body,
        CONTENT_REMOVED
    );
    // a dropped frame whose carried proof names another span is refused by the load
    let mut forged = batches.clone();
    for b in forged.iter_mut() {
        for f in b.frames.iter_mut() {
            if let Frame::Dropped { residue, .. } = f {
                *residue = Some(Hash::of(b"another residue"));
            }
        }
    }
    let mut i = Ingest::create(&src.at("forged")).unwrap();
    for (n, b) in forged.iter().enumerate() {
        i.batch(n as u64 + 1, b).unwrap();
    }
    i.upgrade(store_format()).unwrap();
    assert!(matches!(i.finish(), Err(StoreError::Dump { .. })));
}

/// The closure of `tag` in `reader`, written as one sync batch.
fn stream(
    reader: &StoreReader,
    tag: Hash,
) -> Result<Vec<u8>, rdy_store::stream_error::StreamError> {
    let c = closure(reader, tag, &Default::default())?;
    let header =
        StreamHeader::tag_history(reader.repo_id().unwrap().unwrap(), tag, c.prerequisites);
    let mut source = StoreSource {
        reader,
        end: END_OF_LOG,
    };
    Ok(write_batch(&mut source, &header, &c.records, Vec::new())?.0)
}

fn incoming(
    _: &mut dyn rdy_store::sync_proof::ObjectSource,
    _: &rdy_store::take_batch::UploadFacts,
) -> Result<rdy_store::take_batch::TagChoice, rdy_store::stream_error::StreamError> {
    Ok(rdy_store::take_batch::TagChoice::Incoming)
}

/// A new holder at `holder` that took the history of `tag` from the source.
fn take_into(src: &Src, holder: &Path, tag: Hash) {
    let s = Store::open(&src.path).unwrap();
    let r = s.reader();
    let file = src.at("upload.rdysync");
    std::fs::write(&file, stream(&r, tag).unwrap()).unwrap();
    let t: Tag = r.get_typed(kinds::TAG, &tag).unwrap();
    let repo = s.repo_id().unwrap().unwrap();
    drop(r);
    drop(s);
    let mut h = Store::create_holder(holder, repo).unwrap();
    h.take_batches(
        &[std::fs::File::open(&file).unwrap()],
        &rdy_store::take_batch::UploadTarget {
            tag: t.name,
            landing: t.landing,
        },
        incoming,
    )
    .unwrap();
}

#[test]
fn hc3_holder_uses_local_spans_without_canon() {
    let src = source();
    // a holder that took v1 holds the secret at its own places
    let holder = src.at("holder");
    take_into(&src, &holder, src.v1);
    let frames = framed_at(&holder);
    let secret = find(&frames, kinds::BLOCK, src.secret.version_hash());
    let change_frame = find(&frames, kinds::CHANGE, src.change_id);
    assert_ne!(
        secret.loc,
        src.secret_frame().loc,
        "the holder's own places"
    );
    let mut spans = vec![
        span(&secret, Some(&block_residue(&src.secret)), false),
        span(&change_frame, Some(&change_residue(&src.change)), false),
    ];
    spans.sort_by_key(|s| s.place);
    let rec = record(src.targets(), spans);
    let to = src.at("holder-carried");
    load(&holder, &to, &src.residues(), &[rec.clone()]).unwrap();
    let s = Store::open(&to).unwrap();
    let r = s.reader();
    assert_eq!(r.mode().unwrap(), rdy_store::holder::StoreMode::Holder);
    assert_eq!(r.get_ref("canon").unwrap(), None);
    assert_eq!(
        r.get_block(&src.secret.version_hash()).unwrap().body,
        CONTENT_REMOVED
    );
    assert_eq!(r.carried_counts(&rec).unwrap(), (0, 2));
    // a carried record of the source's places is no span here
    let wrong = src.at("holder-source-places");
    let e = load(&holder, &wrong, &src.residues(), &[src.record(false)]).unwrap_err();
    assert!(matches!(e, StoreError::RedactionCorrupt { .. }), "{e:?}");
    // the holder's export of v1 is refused with the holder's fix
    let e = stream(&r, src.v1).unwrap_err();
    assert_eq!(e.code, "E_REDACTED");
    assert_eq!(e.fix, HOLDER_FIX);
}

#[test]
fn hc3_sync_never_treats_carried_as_approval() {
    let src = source();
    let rec = src.record(false);
    let bytes = postcard::to_allocvec(&rec).unwrap();
    // a carried record is no portable object: it has no links and never travels
    let e = refs_of(kinds::REDACTION_CARRIED, &bytes).unwrap_err();
    assert_eq!(e.to_string(), "unknown kind");
    let to = src.loaded("to", &[rec.clone()]);
    let s = Store::open(&to).unwrap();
    let r = s.reader();
    let c = carried_hash(&rec);
    // it is no removal proof
    let mut source = StoreSource {
        reader: &r,
        end: END_OF_LOG,
    };
    assert!(rdy_store::sync_proof::check(&mut source, c).is_err());
    // a tag that reaches the content it names is refused, with the Project's fix
    let e = stream(&r, src.v1).unwrap_err();
    assert_eq!(e.code, "E_REDACTED");
    assert_eq!(
        e.message,
        format!(
            "Tag v1 reaches content that carried record {c} names, and no removal in this store covers it."
        )
    );
    assert_eq!(e.fix, PROJECT_FIX);
    // another tag exports as before, with no carried record and no placeholder naming one
    let bytes = stream(&r, src.v0).unwrap();
    let (mut sr, _) = StreamReader::open(&bytes[..]).unwrap();
    while let Some(rec) = sr.next().unwrap() {
        assert_ne!(rec.key().kind, kinds::REDACTION_CARRIED);
        if let StreamRecord::Dropped { apply, .. } = rec {
            assert_ne!(apply, c);
        }
        assert_ne!(
            rec.key(),
            ObjectKey {
                kind: kinds::BLOCK,
                hash: src.secret.version_hash()
            }
        );
    }
    sr.finish().unwrap();
}

/// A dump batch of `n` chunk frames of 4 bytes each, starting at `start`.
fn chunk_batch(start: Location, n: u32) -> (DumpBatch, Vec<Framed>) {
    let mut frames = Vec::with_capacity(n as usize);
    let mut placed = Vec::with_capacity(n as usize);
    let mut at = start;
    for i in 0..n {
        let payload = i.to_le_bytes().to_vec();
        let hash = Hash::of(&payload);
        placed.push(Framed {
            loc: at,
            kind: kinds::BLOB_CHUNK,
            hash,
            len: 4,
        });
        at.offset += HEADER + 4;
        frames.push(Frame::Present {
            kind: kinds::BLOB_CHUNK,
            hash,
            payload,
        });
    }
    let digest = batch_digest(placed.iter().map(|f| &f.hash));
    (
        DumpBatch {
            start,
            digest,
            frames,
        },
        placed,
    )
}

#[test]
fn hc3_carried_bounds_refuse_before_append() {
    let src = source();
    assert_eq!(limit::CARRIED_SPANS, "100000 carried spans");
    let at = limit::ALL.iter().position(|l| *l == limit::RECORD).unwrap();
    assert_eq!(limit::ALL[at + 1], limit::CARRIED_SPANS);
    let to = src.at("to");
    let mut i = Ingest::create(&to).unwrap();
    let batches = carried_fixture::batches_with_drops(&src.path, &[]);
    for (n, b) in batches.iter().enumerate() {
        i.batch(n as u64 + 1, b).unwrap();
    }
    let (chunks, placed) = chunk_batch(i.end(), 100_001);
    i.batch(batches.len() as u64 + 1, &chunks).unwrap();
    let blob = vec![RedactTarget::Blob(Hash::of(b"a blob"))];
    let over = record(
        blob.clone(),
        placed.iter().map(|f| span(f, None, true)).collect(),
    );
    assert_eq!(over.spans.len(), 100_001);
    let end = i.end();
    let bytes = log_bytes(&to);
    let e = i.append_carried(&[], &[over]).unwrap_err();
    assert!(matches!(
        e,
        StoreError::RedactionLimit { limit } if limit == limit::CARRIED_SPANS
    ));
    assert_eq!(e.to_string(), "The redaction exceeds 100000 carried spans.");
    assert_eq!(i.end(), end);
    assert_eq!(log_bytes(&to), bytes);
    // 100,000 physical spans fit, and the store opens with them
    let at_bound = record(
        blob,
        placed[..100_000]
            .iter()
            .map(|f| span(f, None, true))
            .collect(),
    );
    i.append_carried(&[], &[at_bound.clone()]).unwrap();
    i.upgrade(store_format()).unwrap();
    i.finish().unwrap();
    let s = Store::open(&to).unwrap();
    assert_eq!(s.reader().carried_counts(&at_bound).unwrap(), (100_000, 0));
}

#[test]
fn hc3_nested_carried_recognition_keeps_earlier_bytes() {
    let src = source();
    // the earlier record holds the secret's frames and the other block
    let r = src.residues();
    let mut spans = vec![
        span(&src.secret_frame(), Some(&r[0]), true),
        span(&src.change_frame(), Some(&r[1]), true),
        span(&src.other_frame(), Some(&block_residue(&src.other)), true),
    ];
    spans.sort_by_key(|s| s.place);
    let earlier = record(src.targets(), spans);
    let to = src.at("to");
    load(
        &src.path,
        &to,
        &[r[0].clone(), r[1].clone(), block_residue(&src.other)],
        &[earlier.clone()],
    )
    .unwrap();
    let earlier_at = carried_of(&Store::open(&to).unwrap().reader())[0].at;
    let earlier_bytes = |dir: &Path| {
        let seg = &log_bytes(dir)[&format!("seg-{:06}.log", earlier_at.segment)];
        let start = (earlier_at.offset + HEADER) as usize;
        seg[start..start + postcard::to_allocvec(&earlier).unwrap().len()].to_vec()
    };
    let kept_bytes = earlier_bytes(&to);
    // the later removal zeroed the secret's frames, and a later record recognizes the zeros
    let mut later = src.record(false);
    later.apply = Hash::of(b"later source apply");
    later.request = Hash::of(b"later source request");
    carry_raw(&src, &to, &[later.clone()], true);
    {
        let mut s = Store::open(&to).unwrap();
        s.finish_redactions().unwrap();
        let rd = s.reader();
        assert_eq!(
            rd.get_block(&src.secret.version_hash()).unwrap().body,
            CONTENT_REMOVED
        );
        // the earlier record is kept as it was, and now counts its zeros as removed
        let all = carried_of(&rd);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].record, earlier);
        assert_eq!(rd.carried_counts(&earlier).unwrap(), (1, 2));
        // neither record grants erasure: the other block, held, is intact after the finish
        assert_eq!(rd.get_block(&src.other.version_hash()).unwrap(), src.other);
        assert_eq!(
            proof_at(&s, src.secret_frame().loc),
            Some(carried_hash(&later))
        );
    }
    assert_eq!(earlier_bytes(&to), kept_bytes);
    // and a nonzero byte under the later record refuses the open; it is not erased
    let bad = src.at("bad");
    copy_store(&to, &bad);
    poke(&bad, &src.change_frame(), 0, 9);
    let damaged = log_bytes(&bad);
    assert_eq!(
        corrupt_at(Store::open(&bad).err().unwrap()),
        src.change_frame().loc
    );
    assert_eq!(log_bytes(&bad), damaged);
}
