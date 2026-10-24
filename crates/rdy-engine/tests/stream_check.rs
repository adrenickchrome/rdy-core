//! ha9 (D-D2NPN verify-on-receive, D-DAGNB, D-WDKWQ; hub i-QD955, i-VZ93S, r-X13HC, r-E0RJX):
//! the stateless batch check. One batch is read once from any reader and checked whole; only a
//! batch that passes returns its keys, in wire order. The check opens no store, reads no config
//! and keeps no state, so a new process can make each call. A batch may hold leaves only, its
//! root and dependencies being prerequisites that another batch or the receiver holds.
//!
//! It also pins the shared snapshot limits: the resource rules of a publication of the whole
//! snapshot, checked by the take for every landing snapshot, give the same results and errors
//! as Publications at N and N+1.
mod common;

use rdy_engine::publication::{self, IMPLICIT_ORG, Limits};
use rdy_engine::stream_check::check_batch;
use rdy_engine::*;
use rdy_model::*;
use rdy_schema::Severity;
use rdy_store::closure::{self, END_OF_LOG, StoreSource};
use rdy_store::kinds::{self, BlobManifest};
use rdy_store::object_refs::{ObjectKey, refs_of};
use rdy_store::sync_stream::{self, StreamHeader, StreamRecord, StreamWriter};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------------------------
// helpers

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

fn write(r: &mut Rdy, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "write to work".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy) {
    let p = r
        .propose(&writer(), "work", "batch", Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap();
}

/// A source project: a doc with prose that references another doc, a figure with a two-chunk
/// blob, landed and tagged v1.0.0. Returns its directory, the project and the tag's hash.
fn source() -> (tempfile::TempDir, Rdy, Hash) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let blob = r.store_mut().put_blob(&vec![7u8; (1 << 20) + 5]).unwrap();
    write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$e", "fields": {"name": "other", "title": "Other", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "See {{$e}}."},
            {"op": "create", "type": "figure", "id": "$f", "parent": "$d",
             "fields": {"blob": blob.to_string(), "media_type": "application/octet-stream", "alt": "fig"}}
        ]),
    );
    land(&mut r);
    r.tag(&founder(), "v1.0.0", None, None).unwrap();
    let tag = tag_hash(&r, "v1.0.0");
    (dir, r, tag)
}

fn tag_hash(r: &Rdy, name: &str) -> Hash {
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap()
        .1
}

fn repo(r: &Rdy) -> RepoId {
    r.read().repo_id().unwrap()
}

/// The closure of `tag` in `r` less `known`, as one batch, with the closure.
fn batch_of(r: &Rdy, tag: Hash, known: &BTreeSet<ObjectKey>) -> (Vec<u8>, closure::Closure) {
    let reader = r.store().reader();
    let c = closure::closure(&reader, tag, known).unwrap();
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        repo(r),
        tag,
        c.prerequisites.clone(),
    );
    let bytes = written(r, &header, &c.records);
    (bytes, c)
}

/// `records` of `r`, in this order, as one batch with `header`.
fn written(r: &Rdy, header: &StreamHeader, records: &[ObjectKey]) -> Vec<u8> {
    let reader = r.store().reader();
    let mut source = StoreSource {
        reader: &reader,
        end: END_OF_LOG,
    };
    closure::write_batch(&mut source, header, records, Vec::new())
        .unwrap()
        .0
}

/// The original bytes of `key` in `r`.
fn payload(r: &Rdy, key: ObjectKey) -> Vec<u8> {
    r.store().get_kind(key.kind, &key.hash).unwrap()
}

/// What a check refused: its code, message and fix.
fn refusal(e: &RdyError) -> (&'static str, String, String) {
    (e.code, e.message.clone(), e.fix.clone())
}

/// The one positioned issue a stream refusal carries.
fn position(e: &RdyError) -> String {
    assert_eq!(e.issues.len(), 1, "{e:?}");
    let i = &e.issues[0];
    assert_eq!(
        (i.severity, i.code, i.block.is_none(), i.fix.as_str()),
        (Severity::Error, e.code, true, e.fix.as_str())
    );
    i.message.clone()
}

/// The core check of `bytes` must refuse, and with the store check's own error.
fn refused_as_store(bytes: &[u8]) -> RdyError {
    let store = sync_stream::check_batch(bytes).unwrap_err();
    let e = check_batch(bytes, None).unwrap_err();
    assert_eq!(
        serde_json::to_value(&e).unwrap(),
        serde_json::to_value(stream_error::to_rdy(store)).unwrap()
    );
    e
}

/// A scratch store used only to encode records as rdy stores them.
struct Encoder {
    _dir: tempfile::TempDir,
    store: rdy_store::Store,
}

impl Encoder {
    fn new() -> Encoder {
        let dir = tempfile::tempdir().unwrap();
        let store = rdy_store::Store::create(&dir.path().join("store")).unwrap();
        Encoder { _dir: dir, store }
    }

    /// A present record of `kind` holding `value`.
    fn record<T: serde::Serialize + 'static>(&mut self, kind: u8, value: &T) -> StreamRecord {
        let hash = self.store.put(kind, value).unwrap();
        StreamRecord::Present {
            encoding: 0,
            key: ObjectKey { kind, hash },
            payload: self.store.get_kind(kind, &hash).unwrap(),
        }
    }
}

