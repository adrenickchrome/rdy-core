//! hc5: the pre-pass of a chain load (D-2FMRR: "Before it writes, the load reads the applies of
//! every file"; D-QF2HQ: a stop before those applies loads and warns; D-69AY3; O4, O5, O7, O11,
//! O13). The scanner checks every link, frames every file in one forward pass that seeks over
//! retained payloads, checks every removal proof in physical order by recorded offsets, resolves
//! the stop to a physical batch end, reads every file after the stop whole, and plans each
//! retained frame a removal claims, with the carried records of later removals. It writes
//! nothing and holds no payload byte.
#[path = "support/carried.rs"]
mod carried_fixture;
#[path = "support/redaction.rs"]
mod redaction_fixture;

use carried_fixture::{Framed, block_residue, carried_hash, change_residue, framed, span};
use rdy_model::redaction::{RedactTarget, RedactionCarried};
use rdy_model::*;
use rdy_store::closure::{END_OF_LOG, StoreSource, closure, write_batch};
use rdy_store::dump::*;
use rdy_store::dump_chain::{BatchMarker, ChainEnd, DumpChainHeader, batch_end};
use rdy_store::dump_scan::*;
use rdy_store::holder::StoreMode;
use rdy_store::kinds;
use rdy_store::segment::Location;
use rdy_store::stream_error::StreamError;
use rdy_store::sync_proof::ObjectSource;
use rdy_store::sync_stream::StreamHeader;
use rdy_store::take_batch::{BatchTaken, TagChoice, TakeError, UploadFacts, UploadTarget};
use rdy_store::{Store, StoreError};
use redaction_fixture::{
    FixtureOptions, Proof, RequestSpec, authorized_batch_fixture, build_request,
};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::rc::Rc;

const SECRET: &str = "canary alpha secret";
/// The length of a container record's frame header.
const HEAD: usize = 37;
const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

fn at(segment: u32, offset: u64) -> Location {
    Location { segment, offset }
}

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

fn creates(blocks: &[&Block]) -> Vec<Op> {
    blocks
        .iter()
        .map(|b| Op::Create {
            block: (*b).clone(),
        })
        .collect()
}

/// Commits `ops` to `view` of the open store, proposes and lands it; the change and landing.
fn land(s: &mut Store, view: &str, ops: Vec<Op>) -> (Hash, Hash) {
    let c = s.commit_to_view(view, change(view, ops)).unwrap();
    let p = s
        .propose(view, "work", prov("agent:w", Role::Writer))
        .unwrap();
    let l = s.land(&p, prov("human:alice", Role::Approver)).unwrap();
    (c, l)
}

fn tag(s: &mut Store, name: &str, landing: Hash) -> Hash {
    s.put_tag(&Tag {
        name: name.into(),
        landing,
        message: None,
        tagged_by: prov("human:alice", Role::Approver),
    })
    .unwrap()
}

/// A removal of `targets` through the store's checked path: the request, its proposal and its
/// approved landing, which erases the payloads. Returns the landing.
fn remove(s: &mut Store, targets: Vec<RedactTarget>, at_ms: i64) -> Hash {
    let d = build_request(s, &RequestSpec::new("redact", targets).at(at_ms));
    let p = s
        .prepare_redaction(&d.carrier, &d.request, &d.residues)
        .unwrap();
    s.commit_redaction(p).unwrap();
    let p = s
        .propose("redact", "Remove content", prov("agent:w", Role::Writer))
        .unwrap();
    s.land(&p, prov("human:alice", Role::Approver)).unwrap()
}

/// A dump of `s`'s sealed batches up to `end`: full, or after `prev`, with `files`; its removal
/// facts are the writer's (the store's resolved spans).
fn dump_at(s: &Store, prev: Option<&ChainEnd>, end: Location, files: &[(&str, &[u8])]) -> Vec<u8> {
    let r = s.reader();
    let end_batch = r.batch_marker_at(end).unwrap().unwrap_or(BatchMarker::ZERO);
    let repo = r.repo_id().unwrap().unwrap();
    let (chain, base) = match prev {
        None => (DumpChainHeader::full(repo, end_batch), ORIGIN),
        Some(p) => (DumpChainHeader::after(p, end_batch), p.end),
    };
    let header = DumpHeader {
        store: rdy_store::format::DumpStore::Features(rdy_store::format::baseline_features()),
        log_end: end,
        rdy: "test".into(),
        chain: Some(chain),
    };
    let mut w = DumpWriter::new(Vec::new(), &header).unwrap();
    for (p, b) in files {
        w.file(p, b).unwrap();
    }
    let mut tally = RemovalTally::default();
    r.for_each_batch_after(base, end, &mut |b| {
        tally.observe(&b)?;
        w.batch(&b)
    })
    .unwrap();
    let facts = tally.resolved(&r, end).unwrap();
    w.finish(facts).unwrap().0
}

fn dump(s: &Store, prev: Option<&ChainEnd>, files: &[(&str, &[u8])]) -> Vec<u8> {
    dump_at(s, prev, s.log_end(), files)
}

/// Where a dump ends: the next increment follows it.
fn end_of(bytes: &[u8]) -> ChainEnd {
    let (_, h, t, _) = read_envelope(&mut Cursor::new(bytes)).unwrap();
    let c = h.chain.unwrap();
    ChainEnd {
        repo_id: c.repo_id,
        store_format: h.store,
        end: h.log_end,
        end_batch: c.end_batch,
        digest: t.digest,
    }
}

/// Writes `bytes` to `dir/name`.
fn put(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn run(files: &[PathBuf], stop: Option<StopMatch>) -> Result<ScanPlan, ScanError> {
    let mut src = ChainSource::open(files)?;
    scan(&mut src, stop)
}

fn ok(files: &[PathBuf], stop: Option<StopMatch>) -> ScanPlan {
    match run(files, stop) {
        Ok(p) => p,
        Err(e) => panic!("the scan refused: {e:?}"),
    }
}

/// The input and the `what` of a damaged-dump refusal.
fn damage(e: ScanError) -> (usize, u64, String) {
    match e {
        ScanError::Input {
            file,
            source: StoreError::Dump { at, what },
        } => (file, at, what),
        other => panic!("not a damaged input: {other:?}"),
    }
}

fn link_of(e: ScanError) -> (usize, ChainLink) {
    match e {
        ScanError::Link { file, link } => (file, link),
        other => panic!("not a link refusal: {other:?}"),
    }
}

fn stop_of(e: ScanError) -> StopFailure {
    match e {
        ScanError::Stop(f) => f,
        other => panic!("not a stop refusal: {other:?}"),
    }
}

/// One container record of a dump: its offset, kind, hash and payload length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RecordAt {
    offset: u64,
    kind: u8,
    hash: Hash,
    len: u64,
}

/// Every record of the dump `bytes`, in file order, by its framing alone.
fn records(bytes: &[u8]) -> Vec<RecordAt> {
    let mut out = vec![];
    let mut at = 12usize;
    while at < bytes.len() {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        out.push(RecordAt {
            offset: at as u64,
            kind: bytes[at + 4],
            hash: Hash(bytes[at + 5..at + HEAD].try_into().unwrap()),
            len: len as u64,
        });
        at += HEAD + len;
    }
    out
}

/// Replaces the payload of the record at `at` in `bytes` with `payload`, and its hash.
fn set_record(bytes: &mut Vec<u8>, at: usize, payload: &[u8]) {
    let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let mut out = bytes[..at].to_vec();
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.push(bytes[at + 4]);
    out.extend_from_slice(&Hash::of(payload).0);
    out.extend_from_slice(payload);
    out.extend_from_slice(&bytes[at + HEAD + len..]);
    *bytes = out;
}

/// Overwrites header payload bytes `[from..from + new.len())` of a format-3 dump, and the
/// header record's hash: only its envelope stays readable.
fn patch_header(bytes: &[u8], from: usize, new: &[u8]) -> Vec<u8> {
    let mut b = bytes.to_vec();
    let len = u32::from_le_bytes(b[12..16].try_into().unwrap()) as usize;
    let mut p = b[12 + HEAD..12 + HEAD + len].to_vec();
    p[from..from + new.len()].copy_from_slice(new);
    set_record(&mut b, 12, &p);
    b
}

/// The offsets of a format-4 header payload's fields, after its baseline manifest.
const M: usize = 183;
const H_STORE: usize = 5;
const H_REPO: usize = 5 + M;
const H_BASE: usize = 21 + M;
const H_BASE_BATCH: usize = 33 + M;
const H_PREV: usize = 81 + M;

/// The header payload of the dump `bytes` with its manifest replaced by `json`, and the header
/// record's hash: only its envelope stays readable.
fn with_manifest(bytes: &[u8], json: &[u8]) -> Vec<u8> {
    let mut b = bytes.to_vec();
    let len = u32::from_le_bytes(b[12..16].try_into().unwrap()) as usize;
    let p = b[12 + HEAD..12 + HEAD + len].to_vec();
    let mut q = vec![p[0]];
    q.extend_from_slice(&(json.len() as u32).to_le_bytes());
    q.extend_from_slice(json);
    q.extend_from_slice(&p[5 + M..]);
    set_record(&mut b, 12, &q);
    b
}

/// The history of a project and its chain:
///
/// 1. a kept block, landed and tagged `v0`;
/// 2. the secret and another block in one change, landed; then tagged `v1` in its own batch;
/// 3. `f0`: the full dump here (the secret's frames hold their bytes);
/// 4. an approved removal of the secret version (`removal`, its landing and apply in one batch),
///    which erases its frames;
/// 5. `i1`: the increment after `f0`;
/// 6. one more landed block; `i2`: the increment after `i1`;
/// 7. `f2`: a full dump of the whole history (the secret's frames dropped).
struct History {
    dir: tempfile::TempDir,
    store: PathBuf,
    secret: Block,
    l0: Hash,
    l1: Hash,
    t1: Hash,
    removal: Hash,
    apply: Hash,
    /// The batch end after the `v1` tag, and after the landing of the secret.
    v1_end: Location,
    l1_end: Location,
    f0: PathBuf,
    i1: PathBuf,
    i2: PathBuf,
    f2: PathBuf,
    /// The secret's block and change frames, as `f0` holds them.
    secret_frames: Vec<Framed>,
}

