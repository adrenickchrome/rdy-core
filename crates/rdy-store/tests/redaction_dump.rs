//! rdx9: dump format 2 carries removed payloads (plan Redaction, Task 9; D-3M81W: rdy "drops their
//! bytes and keeps their hashes"; D-69AY3: every released dump format stays readable; D-F0904: no
//! old kind is re-encoded).
//!
//! A frame whose payload an applied redaction removed travels as one `DROPPED` (0xF3) container
//! record of 102 bytes: its original kind, hash and length, the apply that authorized it and its
//! residue. The batch digests cover the original `(kind, hash)`; the trailer covers the container
//! record. The ingest writes the original header and zeros at the original place, and checks the
//! forward authorization against the complete stream before it finishes the store.
//!
//! Every source holds the shared raw authorized batch (`redaction_fixture`), written while the
//! store is closed; the next open finishes the removal.
#[path = "support/redaction.rs"]
mod redaction_fixture;

use rdy_model::redaction::{RedactTarget, RedactionApply};
use rdy_model::*;
use rdy_store::dump::*;
use rdy_store::format::STORE_FORMAT;
use rdy_store::ingest::Ingest;
use rdy_store::kinds::{self, BLOB_CHUNK, BLOCK, CHANGE, batch_digest};
use rdy_store::segment::Location;
use rdy_store::{Store, StoreError};
use redaction_fixture::{
    Fixture, FixtureOptions, LogFrame, Proof, RequestSpec, authorized_batch_fixture, enc, frames,
    segment_bytes,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The removed block's body; no dump of the redacted source may hold it.
const CANARY: &[u8] = b"canary alpha secret";
/// The length of a `DROPPED` record's payload.
const DROPPED_LEN: usize = 102;
/// The length of a container record's frame header.
const HEAD: usize = 37;

const ALLOWED: &str = "dropped frame is not allowed in this dump format";
const WRONG_LENGTH: &str = "dropped frame has the wrong length";
const INVALID_KIND: &str = "dropped frame has an invalid kind";
const INVALID_RESIDUE: &str = "dropped frame has an invalid residue";
const NO_AUTHORIZATION: &str = "dropped frame has no valid authorization";
const SPAN: &str = "dropped frame does not match its authorized span";
const ENDS_EARLY: &str = "the file ends before its trailer";
const TRAILER_MISMATCH: &str = "the trailer's counts or digest do not match the dump";

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
        provenance: Provenance {
            actor: "agent:w".into(),
            run: Some("w1".into()),
            role: Role::Writer,
            at_ms: 10,
        },
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// A redacted source: one change wrote the secret block `a` and the bystander `b` in segment 0,
/// a two-chunk blob rolled to segment 1, filler blobs rolled on, and one request removed `a` and
/// the blob. Its removed frames are a block, a change and two chunks, all in sealed segments.
struct Src {
    _dir: tempfile::TempDir,
    path: PathBuf,
    fixture: Fixture,
    end: Location,
    batches: Vec<DumpBatch>,
    frames: Vec<LogFrame>,
}

fn source() -> Src {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("src");
    let mut s = Store::create(&path).unwrap();
    s.set_segment_max(4096);
    let a = block("b-00001", "canary alpha secret");
    let b = block("b-00002", "bravo stays");
    s.commit_to_view(
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
    let mut bytes = b"blob canary ".repeat(100_000);
    bytes.truncate(1024 * 1024 + 4096); // two chunks
    let blob = s.put_blob(&bytes).unwrap();
    for i in 0..3 {
        s.put_blob(format!("filler {i} {}", "z".repeat(3000)).as_bytes())
            .unwrap();
    }
    drop(s);
    let spec = RequestSpec::new(
        "work",
        vec![
            RedactTarget::Version {
                id: a.id.clone(),
                version: a.version_hash(),
            },
            RedactTarget::Blob(blob),
        ],
    );
    let fixture = authorized_batch_fixture(
        &path,
        &[spec],
        &FixtureOptions::approved(Proof::local_human()),
    );
    // the open finishes the removal
    let s = Store::open(&path).unwrap();
    let end = s.log_end();
    let mut batches = vec![];
    s.reader()
        .for_each_batch(end, |b| {
            batches.push(b);
            Ok(())
        })
        .unwrap();
    let frames = frames(&s.reader(), end);
    drop(s);
    Src {
        _dir: dir,
        path,
        fixture,
        end,
        batches,
        frames,
    }
}

impl Src {
    /// The source's batches as a dump of this rdy.
    fn dump(&self) -> Vec<u8> {
        write_dump(self.end, &self.batches)
    }

    /// Where each dropped frame of the source is: (batch index, frame index).
    fn dropped(&self) -> Vec<(usize, usize)> {
        let mut out = vec![];
        for (i, b) in self.batches.iter().enumerate() {
            for (j, f) in b.frames.iter().enumerate() {
                if matches!(f, Frame::Dropped { .. }) {
                    out.push((i, j));
                }
            }
        }
        out
    }

    /// The first dropped frame of `kind`: (batch index, frame index).
    fn dropped_of(&self, kind: u8) -> (usize, usize) {
        *self
            .dropped()
            .iter()
            .find(|(i, j)| self.batches[*i].frames[*j].kind() == kind)
            .unwrap()
    }
}

fn header(end: Location) -> DumpHeader {
    DumpHeader {
        store_format: STORE_FORMAT,
        log_end: end,
        rdy: "test".into(),
    }
}

fn write_dump(end: Location, batches: &[DumpBatch]) -> Vec<u8> {
    let mut w = DumpWriter::new(Vec::new(), &header(end)).unwrap();
    for b in batches {
        w.batch(b).unwrap();
    }
    w.finish().unwrap().0
}

fn read_dump(bytes: &[u8]) -> Result<(u32, DumpHeader, Vec<DumpBatch>), StoreError> {
    let (mut r, format, h) = DumpReader::open(bytes)?;
    let mut out = vec![];
    while let Some(item) = r.next()? {
        if let DumpItem::Batch(b) = item {
            out.push(b);
        }
    }
    Ok((format, h, out))
}

/// Ingests `batches` into the new store directory `to` and finishes it.
fn ingest(to: &Path, batches: &[DumpBatch]) -> Result<rdy_store::ingest::IngestReport, StoreError> {
    let mut i = Ingest::create(to)?;
    for (n, b) in batches.iter().enumerate() {
        i.batch(n as u64 + 1, b)?;
    }
    i.upgrade(STORE_FORMAT)?;
    i.finish()
}

fn what(e: StoreError) -> String {
    match e {
        StoreError::Dump { what, .. } => what,
        other => panic!("not a dump refusal: {other:?}"),
    }
}

/// The records of a dump after its prelude, as (kind, payload).
fn records(good: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut at = 12usize;
    let mut out = vec![];
    while at < good.len() {
        let len = u32::from_le_bytes(good[at..at + 4].try_into().unwrap()) as usize;
        out.push((good[at + 4], good[at + HEAD..at + HEAD + len].to_vec()));
        at += HEAD + len;
    }
    out
}

/// One record framed as a store frame, with its true hash.
fn record(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut r = (payload.len() as u32).to_le_bytes().to_vec();
    r.push(kind);
    r.extend_from_slice(&Hash::of(payload).0);
    r.extend_from_slice(payload);
    r
}

/// A stream of hash-valid `recs` behind the prelude of `dump_format`.
fn forge(dump_format: u32, recs: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&dump_format.to_le_bytes());
    for (k, p) in recs {
        out.extend(record(*k, p));
    }
    out
}

/// `recs` with its last record (the trailer) made true again: every record before it counted
/// and chained as `kind || len (u64) || hash`.
fn reseal(recs: &mut [(u8, Vec<u8>)]) {
    let n = recs.len() - 1;
    let mut chain = vec![];
    for (k, p) in &recs[..n] {
        chain.push(*k);
        chain.extend_from_slice(&(p.len() as u64).to_le_bytes());
        chain.extend_from_slice(&Hash::of(p).0);
    }
    let batches = recs[..n].iter().filter(|(k, _)| *k == rec::BATCH).count() as u64;
    let frames = recs[..n]
        .iter()
        .filter(|(k, _)| *k < rec::HEADER || *k == rec::DROPPED)
        .count() as u64;
    let files = recs[..n].iter().filter(|(k, _)| *k == rec::FILE).count() as u32;
    let mut p = batches.to_le_bytes().to_vec();
    p.extend_from_slice(&frames.to_le_bytes());
    p.extend_from_slice(&files.to_le_bytes());
    p.extend_from_slice(&Hash::of(&chain).0);
    recs[n].1 = p;
}

/// The 102 payload bytes of a `DROPPED` record.
fn dropped_bytes(kind: u8, hash: Hash, len: u32, apply: Hash, residue: Option<Hash>) -> Vec<u8> {
    let mut p = vec![kind];
    p.extend_from_slice(&hash.0);
    p.extend_from_slice(&len.to_le_bytes());
    p.extend_from_slice(&apply.0);
    p.push(u8::from(residue.is_some()));
    p.extend_from_slice(&residue.map_or([0u8; 32], |h| h.0));
    p
}

/// The index of the `BATCH` record whose frames hold record `i`, and its number (from 1).
fn batch_of(recs: &[(u8, Vec<u8>)], i: usize) -> (usize, usize) {
    let j = (0..i).rev().find(|j| recs[*j].0 == rec::BATCH).unwrap();
    let n = recs[..=j].iter().filter(|(k, _)| *k == rec::BATCH).count();
    (j, n)
}

/// The batch digest a `BATCH` record carries at bytes 16..48.
fn set_digest(b: &mut DumpBatch) {
    b.digest = batch_digest(b.frames.iter().map(|f| f.hash()));
}

/// True when the directory `to` is no store: the ingest wrote no `FORMAT`, so nothing opens it.
fn no_store(to: &Path) -> bool {
    !to.join(rdy_store::format::FORMAT_FILE).exists() && Store::open(to).is_err()
}

#[test]
fn rdx9_a_dropped_record_preserves_original_offsets_and_hashes() {
    let src = source();
    // one block, one change and the blob's chunks travel as dropped records
    let dropped: Vec<&Frame> = src
        .dropped()
        .iter()
        .map(|(i, j)| &src.batches[*i].frames[*j])
        .collect();
    let kinds_of: Vec<u8> = dropped.iter().map(|f| f.kind()).collect();
    assert_eq!(kinds_of, [BLOCK, CHANGE, BLOB_CHUNK, BLOB_CHUNK]);
    for f in &dropped {
        assert!(f.present_payload().is_none());
        let Frame::Dropped { apply, residue, .. } = f else {
            unreachable!()
        };
        assert_eq!(*apply, src.fixture.applies[0].0, "the first valid apply");
        assert_eq!(residue.is_some(), f.kind() != BLOB_CHUNK);
    }
    // across a segment roll: the spans sit in two sealed segments
    let segs: std::collections::BTreeSet<u32> = src
        .dropped()
        .iter()
        .map(|(i, _)| src.batches[*i].start.segment)
        .collect();
    assert!(
        segs.len() >= 2 && !segs.contains(&src.end.segment),
        "{segs:?}"
    );
    // the dump round-trips every original frame and holds no removed byte
    let bytes = src.dump();
    assert!(bytes.starts_with(b"RDYDUMP\n\x02\x00\x00\x00"));
    assert!(
        !bytes.windows(CANARY.len()).any(|w| w == CANARY),
        "the dump holds the removed block's body"
    );
    let recs = records(&bytes);
    let containers: Vec<&(u8, Vec<u8>)> = recs.iter().filter(|(k, _)| *k == rec::DROPPED).collect();
    assert_eq!(containers.len(), 4);
    assert!(containers.iter().all(|(_, p)| p.len() == DROPPED_LEN));
    let (format, h, back) = read_dump(&bytes).unwrap();
    assert_eq!((format, h.store_format), (DUMP_FORMAT, STORE_FORMAT));
    assert_eq!(back, src.batches, "every frame, dropped ones included");
    // the load: every original position, marker and hash, byte for byte
    let to = src._dir.path().join("dst");
    let report = ingest(&to, &back).unwrap();
    assert_eq!(report.end, src.end, "the original end");
    assert_eq!(
        report.frames,
        src.frames
            .iter()
            .filter(|f| f.kind != kinds::BATCH_COMMIT)
            .count() as u64
    );
    assert_eq!(
        segment_bytes(&to),
        segment_bytes(&src.path),
        "the loaded log is the source log byte for byte"
    );
    let t = Store::open(&to).unwrap();
    let got = frames(&t.reader(), t.log_end());
    assert_eq!(got.len(), src.frames.len());
    for (a, b) in got.iter().zip(&src.frames) {
        assert_eq!(
            (a.loc, a.kind, a.hash, a.len, &a.payload),
            (b.loc, b.kind, b.hash, b.len, &b.payload)
        );
    }
    for (i, j) in src.dropped() {
        let f = &src.batches[i].frames[j];
        assert!(
            t.has_kind(f.kind(), f.hash()).unwrap(),
            "{} stays",
            f.hash()
        );
        assert!(matches!(
            t.reader().get_raw(f.hash()),
            Err(StoreError::Redacted { .. })
        ));
    }
    t.check_objects().unwrap();
}

#[test]
fn rdx9_container_and_source_digests_cover_different_records() {
    let src = source();
    let good = src.dump();
    let recs = records(&good);
    let drops: Vec<usize> = (0..recs.len())
        .filter(|i| recs[*i].0 == rec::DROPPED)
        .collect();
    assert_eq!(drops.len(), 4);
    let read = |bytes: &[u8]| read_dump(bytes).map(|_| ());
    for &i in &drops {
        // the container layer: any payload bit, with the record's hash left as it was
        let at = 12 + recs[..i].iter().map(|(_, p)| HEAD + p.len()).sum::<usize>();
        for bit in 0..DROPPED_LEN * 8 {
            let mut bad = good.clone();
            bad[at + HEAD + bit / 8] ^= 1 << (bit % 8);
            assert_eq!(
                what(read(&bad).unwrap_err()),
                "a record's hash does not match its bytes",
                "record {i}, bit {bit}"
            );
        }
        let (_, n) = batch_of(&recs, i);
        // the source layer: the original kind and hash are the batch digests'
        // (another removable kind that names a residue, so the record itself stays valid)
        let other = if recs[i].1[0] == BLOCK { CHANGE } else { BLOCK };
        let mut r = recs.clone();
        r[i].1[0] = other;
        r[i].1[69] = 1;
        if r[i].1[70..].iter().all(|b| *b == 0) {
            r[i].1[70] = 1;
        }
        assert_eq!(
            what(read(&forge(DUMP_FORMAT, &r)).unwrap_err()),
            format!("batch {n} does not match its digest"),
            "record {i}: original kind"
        );
        for byte in [1usize, 32] {
            let mut r = recs.clone();
            r[i].1[byte] ^= 1;
            assert_eq!(
                what(read(&forge(DUMP_FORMAT, &r)).unwrap_err()),
                format!("batch {n} does not match its digest"),
                "record {i}: original hash byte {byte}"
            );
        }
        // the original length, apply and residue: only the trailer covers the container record
        for (byte, label) in [(33usize, "length"), (41, "apply"), (101, "residue")] {
            let mut r = recs.clone();
            r[i].1[byte] ^= 1;
            if label == "residue" && r[i].1[69] == 0 {
                continue; // a chunk names no residue
            }
            assert_eq!(
                what(read(&forge(DUMP_FORMAT, &r)).unwrap_err()),
                TRAILER_MISMATCH,
                "record {i}: original {label}"
            );
            // resealed, the stream reads, and the ingest refuses it before the store is placed
            reseal(&mut r);
            let (_, _, bs) = read_dump(&forge(DUMP_FORMAT, &r)).unwrap();
            let to = src._dir.path().join(format!("dst-{i}-{label}"));
            let want = match label {
                "length" => SPAN,
                "apply" => NO_AUTHORIZATION,
                _ => INVALID_RESIDUE,
            };
            assert_eq!(
                what(ingest(&to, &bs).unwrap_err()),
                want,
                "record {i}: original {label}, resealed"
            );
            assert!(no_store(&to), "record {i}: {label}");
        }
        // a container that names a kind no redaction removes is refused on read
        let mut r = recs.clone();
        r[i].1[0] = kinds::PROPOSAL;
        assert_eq!(
            what(read(&forge(DUMP_FORMAT, &r)).unwrap_err()),
            INVALID_KIND
        );
    }
}

#[test]
fn rdx9_forward_authorization_is_checked_before_placement() {
    let src = source();
    let landing = src.batches.len() - 1;
    assert!(
        src.batches[landing]
            .frames
            .iter()
            .any(|f| f.kind() == kinds::REDACTION_APPLY),
        "the apply is in the last batch"
    );
    assert!(
        src.dropped().iter().all(|(i, _)| *i < landing),
        "every dropped frame comes before its apply"
    );
    // the valid stream loads
    let ok = src._dir.path().join("ok");
    ingest(&ok, &src.batches).unwrap();
    Store::open(&ok).unwrap().check_objects().unwrap();
    let case = |name: &str, bs: Vec<DumpBatch>, want: &str| {
        let to = src._dir.path().join(name);
        // the stream itself is sound: the writer and the reader take it
        let (_, _, back) = read_dump(&write_dump(src.end, &bs)).unwrap();
        assert_eq!(what(ingest(&to, &back).unwrap_err()), want, "{name}");
        assert!(no_store(&to), "{name}: the target holds no store");
    };
    // missing: the apply records are gone from the landing batch
    let mut bs = src.batches.clone();
    bs[landing]
        .frames
        .retain(|f| f.kind() != kinds::REDACTION_APPLY);
    set_digest(&mut bs[landing]);
    case("missing", bs, NO_AUTHORIZATION);
    // a dropped record that names another apply
    let mut bs = src.batches.clone();
    let (i, j) = src.dropped_of(BLOCK);
    if let Frame::Dropped { apply, .. } = &mut bs[i].frames[j] {
        *apply = Hash::of(b"another apply");
    }
    case("other-apply", bs, NO_AUTHORIZATION);
    // wrong span: the record's original hash is not the one its request authorized
    let mut bs = src.batches.clone();
    if let Frame::Dropped { hash, .. } = &mut bs[i].frames[j] {
        hash.0[0] ^= 1;
    }
    set_digest(&mut bs[i]);
    case("wrong-span", bs, SPAN);
    // wrong residue: not the residue its span names
    let mut bs = src.batches.clone();
    if let Frame::Dropped { residue, .. } = &mut bs[i].frames[j] {
        *residue = Some(Hash::of(b"another residue"));
    }
    case("wrong-residue", bs, INVALID_RESIDUE);
    // a policy landing: the apply names a landing no person approved
    let mut bs = src.batches.clone();
    let frames = &mut bs[landing].frames;
    let li = frames
        .iter()
        .position(|f| f.kind() == kinds::LANDING)
        .unwrap();
    let mut l: LandingRecord = postcard::from_bytes(frames[li].present_payload().unwrap()).unwrap();
    l.assurance = Assurance::Policy;
    l.relayed_by = None;
    l.relay_evidence = None;
    let lp = enc(&l);
    let lid = Hash::of(&lp);
    frames[li] = Frame::Present {
        kind: kinds::LANDING,
        hash: lid,
        payload: lp,
    };
    let mut new_apply = None;
    for f in frames.iter_mut() {
        if f.kind() == kinds::REDACTION_APPLY {
            let mut a: RedactionApply = postcard::from_bytes(f.present_payload().unwrap()).unwrap();
            a.landing = lid;
            let ap = enc(&a);
            new_apply = Some(Hash::of(&ap));
            *f = Frame::Present {
                kind: kinds::REDACTION_APPLY,
                hash: Hash::of(&ap),
                payload: ap,
            };
        }
    }
    set_digest(&mut bs[landing]);
    for (i, j) in src.dropped() {
        if let Frame::Dropped { apply, .. } = &mut bs[i].frames[j] {
            *apply = new_apply.unwrap();
        }
    }
    case("policy", bs, NO_AUTHORIZATION);
}

/// A dump of `dump_format` whose header names `store_format`: one batch at the start of the log
/// whose one data frame is a `DROPPED` record of a block, and its trailer.
fn one_drop(dump_format: u32, store_format: u32) -> Vec<u8> {
    let hash = Hash::of(b"a removed block");
    let len = 15u32;
    let drop = dropped_bytes(
        BLOCK,
        hash,
        len,
        Hash::of(b"apply"),
        Some(Hash::of(b"residue")),
    );
    let mut h = store_format.to_le_bytes().to_vec();
    h.extend_from_slice(&0u32.to_le_bytes());
    h.extend_from_slice(&(37 + u64::from(len)).to_le_bytes());
    h.extend_from_slice(&4u16.to_le_bytes());
    h.extend_from_slice(b"test");
    let mut kinds = vec![BLOCK];
    kinds.extend_from_slice(&hash.0);
    let mut b = 0u32.to_le_bytes().to_vec();
    b.extend_from_slice(&0u64.to_le_bytes());
    b.extend_from_slice(&1u32.to_le_bytes());
    b.extend_from_slice(&batch_digest([&hash]).0);
    b.extend_from_slice(&Hash::of(&kinds).0);
    let mut recs = vec![
        (rec::HEADER, h),
        (rec::BATCH, b),
        (rec::DROPPED, drop),
        (rec::TRAILER, vec![]),
    ];
    reseal(&mut recs);
    forge(dump_format, &recs)
}

#[test]
fn rdx9_a_drop_is_refused_in_every_format_one_pair() {
    let old: Vec<(u32, u32)> = READS.iter().copied().filter(|(d, _)| *d == 1).collect();
    assert_eq!(old, [(1, 10), (1, 11), (1, 12)]);
    for (d, sf) in old {
        let e = read_dump(&one_drop(d, sf)).unwrap_err();
        assert!(
            matches!(&e, StoreError::Dump { at, .. } if *at == (12 + HEAD + 22 + HEAD + 80) as u64),
            "({d}, {sf}): blamed on the dropped record: {e:?}"
        );
        assert_eq!(what(e), ALLOWED, "({d}, {sf})");
    }
    // the same record reads under the current pair, as one source frame
    let (_, _, bs) = read_dump(&one_drop(DUMP_FORMAT, STORE_FORMAT)).unwrap();
    assert_eq!(bs.len(), 1);
    assert_eq!(bs[0].frames.len(), 1);
    assert_eq!(bs[0].frames[0].original_len(), 15);
}

/// A change to a record's payload.
type ByteBend = fn(&mut Vec<u8>);
/// A change to a frame of a batch.
type FrameBend = fn(&mut Frame);

#[test]
fn rdx9_hostile_drop_lengths_and_counts_do_not_allocate_the_claim() {
    let src = source();
    let good = src.dump();
    let recs = records(&good);
    let i = recs.iter().position(|(k, _)| *k == rec::DROPPED).unwrap();
    let at = 12 + recs[..i].iter().map(|(_, p)| HEAD + p.len()).sum::<usize>();
    let read = |bytes: &[u8]| what(read_dump(bytes).unwrap_err());
    // a short record, hash-valid
    let mut r = recs.clone();
    r[i].1.pop();
    assert_eq!(read(&forge(DUMP_FORMAT, &r)), WRONG_LENGTH);
    let mut r = recs.clone();
    r[i].1.push(0);
    assert_eq!(read(&forge(DUMP_FORMAT, &r)), WRONG_LENGTH);
    // a record that claims 4 GiB and ends: refused by its length, never read or allocated
    let mut claim = good[..at].to_vec();
    claim.extend_from_slice(&u32::MAX.to_le_bytes());
    claim.push(rec::DROPPED);
    claim.extend_from_slice(&[0u8; 32]);
    assert_eq!(read(&claim), WRONG_LENGTH);
    // an ordinary frame that claims 4 GiB and ends: the end of the file, not an allocation
    let mut claim = good[..at].to_vec();
    claim.extend_from_slice(&u32::MAX.to_le_bytes());
    claim.push(BLOCK);
    claim.extend_from_slice(&[0u8; 32]);
    claim.extend_from_slice(&[1u8; 1000]);
    assert_eq!(read(&claim), ENDS_EARLY);
    // overflowing offsets: a batch start whose original frames run past u64::MAX
    let (j, _) = batch_of(&recs, i);
    let mut r = recs.clone();
    r[j].1[4..12].copy_from_slice(&(u64::MAX - 40).to_le_bytes());
    reseal(&mut r);
    assert_eq!(read(&forge(DUMP_FORMAT, &r)), SPAN);
    // the residue flag: 0 or 1; 0 with a zero hash; a block or change names one, a chunk none
    let chunk = recs
        .iter()
        .position(|(k, p)| *k == rec::DROPPED && p[0] == BLOB_CHUNK)
        .unwrap();
    let bend: [(usize, ByteBend); 5] = [
        (i, |p| p[69] = 2),
        (i, |p| p[69] = 0xFF),
        (i, |p| p[69] = 0),
        (chunk, |p| p[69] = 1),
        (chunk, |p| p[70] = 1),
    ];
    for (n, (k, b)) in bend.into_iter().enumerate() {
        let mut r = recs.clone();
        b(&mut r[k].1);
        assert_eq!(read(&forge(DUMP_FORMAT, &r)), INVALID_RESIDUE, "case {n}");
    }
    // a huge frame count with a stream that ends: no buffer of the claim
    let last = recs.iter().rposition(|(k, _)| *k == rec::BATCH).unwrap();
    let mut r = recs[..recs.len() - 1].to_vec();
    r[last].1[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(read(&forge(DUMP_FORMAT, &r)), ENDS_EARLY);
    // a stream cut inside the dropped record
    for cut in [
        at + 3,
        at + HEAD,
        at + HEAD + 50,
        at + HEAD + DROPPED_LEN - 1,
    ] {
        assert_eq!(read(&good[..cut]), ENDS_EARLY, "cut at {cut}");
    }
    // the writer refuses what the reader refuses, before it writes a byte
    let (bi, fi) = src.dropped_of(BLOCK);
    let bad: [(FrameBend, &str); 3] = [
        (
            |f| {
                if let Frame::Dropped { kind, .. } = f {
                    *kind = kinds::TAG;
                }
            },
            INVALID_KIND,
        ),
        (
            |f| {
                if let Frame::Dropped { residue, .. } = f {
                    *residue = None;
                }
            },
            INVALID_RESIDUE,
        ),
        (
            |f| {
                if let Frame::Dropped { len, .. } = f {
                    *len = u32::MAX;
                }
            },
            "",
        ),
    ];
    for (bend, want) in bad {
        let mut b = src.batches[bi].clone();
        bend(&mut b.frames[fi]);
        set_digest(&mut b);
        let mut w = DumpWriter::new(Vec::new(), &header(src.end)).unwrap();
        let r = w.batch(&b);
        if want.is_empty() {
            // a long original length is only a number: the writer takes it
            assert!(r.is_ok());
            continue;
        }
        match r {
            Err(StoreError::Refused(m)) => assert_eq!(m, want),
            other => panic!("not refused: {other:?}"),
        }
        let (bytes, t) = w.finish().unwrap();
        assert_eq!(t.batches, 0, "nothing of the refused batch was written");
        assert!(read_dump(&bytes).unwrap().2.is_empty());
    }
}

#[test]
fn rdx9_current_output_is_only_pair_two_thirteen() {
    assert_eq!((DUMP_FORMAT, STORE_FORMAT), (2, 13));
    assert_eq!(READS, &[(1, 10), (1, 11), (1, 12), (2, 13)]);
    assert_eq!(frame_kinds(1, 13), None, "no (1, 13) reader entry");
    assert_eq!(
        frame_kinds(2, 13),
        Some(
            &[
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 13, 15, 16, 17, 18, 19, 20, 21, 22
            ][..]
        )
    );
    // the format-1 pairs keep their kinds
    assert_eq!(
        frame_kinds(1, 12),
        Some(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 13, 15, 16, 17, 18, 19][..])
    );
    // no downgrade writer: a header of an older store format is refused
    for sf in [10, 11, 12] {
        let old = DumpHeader {
            store_format: sf,
            log_end: Location {
                segment: 0,
                offset: 0,
            },
            rdy: "test".into(),
        };
        match DumpWriter::new(Vec::new(), &old) {
            Err(StoreError::Refused(m)) => assert_eq!(
                m,
                format!("store format {sf} in a dump of format 2 is not one this rdy reads")
            ),
            Err(other) => panic!("store format {sf}: {other:?}"),
            Ok(_) => panic!("store format {sf} was taken"),
        }
    }
    // what this rdy writes
    let src = source();
    let bytes = src.dump();
    assert_eq!(&bytes[..12], b"RDYDUMP\n\x02\x00\x00\x00");
    let (format, h, _) = read_dump(&bytes).unwrap();
    assert_eq!((format, h.store_format), (2, 13));
}