/// The TAG key a crafted batch names as its absent root.
fn root() -> ObjectKey {
    ObjectKey {
        kind: kinds::TAG,
        hash: Hash([0x11; 32]),
    }
}

/// `records` as one batch of repo 7s rooted at [`root`], whose prerequisites are the root and
/// `extra`.
fn crafted(records: &[StreamRecord], extra: &[ObjectKey]) -> Vec<u8> {
    let mut pre: Vec<ObjectKey> = extra.to_vec();
    pre.push(root());
    pre.sort();
    pre.dedup();
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        RepoId([7; 16]),
        root().hash,
        pre,
    );
    let mut w = StreamWriter::new(Vec::new(), &header).unwrap();
    for r in records {
        w.record(r).unwrap();
    }
    w.finish().unwrap().0
}

/// The prelude and header frame of a crafted batch with no prerequisite but the root.
fn crafted_head() -> Vec<u8> {
    let b = crafted(&[], &[]);
    // prelude 12 + header frame (37 + 57 + the manifest + 33)
    let manifest = rdy_store::format::baseline_features().to_json().len();
    b[..12 + 37 + 57 + manifest + 33].to_vec()
}

fn frame_head(len: u32, kind: u8, hash: &Hash) -> Vec<u8> {
    let mut f = len.to_le_bytes().to_vec();
    f.push(kind);
    f.extend_from_slice(&hash.0);
    f
}

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn blk(id: &str, ty: &str, parent: Option<&str>, order: &str) -> Block {
    Block {
        id: bid(id),
        block_type: ty.into(),
        parent: parent.map(bid),
        order: order.into(),
        fields: BTreeMap::new(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn s(v: &str) -> FieldValue {
    FieldValue::Str(v.into())
}

fn named(mut b: Block, name: &str) -> Block {
    b.fields.insert("name".into(), s(name));
    b
}

fn with_body(mut b: Block, body: &str) -> Block {
    b.body = body.into();
    b
}

fn with_field(mut b: Block, k: &str, v: FieldValue) -> Block {
    b.fields.insert(k.into(), v);
    b
}

fn link(mut b: Block, kind: &str, target: &str) -> Block {
    b.links.entry(kind.into()).or_default().push(Link {
        target: bid(target),
        part: None,
    });
    b
}

/// A state of `blocks` alone, built in memory.
fn synth(blocks: &[Block]) -> Materialized {
    Materialized::build(
        StateSnapshot {
            blocks: BTreeMap::new(),
        },
        blocks.iter().map(|b| (b.id.clone(), b.clone())).collect(),
    )
}

fn limit_text(part: &str, limit: u64, unit: &str) -> (&'static str, String, String) {
    (
        E_LIMIT,
        format!("Publication {part} exceeds the limit of {limit} {unit}."),
        "Reduce the selection or split the source project.".to_string(),
    )
}

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn ha9_check_opens_no_store_and_reads_no_config() {
    if child_check() {
        return;
    }
    let (_src, r, tag) = source();
    let (bytes, c) = batch_of(&r, tag, &BTreeSet::new());

    // a directory that is no project: a config that does not parse, a store that is no store,
    // and a pack directory that cannot be read as one
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "{{{ this: is: not yaml").unwrap();
    std::fs::create_dir_all(dir.path().join(".rdy/packs")).unwrap();
    std::fs::write(dir.path().join(".rdy/store"), b"not a store").unwrap();
    std::fs::write(dir.path().join(".rdy/packs/broken.yaml"), b"\xff\xfe").unwrap();
    let file = dir.path().join("upload-1.rdysync");
    std::fs::write(&file, &bytes).unwrap();
    let before = listing(dir.path());

    let report = check_batch(std::fs::File::open(&file).unwrap(), Some(repo(&r))).unwrap();
    assert_eq!(report.keys, c.records);
    assert_eq!(report, sync_stream::check_batch(bytes.as_slice()).unwrap());
    assert_eq!(
        listing(dir.path()),
        before,
        "nothing is created, changed or removed"
    );
    assert_eq!(
        std::fs::read(&file).unwrap(),
        bytes,
        "the batch file is the hub's"
    );

    // and in a new process whose working directory is that directory, so a check that read a
    // config or store from its working directory would meet the broken ones; the report goes to
    // a file outside it
    let out = tempfile::tempdir().unwrap();
    let there = in_new_process(
        "ha9_check_opens_no_store_and_reads_no_config",
        Path::new("upload-1.rdysync"),
        Some(repo(&r)),
        &out.path().join("report"),
        Some(dir.path()),
    );
    assert_eq!(there, serde_json::to_string(&report).unwrap());
    assert_eq!(
        listing(dir.path()),
        before,
        "nothing is created, changed or removed"
    );

    // the source project is open (its writer holds the store lock) while its batch is checked
    assert_eq!(check_batch(bytes.as_slice(), None).unwrap(), report);
}

/// Every file under `dir` with its bytes and modified time.
fn listing(dir: &Path) -> BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let m = std::fs::metadata(&p).unwrap();
            if m.is_dir() {
                out.insert(p.clone(), (vec![], m.modified().unwrap()));
                stack.push(p);
            } else {
                out.insert(
                    p.clone(),
                    (std::fs::read(&p).unwrap(), m.modified().unwrap()),
                );
            }
        }
    }
    out
}