fn history() -> History {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("src");
    let mut s = Store::create(&store).unwrap();
    let kept = block("b-00003", "kept text");
    let (_, l0) = land(&mut s, "work", creates(&[&kept]));
    tag(&mut s, "v0", l0);
    let secret = block("b-00001", SECRET);
    let other = block("b-00002", "other text");
    let (c1, l1) = land(&mut s, "work", creates(&[&secret, &other]));
    let l1_end = s.log_end();
    let t1 = tag(&mut s, "v1", l1);
    let v1_end = s.log_end();
    let f0b = dump(&s, None, &[(".rdy.yaml", b"a")]);
    let frames = framed(&s.reader(), s.log_end());
    let secret_frames = vec![
        *frames
            .iter()
            .find(|f| f.kind == kinds::BLOCK && f.hash == secret.version_hash())
            .unwrap(),
        *frames
            .iter()
            .find(|f| rdy_store::codec::logical_kind(f.kind) == kinds::CHANGE && f.hash == c1)
            .unwrap(),
    ];
    let removal = remove(
        &mut s,
        vec![RedactTarget::Version {
            id: secret.id.clone(),
            version: secret.version_hash(),
        }],
        70,
    );
    let apply = framed(&s.reader(), s.log_end())
        .into_iter()
        .find(|f| f.kind == kinds::REDACTION_APPLY)
        .unwrap()
        .hash;
    let i1b = dump(&s, Some(&end_of(&f0b)), &[(".rdy.yaml", b"b")]);
    land(&mut s, "work", creates(&[&block("b-00004", "later text")]));
    let i2b = dump(
        &s,
        Some(&end_of(&i1b)),
        &[(".rdy.yaml", b"c"), (".rdy/packs/x.yaml", b"x: 1")],
    );
    let f2b = dump(&s, None, &[(".rdy.yaml", b"c")]);
    drop(s);
    let f0 = put(dir.path(), "f0.rdydump", &f0b);
    let i1 = put(dir.path(), "i1.rdydump", &i1b);
    let i2 = put(dir.path(), "i2.rdydump", &i2b);
    let f2 = put(dir.path(), "f2.rdydump", &f2b);
    History {
        dir,
        store,
        secret,
        l0,
        l1,
        t1,
        removal,
        apply,
        v1_end,
        l1_end,
        f0,
        i1,
        i2,
        f2,
        secret_frames,
    }
}

impl History {
    fn chain(&self) -> Vec<PathBuf> {
        vec![self.f0.clone(), self.i1.clone(), self.i2.clone()]
    }

    fn bytes(&self, p: &Path) -> Vec<u8> {
        std::fs::read(p).unwrap()
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        put(self.dir.path(), name, bytes)
    }

    /// The end of `f0`.
    fn f0_end(&self) -> Location {
        end_of(&self.bytes(&self.f0)).end
    }
}

/// A frozen released dump (dump format 2).
fn legacy_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../rdy-qa/fixtures/dump/v2-s15.rdydump")
}

#[test]
fn hc5_every_link_field_is_checked_before_target_writes() {
    let h = history();
    // no file, and the kind and format rules
    assert!(matches!(
        ChainSource::open(&[]).unwrap_err(),
        ScanError::Input { file: 0, .. }
    ));
    assert_eq!(
        link_of(ChainSource::open(std::slice::from_ref(&h.i1)).unwrap_err()),
        (0, ChainLink::FirstIsIncrement)
    );
    assert_eq!(
        link_of(ChainSource::open(&[h.f0.clone(), h.f2.clone()]).unwrap_err()),
        (1, ChainLink::LaterIsFull)
    );
    let legacy = legacy_fixture();
    assert_eq!(
        link_of(ChainSource::open(&[h.f0.clone(), legacy.clone()]).unwrap_err()),
        (1, ChainLink::Legacy { format: 2 })
    );
    assert_eq!(
        link_of(ChainSource::open(&[legacy.clone(), h.i1.clone()]).unwrap_err()),
        (0, ChainLink::Legacy { format: 2 })
    );
    // a released single file opens for the single-file load, and takes no chain scan or stop
    let mut one = ChainSource::open(std::slice::from_ref(&legacy)).unwrap();
    assert_eq!(one.legacy(), Some(2));
    assert_eq!(
        link_of(scan(&mut one, None).unwrap_err()),
        (0, ChainLink::Legacy { format: 2 })
    );
    assert_eq!(
        link_of(scan(&mut one, Some(StopMatch::Position(at(0, 1)))).unwrap_err()),
        (0, ChainLink::LegacyStop { format: 2 })
    );
    // each field of the link, at the file that breaks it (here the third)
    let i2 = h.bytes(&h.i2);
    let c2 = read_envelope(&mut Cursor::new(&i2))
        .unwrap()
        .1
        .chain
        .unwrap();
    let with = |name: &str, bytes: Vec<u8>| vec![h.f0.clone(), h.i1.clone(), h.write(name, &bytes)];
    let other = RepoId([0x5A; 16]);
    assert_eq!(
        link_of(run(&with("repo", patch_header(&i2, H_REPO, &other.0)), None).unwrap_err()),
        (
            2,
            ChainLink::Repo {
                file: other,
                chain: c2.repo_id
            }
        )
    );
    let moved = at(c2.base.segment, c2.base.offset + 1);
    assert_eq!(
        link_of(
            run(
                &with(
                    "base",
                    patch_header(&i2, H_BASE + 4, &moved.offset.to_le_bytes())
                ),
                None
            )
            .unwrap_err()
        ),
        (
            2,
            ChainLink::Position {
                start: moved,
                prev_end: c2.base
            }
        )
    );
    assert_eq!(
        link_of(
            run(
                &with("marker", patch_header(&i2, H_BASE_BATCH + 16, &[0xEE; 32])),
                None
            )
            .unwrap_err()
        ),
        (2, ChainLink::Marker)
    );
    assert_eq!(
        link_of(run(&with("prev", patch_header(&i2, H_PREV, &[0x77; 32])), None).unwrap_err()),
        (
            2,
            ChainLink::Digest {
                named: Hash([0x77; 32]),
                actual: c2.prev
            }
        )
    );
    // the precedence within a link: the repo before the base, the base before the digest
    let both = patch_header(&patch_header(&i2, H_REPO, &other.0), H_PREV, &[0x77; 32]);
    assert!(matches!(
        link_of(run(&with("both", both), None).unwrap_err()).1,
        ChainLink::Repo { .. }
    ));
    let both = patch_header(
        &patch_header(&i2, H_BASE + 4, &moved.offset.to_le_bytes()),
        H_PREV,
        &[0x77; 32],
    );
    assert!(matches!(
        link_of(run(&with("both2", both), None).unwrap_err()).1,
        ChainLink::Position { .. }
    ));
    // a manifest this rdy does not read is refused with the envelope itself
    assert_eq!(rdy_store::format::baseline_features().to_json().len(), M);
    let e = run(&with("store", patch_header(&i2, H_STORE + 20, b"2")), None).unwrap_err();
    let (file, _, what) = damage(e);
    assert_eq!(file, 2);
    assert_eq!(
        what,
        "store feature manifest is invalid: unsupported manifest version"
    );
    // a manifest that is not the chain's is a broken link
    let other = rdy_store::format::StoreFeatures::of(vec![rdy_store::format::StoreFeature {
        name: "zz".into(),
        class: rdy_store::format::FeatureClass::Compatible,
        required_rdy: "1.0.0".into(),
    }]);
    assert_eq!(
        link_of(
            run(
                &with("features", with_manifest(&i2, &other.to_json())),
                None
            )
            .unwrap_err()
        ),
        (
            2,
            ChainLink::StoreFormat {
                file: rdy_store::format::DumpStore::Features(other),
                chain: rdy_store::format::DumpStore::Features(
                    rdy_store::format::baseline_features()
                ),
            }
        )
    );
    // and links come before any framing: a broken link of the third file wins over damage in
    // the second file's body
    let mut i1 = h.bytes(&h.i1);
    let n = i1.len();
    i1[n / 2] ^= 0xFF;
    let files = vec![
        h.f0.clone(),
        h.write("i1-damaged", &i1),
        h.write("prev3", &patch_header(&i2, H_PREV, &[0x77; 32])),
    ];
    // the damaged second file breaks its own trailer digest, which the third file names
    assert!(matches!(
        run(&files, None).unwrap_err(),
        ScanError::Link { file: 2, .. }
    ));
    // the whole chain links
    let plan = ok(&h.chain(), None);
    assert_eq!(plan.envelopes().len(), 3);
}

#[test]
fn hc5_every_file_is_verified_past_the_stop() {
    let h = history();
    let stop = || Some(StopMatch::Position(h.f0_end()));
    let plan = ok(&h.chain(), stop());
    assert_eq!(
        plan.files().iter().map(|f| f.role).collect::<Vec<_>>(),
        vec![FileRole::Stop, FileRole::After, FileRole::After]
    );
    assert_eq!(plan.end(), h.f0_end());
    let i2 = h.bytes(&h.i2);
    let with = |name: &str, bytes: &[u8]| vec![h.f0.clone(), h.i1.clone(), h.write(name, bytes)];
    // data: a byte of a retained payload the forward pass seeks over
    let later = "later text";
    let pos = i2
        .windows(later.len())
        .position(|w| w == later.as_bytes())
        .unwrap();
    let mut data = i2.clone();
    data[pos] ^= 0x01;
    let files = with("data", &data);
    // without a stop the file contributes its batches: the ingest hashes them
    ok(&files, None);
    let (file, _, what) = damage(run(&files, stop()).unwrap_err());
    assert_eq!(
        (file, what.as_str()),
        (2, "a record's hash does not match its bytes")
    );
    // proof: a present frame turned into a dropped frame that names no valid apply
    let proof = drop_one_block(&i2);
    for s in [None, stop()] {
        let (file, _, what) = damage(run(&with("proof", &proof), s).unwrap_err());
        assert_eq!(
            (file, what.as_str()),
            (2, "dropped frame has no valid authorization")
        );
    }
    // trailer: its counts
    let t_at = i2.len() - (HEAD + TRAILER_LEN);
    let mut tp = i2[t_at + HEAD..].to_vec();
    tp[0] ^= 0x01;
    let mut trailer = i2.clone();
    set_record(&mut trailer, t_at, &tp);
    let (file, at_, what) = damage(run(&with("trailer", &trailer), stop()).unwrap_err());
    assert_eq!(
        (file, at_, what.as_str()),
        (
            2,
            t_at as u64,
            "the trailer's counts or digest do not match the dump"
        )
    );
    // the end of the file: a second trailer after the first
    let mut eof = i2.clone();
    eof.extend_from_slice(&i2[t_at..]);
    let (file, at_, what) = damage(run(&with("eof", &eof), stop()).unwrap_err());
    assert_eq!(
        (file, at_, what.as_str()),
        (2, i2.len() as u64, "bytes follow the trailer")
    );
    // and the same damage in the second file, wholly after the stop too
    let i1 = h.bytes(&h.i1);
    let mut d1 = i1.clone();
    let p1 = i1.len() - (HEAD + TRAILER_LEN) - 40;
    d1[p1] ^= 0x01;
    let files = vec![h.f0.clone(), h.write("i1-data", &d1)];
    let (file, _, _) = damage(run(&files, stop()).unwrap_err());
    assert_eq!(file, 1);
}

