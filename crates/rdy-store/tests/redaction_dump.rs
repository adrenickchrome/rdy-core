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
const ALLOCATION: &str = "record allocation failed";
const BUDGET: &str = "dropped frames claim more than 1073741824 bytes before their authorization";
/// The most bytes one dropped frame may claim: the store's payload bound (`MAX_BLOB_BYTES`).
const FRAME_BOUND: u32 = 64 << 20;

/// The refusal of a dropped frame that claims `len` bytes, over the frame bound.
fn claim(len: u32) -> String {
    format!("a dropped frame claims {len} bytes; a frame holds at most 67108864")
}

/// A global allocator that fails one chosen allocation of the armed thread, so a test can make
/// any one allocation of the reader or the ingest fail, deterministically. Other threads, and
/// the armed thread once its failure has fired, allocate as usual.
mod failing_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    pub struct Failing;

    thread_local! {
        /// Allocations of this thread until the one that fails; 0: none fails.
        static LEFT: Cell<u64> = const { Cell::new(0) };
        /// Whether the armed failure fired.
        static FIRED: Cell<bool> = const { Cell::new(false) };
    }

    fn fail_now() -> bool {
        LEFT.try_with(|left| match left.get() {
            0 => false,
            1 => {
                left.set(0);
                FIRED.with(|f| f.set(true));
                true
            }
            n => {
                left.set(n - 1);
                false
            }
        })
        .unwrap_or(false)
    }

    // SAFETY: every call goes to `System` with the caller's arguments unchanged, or returns null,
    // which `GlobalAlloc` allows for a failed allocation.
    #[allow(unsafe_code)]
    unsafe impl GlobalAlloc for Failing {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if fail_now() {
                return std::ptr::null_mut();
            }
            // SAFETY: the caller's layout, as `GlobalAlloc::alloc` requires.
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            if fail_now() {
                return std::ptr::null_mut();
            }
            // SAFETY: the caller's layout, as `GlobalAlloc::alloc_zeroed` requires.
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: `ptr` came from `System` with `layout` (every allocation here is System's).
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            if fail_now() {
                return std::ptr::null_mut();
            }
            // SAFETY: `ptr` came from `System` with `layout`; the caller's new size.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    /// Makes the `nth` allocation of this thread from now fail (1 is the next one).
    pub fn arm(nth: u64) {
        FIRED.with(|f| f.set(false));
        LEFT.with(|l| l.set(nth));
    }

    /// Disarms this thread; returns whether the armed failure fired.
    pub fn disarm() -> bool {
        LEFT.with(|l| l.set(0));
        FIRED.with(|f| f.get())
    }
}

#[global_allocator]
static ALLOC: failing_alloc::Failing = failing_alloc::Failing;

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
    case("policy", policy_batches(&src), NO_AUTHORIZATION);
}

/// The batches of `src` with its landing made a policy landing, no person's approval: its apply
/// is a canonical record that names that landing, and every dropped record names that apply. No
/// authorization of the stream is valid.
fn policy_batches(src: &Src) -> Vec<DumpBatch> {
    let landing = src.batches.len() - 1;
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
    bs
}

/// A dump of `dump_format` whose header names `store_format`: one batch at the start of the log
/// whose one data frame is a `DROPPED` record of a block, and its trailer.
fn one_drop(dump_format: u32, store_format: u32) -> Vec<u8> {
    drop_stream(
        dump_format,
        store_format,
        BLOCK,
        Hash::of(b"a removed block"),
        15,
        Hash::of(b"apply"),
        Some(Hash::of(b"residue")),
    )
}