#[test]
fn ha9_keys_are_returned_in_wire_order() {
    let (_src, r, tag) = source();
    let (bytes, c) = batch_of(&r, tag, &BTreeSet::new());
    let report = check_batch(bytes.as_slice(), Some(repo(&r))).unwrap();
    assert_eq!(report.keys, c.records, "the batch's own order");
    assert_eq!(report.records, c.records.len() as u64);
    assert_eq!(report.header.tag, tag);
    assert!(report.header.prerequisites.is_empty());
    let mut sorted = report.keys.clone();
    sorted.sort();
    assert_ne!(report.keys, sorted, "wire order, not key order");

    // another valid order of the same records: every record that needs nothing first, in
    // reverse, then the rest in their order
    let (leaves, rest): (Vec<ObjectKey>, Vec<ObjectKey>) = c
        .records
        .iter()
        .partition(|k| refs_of(k.kind, &payload(&r, **k)).unwrap().is_empty());
    assert!(leaves.len() >= 3, "{leaves:?}");
    let mut order: Vec<ObjectKey> = leaves.iter().rev().copied().collect();
    order.extend(rest);
    assert_ne!(order, c.records);
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        repo(&r),
        tag,
        vec![],
    );
    let again = written(&r, &header, &order);
    let report2 = check_batch(again.as_slice(), None).unwrap();
    assert_eq!(report2.keys, order, "exactly the wire sequence");
    assert_eq!(report2, sync_stream::check_batch(again.as_slice()).unwrap());
    assert_ne!(report2.digest, report.digest);
}

#[test]
fn ha9_failed_batch_returns_no_report() {
    let (_src, r, tag) = source();
    let (bytes, c) = batch_of(&r, tag, &BTreeSet::new());
    let n = c.records.len() as u64;
    let trailer_at = bytes.len() - 85;

    // the final frame, the trailer, with its digest changed (its frame hash made to match)
    let mut trailer = bytes[trailer_at + 37..].to_vec();
    *trailer.last_mut().unwrap() ^= 1;
    let mut bad = bytes[..trailer_at].to_vec();
    bad.extend(frame_head(48, 0xFF, &Hash::of(&trailer)));
    bad.extend(&trailer);
    let e = refused_as_store(&bad);
    assert_eq!(
        refusal(&e),
        (
            E_STREAM_TRUNCATED,
            "stream trailer is missing or does not match".to_string(),
            "request the complete stream again".to_string()
        )
    );
    assert_eq!(
        position(&e),
        format!("record {} at byte {trailer_at}; object none", n + 1)
    );

    // the trailer's bytes changed: its frame hash no longer matches
    let mut bad = bytes.clone();
    *bad.last_mut().unwrap() ^= 1;
    let e = refused_as_store(&bad);
    assert_eq!(
        refusal(&e).1,
        format!(
            "stream record {} at byte {trailer_at}: hash mismatch",
            n + 1
        )
    );

    // the trailer cut short, then gone
    for cut in [bytes.len() - 1, trailer_at + 10, trailer_at] {
        let e = refused_as_store(&bytes[..cut]);
        assert_eq!(e.code, E_STREAM_TRUNCATED, "cut at {cut}");
    }

    // the last object record's payload changed: its own hash no longer matches
    let last = *c.records.last().unwrap();
    let last_len = payload(&r, last).len();
    let last_at = trailer_at - 37 - last_len;
    let mut bad = bytes.clone();
    bad[trailer_at - 1] ^= 1;
    let e = refused_as_store(&bad);
    assert_eq!(
        refusal(&e).1,
        format!("stream record {n} at byte {last_at}: hash mismatch")
    );
    assert_eq!(e.code, E_BAD_RECORD);
    assert!(position(&e).starts_with(&format!("record {n} at byte {last_at}; object ")));

    // a byte after the trailer
    let mut bad = bytes.clone();
    bad.push(0);
    let e = refused_as_store(&bad);
    assert_eq!(
        refusal(&e).1,
        format!(
            "stream record {} at byte {}: trailing bytes",
            n + 2,
            bytes.len()
        )
    );

    // a failure leaves nothing behind: the intact batch passes at once
    assert_eq!(check_batch(bytes.as_slice(), None).unwrap().keys, c.records);
}