/// `bytes`, a format-3 dump, with its first present BLOCK frame turned into a dropped frame that
/// names an apply no file holds; its digests and trailer written again.
fn drop_one_block(bytes: &[u8]) -> Vec<u8> {
    let (mut rd, _, header) = DumpReader::open(bytes).unwrap();
    let mut files = vec![];
    let mut batches = vec![];
    while let Some(item) = rd.next().unwrap() {
        match item {
            DumpItem::File { path, bytes } => files.push((path, bytes)),
            DumpItem::Batch(b) => batches.push(b),
        }
    }
    let trailer = *rd.trailer().unwrap();
    let mut done = false;
    for b in &mut batches {
        for f in &mut b.frames {
            if !done && f.kind() == kinds::BLOCK {
                *f = Frame::Dropped {
                    kind: kinds::BLOCK,
                    hash: *f.hash(),
                    len: f.original_len(),
                    apply: Hash::of(b"no such apply"),
                    residue: Some(Hash::of(b"no such residue")),
                };
                done = true;
            }
        }
    }
    assert!(done);
    let mut w = DumpWriter::new(Vec::new(), &header).unwrap();
    for (p, b) in &files {
        w.file(p, b).unwrap();
    }
    for b in &batches {
        w.batch(b).unwrap();
    }
    w.finish(RemovalFacts {
        removals: trailer.removals,
        first_removed: trailer.first_removed,
    })
    .unwrap()
    .0
}

/// The reads of input bytes during `f`.
fn reads_of<T>(f: impl FnOnce() -> T) -> (T, Vec<ScanRead>) {
    let log: Rc<RefCell<Vec<ScanRead>>> = Rc::new(RefCell::new(vec![]));
    let sink = log.clone();
    on_scan_read(Some(Box::new(move |r| sink.borrow_mut().push(r))));
    let out = f();
    on_scan_read(None);
    let got = log.borrow().clone();
    (out, got)
}

#[test]
fn hc5_prepass_seeks_over_retained_payloads() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    // one chunk of 1 MiB, its canary in the middle
    let mut blob = vec![b'x'; 1 << 20];
    let canary = b"retained payload canary";
    blob[1 << 19..(1 << 19) + canary.len()].copy_from_slice(canary);
    s.put_blob(&blob).unwrap();
    land(&mut s, "work", creates(&[&block("b-00001", "a block")]));
    let f = put(dir.path(), "f.rdydump", &dump(&s, None, &[]));
    drop(s);
    let bytes = std::fs::read(&f).unwrap();
    let c_at = bytes
        .windows(canary.len())
        .position(|w| w == canary)
        .unwrap() as u64;
    let (plan, reads) = reads_of(|| run(std::slice::from_ref(&f), None));
    plan.unwrap();
    let touches = |r: &ScanRead| r.offset < c_at + canary.len() as u64 && c_at < r.offset + r.len;
    assert!(
        !reads.iter().any(touches),
        "a read reached the retained payload: {:?}",
        reads.iter().find(|r| touches(r))
    );
    let read: u64 = reads.iter().map(|r| r.len).sum();
    assert!(
        read < bytes.len() as u64 / 4,
        "{read} of {} bytes read",
        bytes.len()
    );
    assert!(reads.iter().all(|r| r.pass == ScanPass::Forward));
}

#[test]
fn hc5_proof_lookups_use_recorded_offsets() {
    // a secret with a second physical copy: the removal's two block spans share one residue
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let secret = block("b-00001", SECRET);
    land(&mut s, "work", creates(&[&secret]));
    let raw = s.get_raw(&secret.version_hash()).unwrap().1;
    drop(s);
    carried_fixture::append_raw(&path, &[(kinds::BLOCK, raw)], true);
    let mut s = Store::open(&path).unwrap();
    let f0 = dump(&s, None, &[]);
    remove(
        &mut s,
        vec![RedactTarget::Version {
            id: secret.id.clone(),
            version: secret.version_hash(),
        }],
        70,
    );
    let i1 = dump(&s, Some(&end_of(&f0)), &[]);
    drop(s);
    let files = vec![
        put(dir.path(), "f0.rdydump", &f0),
        put(dir.path(), "i1.rdydump", &i1),
    ];
    let (plan, reads) = reads_of(|| run(&files, None));
    let plan = plan.unwrap();
    assert!(plan.carried().is_empty());
    assert_eq!(
        plan.planned_places().len(),
        3,
        "two block copies and the change"
    );
    // one forward pass per file: each read starts where the one before it ended or later
    for file in 0..2 {
        let fwd: Vec<&ScanRead> = reads
            .iter()
            .filter(|r| r.file == file && r.pass == ScanPass::Forward)
            .collect();
        // the envelope reads its header and trailer first; the framing pass then reads forward
        let start = fwd
            .iter()
            .position(|r| r.offset == 0)
            .and_then(|p| {
                fwd[p + 1..]
                    .iter()
                    .position(|r| r.offset == 0)
                    .map(|q| p + 1 + q)
            })
            .expect("the framing pass reads from the start");
        for w in fwd[start..].windows(2) {
            assert!(
                w[0].offset + w[0].len <= w[1].offset,
                "file {file}: {:?} then {:?}",
                w[0],
                w[1]
            );
        }
    }
    // the lookups: each starts at a recorded record, and each record is read once
    let recs = records(&i1);
    let starts: Vec<u64> = reads
        .iter()
        .filter(|r| r.pass == ScanPass::Lookup)
        .filter(|r| recs.iter().any(|x| x.offset == r.offset))
        .map(|r| r.offset)
        .collect();
    let distinct: BTreeSet<u64> = starts.iter().copied().collect();
    assert_eq!(
        starts.len(),
        distinct.len(),
        "a record read twice: {starts:?}"
    );
    let kinds_read: Vec<u8> = distinct
        .iter()
        .map(|o| rdy_store::codec::logical_kind(recs.iter().find(|x| x.offset == *o).unwrap().kind))
        .collect();
    // the apply's landing, its request and its two distinct residues: nothing else
    let mut sorted = kinds_read.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        vec![
            kinds::LANDING,
            kinds::REDACTION,
            kinds::REDACTION_RESIDUE,
            kinds::REDACTION_RESIDUE
        ]
    );
    let lookup_bytes: u64 = reads
        .iter()
        .filter(|r| r.pass == ScanPass::Lookup)
        .map(|r| r.len)
        .sum();
    let dep_bytes: u64 = distinct
        .iter()
        .map(|o| HEAD as u64 + recs.iter().find(|x| x.offset == *o).unwrap().len)
        .sum();
    assert_eq!(lookup_bytes, dep_bytes, "each dependency read once, whole");
    // the request and residues are in the apply's file; nothing reads the first file again
    assert!(
        reads
            .iter()
            .filter(|r| r.pass == ScanPass::Lookup)
            .all(|r| r.file == 1)
    );
}

#[test]
fn hc5_stops_select_physical_batch_ends() {
    let h = history();
    let chain = h.chain();
    // a tag stop selects its TAG's batch; a landing stop its landing's batch
    let by_tag = ok(&chain, Some(StopMatch::TagName("v1".into())));
    assert_eq!(by_tag.end(), h.v1_end);
    assert_eq!(by_tag.tag(), Some("v1"));
    assert_eq!(by_tag.landing(), Some(h.l1));
    let by_landing = ok(
        &chain,
        Some(StopMatch::LandingPrefix(h.l1.to_string()[..10].into())),
    );
    assert_eq!(by_landing.end(), h.l1_end);
    assert_eq!(by_landing.landing(), Some(h.l1));
    assert_eq!(by_landing.tag(), None);
    assert!(h.l1_end < h.v1_end);
    assert!(by_tag.explicit_stop());
    // the batch of a removal's landing holds its apply too: the apply is in the prefix
    let shared = ok(
        &chain,
        Some(StopMatch::LandingPrefix(h.removal.to_string())),
    );
    assert!(shared.carried().is_empty());
    let s0 = h.secret_frames[0];
    assert!(matches!(
        shared.frame_action(s0.loc, s0.kind, s0.hash, s0.len).unwrap(),
        FrameAction::Zero { authority, .. } if authority == h.apply
    ));
    // a position stop: exact batch ends only
    assert_eq!(
        ok(&chain, Some(StopMatch::Position(h.v1_end))).end(),
        h.v1_end
    );
    for p in [
        ORIGIN,
        at(h.v1_end.segment, h.v1_end.offset - 1),
        at(h.v1_end.segment, h.v1_end.offset + 1),
        // a frame boundary inside a batch: the end of the secret's block frame
        s0.end(),
    ] {
        assert_eq!(
            stop_of(run(&chain, Some(StopMatch::Position(p))).unwrap_err()),
            StopFailure::NotBatchEnd(p)
        );
    }
    // a stop at the chain's own end is explicit, and the same end as none
    let last = end_of(&h.bytes(&h.i2)).end;
    let at_end = ok(&chain, Some(StopMatch::Position(last)));
    assert!(at_end.explicit_stop());
    assert_eq!(at_end.end(), ok(&chain, None).end());
    assert!(!ok(&chain, None).explicit_stop());
    // a stop at a file's end: that file owns the project files
    let boundary = ok(&chain, Some(StopMatch::Position(h.f0_end())));
    assert_eq!(boundary.owner(), 0);
    assert_eq!(
        boundary.files()[0].kept_batches,
        boundary.files()[0].batches
    );
    // no match, and missing tags
    assert_eq!(
        stop_of(run(&chain, Some(StopMatch::LandingPrefix("zz".into()))).unwrap_err()),
        StopFailure::MissingLanding("zz".into())
    );
    assert_eq!(
        stop_of(run(&chain, Some(StopMatch::TagName("v9".into()))).unwrap_err()),
        StopFailure::MissingTag("v9".into())
    );
    // ambiguous prefixes, duplicate landings and duplicate tags
    stops_in_a_crowded_log();
    stops_across_a_rolled_log();
    let _ = (h.l0, h.t1, &h.store, &h.secret);
}