/// A dump of `dump_format` whose header names `store_format`: one batch at the start of the log
/// whose one data frame is a `DROPPED` record of `kind` that claims `len` bytes, and its trailer.
fn drop_stream(
    dump_format: u32,
    store_format: u32,
    kind: u8,
    hash: Hash,
    len: u32,
    apply: Hash,
    residue: Option<Hash>,
) -> Vec<u8> {
    let drop = dropped_bytes(kind, hash, len, apply, residue);
    let mut h = store_format.to_le_bytes().to_vec();
    h.extend_from_slice(&0u32.to_le_bytes());
    h.extend_from_slice(&(37 + u64::from(len)).to_le_bytes());
    h.extend_from_slice(&4u16.to_le_bytes());
    h.extend_from_slice(b"test");
    let mut kinds = vec![kind];
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
            // the frame bound: no stored payload is longer than 64 MiB
            "a dropped frame claims 4294967295 bytes; a frame holds at most 67108864",
        ),
    ];
    for (bend, want) in bad {
        let mut b = src.batches[bi].clone();
        bend(&mut b.frames[fi]);
        set_digest(&mut b);
        let mut w = DumpWriter::new(Vec::new(), &header(src.end)).unwrap();
        let r = w.batch(&b);
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
fn rdx9_current_output_uses_the_current_store_pair() {
    assert_eq!((DUMP_FORMAT, STORE_FORMAT), (2, 14));
    assert_eq!(READS, &[(1, 10), (1, 11), (1, 12), (2, 13), (2, 14)]);
    assert_eq!(frame_kinds(1, 13), None, "no (1, 13) reader entry");
    assert_eq!(
        frame_kinds(2, 13),
        Some(
            &[
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 13, 15, 16, 17, 18, 19, 20, 21, 22
            ][..]
        )
    );
    // the current pair: the kinds of (2, 13) and SIGNATURE (11)
    assert_eq!(
        frame_kinds(2, 14),
        Some(
            &[
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 16, 17, 18, 19, 20, 21, 22
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
    assert_eq!((format, h.store_format), (DUMP_FORMAT, STORE_FORMAT));
}

/// The bytes of the log of the store directory `to`, in all its segments.
fn log_bytes(to: &Path) -> u64 {
    segment_bytes(to).values().map(|b| b.len() as u64).sum()
}

/// A dropped blob chunk of `len` bytes, the `i`-th of a test, that names `apply`.
fn drop_chunk(i: u32, len: u32, apply: Hash) -> Frame {
    Frame::Dropped {
        kind: BLOB_CHUNK,
        hash: Hash::of(format!("chunk {i}").as_bytes()),
        len,
        apply,
        residue: None,
    }
}

/// A batch of `frames` that starts where the log of `i` ends, with its true digest.
fn batch_at(i: &Ingest, frames: Vec<Frame>) -> DumpBatch {
    let mut b = DumpBatch {
        start: i.end(),
        digest: Hash([0u8; 32]),
        frames,
    };
    set_digest(&mut b);
    b
}

#[test]
fn rdx9_reader_allocation_failure_is_a_refusal() {
    let src = source();
    // a project file, ordinary frames over many batches, and every kind of dropped record
    let mut w = DumpWriter::new(Vec::new(), &header(src.end)).unwrap();
    w.file(".rdy.yaml", b"project: t\n").unwrap();
    for b in &src.batches {
        w.batch(b).unwrap();
    }
    let bytes = w.finish().unwrap().0;
    let read_all = |bytes: &[u8]| -> Result<u64, StoreError> {
        let (mut r, _, _) = DumpReader::open(bytes)?;
        let mut items = 0u64;
        while r.next()?.is_some() {
            items += 1;
        }
        Ok(items)
    };
    let items = read_all(&bytes).unwrap();
    // each allocation of the read in turn fails: each is the dump refusal, never an abort
    let mut refused = 0;
    for nth in 1u64.. {
        failing_alloc::arm(nth);
        let got = read_all(&bytes);
        if !failing_alloc::disarm() {
            assert_eq!(got.unwrap(), items, "the read past its last allocation");
            break;
        }
        assert_eq!(what(got.unwrap_err()), ALLOCATION, "allocation {nth}");
        refused += 1;
    }
    assert!(refused > 0);
}

#[test]
fn rdx9_ingest_allocation_failure_is_a_refusal() {
    let dir = tempfile::tempdir().unwrap();
    // a dropped frame, an ordinary frame, and the apply that the dropped frame names
    let apply = b"an apply".to_vec();
    let frames = vec![
        drop_chunk(1, 100, Hash::of(&apply)),
        Frame::Present {
            kind: BLOB_CHUNK,
            hash: Hash::of(b"a chunk"),
            payload: b"a chunk".to_vec(),
        },
        Frame::Present {
            kind: kinds::REDACTION_APPLY,
            hash: Hash::of(&apply),
            payload: apply.clone(),
        },
    ];
    let mut refused = 0;
    for nth in 1u64.. {
        let mut i = Ingest::create(&dir.path().join(format!("to-{nth}"))).unwrap();
        let b = batch_at(&i, frames.clone());
        failing_alloc::arm(nth);
        let got = i.batch(1, &b);
        if !failing_alloc::disarm() {
            got.unwrap();
            break;
        }
        assert_eq!(what(got.unwrap_err()), ALLOCATION, "allocation {nth}");
        refused += 1;
    }
    assert!(refused > 0);
}

#[test]
fn rdx9_a_drop_claim_is_bounded_before_it_is_written() {
    let dir = tempfile::tempdir().unwrap();
    // the 416-byte stream: one dropped chunk claims 32 MiB and names an apply no stream holds
    let len = 32 << 20;
    let stream = drop_stream(
        DUMP_FORMAT,
        STORE_FORMAT,
        BLOB_CHUNK,
        Hash::of(b"a chunk"),
        len,
        Hash::of(b"no such apply"),
        None,
    );
    assert_eq!(stream.len(), 416);
    let (_, _, bs) = read_dump(&stream).unwrap();
    let to = dir.path().join("claim");
    let mut i = Ingest::create(&to).unwrap();
    i.batch(1, &bs[0]).unwrap();
    // it writes its one span and its marker, no more
    assert!(
        log_bytes(&to) <= HEAD as u64 + u64::from(len) + HEAD as u64 + 64,
        "{} bytes",
        log_bytes(&to)
    );
    i.upgrade(STORE_FORMAT).unwrap();
    assert_eq!(what(i.finish().unwrap_err()), NO_AUTHORIZATION);
    assert!(no_store(&to));

    // over the frame bound: refused by the reader at the record, before anything is written
    let over = |len: u32| {
        drop_stream(
            DUMP_FORMAT,
            STORE_FORMAT,
            BLOB_CHUNK,
            Hash::of(b"a chunk"),
            len,
            Hash::of(b"an apply"),
            None,
        )
    };
    let e = read_dump(&over(FRAME_BOUND + 1)).unwrap_err();
    assert!(
        matches!(&e, StoreError::Dump { at, .. } if *at == (12 + HEAD + 22 + HEAD + 80) as u64),
        "blamed on the dropped record: {e:?}"
    );
    assert_eq!(what(e), claim(FRAME_BOUND + 1));
    assert_eq!(
        what(read_dump(&over(u32::MAX)).unwrap_err()),
        claim(u32::MAX)
    );
    // the bound itself reads
    let (_, _, at_bound) = read_dump(&over(FRAME_BOUND)).unwrap();
    assert_eq!(at_bound[0].frames[0].original_len(), FRAME_BOUND);
    // and the ingest refuses it too, given a batch directly, before it writes a byte
    let to = dir.path().join("over");
    let mut i = Ingest::create(&to).unwrap();
    let b = batch_at(
        &i,
        vec![drop_chunk(1, FRAME_BOUND + 1, Hash::of(b"an apply"))],
    );
    assert_eq!(what(i.batch(1, &b).unwrap_err()), claim(FRAME_BOUND + 1));
    assert_eq!(
        log_bytes(&to),
        0,
        "nothing of the refused claim was written"
    );
    drop(i);
    assert!(no_store(&to));
}

#[test]
fn rdx9_unauthorized_claims_are_bounded_in_total() {
    let dir = tempfile::tempdir().unwrap();
    let a = Hash::of(b"apply a");
    // 16 frames of 64 MiB: exactly 1 GiB waits for its authorization, which the bound allows
    let full = |i: &Ingest, apply: Hash, from: u32| {
        batch_at(
            i,
            (from..from + 16)
                .map(|n| drop_chunk(n, FRAME_BOUND, apply))
                .collect(),
        )
    };
    {
        let to = dir.path().join("crossing");
        let mut i = Ingest::create(&to).unwrap();
        let first = full(&i, a, 0);
        i.batch(1, &first).unwrap();
        let before = log_bytes(&to);
        let spans = 16 * (HEAD as u64 + u64::from(FRAME_BOUND));
        assert!(
            before > spans + HEAD as u64 && before <= spans + HEAD as u64 + 64,
            "16 spans and their marker: {before}"
        );
        // one more byte crosses it: refused at that record, before it is written
        let crossing = batch_at(
            &i,
            vec![
                Frame::Present {
                    kind: BLOB_CHUNK,
                    hash: Hash::of(b"x"),
                    payload: b"x".to_vec(),
                },
                drop_chunk(16, 1, a),
            ],
        );
        assert_eq!(what(i.batch(2, &crossing).unwrap_err()), BUDGET);
        assert_eq!(
            log_bytes(&to),
            before + HEAD as u64 + 1,
            "the frame before the crossing record, and nothing of it"
        );
        drop(i);
        assert!(no_store(&to));
    }
}

#[test]
fn rdx9_only_a_valid_authorization_releases_its_claim() {
    let src = source();
    // what the source's dropped frames claim, and the apply they name
    let claimed: u64 = src
        .dropped()
        .iter()
        .map(|(i, j)| u64::from(src.batches[*i].frames[*j].original_len()))
        .sum();
    assert!(claimed > 0 && claimed < u64::from(FRAME_BOUND));
    let (i0, j0) = src.dropped()[0];
    let Frame::Dropped { apply: valid, .. } = src.batches[i0].frames[j0].clone() else {
        unreachable!()
    };
    let ingest_all = |i: &mut Ingest, bs: &[DumpBatch]| {
        for (n, b) in bs.iter().enumerate() {
            i.batch(n as u64 + 1, b).unwrap();
        }
    };
    {
        // invalid applies release nothing: a policy landing's canonical apply (the source's
        // dropped frames name it), and a malformed apply whose hash 15 frames of 64 MiB name
        let to = src._dir.path().join("invalid");
        let mut i = Ingest::create(&to).unwrap();
        let bs = policy_batches(&src);
        ingest_all(&mut i, &bs);
        let n = bs.len() as u64;
        let malformed = b"apply a".to_vec();
        let a = Hash::of(&malformed);
        let fill = batch_at(&i, (0..15).map(|k| drop_chunk(k, FRAME_BOUND, a)).collect());
        i.batch(n + 1, &fill).unwrap();
        let apply = batch_at(
            &i,
            vec![Frame::Present {
                kind: kinds::REDACTION_APPLY,
                hash: a,
                payload: malformed,
            }],
        );
        i.batch(n + 2, &apply).unwrap();
        // 1 GiB plus the source's claim waits, as no apply was valid: refused before it is
        // written
        let before = log_bytes(&to);
        let next = batch_at(&i, vec![drop_chunk(15, FRAME_BOUND, a)]);
        assert_eq!(what(i.batch(n + 3, &next).unwrap_err()), BUDGET);
        assert_eq!(
            log_bytes(&to),
            before,
            "nothing of the refused claim was written"
        );
        drop(i);
        assert!(no_store(&to));
    }
    {
        // a valid apply releases exactly its spans: 16 frames of 64 MiB fit after the source's
        // dropped frames once its authorization is validated
        let to = src._dir.path().join("valid");
        let mut i = Ingest::create(&to).unwrap();
        ingest_all(&mut i, &src.batches);
        let n = src.batches.len() as u64;
        // these name the valid apply too, but it does not authorize their spans
        let fill = batch_at(
            &i,
            (0..16).map(|k| drop_chunk(k, FRAME_BOUND, valid)).collect(),
        );
        i.batch(n + 1, &fill).unwrap();
        let next = batch_at(&i, vec![drop_chunk(16, 1, valid)]);
        assert_eq!(what(i.batch(n + 2, &next).unwrap_err()), BUDGET);
        drop(i);
        assert!(no_store(&to));
    }
}

/// A closed store holding one change that creates the secret block `a` and the bystander block
/// `b`, and a two-chunk blob, with one authorized request (altered by `on_draft`) written by the
/// raw fixture and never opened since: its removal is not erased yet.
struct Raw {
    _dir: tempfile::TempDir,
    path: PathBuf,
    fixture: Fixture,
}

fn raw_source(
    on_draft: impl Fn(&mut redaction_fixture::Draft, &Block, &Block, Hash) + 'static,
) -> Raw {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("raw");
    let mut s = Store::create(&path).unwrap();
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
    let mut opts = FixtureOptions::approved(Proof::local_human());
    opts.on_draft = Some(Box::new(move |_, d| on_draft(d, &a, &b, blob)));
    let fixture = authorized_batch_fixture(&path, &[spec], &opts);
    Raw {
        _dir: dir,
        path,
        fixture,
    }
}

/// The batches of the closed store at `path`, read from its segment files (every frame present),
/// and where each frame is: (location, batch index, frame index).
fn raw_batches(path: &Path) -> (Vec<DumpBatch>, Vec<(Location, usize, usize)>) {
    let mut out = vec![];
    let mut places = vec![];
    for (seg, (_, bytes)) in segment_bytes(path).iter().enumerate() {
        let mut at = 0usize;
        let mut start = None;
        let mut frames = vec![];
        while at + HEAD <= bytes.len() {
            let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            let kind = bytes[at + 4];
            let hash = Hash(bytes[at + 5..at + HEAD].try_into().unwrap());
            let payload = bytes[at + HEAD..at + HEAD + len].to_vec();
            let loc = Location {
                segment: seg as u32,
                offset: at as u64,
            };
            if kind == kinds::BATCH_COMMIT {
                let mut b = DumpBatch {
                    start: start.take().unwrap(),
                    digest: Hash([0; 32]),
                    frames: std::mem::take(&mut frames),
                };
                set_digest(&mut b);
                out.push(b);
            } else {
                start.get_or_insert(loc);
                places.push((loc, out.len(), frames.len()));
                frames.push(Frame::Present {
                    kind,
                    hash,
                    payload,
                });
            }
            at += HEAD + len;
        }
        assert!(frames.is_empty(), "every batch of the raw store is sealed");
    }
    (out, places)
}

/// `batches` as a dump of a store that erased the request's spans of `kinds_dropped`: each such
/// frame travels as a dropped record naming the apply and the span's residue.
fn with_drops(raw: &Raw, kinds_dropped: &[u8]) -> Vec<DumpBatch> {
    let (mut batches, places) = raw_batches(&raw.path);
    let apply = raw.fixture.applies[0].0;
    for span in &raw.fixture.requests[0].draft.request.manifest.spans {
        if !kinds_dropped.contains(&span.kind) {
            continue;
        }
        let loc = Location {
            segment: span.place.segment,
            offset: span.place.offset,
        };
        let (_, i, j) = places.iter().find(|(l, ..)| *l == loc).unwrap();
        batches[*i].frames[*j] = Frame::Dropped {
            kind: span.kind,
            hash: span.hash,
            len: span.len,
            apply,
            residue: span.residue,
        };
    }
    batches
}

/// Final review fix round 1, findings 1 and 2, the imported case: a load checks the log's
/// applies with the store's full semantic validation before it finishes the store. A dump whose
/// request selects another block than its manifest removes, whose manifest omits the change
/// that embeds the removed version (left intact in the dump), or whose change residue drops the
/// sibling's op while the original change travels intact, is refused, and the target holds no
/// store. A consistent dump, with all spans dropped or with an intact span still to erase,
/// loads and opens.
#[test]
fn rdx9_a_load_refuses_an_inconsistent_request() {
    let all = [BLOCK, CHANGE, BLOB_CHUNK];
    let no_change = [BLOCK, BLOB_CHUNK];
    // consistent: every span dropped, or the change span still intact
    for (name, dropped) in [("all dropped", &all[..]), ("change intact", &no_change[..])] {
        let raw = raw_source(|_, _, _, _| {});
        let to = raw._dir.path().join("ok");
        ingest(&to, &with_drops(&raw, dropped)).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let s = Store::open(&to).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        s.check_objects().unwrap();
    }
    type Hook = Box<dyn Fn(&mut redaction_fixture::Draft, &Block, &Block, Hash)>;
    let cases: Vec<(&str, Hook, &[u8])> = vec![
        (
            "the selector names another block",
            Box::new(|d, _, b, blob| {
                d.request.targets = vec![
                    RedactTarget::Version {
                        id: b.id.clone(),
                        version: b.version_hash(),
                    },
                    RedactTarget::Blob(blob),
                ];
            }),
            &all,
        ),
        (
            "the manifest omits the embedding change",
            Box::new(|d, _, _, _| {
                d.request.manifest.spans.retain(|s| s.kind != CHANGE);
                d.residues
                    .retain(|r| matches!(r, rdy_model::redaction::RedactionResidue::Block(_)));
            }),
            &no_change,
        ),
        (
            "the change residue drops the sibling's op",
            Box::new(|d, _, _, _| {
                for r in &mut d.residues {
                    if let rdy_model::redaction::RedactionResidue::Change(c) = r {
                        c.ops.truncate(1);
                    }
                }
                let residues = d.residues.clone();
                redaction_fixture::relink(&mut d.request, &residues);
            }),
            &no_change,
        ),
        // fix round 2, finding 2: a dropped change whose residue pairs the sibling's stored
        // version with another id
        (
            "the dropped change's residue gives the sibling's version another id",
            Box::new(sibling_with_another_id),
            &all,
        ),
        (
            "the dropped change's residue gives the sibling's version another id, the block and \
             chunks intact",
            Box::new(sibling_with_another_id),
            &[CHANGE],
        ),
    ];
    for (name, hook, dropped) in cases {
        let raw = raw_source(move |d, a, b, blob| hook(d, a, b, blob));
        let to = raw._dir.path().join("refused");
        let e = ingest(&to, &with_drops(&raw, dropped)).unwrap_err();
        assert_eq!(what(e), NO_AUTHORIZATION, "{name}");
        assert!(no_store(&to), "{name}: the target holds no store");
    }
}

/// A draft hook whose change residue pairs the sibling's (`b`'s) stored version with an id that
/// is not its block's.
fn sibling_with_another_id(d: &mut redaction_fixture::Draft, _: &Block, b: &Block, _: Hash) {
    for r in &mut d.residues {
        if let rdy_model::redaction::RedactionResidue::Change(c) = r {
            for op in &mut c.ops {
                if let rdy_model::redaction::ResidualOp::Create { id, version } = op
                    && *version == b.version_hash()
                {
                    *id = BlockId::parse("b-00003").unwrap();
                }
            }
        }
    }
    let residues = d.residues.clone();
    redaction_fixture::relink(&mut d.request, &residues);
}