#[test]
fn ha9_batch_can_precede_root_and_dependencies() {
    let (_src, r, tag) = source();
    let full = closure::closure(&r.store().reader(), tag, &BTreeSet::new()).unwrap();
    let root = ObjectKey {
        kind: kinds::TAG,
        hash: tag,
    };
    // leaves only: records that need nothing; the root is the one prerequisite
    let leaves: Vec<ObjectKey> = full
        .records
        .iter()
        .copied()
        .filter(|k| refs_of(k.kind, &payload(&r, *k)).unwrap().is_empty())
        .collect();
    assert!(leaves.iter().any(|k| k.kind == kinds::BLOCK));
    assert!(leaves.iter().any(|k| k.kind == kinds::BLOB_CHUNK));
    assert!(!leaves.contains(&root));
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        repo(&r),
        tag,
        vec![root],
    );
    let first = written(&r, &header, &leaves);
    let a = check_batch(first.as_slice(), Some(repo(&r))).unwrap();
    assert_eq!(a.keys, leaves);
    assert_eq!(a.header.prerequisites, vec![root]);

    // the rest, which needs the leaves: they are its prerequisites, exactly those it needs
    let known: BTreeSet<ObjectKey> = leaves.iter().copied().collect();
    let (second, rest) = batch_of(&r, tag, &known);
    let b = check_batch(second.as_slice(), Some(repo(&r))).unwrap();
    assert_eq!(b.keys, rest.records);
    assert_eq!(b.header.prerequisites, rest.prerequisites);
    assert!(!b.header.prerequisites.is_empty());
    assert!(b.header.prerequisites.iter().all(|k| known.contains(k)));
    assert!(b.keys.contains(&root));

    // either order, each alone: together they hold the closure once
    let mut union: Vec<ObjectKey> = b.keys.iter().chain(&a.keys).copied().collect();
    union.sort();
    let mut all = full.records.clone();
    all.sort();
    assert_eq!(union, all);

    // a batch that lists a prerequisite it does not need is not valid alone
    let mut extra = vec![root, *leaves.first().unwrap()];
    extra.sort();
    let header =
        StreamHeader::tag_history(rdy_store::format::baseline_features(), repo(&r), tag, extra);
    let bad = written(&r, &header, &leaves[1..]);
    let e = refused_as_store(&bad);
    let first = leaves.first().unwrap();
    assert_eq!(
        e.message,
        format!(
            "stream record 0 at byte 12: unneeded prerequisite {}:{}",
            first.kind, first.hash
        )
    );
    assert_eq!(e.issues[0].message, "record 0 at byte 12; object none");
}

/// The child half of [`ha9_check_has_no_process_state`] and
/// [`ha9_check_opens_no_store_and_reads_no_config`]: in a new process, checks the batch file the
/// parent names and writes the report's JSON, or the error's code and message.
fn child_check() -> bool {
    let Ok(file) = std::env::var("RDY_HA9_BATCH") else {
        return false;
    };
    let out = std::env::var("RDY_HA9_OUT").unwrap();
    let repo = std::env::var("RDY_HA9_REPO").ok().map(|hex| {
        let b: Vec<u8> = (0..16)
            .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
            .collect();
        RepoId(b.try_into().unwrap())
    });
    let text = match check_batch(std::fs::File::open(file).unwrap(), repo) {
        Ok(report) => serde_json::to_string(&report).unwrap(),
        Err(e) => format!("{}: {}", e.code, e.message),
    };
    std::fs::write(out, text).unwrap();
    true
}

/// Runs this test binary's test `test` in a new process on `file`, in working directory `cwd`
/// when given. The working directory is the child's own, so tests that run in parallel keep
/// theirs.
fn in_new_process(
    test: &str,
    file: &Path,
    repo: Option<RepoId>,
    out: &Path,
    cwd: Option<&Path>,
) -> String {
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", test, "--test-threads=1"])
        .env("RDY_HA9_BATCH", file)
        .env("RDY_HA9_OUT", out);
    if let Some(r) = repo {
        cmd.env("RDY_HA9_REPO", r.to_string());
    }
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let status = cmd.status().unwrap();
    assert!(status.success());
    std::fs::read_to_string(out).unwrap()
}

const NO_STATE: &str = "ha9_check_has_no_process_state";

#[test]
fn ha9_check_has_no_process_state() {
    if child_check() {
        return;
    }
    let (_src, r, tag) = source();
    let (bytes, _) = batch_of(&r, tag, &BTreeSet::new());
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.rdysync");
    std::fs::write(&good, &bytes).unwrap();
    let mut cut = bytes.clone();
    cut.truncate(bytes.len() - 3);
    let bad = dir.path().join("bad.rdysync");
    std::fs::write(&bad, &cut).unwrap();
    let here =
        serde_json::to_string(&check_batch(bytes.as_slice(), Some(repo(&r))).unwrap()).unwrap();

    // a failed check in one process, then the good batch in each of two new processes: each
    // call stands alone, and a restart between calls loses nothing
    let failed = in_new_process(
        NO_STATE,
        &bad,
        Some(repo(&r)),
        &dir.path().join("out-1"),
        None,
    );
    assert_eq!(
        failed,
        "E_STREAM_TRUNCATED: stream trailer is missing or does not match"
    );
    let first = in_new_process(
        NO_STATE,
        &good,
        Some(repo(&r)),
        &dir.path().join("out-2"),
        None,
    );
    let second = in_new_process(
        NO_STATE,
        &good,
        Some(repo(&r)),
        &dir.path().join("out-3"),
        None,
    );
    assert_eq!(first, here);
    assert_eq!(second, here);
    // and in this process, after its own failure
    assert!(check_batch(cut.as_slice(), None).is_err());
    let again = serde_json::to_string(&check_batch(bytes.as_slice(), None).unwrap()).unwrap();
    assert_eq!(again, here);
    let report: Value = serde_json::from_str(&here).unwrap();
    assert_eq!(report["header"]["repo_id"], json!(repo(&r).to_string()));
    assert_eq!(report["header"]["purpose"], json!("tag_history"));
}