/// Landing prefixes over many landings, a landing frame copied into a later batch, and tag
/// objects of one name.
fn stops_in_a_crowded_log() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let mut landings: Vec<Hash> = vec![];
    let mut shared: Option<char> = None;
    for n in 0..40 {
        let (_, l) = land(
            &mut s,
            "work",
            creates(&[&block(&format!("b-{:05}", n + 1), &format!("body {n}"))]),
        );
        let c = l.to_string().chars().next().unwrap();
        if landings.iter().any(|x| x.to_string().starts_with(c)) {
            shared = Some(c);
        }
        landings.push(l);
        if shared.is_some() {
            break;
        }
    }
    let first_end = {
        let frames = framed(&s.reader(), s.log_end());
        let lf = frames
            .iter()
            .find(|f| f.kind == kinds::LANDING && f.hash == landings[0])
            .unwrap();
        // the end of the batch that holds the first landing
        let mut end = None;
        s.reader()
            .for_each_batch(s.log_end(), |b| {
                let e = batch_end(&b).unwrap();
                if end.is_none() && b.start <= lf.loc && lf.loc < e {
                    end = Some(e);
                }
                Ok(())
            })
            .unwrap();
        end.unwrap()
    };
    let landing0 = s.reader().get_kind(kinds::LANDING, &landings[0]).unwrap();
    let t1 = tag(&mut s, "v1", landings[0]);
    let tag1 = s.reader().get_kind(kinds::TAG, &t1).unwrap();
    let after_tag = s.log_end();
    drop(s);
    // a second physical copy of the first landing, and of the tag, in later batches
    carried_fixture::append_raw(&path, &[(kinds::LANDING, landing0)], true);
    carried_fixture::append_raw(&path, &[(kinds::TAG, tag1.clone())], true);
    let s = Store::open(&path).unwrap();
    let f = put(dir.path(), "f.rdydump", &dump(&s, None, &[]));
    drop(s);
    let files = vec![f.clone()];
    let c = shared.expect("two landings share a first digit");
    assert_eq!(
        stop_of(run(&files, Some(StopMatch::LandingPrefix(c.to_string()))).unwrap_err()),
        StopFailure::AmbiguousLanding(c.to_string())
    );
    // the duplicate landing selects the first batch that holds it
    let one = ok(
        &files,
        Some(StopMatch::LandingPrefix(landings[0].to_string())),
    );
    assert_eq!(one.end(), first_end);
    // the repeated equal tag object selects its first batch
    assert_eq!(
        ok(&files, Some(StopMatch::TagName("v1".into()))).end(),
        after_tag
    );
    // a distinct tag object of the same name: the tags are damaged
    let other = Tag {
        name: "v1".into(),
        landing: landings[1],
        message: Some("another".into()),
        tagged_by: prov("human:alice", Role::Approver),
    };
    let bytes = postcard::to_allocvec(&other).unwrap();
    carried_fixture::append_raw(&path, &[(kinds::TAG, bytes)], true);
    let s = Store::open(&path).unwrap();
    let g = put(dir.path(), "g.rdydump", &dump(&s, None, &[]));
    drop(s);
    assert_eq!(
        stop_of(run(&[g], Some(StopMatch::TagName("v1".into()))).unwrap_err()),
        StopFailure::DuplicateTag("v1".into())
    );
}

/// A log whose small segments roll: stops at batch ends in later segments, and an increment that
/// starts at offset zero of the next segment.
fn stops_across_a_rolled_log() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    s.set_segment_max(4096);
    for n in 0..3 {
        land(
            &mut s,
            "work",
            creates(&[&block(&format!("b-{:05}", n + 1), &"q".repeat(1500))]),
        );
    }
    let f = dump(&s, None, &[]);
    for n in 3..6 {
        land(
            &mut s,
            "work",
            creates(&[&block(&format!("b-{:05}", n + 1), &"q".repeat(1500))]),
        );
    }
    let i = dump(&s, Some(&end_of(&f)), &[]);
    let mut ends = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |b| {
            ends.push(batch_end(&b).unwrap());
            Ok(())
        })
        .unwrap();
    drop(s);
    let files = vec![
        put(dir.path(), "f.rdydump", &f),
        put(dir.path(), "i.rdydump", &i),
    ];
    assert!(
        ends.iter().any(|e| e.segment >= 2),
        "the log rolled: {ends:?}"
    );
    for e in &ends {
        let plan = ok(&files, Some(StopMatch::Position(*e)));
        assert_eq!(plan.end(), *e);
        // offset zero of a segment ends no batch
        assert_eq!(
            stop_of(run(&files, Some(StopMatch::Position(at(e.segment, 0)))).unwrap_err()),
            StopFailure::NotBatchEnd(at(e.segment, 0))
        );
    }
}

#[test]
fn hc5_later_apply_distinguishes_held_and_missing_bytes() {
    let h = history();
    // the secret's frames hold their bytes in the retained prefix: held
    let held = ok(
        &[h.f0.clone(), h.i1.clone()],
        Some(StopMatch::Position(h.f0_end())),
    );
    assert_eq!(held.carried().len(), 1);
    let r = &held.carried()[0];
    assert_eq!(r.apply, h.apply);
    assert_eq!(r.landing, h.removal);
    assert_eq!(r.approved_by.actor, "human:alice");
    assert_eq!(
        r.spans
            .iter()
            .map(|s| (
                Location {
                    segment: s.place.segment,
                    offset: s.place.offset
                },
                s.held
            ))
            .collect::<Vec<_>>(),
        h.secret_frames
            .iter()
            .map(|f| (f.loc, true))
            .collect::<Vec<_>>()
    );
    for f in &h.secret_frames {
        assert_eq!(
            held.frame_action(f.loc, f.kind, f.hash, f.len).unwrap(),
            FrameAction::Keep
        );
    }
    // both residues lie after the stop: they come with the record, by hash
    assert_eq!(held.residues().len(), 2);
    let hashes: Vec<Hash> = held
        .residues()
        .iter()
        .map(carried_fixture::residue_hash)
        .collect();
    let mut sorted = hashes.clone();
    sorted.sort();
    assert_eq!(hashes, sorted);
    // the full dump after the removal holds them dropped: missing, and written as zeros named by
    // the carried record
    let missing = ok(
        std::slice::from_ref(&h.f2),
        Some(StopMatch::Position(h.v1_end)),
    );
    assert_eq!(missing.carried().len(), 1);
    let r = &missing.carried()[0];
    assert!(r.spans.iter().all(|s| !s.held));
    let c = carried_hash(r);
    for f in &h.secret_frames {
        assert_eq!(
            missing.frame_action(f.loc, f.kind, f.hash, f.len).unwrap(),
            FrameAction::Zero {
                authority: c,
                residue: r
                    .spans
                    .iter()
                    .find(|s| s.place.offset == f.loc.offset)
                    .unwrap()
                    .residue
            }
        );
    }
    // the carried record keeps its source's identity, with no new time
    let full = ok(std::slice::from_ref(&h.f2), None);
    assert!(full.carried().is_empty());
}

#[test]
fn hc5_pre_stop_apply_marks_present_input_for_zero_write() {
    let h = history();
    let files = vec![h.f0.clone(), h.i1.clone()];
    let end = end_of(&h.bytes(&h.i1)).end;
    for stop in [None, Some(StopMatch::Position(end))] {
        let plan = ok(&files, stop);
        assert!(plan.carried().is_empty());
        assert!(plan.residues().is_empty());
        for f in &h.secret_frames {
            match plan.frame_action(f.loc, f.kind, f.hash, f.len).unwrap() {
                FrameAction::Zero { authority, residue } => {
                    assert_eq!(authority, h.apply);
                    assert!(residue.is_some());
                }
                other => panic!("{other:?}"),
            }
            // the same place with another identity is no span of the apply
            assert_eq!(
                plan.frame_action(f.loc, f.kind, Hash([1; 32]), f.len)
                    .unwrap_err()
                    .to_string(),
                "dump damaged at byte 0: dropped frame does not match its authorized span"
            );
        }
        assert_eq!(
            plan.planned_places(),
            h.secret_frames.iter().map(|f| f.loc).collect::<Vec<_>>()
        );
        // any other frame is kept
        assert_eq!(
            plan.frame_action(ORIGIN, kinds::REF_UPDATE, Hash([2; 32]), 9)
                .unwrap(),
            FrameAction::Keep
        );
    }
    // a frame the source already dropped is its own validated hole
    let plan = ok(std::slice::from_ref(&h.f2), None);
    for f in &h.secret_frames {
        assert_eq!(
            plan.frame_action(f.loc, f.kind, f.hash, f.len).unwrap(),
            FrameAction::Keep
        );
    }
}

/// A source project whose secret is in one landed change, and a stopped load of it whose
/// carried record names the secret's block and change frames.
struct Loaded {
    dir: tempfile::TempDir,
    src: PathBuf,
    to: PathBuf,
    secret: Block,
    record: RedactionCarried,
    frames: Vec<Framed>,
}

fn loaded(held: bool) -> Loaded {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let mut s = Store::create(&src).unwrap();
    let kept = block("b-00003", "kept text");
    land(&mut s, "work", creates(&[&kept]));
    let secret = block("b-00001", SECRET);
    let other = block("b-00002", "other text");
    let c = change("work", creates(&[&secret, &other]));
    let cid = s.commit_to_view("work", c).unwrap();
    let stored = s.change(&cid).unwrap();
    let p = s
        .propose("work", "secret", prov("agent:w", Role::Writer))
        .unwrap();
    s.land(&p, prov("human:alice", Role::Approver)).unwrap();
    let all = framed(&s.reader(), s.log_end());
    drop(s);
    let bf = *all
        .iter()
        .find(|f| f.kind == kinds::BLOCK && f.hash == secret.version_hash())
        .unwrap();
    let cf = *all
        .iter()
        .find(|f| rdy_store::codec::logical_kind(f.kind) == kinds::CHANGE && f.hash == cid)
        .unwrap();
    let residues = vec![block_residue(&secret), change_residue(&stored)];
    let mut spans = vec![
        span(&bf, Some(&residues[0]), held),
        span(&cf, Some(&residues[1]), held),
    ];
    spans.sort_by_key(|s| (s.place.segment, s.place.offset));
    let record = carried_fixture::record(
        vec![RedactTarget::Version {
            id: secret.id.clone(),
            version: secret.version_hash(),
        }],
        spans,
    );
    let to = dir.path().join("to");
    carried_fixture::load(&src, &to, &residues, std::slice::from_ref(&record)).unwrap();
    Loaded {
        dir,
        src,
        to,
        secret,
        record,
        frames: vec![bf, cf],
    }
}

#[test]
fn hc5_carried_proof_must_be_in_the_same_file() {
    let l = loaded(false);
    let s = Store::open(&l.to).unwrap();
    // the whole loaded store in one file: the dropped frames and the carried record together
    let whole = put(l.dir.path(), "whole.rdydump", &dump(&s, None, &[]));
    let plan = ok(std::slice::from_ref(&whole), None);
    for f in &l.frames {
        assert_eq!(
            plan.frame_action(f.loc, f.kind, f.hash, f.len).unwrap(),
            FrameAction::Keep
        );
    }
    // the prefix in one file, the carried batch in the next: the dropped frames name a record
    // of another file
    let prefix_end = Store::open(&l.src).unwrap().log_end();
    let pre = dump_at(&s, None, prefix_end, &[]);
    let inc = dump_at(&s, Some(&end_of(&pre)), s.log_end(), &[]);
    drop(s);
    let files = vec![
        put(l.dir.path(), "pre.rdydump", &pre),
        put(l.dir.path(), "inc.rdydump", &inc),
    ];
    let (file, at_, what) = damage(run(&files, None).unwrap_err());
    assert_eq!(
        (file, what.as_str()),
        (0, "dropped frame has no valid authorization")
    );
    let first_drop = records(&pre)
        .into_iter()
        .find(|r| r.kind == rec::DROPPED)
        .unwrap();
    assert_eq!(at_, first_drop.offset);
    // the same with a stop at the boundary: the proofs of every file are checked
    let (file, _, _) = damage(run(&files, Some(StopMatch::Position(prefix_end))).unwrap_err());
    assert_eq!(file, 0);
    let _ = &l.secret;
}

#[test]
fn hc5_nested_stop_preserves_source_provenance() {
    // a stopped load held the secret's frames (C1, held), then removed them locally (A2)
    let l = loaded(true);
    let c1 = carried_hash(&l.record);
    let prefix_end = Store::open(&l.src).unwrap().log_end();
    let mut s = Store::open(&l.to).unwrap();
    let c1_end = s.log_end();
    let a2_landing = remove(
        &mut s,
        vec![RedactTarget::Version {
            id: l.secret.id.clone(),
            version: l.secret.version_hash(),
        }],
        90,
    );
    let a2 = framed(&s.reader(), s.log_end())
        .into_iter()
        .find(|f| f.kind == kinds::REDACTION_APPLY)
        .unwrap()
        .hash;
    let whole = put(l.dir.path(), "whole.rdydump", &dump(&s, None, &[]));
    drop(s);
    let files = vec![whole];
    // after C1, before A2: C1 stays as it is; A2's record recognizes the zeros under it
    let plan = ok(&files, Some(StopMatch::Position(c1_end)));
    assert_eq!(plan.carried().len(), 1, "{:?}", plan.carried());
    let c2 = &plan.carried()[0];
    assert_eq!((c2.apply, c2.landing), (a2, a2_landing));
    assert_ne!(c2.apply, l.record.apply);
    assert!(c2.spans.iter().all(|s| !s.held));
    let c2h = carried_hash(c2);
    for f in &l.frames {
        assert!(matches!(
            plan.frame_action(f.loc, f.kind, f.hash, f.len).unwrap(),
            FrameAction::Zero { authority, .. } if authority == c2h
        ));
    }
    // before C1 too: C1's provenance is copied (its source request, apply, landing and
    // approver), then A2's, in physical order
    let plan = ok(&files, Some(StopMatch::Position(prefix_end)));
    let got: Vec<(Hash, Hash, Hash)> = plan
        .carried()
        .iter()
        .map(|r| (r.request, r.apply, r.landing))
        .collect();
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!(
        got[0],
        (
            carried_fixture::source_request(),
            carried_fixture::source_apply(),
            carried_fixture::source_landing()
        )
    );
    assert_eq!(plan.carried()[0].approved_by, carried_fixture::approver());
    assert_eq!(got[1].1, a2);
    assert!(
        plan.carried()
            .iter()
            .all(|r| r.spans.iter().all(|s| !s.held))
    );
    assert_ne!(
        carried_hash(&plan.carried()[0]),
        c1,
        "a new record: its held flags differ"
    );
}

/// The holder base: a project with the secret, tagged `v1`; a holder that took `v1`; then the
/// project's approved removal of the secret, tagged `v2`.
struct HolderBase {
    dir: tempfile::TempDir,
    src: PathBuf,
    holder: PathBuf,
    secret: Block,
    v2: (Hash, Hash),
    fixture: redaction_fixture::Fixture,
}

fn upload(path: &Path, tag: Hash, out: &Path) -> Vec<File> {
    let s = Store::open(path).unwrap();
    let r = s.reader();
    let c = closure(&r, tag, &BTreeSet::new()).unwrap();
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        r.repo_id().unwrap().unwrap(),
        tag,
        c.prerequisites,
    );
    let mut source = StoreSource {
        reader: &r,
        end: END_OF_LOG,
    };
    write_batch(&mut source, &header, &c.records, File::create(out).unwrap()).unwrap();
    vec![File::open(out).unwrap()]
}

fn incoming(_: &mut dyn ObjectSource, _: &UploadFacts) -> Result<TagChoice, StreamError> {
    Ok(TagChoice::Incoming)
}

fn take(
    h: &mut Store,
    files: &[File],
    name: &str,
    landing: Hash,
) -> Result<BatchTaken, TakeError<StreamError>> {
    h.take_batches(
        files,
        &UploadTarget {
            tag: name.into(),
            landing,
        },
        incoming,
    )
}

fn holder_base() -> HolderBase {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let mut s = Store::create(&src).unwrap();
    // padding, so the holder's places differ from the source's
    s.put(kinds::BLOCK, &block("b-00009", &"p".repeat(4096)))
        .unwrap();
    let secret = block("b-00001", SECRET);
    let (_, l1) = land(
        &mut s,
        "work",
        creates(&[&secret, &block("b-00002", "beta")]),
    );
    let t1 = tag(&mut s, "v1", l1);
    let repo = s.repo_id().unwrap().unwrap();
    drop(s);
    let holder = dir.path().join("holder");
    drop(Store::create_holder(&holder, repo).unwrap());
    {
        let mut h = Store::open(&holder).unwrap();
        let files = upload(&src, t1, &dir.path().join("v1.rdysync"));
        take(&mut h, &files, "v1", l1).unwrap();
    }
    let fixture = authorized_batch_fixture(
        &src,
        &[RequestSpec::new(
            "work",
            vec![RedactTarget::Version {
                id: secret.id.clone(),
                version: secret.version_hash(),
            }],
        )
        .at(60)],
        &FixtureOptions::approved(Proof::local_human()),
    );
    let mut s = Store::open(&src).unwrap();
    let l2 = fixture.landing.unwrap();
    let t2 = tag(&mut s, "v2", l2);
    drop(s);
    HolderBase {
        dir,
        src,
        holder,
        secret,
        v2: (t2, l2),
        fixture,
    }
}

#[test]
fn hc5_holder_proofs_resolve_local_spans() {
    let b = holder_base();
    let mut h = Store::open(&b.holder).unwrap();
    let hf0 = dump(&h, None, &[]);
    let local: Vec<Framed> = framed(&h.reader(), h.log_end())
        .into_iter()
        .filter(|f| f.kind == kinds::BLOCK && f.hash == b.secret.version_hash())
        .collect();
    assert_eq!(local.len(), 1);
    let hf0_end = h.log_end();
    let files = upload(&b.src, b.v2.0, &b.dir.path().join("v2.rdysync"));
    take(&mut h, &files, "v2", b.v2.1).unwrap();
    let hi1 = dump(&h, Some(&end_of(&hf0)), &[]);
    let hf2 = dump(&h, None, &[]);
    let apply = b.fixture.applies[0].0;
    drop(h);
    let chain = vec![
        put(b.dir.path(), "hf0.rdydump", &hf0),
        put(b.dir.path(), "hi1.rdydump", &hi1),
    ];
    // the source's manifest places are not the holder's
    let source_places: Vec<Location> = b.fixture.requests[0]
        .draft
        .request
        .manifest
        .spans
        .iter()
        .map(|s| at(s.place.segment, s.place.offset))
        .collect();
    assert!(!source_places.contains(&local[0].loc));
    // the whole chain: the holder's own copy is zeroed by the taken apply
    let plan = ok(&chain, None);
    assert_eq!(plan.mode(), StoreMode::Holder);
    let f = local[0];
    assert!(matches!(
        plan.frame_action(f.loc, f.kind, f.hash, f.len).unwrap(),
        FrameAction::Zero { authority, .. } if authority == apply
    ));
    assert!(plan.carried().is_empty());
    // stopped before the take: a carried record of the local copies, held
    let stopped = ok(&chain, Some(StopMatch::Position(hf0_end)));
    assert_eq!(stopped.carried().len(), 1);
    let r = &stopped.carried()[0];
    assert_eq!(r.apply, apply);
    let places: Vec<Location> = r
        .spans
        .iter()
        .map(|s| at(s.place.segment, s.place.offset))
        .collect();
    assert!(places.contains(&f.loc), "{places:?}");
    assert!(places.iter().all(|p| !source_places.contains(p)));
    assert!(r.spans.iter().all(|s| s.held));
    // the full dump after the take: the dropped copies are the apply's own holes
    let plan = ok(&[put(b.dir.path(), "hf2.rdydump", &hf2)], None);
    assert_eq!(
        plan.frame_action(f.loc, f.kind, f.hash, f.len).unwrap(),
        FrameAction::Keep
    );
}