#[test]
fn ha9_repo_mismatch_uses_the_core_error() {
    let (_src, r, tag) = source();
    let (bytes, _) = batch_of(&r, tag, &BTreeSet::new());
    let source_repo = repo(&r);
    let holder = RepoId([0xe5; 16]);
    assert_ne!(source_repo, holder);
    let e = check_batch(bytes.as_slice(), Some(holder)).unwrap_err();
    let (src_hex, holder_hex) = (source_repo.to_string(), holder.to_string());
    assert_eq!((src_hex.len(), holder_hex.len()), (32, 32), "full ids");
    assert_eq!(
        refusal(&e),
        (
            E_REPO_MISMATCH,
            format!("stream repo {src_hex} does not match holder repo {holder_hex}"),
            "use the holder for the stream's repo".to_string()
        )
    );
    assert!(e.issues.is_empty());

    // checked at the header, before a damaged record or trailer is read
    let mut damaged = bytes.clone();
    *damaged.last_mut().unwrap() ^= 1;
    assert_eq!(
        check_batch(damaged.as_slice(), Some(holder))
            .unwrap_err()
            .code,
        E_REPO_MISMATCH
    );
    assert_eq!(
        check_batch(damaged.as_slice(), Some(source_repo))
            .unwrap_err()
            .code,
        E_BAD_RECORD
    );

    // the source's own repo passes, and so does a new holder's None, which reports the repo
    let same = check_batch(bytes.as_slice(), Some(source_repo)).unwrap();
    let new = check_batch(bytes.as_slice(), None).unwrap();
    assert_eq!(same, new);
    assert_eq!(new.header.repo_id, source_repo);
}

#[test]
fn ha9_huge_lengths_and_deep_values_refuse_without_panics() {
    let head = crafted_head();
    assert_eq!(
        check_batch(crafted(&[], &[]).as_slice(), None)
            .unwrap()
            .keys,
        vec![]
    );

    // a header that claims 4 GiB, and a record that does: refused by the claim alone
    let mut b = b"RDYSYNC\n".to_vec();
    b.extend_from_slice(&2u32.to_le_bytes());
    b.extend(frame_head(u32::MAX, 0xF0, &Hash([0; 32])));
    let e = refused_as_store(&b);
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_LIMIT, "stream exceeds 3365593 header bytes")
    );
    // the stream bound names no object; a record bound names the record's
    let zero = |kind: u8| format!("{kind}:{}", Hash([0; 32]));
    for (kind, len, label, object) in [
        (
            kinds::BLOCK,
            u32::MAX,
            "2147483648 total bytes",
            "none".to_string(),
        ),
        (
            kinds::BLOCK,
            16_777_217,
            "16777216 record bytes",
            zero(kinds::BLOCK),
        ),
        (
            kinds::BLOB_MANIFEST,
            16_777_217,
            "16777216 record bytes",
            zero(kinds::BLOB_MANIFEST),
        ),
        (
            kinds::BLOB_CHUNK,
            1_048_577,
            "1048576 chunk bytes",
            zero(kinds::BLOB_CHUNK),
        ),
    ] {
        let mut b = head.clone();
        b.extend(frame_head(len, kind, &Hash([0; 32])));
        let e = refused_as_store(&b);
        assert_eq!(
            (e.code, e.message.clone()),
            (E_LIMIT, format!("stream exceeds {label}"))
        );
        assert_eq!(
            position(&e),
            format!("record 1 at byte {}; object {object}", head.len())
        );
    }

    // deep and long values: the bounded decode refuses them before they are built
    let mut enc = Encoder::new();
    let mut deep = s("x");
    for _ in 0..200 {
        deep = FieldValue::List(vec![deep]);
    }
    let deep = enc.record(
        kinds::BLOCK,
        &with_field(blk("p-DEEP1", "prose", None, "a"), "rows", deep),
    );
    let e = refused_as_store(&crafted(&[deep], &[]));
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_LIMIT, "stream exceeds 64 nesting")
    );
    let long = enc.record(
        kinds::BLOCK,
        &with_body(blk("p-W1DE1", "prose", None, "a"), &"x".repeat(1_048_577)),
    );
    let e = refused_as_store(&crafted(&[long], &[]));
    assert_eq!(e.message, "stream exceeds 1048576 text bytes");

    // a blob manifest's claimed length: at most the publication blob limit, with the chunks
    // that length gives
    let max = 67_108_864u64;
    let chunks = |n: u8| -> Vec<Hash> { (0..n).map(|i| Hash([i + 1; 32])).collect() };
    let keys = |m: &BlobManifest| -> Vec<ObjectKey> {
        m.chunks
            .iter()
            .map(|h| ObjectKey {
                kind: kinds::BLOB_CHUNK,
                hash: *h,
            })
            .collect()
    };
    let ok = BlobManifest {
        len: max,
        chunks: chunks(64),
    };
    let rec = enc.record(kinds::BLOB_MANIFEST, &ok);
    check_batch(crafted(&[rec], &keys(&ok)).as_slice(), None).unwrap();
    let over = BlobManifest {
        len: max + 1,
        chunks: chunks(65),
    };
    let rec = enc.record(kinds::BLOB_MANIFEST, &over);
    let key = rec.key();
    let b = crafted(&[rec], &keys(&over));
    sync_stream::check_batch(b.as_slice()).unwrap();
    let e = check_batch(b.as_slice(), None).unwrap_err();
    assert_eq!(refusal(&e), limit_text("blob", max, "bytes"));
    let head_len =
        12 + 37 + 57 + rdy_store::format::baseline_features().to_json().len() + 33 * (65 + 1);
    assert_eq!(
        position(&e),
        format!("record 1 at byte {head_len}; object {key}")
    );
    for wrong in [
        BlobManifest {
            len: 5,
            chunks: chunks(2),
        },
        BlobManifest {
            len: (1 << 20) + 1,
            chunks: chunks(1),
        },
        BlobManifest {
            len: 0,
            chunks: chunks(1),
        },
    ] {
        let rec = enc.record(kinds::BLOB_MANIFEST, &wrong);
        let b = crafted(&[rec], &keys(&wrong));
        let e = check_batch(b.as_slice(), None).unwrap_err();
        assert_eq!(e.code, E_BAD_RECORD, "{wrong:?}");
        assert!(e.message.ends_with(": record does not decode"), "{e}");
    }
    let empty = BlobManifest {
        len: 0,
        chunks: vec![],
    };
    let rec = enc.record(kinds::BLOB_MANIFEST, &empty);
    check_batch(crafted(&[rec], &[]).as_slice(), None).unwrap();
    // the empty manifest with a length varint that is not minimal: it decodes to the same
    // value, but it is not its own canonical encoding, so it is not what `put_blob` writes and a
    // blob read refuses it. A blob object that is invalid on its own is refused at once
    let payload = vec![0x80, 0x00, 0x00];
    let key = ObjectKey {
        kind: kinds::BLOB_MANIFEST,
        hash: Hash::of(&payload),
    };
    let b = crafted(
        &[StreamRecord::Present {
            encoding: 0,
            key,
            payload,
        }],
        &[],
    );
    sync_stream::check_batch(b.as_slice()).unwrap();
    let e = check_batch(b.as_slice(), None).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            E_BAD_RECORD,
            format!(
                "stream record 1 at byte {}: record does not decode",
                head.len()
            )
            .as_str()
        )
    );
    assert_eq!(
        position(&e),
        format!("record 1 at byte {}; object {key}", head.len())
    );

    // a block's own publication limits, each at N (passes) and N - 1 (refused at the record)
    let mut table = blk("tb-TAB01", "table", None, "a");
    table.fields.insert(
        "rows".into(),
        FieldValue::List(vec![FieldValue::Map(BTreeMap::from([(
            "deep~key".to_string(),
            s("{{d-DR0P1}} {{in-snapshot-key}}"),
        )]))]),
    );
    let table = link(with_body(table, "Body {{d-KEEP1}}."), "see_also", "d-KEEP1");
    let edges = rdy_graph::edges_of(&table).len() as u64;
    let rec = enc.record(kinds::BLOCK, &table);
    let key = rec.key();
    let b = crafted(&[rec], &[]);
    type Set = fn(&mut Limits, u64);
    let cases: Vec<(&str, &str, u64, Set)> = vec![
        ("text value", "bytes", 31, |l, n| l.text_value = n as usize),
        ("text", "bytes", 31 + 17, |l, n| l.text = n as usize),
        ("references", "occurrences", 4, |l, n| {
            l.references = n as usize
        }),
        ("field depth", "levels", 3, |l, n| {
            l.field_depth = n as usize
        }),
        ("graph", "edges", edges, |l, n| l.graph = n as usize),
    ];
    // a reference row's location and token are bounded only when its target lies outside the
    // snapshot, which one record cannot tell: the take checks them with the snapshot
    let deferred = Limits {
        location: "/fields/rows/0/deep~0key".len() - 1,
        token: "in-snapshot-key".len() - 1,
        ..Limits::default()
    };
    {
        let _g = publication::override_limits_for_tests(deferred);
        check_batch(b.as_slice(), None).unwrap();
    }
    for (part, unit, n, set) in cases {
        let mut l = Limits::default();
        set(&mut l, n);
        {
            let _g = publication::override_limits_for_tests(l);
            check_batch(b.as_slice(), None).unwrap_or_else(|e| panic!("{part} at {n}: {e}"));
        }
        set(&mut l, n - 1);
        let _g = publication::override_limits_for_tests(l);
        let e = check_batch(b.as_slice(), None).unwrap_err();
        assert_eq!(refusal(&e), limit_text(part, n - 1, unit));
        assert_eq!(
            position(&e),
            format!("record 1 at byte {}; object {key}", head.len())
        );
    }
}