/// A full dump of a fresh store's batches, then one more batch of `drops` dropped chunk frames
/// of `len` bytes each, which no apply authorizes.
fn forged_drops(dir: &Path, drops: usize, len: u32) -> PathBuf {
    let mut s = Store::create(&dir.join(format!("s-{drops}-{len}"))).unwrap();
    land(&mut s, "work", creates(&[&block("b-00001", "a block")]));
    let r = s.reader();
    let end = s.log_end();
    let mut batches = vec![];
    r.for_each_batch(end, |b| {
        batches.push(b);
        Ok(())
    })
    .unwrap();
    let frames: Vec<Frame> = (0..drops)
        .map(|i| Frame::Dropped {
            kind: kinds::BLOB_CHUNK,
            hash: Hash::of(&(i as u64).to_le_bytes()),
            len,
            apply: Hash::of(b"an apply no file holds"),
            residue: None,
        })
        .collect();
    let digest = kinds::batch_digest(frames.iter().map(|f| f.hash()));
    let forged = DumpBatch {
        start: end,
        digest,
        frames,
    };
    let log_end = batch_end(&forged).unwrap();
    let header = DumpHeader {
        store: rdy_store::format::DumpStore::Features(rdy_store::format::baseline_features()),
        log_end,
        rdy: "test".into(),
        chain: Some(DumpChainHeader::full(
            r.repo_id().unwrap().unwrap(),
            BatchMarker::of(&forged),
        )),
    };
    let path = dir.join(format!("drops-{drops}-{len}.rdydump"));
    let mut w = DumpWriter::new(
        std::io::BufWriter::new(File::create(&path).unwrap()),
        &header,
    )
    .unwrap();
    for b in &batches {
        w.batch(b).unwrap();
    }
    w.batch(&forged).unwrap();
    w.finish(RemovalFacts::default()).unwrap();
    path
}

#[test]
fn hc5_pending_drops_keep_the_ingest_bounds() {
    let dir = tempfile::tempdir().unwrap();
    // bytes: 16 frames of 64 MiB are 1 GiB, which waits; one more is over the bound. No claimed
    // payload is ever allocated or read.
    let max = rdy_store::MAX_BLOB_BYTES as u32;
    let n = (rdy_store::ingest::MAX_UNAUTHORIZED_BYTES / u64::from(max)) as usize;
    let (_, _, what) = damage(run(&[forged_drops(dir.path(), n, max)], None).unwrap_err());
    assert_eq!(what, "dropped frame has no valid authorization");
    let (file, at_, what) = damage(run(&[forged_drops(dir.path(), n + 1, max)], None).unwrap_err());
    assert_eq!(
        what,
        "dropped frames claim more than 1073741824 bytes before their authorization"
    );
    assert_eq!(file, 0);
    assert!(at_ > 0);
    // count: 1,000,000 records wait; one more is refused
    let n = rdy_store::ingest::MAX_PENDING_DROPS;
    let (_, _, what) = damage(run(&[forged_drops(dir.path(), n, 1)], None).unwrap_err());
    assert_eq!(what, "dropped frame has no valid authorization");
    let (_, _, what) = damage(run(&[forged_drops(dir.path(), n + 1, 1)], None).unwrap_err());
    assert_eq!(what, "record allocation failed");
}

// ---------------------------------------------------------------------------------------------
// Fix round 1: hostile batch places, padded refs, memory, carried releases and the meaning pass.

/// What a format-3 dump holds: its header, its project files, its batches and its trailer.
struct Items {
    header: DumpHeader,
    files: Vec<(String, Vec<u8>)>,
    batches: Vec<DumpBatch>,
    trailer: DumpTrailer,
}

fn items(bytes: &[u8]) -> Items {
    let (mut rd, _, header) = DumpReader::open(bytes).unwrap();
    let mut files = vec![];
    let mut batches = vec![];
    while let Some(item) = rd.next().unwrap() {
        match item {
            DumpItem::File { path, bytes } => files.push((path, bytes)),
            DumpItem::Batch(b) => batches.push(b),
        }
    }
    let trailer = *rd.trailer().unwrap();
    Items {
        header,
        files,
        batches,
        trailer,
    }
}

impl Items {
    /// The dump of these items, written again; its header's end and end marker follow its last
    /// batch when `follow_end`.
    fn write(&self, follow_end: bool) -> Vec<u8> {
        let mut header = self.header.clone();
        if follow_end {
            let last = self.batches.last().unwrap();
            header.log_end = batch_end(last).unwrap_or(last.start);
            let mut chain = header.chain.unwrap();
            chain.end_batch = BatchMarker::of(last);
            header.chain = Some(chain);
        }
        let mut w = DumpWriter::new(Vec::new(), &header).unwrap();
        for (p, b) in &self.files {
            w.file(p, b).unwrap();
        }
        for b in &self.batches {
            w.batch(b).unwrap();
        }
        w.finish(RemovalFacts {
            removals: self.trailer.removals,
            first_removed: self.trailer.first_removed,
        })
        .unwrap()
        .0
    }
}

fn present(kind: u8, payload: Vec<u8>) -> Frame {
    Frame::Present {
        kind,
        hash: Hash::of(&payload),
        payload,
    }
}

fn batch_of(start: Location, frames: Vec<Frame>) -> DumpBatch {
    DumpBatch {
        start,
        digest: kinds::batch_digest(frames.iter().map(|f| f.hash())),
        frames,
    }
}

/// An increment after the dump `prev` that holds `batches` (the first at `prev`'s end) and no
/// removal facts.
fn increment(prev: &[u8], batches: Vec<DumpBatch>) -> Vec<u8> {
    let last = batches.last().unwrap();
    let header = DumpHeader {
        store: rdy_store::format::DumpStore::Features(rdy_store::format::baseline_features()),
        log_end: batch_end(last).unwrap(),
        rdy: "test".into(),
        chain: Some(DumpChainHeader::after(&end_of(prev), BatchMarker::of(last))),
    };
    let mut w = DumpWriter::new(Vec::new(), &header).unwrap();
    for b in &batches {
        w.batch(b).unwrap();
    }
    w.finish(RemovalFacts::default()).unwrap().0
}

/// `next`, an increment, written again after `prev` (whose trailer digest changed).
fn relink(next: &[u8], prev: &[u8]) -> Vec<u8> {
    let mut it = items(next);
    let mut chain = it.header.chain.unwrap();
    chain.prev = end_of(prev).digest;
    it.header.chain = Some(chain);
    it.write(false)
}

/// The offset of the `n`-th BATCH record (from 0) of the dump `bytes`.
fn batch_record(bytes: &[u8], n: usize) -> u64 {
    records(bytes)
        .into_iter()
        .filter(|r| r.kind == rec::BATCH)
        .nth(n)
        .unwrap()
        .offset
}

/// The offset of the first record of `kind` and `hash` in the dump `bytes`.
fn record_of(bytes: &[u8], kind: u8, hash: Hash) -> u64 {
    records(bytes)
        .into_iter()
        .find(|r| r.kind == kind && r.hash == hash)
        .unwrap()
        .offset
}

/// The present payload of the frame of `kind` and `hash` in `items`.
fn payload_of(it: &Items, kind: u8, hash: Hash) -> Vec<u8> {
    it.batches
        .iter()
        .flat_map(|b| b.frames.iter())
        .find(|f| rdy_store::codec::logical_kind(f.kind()) == kind && *f.hash() == hash)
        .and_then(|f| f.present_payload())
        .unwrap()
        .to_vec()
}

fn secret_target(b: &Block) -> Vec<RedactTarget> {
    vec![RedactTarget::Version {
        id: b.id.clone(),
        version: b.version_hash(),
    }]
}

/// A batch at `start` that claims the secret's block place of `h`: a dropped frame of the
/// secret's identity, named by a carried record of that span (`held: false`) in the same batch.
fn claiming_batch(h: &History, start: Location) -> DumpBatch {
    let s0 = h.secret_frames[0];
    let residue = block_residue(&h.secret);
    let c = carried_fixture::record(
        secret_target(&h.secret),
        vec![span(&s0, Some(&residue), false)],
    );
    batch_of(
        start,
        vec![
            Frame::Dropped {
                kind: s0.kind,
                hash: s0.hash,
                len: s0.len,
                apply: carried_hash(&c),
                residue: Some(carried_fixture::residue_hash(&residue)),
            },
            present(kinds::REDACTION_CARRIED, postcard::to_allocvec(&c).unwrap()),
        ],
    )
}

#[test]
fn hc5_batches_follow_each_other_in_every_file() {
    let h = history();
    let s0 = h.secret_frames[0];
    let i1_end = end_of(&h.bytes(&h.i1)).end;
    // a later file: i2's batches, then one that starts back at the secret's place in f0
    let mut it = items(&h.bytes(&h.i2));
    let n = it.batches.len();
    it.batches.push(claiming_batch(&h, s0.loc));
    let i2x = it.write(true);
    let files = vec![h.f0.clone(), h.i1.clone(), h.write("i2x", &i2x)];
    for stop in [None, Some(StopMatch::Position(i1_end))] {
        let (file, at_, what) = damage(run(&files, stop).unwrap_err());
        assert_eq!(
            (file, at_, what.as_str()),
            (2, batch_record(&i2x, n), BATCH_GAP)
        );
    }
    // the same batch in the middle of i1, with no stop
    let mut it = items(&h.bytes(&h.i1));
    it.batches.insert(1, claiming_batch(&h, s0.loc));
    let i1x = it.write(false);
    let files = vec![h.f0.clone(), h.write("i1x", &i1x)];
    let (file, at_, what) = damage(run(&files, None).unwrap_err());
    assert_eq!(
        (file, at_, what.as_str()),
        (1, batch_record(&i1x, 1), BATCH_GAP)
    );
    // and a batch that starts past the end of the batch before it
    let mut it = items(&h.bytes(&h.i2));
    let gap = it.batches.last().map(|b| batch_end(b).unwrap()).unwrap();
    it.batches.push(batch_of(
        at(gap.segment, gap.offset + 1),
        vec![present(
            kinds::BLOCK,
            postcard::to_allocvec(&block("b-00007", "x")).unwrap(),
        )],
    ));
    let i2g = it.write(true);
    let (file, at_, what) =
        damage(run(&[h.f0.clone(), h.i1.clone(), h.write("i2g", &i2g)], None).unwrap_err());
    assert_eq!(
        (file, at_, what.as_str()),
        (2, batch_record(&i2g, n), BATCH_GAP)
    );
}

#[test]
fn hc5_batch_start_near_the_end_of_the_position_space_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    land(&mut s, "work", creates(&[&block("b-00001", "a block")]));
    let f = dump(&s, None, &[]);
    drop(s);
    let mut it = items(&f);
    let n = it.batches.len();
    it.batches.push(batch_of(
        at(0, u64::MAX - 40),
        vec![present(
            kinds::BLOCK,
            postcard::to_allocvec(&block("b-00002", "far")).unwrap(),
        )],
    ));
    let bytes = it.write(true);
    let p = put(dir.path(), "far.rdydump", &bytes);
    let (file, at_, what) = damage(run(&[p], None).unwrap_err());
    assert_eq!(
        (file, at_, what.as_str()),
        (0, batch_record(&bytes, n), BATCH_GAP)
    );
}

#[test]
fn hc5_every_ref_update_counts_for_mode_and_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let pad = |p: &mut Vec<u8>| p.extend(std::iter::repeat_n(0u8, 70_000));
    // a project's last batch sets another repo id, padded past 64 KiB: postcard reads the ref
    // as the store does, so the header's repo is contradicted
    let mut s = Store::create(&dir.path().join("p")).unwrap();
    land(&mut s, "work", creates(&[&block("b-00001", "a block")]));
    let f = dump(&s, None, &[]);
    drop(s);
    let mut it = items(&f);
    let start = batch_end(it.batches.last().unwrap()).unwrap();
    let mut p = postcard::to_allocvec(&kinds::RefUpdate {
        name: "meta/repo_id".into(),
        value: vec![7u8; 16],
        at: start,
    })
    .unwrap();
    pad(&mut p);
    it.batches
        .push(batch_of(start, vec![present(kinds::REF_UPDATE, p)]));
    let bytes = it.write(true);
    let (file, _, what) =
        damage(run(&[put(dir.path(), "repo.rdydump", &bytes)], None).unwrap_err());
    assert_eq!((file, what.as_str()), (0, METADATA_MISMATCH));
    // a holder's first-batch mode ref, padded past 64 KiB: the scan reads the holder's mode
    let repo = RepoId([0x31; 16]);
    drop(Store::create_holder(&dir.path().join("h"), repo).unwrap());
    let h = Store::open(&dir.path().join("h")).unwrap();
    let hf = dump(&h, None, &[]);
    drop(h);
    let plain = ok(&[put(dir.path(), "h.rdydump", &hf)], None);
    assert_eq!(plain.mode(), StoreMode::Holder);
    let mut it = items(&hf);
    let mut padded = false;
    for f in it.batches[0].frames.iter_mut() {
        if let Frame::Present {
            kind: kinds::REF_UPDATE,
            payload,
            ..
        } = f
            && rdy_store::holder::names_store_mode(payload)
        {
            let mut p = payload.clone();
            pad(&mut p);
            assert!(rdy_store::holder::names_store_mode(&p));
            *f = present(kinds::REF_UPDATE, p);
            padded = true;
        }
    }
    assert!(padded);
    // every later batch moves by the padding
    let mut start = it.batches[0].start;
    for b in it.batches.iter_mut() {
        *b = batch_of(start, std::mem::take(&mut b.frames));
        start = batch_end(b).unwrap();
    }
    let bytes = it.write(true);
    let plan = ok(&[put(dir.path(), "hpad.rdydump", &bytes)], None);
    assert_eq!(plan.mode(), StoreMode::Holder);
}

/// The memory a scan of this thread holds at its peak, in bytes (the counting allocator).
mod counting {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    pub struct Counting;

    thread_local! {
        static NOW: Cell<usize> = const { Cell::new(0) };
        static PEAK: Cell<usize> = const { Cell::new(0) };
    }

    // SAFETY: every call goes to `System` with the caller's arguments unchanged; the counters
    // are this thread's own cells. The default `realloc` and `alloc_zeroed` come back here.
    #[allow(unsafe_code)]
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let _ = NOW.try_with(|n| {
                n.set(n.get() + l.size());
                let _ = PEAK.try_with(|p| p.set(p.get().max(n.get())));
            });
            // SAFETY: the caller's layout, as `GlobalAlloc::alloc` requires.
            unsafe { System.alloc(l) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, l: Layout) {
            let _ = NOW.try_with(|n| n.set(n.get().saturating_sub(l.size())));
            // SAFETY: `ptr` came from `System` with `l` (every allocation here is System's).
            unsafe { System.dealloc(ptr, l) }
        }
    }

    /// The peak of this thread's live allocations during `f`, above what was live before it.
    pub fn peak_during<T>(f: impl FnOnce() -> T) -> (T, usize) {
        let base = NOW.with(|n| n.get());
        PEAK.with(|p| p.set(base));
        let out = f();
        (out, PEAK.with(|p| p.get()) - base)
    }
}

#[global_allocator]
static COUNTING: counting::Counting = counting::Counting;

/// A full dump of a fresh store, then one batch of `n` LANDING frames and `n` REDACTION_APPLY
/// frames of eight bytes each.
fn landing_flood(dir: &Path, n: usize) -> PathBuf {
    let mut s = Store::create(&dir.join(format!("flood-{n}"))).unwrap();
    land(&mut s, "work", creates(&[&block("b-00001", "a block")]));
    let f = dump(&s, None, &[]);
    drop(s);
    let mut it = items(&f);
    let start = batch_end(it.batches.last().unwrap()).unwrap();
    let mut frames = vec![];
    for i in 0..n {
        frames.push(present(kinds::LANDING, (i as u64).to_le_bytes().repeat(4)));
    }
    for i in 0..n {
        frames.push(present(
            kinds::REDACTION_APPLY,
            (i as u64).to_le_bytes().to_vec(),
        ));
    }
    it.batches.push(batch_of(start, frames));
    // the trailer counts the apply frames
    it.trailer.removals = n as u32;
    put(dir, &format!("flood-{n}.rdydump"), &it.write(true))
}

#[test]
fn hc5_landing_flood_holds_linear_memory() {
    let dir = tempfile::tempdir().unwrap();
    let small = landing_flood(dir.path(), 1000);
    let large = landing_flood(dir.path(), 2000);
    let (e, at_small) = counting::peak_during(|| run(std::slice::from_ref(&small), None));
    assert_eq!(
        damage(e.unwrap_err()).2,
        "dropped frame has no valid authorization"
    );
    let (e, at_large) = counting::peak_during(|| run(std::slice::from_ref(&large), None));
    assert_eq!(
        damage(e.unwrap_err()).2,
        "dropped frame has no valid authorization"
    );
    // twice the input holds at most about twice the memory, never four times
    assert!(
        at_large < 3 * at_small,
        "{at_small} bytes at 1000 landings, {at_large} bytes at 2000"
    );
}

/// The proof the store gives the frame at `loc`: the record a dropped frame there names.
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

/// A holder loaded with a stop before its take of `v2`: its secret copy `p` is zero, and the
/// carried record `record` (of the taken apply) names it `held: false`.
struct LoadedHolder {
    b: HolderBase,
    loaded: PathBuf,
    record: RedactionCarried,
    p: Framed,
    apply: Hash,
}

fn loaded_holder() -> LoadedHolder {
    let b = holder_base();
    let pre = b.dir.path().join("pre");
    carried_fixture::copy_store(&b.holder, &pre);
    let mut h = Store::open(&b.holder).unwrap();
    let hf0_end = h.log_end();
    let p = *framed(&h.reader(), h.log_end())
        .iter()
        .find(|f| f.kind == kinds::BLOCK && f.hash == b.secret.version_hash())
        .unwrap();
    let files = upload(&b.src, b.v2.0, &b.dir.path().join("v2-a.rdysync"));
    take(&mut h, &files, "v2", b.v2.1).unwrap();
    let hf2 = put(b.dir.path(), "hf2.rdydump", &dump(&h, None, &[]));
    drop(h);
    let plan = ok(&[hf2], Some(StopMatch::Position(hf0_end)));
    assert_eq!(plan.carried().len(), 1);
    let record = plan.carried()[0].clone();
    assert!(record.spans.iter().all(|s| !s.held));
    let loaded = b.dir.path().join("loaded");
    carried_fixture::load(&pre, &loaded, plan.residues(), plan.carried()).unwrap();
    let apply = b.fixture.applies[0].0;
    LoadedHolder {
        b,
        loaded,
        record,
        p,
        apply,
    }
}

impl LoadedHolder {
    /// The loaded holder takes `v2`, whose apply now covers the copy.
    fn take_v2(&self, s: &mut Store, name: &str) {
        let files = upload(&self.b.src, self.b.v2.0, &self.b.dir.path().join(name));
        take(s, &files, "v2", self.b.v2.1).unwrap();
    }
}

#[test]
fn hc5_carried_record_releases_only_drops_it_names() {
    // a stopped load, then a take of the matching apply, then a dump: the copy's dropped frame
    // names the apply, and the carried record before it leaves that drop to the apply
    let l = loaded_holder();
    let mut s = Store::open(&l.loaded).unwrap();
    assert_eq!(proof_at(&s, l.p.loc), Some(carried_hash(&l.record)));
    l.take_v2(&mut s, "v2-b.rdysync");
    assert_eq!(proof_at(&s, l.p.loc), Some(l.apply));
    let whole = put(l.b.dir.path(), "whole.rdydump", &dump(&s, None, &[]));
    drop(s);
    let plan = ok(&[whole], None);
    assert_eq!(plan.mode(), StoreMode::Holder);
    assert_eq!(
        plan.frame_action(l.p.loc, l.p.kind, l.p.hash, l.p.len)
            .unwrap(),
        FrameAction::Keep
    );
}

#[test]
fn hc5_drop_named_by_its_earlier_carried_proof_loads() {
    // f0 before the take (the copy's dropped frame names the carried record), i1 after it (the
    // taken apply covers the copy)
    let l = loaded_holder();
    let c = carried_hash(&l.record);
    let mut s = Store::open(&l.loaded).unwrap();
    let f0 = dump(&s, None, &[]);
    l.take_v2(&mut s, "v2-c.rdysync");
    let i1 = dump(&s, Some(&end_of(&f0)), &[]);
    drop(s);
    let files = vec![
        put(l.b.dir.path(), "lf0.rdydump", &f0),
        put(l.b.dir.path(), "li1.rdydump", &i1),
    ];
    ok(&files, None);
    // the whole load of the same batches finishes
    let to = l.b.dir.path().join("whole-load");
    let mut ingest = rdy_store::ingest::Ingest::create(&to).unwrap();
    let mut n = 0;
    for bytes in [&f0, &i1] {
        for b in items(bytes).batches {
            n += 1;
            ingest.batch(n, &b).unwrap();
        }
    }
    ingest.upgrade(&carried_fixture::store_format()).unwrap();
    ingest.finish().unwrap();
    // the loaded store names the apply as the copy's proof, and keeps the carried record
    let s = Store::open(&to).unwrap();
    assert_eq!(proof_at(&s, l.p.loc), Some(l.apply));
    let mut kept = vec![];
    s.reader()
        .visit_carried(END_OF_LOG, &mut |x| {
            kept.push(x.hash);
            Ok(())
        })
        .unwrap();
    assert_eq!(kept, vec![c]);
}