#[test]
fn ha9_shared_snapshot_limits_match_publications() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let small = r.store_mut().put_blob(b"four").unwrap();
    let large = r.store_mut().put_blob(b"ten bytes!").unwrap();
    let st = r.read();
    let mut table = blk("tb-TAB01", "table", Some("d-H0ST1"), "c");
    table.fields.insert(
        "rows".into(),
        FieldValue::List(vec![FieldValue::Map(BTreeMap::from([(
            "deep~key".to_string(),
            s("{{d-DR0P1}}"),
        )]))]),
    );
    let blocks = vec![
        named(blk("fd-T0P01", "folder", None, "b"), "top"),
        named(blk("fd-AREA1", "folder", Some("fd-T0P01"), "a"), "area"),
        named(
            blk("d-H0ST1", "doc", Some("fd-AREA1"), "a"),
            "a-rather-long-doc-name",
        ),
        with_body(
            blk("p-H0ST1", "prose", Some("d-H0ST1"), "a"),
            "One {{d-DR0P1}}, three {{missing-key-token}}.",
        ),
        table,
        link(
            with_field(
                blk("fg-F1G01", "figure", Some("d-H0ST1"), "d"),
                "blob",
                s(&small.to_string()),
            ),
            "see_also",
            "d-DR0P1",
        ),
        with_field(
            blk("fg-F1G02", "figure", Some("d-H0ST1"), "e"),
            "blob",
            s(&large.to_string()),
        ),
    ];
    let m = synth(&blocks);
    let reader = st.reader();
    let manifest = |h: &Hash| -> Result<Option<BlobManifest>, RdyError> {
        Ok(Some(reader.get_typed(kinds::BLOB_MANIFEST, h)?))
    };
    // Publications: the whole snapshot (the implicit profile), then its assets
    let publications = || -> Result<(), RdyError> {
        let res = publication::resolve_in_for_tests(&st, &m, IMPLICIT_ORG)?;
        publication::load_assets(&st, &res)?;
        Ok(())
    };
    let shared = || publication::check_snapshot_limits_for_tests(&blocks, manifest);
    publications().unwrap();
    shared().unwrap();

    // what the fixture holds, counted independently
    let edges: u64 = blocks
        .iter()
        .map(|b| rdy_graph::edges_of(b).len() as u64 + u64::from(b.parent.is_some()))
        .sum();
    let texts: Vec<u64> = vec![
        "One {{d-DR0P1}}, three {{missing-key-token}}.".len() as u64,
        "{{d-DR0P1}}".len() as u64,
        "top".len() as u64,
        "area".len() as u64,
        "a-rather-long-doc-name".len() as u64,
        small.to_string().len() as u64,
        large.to_string().len() as u64,
    ];
    type Set = fn(&mut Limits, u64);
    let cases: Vec<(&str, &str, u64, Set)> = vec![
        ("selection", "blocks", blocks.len() as u64, |l, n| {
            l.selection = n as usize
        }),
        ("graph", "edges", edges, |l, n| l.graph = n as usize),
        (
            "text value",
            "bytes",
            *texts.iter().max().unwrap(),
            |l, n| l.text_value = n as usize,
        ),
        ("text", "bytes", texts.iter().sum(), |l, n| {
            l.text = n as usize
        }),
        // p-H0ST1 2, the table 1, the figure's link 1
        ("references", "occurrences", 4, |l, n| {
            l.references = n as usize
        }),
        ("attachments", "rows", 2, |l, n| l.attachments = n as usize),
        ("field depth", "levels", 3, |l, n| {
            l.field_depth = n as usize
        }),
        (
            "reference location",
            "bytes",
            "/fields/rows/0/deep~0key".len() as u64,
            |l, n| l.location = n as usize,
        ),
        (
            "reference token",
            "bytes",
            "missing-key-token".len() as u64,
            |l, n| l.token = n as usize,
        ),
        (
            "selected path",
            "bytes",
            "top/area/a-rather-long-doc-name".len() as u64,
            |l, n| l.path = n as usize,
        ),
        ("blob", "bytes", 10, |l, n| l.blob = n),
        ("assets", "bytes", 14, |l, n| l.assets = n),
    ];
    assert_eq!(cases.len(), 12, "every closed row but the profile name");
    for (part, unit, n, set) in cases {
        let mut l = Limits::default();
        set(&mut l, n);
        {
            let _g = publication::override_limits_for_tests(l);
            publications().unwrap_or_else(|e| panic!("publications, {part} at {n}: {e}"));
            shared().unwrap_or_else(|e| panic!("shared, {part} at {n}: {e}"));
        }
        set(&mut l, n - 1);
        let _g = publication::override_limits_for_tests(l);
        let p = publications().unwrap_err();
        let s = shared().unwrap_err();
        assert_eq!(refusal(&p), limit_text(part, n - 1, unit), "{part}");
        assert_eq!(refusal(&s), refusal(&p), "{part}");
    }

    // the first failing phase wins, in the same order
    let l = Limits {
        graph: 1,
        text: 1,
        path: 1,
        assets: 1,
        ..Limits::default()
    };
    let _g = publication::override_limits_for_tests(l);
    assert_eq!(
        refusal(&shared().unwrap_err()),
        limit_text("text", 1, "bytes")
    );
    assert_eq!(
        refusal(&shared().unwrap_err()),
        refusal(&publications().unwrap_err())
    );
    drop(_g);

    // the same tree rule and text
    let mut loose = blocks.clone();
    loose.push(blk("p-H0ST3", "prose", None, "z"));
    let e = publication::check_snapshot_limits_for_tests(&loose, manifest).unwrap_err();
    let p = publication::resolve_in_for_tests(&st, &synth(&loose), IMPLICIT_ORG).unwrap_err();
    assert_eq!(
        refusal(&e),
        (
            E_BAD_INPUT,
            "Publication selection has an invalid document tree.".to_string(),
            "Run rdy fsck and fix the document tree.".to_string()
        )
    );
    assert_eq!(refusal(&e), refusal(&p));
    let mut looped = blocks.clone();
    looped[0].parent = Some(bid("fd-AREA1"));
    let e = publication::check_snapshot_limits_for_tests(&looped, manifest).unwrap_err();
    assert_eq!(
        e.message,
        "Publication selection has an invalid document tree."
    );
    // two records of one block are no tree
    let mut twice = blocks.clone();
    twice.push(with_body(blocks[3].clone(), "again"));
    let e = publication::check_snapshot_limits_for_tests(&twice, manifest).unwrap_err();
    assert_eq!(
        e.message,
        "Publication selection has an invalid document tree."
    );

    // a missing manifest is skipped: the caller owns completeness
    let l = Limits {
        blob: 0,
        assets: 0,
        ..Limits::default()
    };
    let _g = publication::override_limits_for_tests(l);
    publication::check_snapshot_limits_for_tests(&blocks, |_| Ok(None)).unwrap();
    assert_eq!(
        refusal(&shared().unwrap_err()),
        limit_text("blob", 0, "bytes")
    );
    drop(_g);

    // a reference row's location and token are bounded under Publications' own condition: only
    // a reference whose target lies outside the snapshot is a row. One whose target is in the
    // snapshot passes both, by id and by key; one whose target is not refuses both alike
    let inner = vec![
        with_field(
            named(blk("d-H0ST1", "doc", None, "a"), "doc"),
            "key",
            s("doc"),
        ),
        with_body(
            blk("p-H0ST4", "prose", Some("d-H0ST1"), "a"),
            "{{d-H0ST1}} {{doc}}",
        ),
    ];
    let outer = vec![
        inner[0].clone(),
        with_body(
            blk("p-H0ST4", "prose", Some("d-H0ST1"), "a"),
            "{{d-ELSE1}} {{doc}}",
        ),
    ];
    let l = Limits {
        token: "doc".len() - 1,
        ..Limits::default()
    };
    let _g = publication::override_limits_for_tests(l);
    publication::resolve_in_for_tests(&st, &synth(&inner), IMPLICIT_ORG).unwrap();
    publication::check_snapshot_limits_for_tests(&inner, manifest).unwrap();
    let p = publication::resolve_in_for_tests(&st, &synth(&outer), IMPLICIT_ORG).unwrap_err();
    let s = publication::check_snapshot_limits_for_tests(&outer, manifest).unwrap_err();
    assert_eq!(
        refusal(&p),
        limit_text("reference token", "doc".len() as u64 - 1, "bytes")
    );
    assert_eq!(refusal(&s), refusal(&p));
}