#[test]
fn hc5_span_header_is_checked_against_its_frame() {
    // (a) f0's secret block frame holds other bytes of the same length: the apply's span names
    // a frame whose header is not the span's
    let h = history();
    let s0 = h.secret_frames[0];
    let mut it = items(&h.bytes(&h.f0));
    for b in it.batches.iter_mut() {
        let mut changed = false;
        for f in b.frames.iter_mut() {
            if f.kind() == kinds::BLOCK && *f.hash() == s0.hash {
                let mut p = f.present_payload().unwrap().to_vec();
                let i = p.windows(6).position(|w| w == b"canary").unwrap();
                p[i] = b'C';
                *f = present(kinds::BLOCK, p);
                changed = true;
            }
        }
        if changed {
            *b = batch_of(b.start, std::mem::take(&mut b.frames));
        }
    }
    let f0x = it.write(false);
    let i1 = h.bytes(&h.i1);
    let i1x = relink(&i1, &f0x);
    let files = vec![h.write("f0x", &f0x), h.write("i1x", &i1x)];
    let apply_at = record_of(&i1x, kinds::REDACTION_APPLY, h.apply);
    // the apply in a file wholly after the stop is checked too
    for stop in [None, Some(StopMatch::Position(h.f0_end()))] {
        let (file, at_, what) = damage(run(&files, stop).unwrap_err());
        assert_eq!(
            (file, at_, what.as_str()),
            (1, apply_at, "dropped frame has no valid authorization")
        );
    }
}

#[test]
fn hc5_meaning_of_applies_is_checked_before_target_writes() {
    // (b) an intact copy of the removed version after the removal: the apply no longer covers
    // every copy
    let h = history();
    let s0 = h.secret_frames[0];
    let raw = payload_of(&items(&h.bytes(&h.f0)), kinds::BLOCK, s0.hash);
    let i1 = h.bytes(&h.i1);
    let start = end_of(&i1).end;
    let i2x = increment(&i1, vec![batch_of(start, vec![present(kinds::BLOCK, raw)])]);
    let files = vec![h.f0.clone(), h.i1.clone(), h.write("i2-copy", &i2x)];
    let copy_at = record_of(&i2x, kinds::BLOCK, s0.hash);
    for stop in [None, Some(StopMatch::Position(h.f0_end()))] {
        let (file, at_, what) = damage(run(&files, stop).unwrap_err());
        assert_eq!(
            (file, at_, what.as_str()),
            (2, copy_at, "dropped frame has no valid authorization")
        );
    }
}

#[test]
fn hc5_holder_uncovered_embedding_is_refused() {
    // (c) a holder's intact change outside the approved tuples that embeds the removed version
    let b = holder_base();
    let mut h = Store::open(&b.holder).unwrap();
    let hf0 = dump(&h, None, &[]);
    let hf0_end = h.log_end();
    let files = upload(&b.src, b.v2.0, &b.dir.path().join("v2.rdysync"));
    take(&mut h, &files, "v2", b.v2.1).unwrap();
    let hi1 = dump(&h, Some(&end_of(&hf0)), &[]);
    drop(h);
    let mut c = change("work", creates(&[&b.secret]));
    c.message = "another copy".into();
    let raw = postcard::to_allocvec(&c).unwrap();
    let hash = Hash::of(&raw);
    let hix = increment(
        &hi1,
        vec![batch_of(
            end_of(&hi1).end,
            vec![present(kinds::CHANGE, raw)],
        )],
    );
    let chain = vec![
        put(b.dir.path(), "hf0.rdydump", &hf0),
        put(b.dir.path(), "hi1.rdydump", &hi1),
        put(b.dir.path(), "hix.rdydump", &hix),
    ];
    let change_at = record_of(&hix, kinds::CHANGE, hash);
    for stop in [None, Some(StopMatch::Position(hf0_end))] {
        let (file, at_, what) = damage(run(&chain, stop).unwrap_err());
        assert_eq!(
            (file, at_, what.as_str()),
            (2, change_at, "dropped frame has no valid authorization")
        );
    }
}

#[test]
fn hc5_carried_records_keep_their_store_rules() {
    // (d) a carried record (held, over f0's intact secret frames) whose approver is no Approver
    let h = history();
    let f0 = h.bytes(&h.f0);
    let it = items(&f0);
    let c1 = h.secret_frames[1].hash;
    let stored: Change = rdy_store::codec::decode_record(
        rdy_store::codec::raw_kind(kinds::CHANGE, rdy_store::codec::latest(kinds::CHANGE)),
        &payload_of(&it, kinds::CHANGE, c1),
    )
    .unwrap();
    let residues = vec![block_residue(&h.secret), change_residue(&stored)];
    let spans = vec![
        span(&h.secret_frames[0], Some(&residues[0]), true),
        span(&h.secret_frames[1], Some(&residues[1]), true),
    ];
    let with = |role: Role, name: &str| {
        let mut r = carried_fixture::record(secret_target(&h.secret), spans.clone());
        r.approved_by.role = role;
        let frames: Vec<Frame> = carried_fixture::carried_frames(&residues, &[r.clone()])
            .into_iter()
            .map(|(k, p)| present(k, p))
            .collect();
        let ix = increment(&f0, vec![batch_of(end_of(&f0).end, frames)]);
        let at_ = record_of(&ix, kinds::REDACTION_CARRIED, carried_hash(&r));
        (vec![h.f0.clone(), h.write(name, &ix)], at_)
    };
    let (good, _) = with(Role::Approver, "carried-approver");
    ok(&good, None);
    let (bad, at_) = with(Role::Writer, "carried-writer");
    for stop in [None, Some(StopMatch::Position(h.v1_end))] {
        let (file, got, what) = damage(run(&bad, stop).unwrap_err());
        assert_eq!(
            (file, got, what.as_str()),
            (1, at_, "dropped frame has no valid authorization")
        );
    }
}

#[test]
fn hc5_distinct_carried_records_of_one_source_apply_conflict() {
    // (e) a later carried record of the retained record's source apply, with other spans
    let l = loaded(true);
    let s = Store::open(&l.to).unwrap();
    let f0 = dump(&s, None, &[]);
    drop(s);
    let mut other = l.record.clone();
    other.spans.truncate(1);
    assert_ne!(carried_hash(&other), carried_hash(&l.record));
    let ix = increment(
        &f0,
        vec![batch_of(
            end_of(&f0).end,
            vec![present(
                kinds::REDACTION_CARRIED,
                postcard::to_allocvec(&other).unwrap(),
            )],
        )],
    );
    let files = vec![
        put(l.dir.path(), "lf0.rdydump", &f0),
        put(l.dir.path(), "lix.rdydump", &ix),
    ];
    let at_ = record_of(&ix, kinds::REDACTION_CARRIED, carried_hash(&other));
    for stop in [None, Some(StopMatch::Position(end_of(&f0).end))] {
        let (file, got, what) = damage(run(&files, stop).unwrap_err());
        assert_eq!((file, got, what.as_str()), (1, at_, PROVENANCE_CONFLICT));
    }
}

// ---------------------------------------------------------------------------------------------
// Fix round 2: the carried records of a chain are read one at a time.

/// The length of the approver name each flooded carried record holds, in bytes.
const FLOOD_ACTOR: usize = 200_000;

/// `f0` of `h`, then two increments with `n` identical copies of one carried record (held, over
/// `f0`'s intact secret frames, whose approver's name is `FLOOD_ACTOR` bytes long): in the first
/// increment, one batch with the residues and `n / 2` copies; in the second, `n / 2` batches of
/// one copy each. Returns the chain and the encoded length of one copy.
fn carried_flood(h: &History, n: usize) -> (Vec<PathBuf>, usize) {
    let f0 = h.bytes(&h.f0);
    let it = items(&f0);
    let c1 = h.secret_frames[1].hash;
    let stored: Change = rdy_store::codec::decode_record(
        rdy_store::codec::raw_kind(kinds::CHANGE, rdy_store::codec::latest(kinds::CHANGE)),
        &payload_of(&it, kinds::CHANGE, c1),
    )
    .unwrap();
    let residues = vec![block_residue(&h.secret), change_residue(&stored)];
    let spans = vec![
        span(&h.secret_frames[0], Some(&residues[0]), true),
        span(&h.secret_frames[1], Some(&residues[1]), true),
    ];
    let mut r = carried_fixture::record(secret_target(&h.secret), spans);
    r.approved_by.actor = format!("human:{}", "a".repeat(FLOOD_ACTOR));
    let copy = postcard::to_allocvec(&r).unwrap();
    let mut first: Vec<Frame> = carried_fixture::carried_frames(&residues, &[r])
        .into_iter()
        .map(|(k, p)| present(k, p))
        .collect();
    for _ in 1..n / 2 {
        first.push(present(kinds::REDACTION_CARRIED, copy.clone()));
    }
    let i1 = increment(&f0, vec![batch_of(end_of(&f0).end, first)]);
    let mut batches = vec![];
    let mut start = end_of(&i1).end;
    for _ in 0..n / 2 {
        let b = batch_of(start, vec![present(kinds::REDACTION_CARRIED, copy.clone())]);
        start = batch_end(&b).unwrap();
        batches.push(b);
    }
    let i2 = increment(&i1, batches);
    (
        vec![
            h.f0.clone(),
            h.write(&format!("flood-{n}-i1.rdydump"), &i1),
            h.write(&format!("flood-{n}-i2.rdydump"), &i2),
        ],
        copy.len(),
    )
}

#[test]
fn hc5_carried_record_flood_holds_one_record_at_a_time() {
    let h = history();
    let (small, one) = carried_flood(&h, 8);
    let (large, _) = carried_flood(&h, 16);
    for stop in [None, Some(StopMatch::Position(h.f0_end()))] {
        let (plan, at_small) = counting::peak_during(|| run(&small, stop.clone()));
        let tail = plan.unwrap().carried().len();
        let (plan, at_large) = counting::peak_during(|| run(&large, stop.clone()));
        assert_eq!(plan.unwrap().carried().len(), tail);
        // a stop before the copies prepares one carried record of them; no stop, none
        assert_eq!(tail, usize::from(stop.is_some()));
        // eight more copies, in one batch and in more batches of another file, hold less than
        // one more record
        assert!(
            at_large < at_small + one,
            "stop {stop:?}: {at_small} bytes at 8 copies, {at_large} bytes at 16 (one copy: {one})"
        );
    }
}