/// A table whose one row holds a map with a 1024-byte key whose value is a reference to `to`.
fn long_key_rows(to: &str) -> Value {
    let mut row = serde_json::Map::new();
    row.insert("k".repeat(1024), json!(format!("{{{{{to}}}}}")));
    json!([[Value::Object(row)]])
}

#[test]
fn ha9_reference_rows_keep_publications_conditions() {
    // a normally written, landed and tagged source: a table under a doc whose row is a map with
    // a 1024-byte key, holding a reference to that doc. Its location is over the 1024-byte
    // limit, but its target is in every selection that holds the table, so it is no row
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let res = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rows"}}
        ]),
    );
    let d = res.created["$d"].clone();
    write(
        &mut r,
        json!([{"op": "create", "type": "table", "parent": res.created["$s"].clone(),
                "fields": {"columns": ["a"], "rows": long_key_rows(&d)}}]),
    );
    land(&mut r);
    r.tag(&founder(), "v1.0.0", None, None).unwrap();
    let tag = tag_hash(&r, "v1.0.0");
    let st = r.read();
    let scope = st.resolve_scope("tag:v1.0.0").unwrap();
    let location = format!("/fields/rows/0/0/{}", "k".repeat(1024));
    assert!(location.len() > publication::limits().location);

    // the production limits: Publications publishes it, the shared snapshot check and the batch
    // check pass it
    let res = publication::resolve(&st, &scope, IMPLICIT_ORG).unwrap();
    publication::load_assets(&st, &res).unwrap();
    let m = st.materialized(&scope).unwrap();
    let reader = st.reader();
    let manifest = |h: &Hash| -> Result<Option<BlobManifest>, RdyError> {
        Ok(Some(reader.get_typed(kinds::BLOB_MANIFEST, h)?))
    };
    publication::check_snapshot_limits_for_tests(m.iter(), manifest).unwrap();
    let (bytes, c) = batch_of(&r, tag, &BTreeSet::new());
    assert_eq!(
        check_batch(bytes.as_slice(), Some(repo(&r))).unwrap().keys,
        c.records
    );

    // the same row whose target is outside the snapshot is a row, and both refuse it alike
    let mut blocks: Vec<Block> = m.iter().cloned().collect();
    let table = blocks.iter_mut().find(|b| b.block_type == "table").unwrap();
    let mut row = BTreeMap::new();
    row.insert("k".repeat(1024), s("{{d-ELSE1}}"));
    table.fields.insert(
        "rows".into(),
        FieldValue::List(vec![FieldValue::List(vec![FieldValue::Map(row)])]),
    );
    let p = publication::resolve_in_for_tests(&st, &synth(&blocks), IMPLICIT_ORG).unwrap_err();
    let e = publication::check_snapshot_limits_for_tests(&blocks, manifest).unwrap_err();
    assert_eq!(refusal(&p), limit_text("reference location", 1024, "bytes"));
    assert_eq!(refusal(&e), refusal(&p));
}
