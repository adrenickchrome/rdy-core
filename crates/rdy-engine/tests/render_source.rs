//! hpb5 (D-D2NPN, D-48NYM, D-0VCQ5, D-KDFZ3, D-F0904, D-69AY3; hub r-7GS51, r-VHP43, p-YEB6R): a
//! checked render source without a store. A tag history stream is copied to one anonymous scratch
//! file, checked by the take's own checks with nothing held, and then read as a publication
//! source: it materializes the tagged state a project reads, refuses what a take refuses, marks
//! a redacted tag as the project marks it, keeps every stream bound before allocating, opens no
//! store or config, and closes its scratch on every exit.
mod common;

use rdy_engine::publication::{self, IMPLICIT_ORG, PublicationSource, ReadPublicationSource};
use rdy_engine::redaction::RedactInput;
use rdy_engine::render_inputs::{RenderInputs, capture};
use rdy_engine::render_source::{
    ReadTagHistorySource, RenderSource, ScratchFile, TagHistorySource, tag_is_redacted,
};
use rdy_engine::stream_take::take;
use rdy_engine::*;
use rdy_model::*;
use rdy_schema::Pack;
use rdy_store::LandRequest;
use rdy_store::closure::write_batch;
use rdy_store::kinds::{self, BlobManifest};
use rdy_store::object_refs::{ObjectKey, ObjectRef};
use rdy_store::sync_stream::{SYNC_KINDS, StreamHeader, StreamReader, StreamRecord};
use rdy_store::take_batch::UploadTarget;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const CANARY: &str = "ZEBRA-7731";

// ---------------------------------------------------------------------------------------------
// source projects

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
            message: "hpb5".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy) -> Hash {
    let p = r
        .propose(&writer(), "work", "batch", Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap().parse().unwrap()
}

fn tag(r: &mut Rdy, name: &str) -> Hash {
    r.tag(&founder(), name, None, None).unwrap();
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap()
        .1
}

fn landing_of(r: &Rdy, tag: Hash) -> Hash {
    r.store().read_tag(&tag).unwrap().landing
}

fn repo(r: &Rdy) -> RepoId {
    r.read().repo_id().unwrap()
}

/// Removes `targets` (block ids or `blob:<hash>`) with one request, landed by a person.
fn remove(r: &mut Rdy, targets: &[String]) {
    let s = writer();
    let req = r
        .redact(
            &s,
            RedactInput {
                targets: targets.to_vec(),
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
    assert!(req.redaction.is_some());
    let p = r.propose(&s, "work", "Remove the code", None).unwrap();
    let version: Hash = p.id.parse().unwrap();
    r.seal_open().unwrap();
    r.store_mut().set_held(false).unwrap();
    r.store_mut()
        .land_atomic(LandRequest {
            proposal: version,
            accept: None,
            approver: Provenance {
                actor: "human:t".into(),
                run: None,
                role: Role::Approver,
                at_ms: 1_000_000,
            },
            alt: None,
            relay: None,
            evidence: None,
        })
        .unwrap();
    r.store_mut().set_held(true).unwrap();
    r.invalidate();
}

/// The seeded project: landing 1 makes folder `top`, doc `plan` and an admin doc with a public
/// and an org profile selecting `plan` (tag `v0.1.0`); landing 2 adds a prose block holding the
/// canary, a sibling, figures on a kept blob and a removed blob, and a figure-like prose block
/// whose `blob` field names the removed blob (tag `v1.0.0`). Then one request removes the canary
/// block and the removed blob, and canon's head is tagged `v2.0.0`.
struct Seeded {
    _dir: tempfile::TempDir,
    r: Rdy,
    ids: BTreeMap<String, String>,
    kept: Hash,
    removed: Hash,
    v0: Hash,
    v1: Hash,
    v2: Hash,
    /// The canary block's version before its removal.
    canary_version: Hash,
    /// The tag v1.0.0 stream before the removal.
    v1_before: Vec<u8>,
}

fn seeded() -> Seeded {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let kept = r.store_mut().put_blob(b"kept figure bytes").unwrap();
    let mut removed_bytes = vec![7u8; 1 << 20];
    removed_bytes.extend_from_slice(b"removed tail");
    let removed = r.store_mut().put_blob(&removed_bytes).unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "folder", "id": "$top", "fields": {"name": "top"}},
            {"op": "create", "type": "doc", "id": "$d", "parent": "$top", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let mut ids: BTreeMap<String, String> = w.created.clone().into_iter().collect();
    let p = write(
        &mut r,
        json!([
            {"op": "create", "type": "publication_profile", "id": "$pub", "parent": ids["$admin"],
             "fields": {"key": "pubsite", "title": "Public site", "audience": "public", "select": [ids["$d"]]}},
            {"op": "create", "type": "publication_profile", "id": "$org", "parent": ids["$admin"],
             "fields": {"key": "orgsite", "title": "Org site", "audience": "org", "select": [ids["$d"]]}}
        ]),
    );
    ids.extend(p.created.clone());
    land(&mut r);
    let v0 = tag(&mut r, "v0.1.0");
    let fig = |ph: &str, blob: &Hash| {
        json!({"op": "create", "type": "figure", "id": ph, "parent": ids["$d"],
            "fields": {"blob": blob.to_string(), "media_type": "application/octet-stream", "alt": ph}})
    };
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "prose", "id": "$p1", "parent": ids["$d"], "body": format!("The launch code is {CANARY}.")},
            {"op": "create", "type": "prose", "id": "$p2", "parent": ids["$d"], "body": "Readable sibling."},
            fig("$fk", &kept),
            fig("$fr", &removed)
        ]),
    );
    ids.extend(w.created.clone());
    land(&mut r);
    let v1 = tag(&mut r, "v1.0.0");
    let canary_version = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .version_of(&BlockId::parse(&ids["$p1"]).unwrap())
        .unwrap();
    let v1_before = export(&r, v1);
    remove(&mut r, &[ids["$p1"].clone(), format!("blob:{removed}")]);
    let v2 = tag(&mut r, "v2.0.0");
    Seeded {
        _dir: dir,
        r,
        ids,
        kept,
        removed,
        v0,
        v1,
        v2,
        canary_version,
        v1_before,
    }
}

// ---------------------------------------------------------------------------------------------
// streams

/// The whole history of `tag` as one batch, as the export writes it.
fn export(r: &Rdy, tag: Hash) -> Vec<u8> {
    let mut out = Vec::new();
    rdy_engine::stream_export::write(&r.read(), tag, &BTreeSet::new(), &mut out).unwrap();
    out
}

/// The records of a valid batch, in wire order, and its header.
fn records_of(bytes: &[u8]) -> (StreamHeader, Vec<StreamRecord>) {
    let (mut r, header) = StreamReader::open(Cursor::new(bytes.to_vec())).unwrap();
    let mut out = vec![];
    while let Some(rec) = r.next().unwrap() {
        out.push(rec);
    }
    (header, out)
}

/// `records` (in that order) as one batch rooted at `root` of `repo`, with exactly the
/// prerequisites they need: every link and proof none of them holds, and the root when none is
/// it.
fn rebatch(repo: RepoId, root: Hash, records: &[StreamRecord]) -> Vec<u8> {
    let held: BTreeSet<ObjectKey> = records.iter().map(StreamRecord::key).collect();
    let resolve = |l: ObjectRef| match l.kind {
        Some(kind) => ObjectKey { kind, hash: l.hash },
        None => SYNC_KINDS
            .iter()
            .map(|&kind| ObjectKey { kind, hash: l.hash })
            .find(|k| held.contains(k))
            .unwrap_or(ObjectKey {
                kind: kinds::BLOCK,
                hash: l.hash,
            }),
    };
    let mut needed = BTreeSet::new();
    for r in records {
        // a record over its decode bounds names nothing here: the reader refuses it
        for l in r.refs().unwrap_or_default() {
            needed.insert(resolve(l));
        }
        needed.extend(r.proof());
    }
    needed.insert(ObjectKey {
        kind: kinds::TAG,
        hash: root,
    });
    needed.retain(|k| !held.contains(k));
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        repo,
        root,
        needed.into_iter().collect(),
    );
    let mut source: BTreeMap<ObjectKey, StreamRecord> =
        records.iter().map(|r| (r.key(), r.clone())).collect();
    let order: Vec<ObjectKey> = records.iter().map(StreamRecord::key).collect();
    write_batch(&mut source, &header, &order, Vec::new())
        .unwrap()
        .0
}

fn files(dir: &Path, name: &str, bytes: &[u8]) -> Vec<File> {
    let p = dir.join(format!("{name}.rdysync"));
    std::fs::write(&p, bytes).unwrap();
    vec![File::open(&p).unwrap()]
}

/// The take of `bytes` (for the tag named `name` at `landing`) into a new holder of `repo`.
fn take_into_holder(
    dir: &Path,
    name: &str,
    repo: RepoId,
    bytes: &[u8],
    target: &UploadTarget,
) -> Result<Rdy, RdyError> {
    let root = dir.join(format!("holder-{name}"));
    let mut h = Rdy::create_holder(&root, repo).unwrap();
    take(&mut h, &files(dir, name, bytes), target.clone())?;
    Ok(h)
}

/// The record bytes of `value`, as the store encodes it.
fn enc<T: serde::Serialize>(value: &T) -> Vec<u8> {
    let mut buf = rdy_model::redaction::record_buffer().unwrap();
    rdy_model::redaction::encode_into(value, &mut buf)
        .unwrap()
        .to_vec()
}

/// The error of a refused call.
fn refused<T>(r: Result<T, RdyError>) -> RdyError {
    match r {
        Ok(_) => panic!("accepted"),
        Err(e) => e,
    }
}

fn refusal(e: &RdyError) -> (&str, &str, &str, Vec<String>) {
    (
        e.code,
        e.message.as_str(),
        e.fix.as_str(),
        e.issues.iter().map(|i| i.message.clone()).collect(),
    )
}

fn assert_every_field(a: &publication::Resolved, b: &publication::Resolved, what: &str) {
    assert_eq!(a.profile, b.profile, "{what}: profile");
    assert_eq!(a.m.snapshot(), b.m.snapshot(), "{what}: snapshot");
    assert_eq!(
        a.m.iter().collect::<Vec<_>>(),
        b.m.iter().collect::<Vec<_>>(),
        "{what}: selected records"
    );
    assert_eq!(a.graph, b.graph, "{what}: graph");
    assert_eq!(a.entries, b.entries, "{what}: entries");
    assert_eq!(a.docs, b.docs, "{what}: docs");
    assert_eq!(a.attachments, b.attachments, "{what}: attachments");
    assert_eq!(a.references, b.references, "{what}: references");
    assert_eq!(a.selection_hash, b.selection_hash, "{what}: selection hash");
}

fn inputs(r: &Rdy) -> RenderInputs {
    capture(&r.read()).unwrap()
}

fn render(bytes: &[u8], inputs: RenderInputs) -> Result<RenderSource, RdyError> {
    RenderSource::read(Cursor::new(bytes.to_vec()), inputs)
}

// ---------------------------------------------------------------------------------------------
// scratch files the tests own

/// What the scratch files a test made did: how many were made and closed, the bytes read back
/// from them and their seeks. A test fails every later read with `fail_reads`, or from the
/// `fail_at_seek`th seek on (0: never).
#[derive(Default)]
struct Seen {
    opened: AtomicU64,
    closed: AtomicU64,
    read: AtomicU64,
    written: AtomicU64,
    seeks: AtomicU64,
    fail_reads: AtomicBool,
    fail_at_seek: AtomicU64,
}

impl Seen {
    fn get(c: &AtomicU64) -> u64 {
        c.load(Ordering::SeqCst)
    }

    fn add(c: &AtomicU64, n: u64) {
        c.fetch_add(n, Ordering::SeqCst);
    }
}

/// A scratch file in memory that reports to `seen`; `fail_write` fails every write, and `sink`
/// keeps no byte (every read then sees the end of the file).
struct TestScratch {
    data: Cursor<Vec<u8>>,
    seen: Arc<Seen>,
    fail_write: bool,
    sink: bool,
}

impl Drop for TestScratch {
    fn drop(&mut self) {
        Seen::add(&self.seen.closed, 1);
    }
}

impl Read for TestScratch {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.seen.fail_reads.load(Ordering::SeqCst) {
            return Err(io::Error::other("bad sector"));
        }
        let n = self.data.read(buf)?;
        Seen::add(&self.seen.read, n as u64);
        Ok(n)
    }
}

impl Write for TestScratch {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.fail_write {
            return Err(io::Error::other("disk full"));
        }
        Seen::add(&self.seen.written, buf.len() as u64);
        if self.sink {
            return Ok(buf.len());
        }
        self.data.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for TestScratch {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let n = self.seen.seeks.fetch_add(1, Ordering::SeqCst) + 1;
        let at = Seen::get(&self.seen.fail_at_seek);
        if at != 0 && n >= at {
            self.seen.fail_reads.store(true, Ordering::SeqCst);
        }
        self.data.seek(pos)
    }
}

fn scratch(
    seen: &Arc<Seen>,
    fail_write: bool,
    sink: bool,
) -> impl FnMut() -> io::Result<Box<dyn ScratchFile>> + use<> {
    let seen = Arc::clone(seen);
    move || {
        Seen::add(&seen.opened, 1);
        Ok(Box::new(TestScratch {
            data: Cursor::new(Vec::new()),
            seen: Arc::clone(&seen),
            fail_write,
            sink,
        }) as Box<dyn ScratchFile>)
    }
}

/// A reader of `prefix`, then filler bytes, `len` bytes in all, that counts what it gave.
struct Counting {
    prefix: Vec<u8>,
    len: u64,
    given: Arc<AtomicU64>,
}

impl Read for Counting {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let at = self.given.load(Ordering::SeqCst);
        if at >= self.len {
            return Ok(0);
        }
        let n = (buf.len() as u64).min(self.len - at) as usize;
        for (i, b) in buf[..n].iter_mut().enumerate() {
            let pos = at + i as u64;
            *b = if (pos as usize) < self.prefix.len() {
                self.prefix[pos as usize]
            } else {
                0
            };
        }
        self.given.store(at + n as u64, Ordering::SeqCst);
        Ok(n)
    }
}

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn hpb5_stream_materializes_like_a_project() {
    let s = seeded();
    let read = s.r.read();
    let profiles = [
        "pubsite".to_string(),
        s.ids["$pub"].clone(),
        "orgsite".to_string(),
        IMPLICIT_ORG.to_string(),
    ];
    for (name, tag) in [("v0.1.0", s.v0), ("v1.0.0", s.v1), ("v2.0.0", s.v2)] {
        let rs = render(&export(&s.r, tag), inputs(&s.r)).unwrap();
        assert_eq!(rs.facts().root_hash, tag);
        assert_eq!(rs.facts().root.name, name);
        assert_eq!(rs.inputs(), &inputs(&s.r));
        // the tagged blocks, by id, with their original version hashes
        let scope = read.resolve_scope(&format!("tag:{name}")).unwrap();
        let want = read.materialized(&scope).unwrap();
        let got = rs.materialized();
        assert_eq!(got.snapshot(), want.snapshot(), "{name}: snapshot");
        assert_eq!(
            got.iter().collect::<Vec<_>>(),
            want.iter().collect::<Vec<_>>(),
            "{name}: blocks"
        );
        for (id, v) in want.versions() {
            assert_eq!(got.version_of(id), Some(*v), "{name}: {id}");
        }
        // the same profiles, selections and assets
        let title = read.project_name();
        let description = read.project_description();
        let store = ReadPublicationSource::with_settings(
            &read,
            read.pack(),
            &title,
            description.as_deref(),
        );
        for p in &profiles {
            let want = publication::resolve(&read, &scope, p).unwrap();
            let got = publication::resolve_source(&rs, rs.materialized(), p).unwrap();
            assert_every_field(&want, &got, &format!("{name} {p}"));
            if want.require_output().is_ok() {
                assert_eq!(
                    publication::load_source_assets(&rs, &got).unwrap(),
                    publication::load_assets(&read, &want).unwrap(),
                    "{name} {p}: assets"
                );
            }
        }
        for h in [s.kept, s.removed, Hash::of(b"absent")] {
            let held = rs.facts().objects.contains(&ObjectKey {
                kind: kinds::BLOB_MANIFEST,
                hash: h,
            });
            if !held {
                // a blob the tag's history does not reach is no blob of the stream
                assert_eq!(rs.manifest(&h).unwrap(), None, "{name} {h}");
                assert!(!rs.blob_available(&h).unwrap());
                assert_eq!(rs.removed_blob(&h).unwrap(), None);
                continue;
            }
            assert_eq!(rs.manifest(&h).unwrap(), store.manifest(&h).unwrap(), "{h}");
            assert_eq!(
                rs.blob_available(&h).unwrap(),
                store.blob_available(&h).unwrap(),
                "{name} {h}"
            );
            assert_eq!(
                rs.removed_blob(&h).unwrap(),
                store.removed_blob(&h).unwrap(),
                "{name} {h}"
            );
            let (mut a, mut b) = (vec![], vec![]);
            rs.visit_blob_redactions(&h, &mut |r| {
                a.push(r);
                Ok(())
            })
            .unwrap();
            store
                .visit_blob_redactions(&h, &mut |r| {
                    b.push(r);
                    Ok(())
                })
                .unwrap();
            assert_eq!(a, b, "{name} {h}");
        }
        // the marker is the project's
        let info = read
            .versions()
            .unwrap()
            .into_iter()
            .find(|v| v.name == name)
            .unwrap();
        assert_eq!(rs.tag_redacted(), info.redacted, "{name}");
    }
    // the removed block reads as its stub, by its original version, and the removed blob yields
    // its removal request, never bytes
    let rs = render(&export(&s.r, s.v1), inputs(&s.r)).unwrap();
    let p1 = BlockId::parse(&s.ids["$p1"]).unwrap();
    let stub = rs.materialized().get(&p1).unwrap();
    assert!(stub.is_redacted());
    assert_eq!(stub.version_hash(), s.canary_version);
    assert!(!stub.body.contains(CANARY));
    let request = rs.removed_blob(&s.removed).unwrap().unwrap();
    assert!(!rs.blob_available(&s.removed).unwrap());
    let e = rs.blob(&s.removed).unwrap_err();
    assert!(e.message.contains(&request.to_string()), "{e}");
    assert_eq!(rs.blob(&s.kept).unwrap(), b"kept figure bytes");
    assert!(rs.tag_redacted());
    // before any removal, v0.1.0 holds nothing removed
    assert!(
        !render(&export(&s.r, s.v0), inputs(&s.r))
            .unwrap()
            .tag_redacted()
    );
}

#[test]
fn hpb5_full_validation_reuses_take_checks() {
    let s = seeded();
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(&s.r);
    let good = export(&s.r, s.v1);
    let target = UploadTarget {
        tag: "v1.0.0".into(),
        landing: landing_of(&s.r, s.v1),
    };
    // the valid stream passes both
    take_into_holder(dir.path(), "good", repo, &good, &target).unwrap();
    render(&good, inputs(&s.r)).unwrap();
    let (_, records) = records_of(&good);
    let mut corpus: Vec<(String, Vec<u8>, UploadTarget)> = vec![];
    let mut add = |name: &str, bytes: Vec<u8>| corpus.push((name.into(), bytes, target.clone()));
    // framing: a flipped payload byte, a cut file, trailing bytes, a damaged trailer
    let mut flipped = good.clone();
    let at = flipped.len() - 86;
    flipped[at] ^= 1;
    add("hash", flipped);
    let mut cut = good.clone();
    cut.pop();
    add("cut", cut);
    let mut trailing = good.clone();
    trailing.push(0);
    add("trailing", trailing);
    let mut trailer = good.clone();
    let n = trailer.len();
    trailer[n - 30] ^= 1;
    add("trailer", trailer);
    // an unused record
    let mut orphan = records.clone();
    orphan.push(StreamRecord::Present {
        encoding: 0,
        key: ObjectKey {
            kind: kinds::BLOB_CHUNK,
            hash: Hash::of(b"stray"),
        },
        payload: b"stray".to_vec(),
    });
    add("orphan", rebatch(repo, s.v1, &orphan));
    // a missing dependency: the first block is only a prerequisite
    let first_block = records
        .iter()
        .position(|r| r.key().kind == kinds::BLOCK)
        .unwrap();
    let mut without = records.clone();
    without.remove(first_block);
    add("missing", rebatch(repo, s.v1, &without));
    // an unproved drop: a dropped record whose tuple its apply does not approve
    let mut unproved = records.clone();
    let d = unproved
        .iter()
        .position(|r| matches!(r, StreamRecord::Dropped { .. }))
        .expect("a dropped record");
    if let StreamRecord::Dropped { original_len, .. } = &mut unproved[d] {
        *original_len += 1;
    }
    add("unproved", rebatch(repo, s.v1, &unproved));
    // a root that names no version
    let root = records
        .iter()
        .find_map(|r| match r {
            StreamRecord::Present { key, payload, .. }
                if key.kind == kinds::TAG && key.hash == s.v1 =>
            {
                Some(rdy_store::bounded::decode::<Tag>(payload).unwrap())
            }
            _ => None,
        })
        .unwrap();
    let renamed = Tag {
        name: "release".into(),
        ..root
    };
    let renamed_bytes = enc(&renamed);
    let renamed_key = ObjectKey {
        kind: kinds::TAG,
        hash: Hash::of(&renamed_bytes),
    };
    let mut retagged: Vec<StreamRecord> = records
        .iter()
        .filter(|r| r.key().hash != s.v1)
        .filter(|r| match r {
            // the old tag's relay answer and signatures name it
            StreamRecord::Present { key, payload, .. } if key.kind == kinds::ANSWER => {
                !rdy_store::bounded::decode::<Answer>(payload)
                    .unwrap()
                    .targets
                    .contains(&s.v1)
            }
            _ => true,
        })
        .cloned()
        .collect();
    retagged.push(StreamRecord::Present {
        encoding: 0,
        key: renamed_key,
        payload: renamed_bytes,
    });
    let retarget = UploadTarget {
        tag: "release".into(),
        landing: target.landing,
    };
    corpus.push((
        "release".into(),
        rebatch(repo, renamed_key.hash, &retagged),
        retarget,
    ));
    // take step 7: a present change that embeds the removed canary version, which no approval
    // covers, attached to the closure by an ordinary answer to the tag's landing
    let (_, before) = records_of(&s.v1_before);
    let canary = before
        .iter()
        .find_map(|r| match r {
            StreamRecord::Present { key, payload, .. }
                if key.kind == kinds::BLOCK && key.hash == s.canary_version =>
            {
                Some(rdy_store::bounded::decode::<Block>(payload).unwrap())
            }
            _ => None,
        })
        .expect("the canary block before its removal");
    assert!(canary.body.contains(CANARY));
    let embedding = Change {
        view: "work".into(),
        ops: vec![Op::Create { block: canary }],
        deps: vec![],
        provenance: prov(),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    };
    let embedding_bytes = enc(&embedding);
    let embedding_key = ObjectKey {
        kind: kinds::CHANGE,
        hash: Hash::of(&embedding_bytes),
    };
    let attach = Answer {
        item: "D-x@1".into(),
        act: AnswerAct::Confirm,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets: vec![target.landing, embedding_key.hash],
        person: prov(),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: None,
    };
    let attach_bytes = enc(&attach);
    let mut embedded = records.clone();
    embedded.push(StreamRecord::Present {
        encoding: 0,
        key: embedding_key,
        payload: embedding_bytes,
    });
    embedded.push(StreamRecord::Present {
        encoding: 0,
        key: ObjectKey {
            kind: kinds::ANSWER,
            hash: Hash::of(&attach_bytes),
        },
        payload: attach_bytes,
    });
    let uncovered = rebatch(repo, s.v1, &embedded);
    let e = refused(take_into_holder(
        dir.path(),
        "uncovered-take",
        repo,
        &uncovered,
        &target,
    ));
    assert_eq!(
        (e.code, e.message.clone()),
        (
            E_REDACTION_STALE,
            format!(
                "the imported removal does not cover receiver change {}",
                embedding_key.hash
            )
        ),
        "{e}"
    );
    corpus.push(("uncovered".into(), uncovered, target.clone()));
    for (name, bytes, t) in &corpus {
        let by_take = refused(take_into_holder(dir.path(), name, repo, bytes, t));
        let by_render = render(bytes, inputs(&s.r)).unwrap_err();
        assert_eq!(refusal(&by_render), refusal(&by_take), "{name}");
    }
    // a snapshot over the Publications limits: both refuse with the same limit
    let base = publication::limits();
    let _guard = publication::override_limits_for_tests(publication::Limits {
        selection: 3,
        ..base
    });
    let by_take = refused(take_into_holder(dir.path(), "limits", repo, &good, &target));
    let by_render = render(&good, inputs(&s.r)).unwrap_err();
    assert_eq!(refusal(&by_render), refusal(&by_take));
    assert_eq!(by_render.code, E_LIMIT);
    assert!(
        by_render
            .message
            .starts_with("Publication selection exceeds the limit of 3"),
        "{}",
        by_render.message
    );
}

#[test]
fn hpb5_render_source_refuses_missing_proofs_and_prerequisites() {
    let s = seeded();
    let repo = repo(&s.r);
    let good = export(&s.r, s.v1);
    let (_, records) = records_of(&good);
    // the removal landed after the tag; its proof travels with the tag's history, and the
    // removed block materializes through its approved residue
    let rs = render(&good, inputs(&s.r)).unwrap();
    let p1 = BlockId::parse(&s.ids["$p1"]).unwrap();
    assert_eq!(rs.materialized().version_of(&p1), Some(s.canary_version));
    assert!(rs.materialized().get(&p1).unwrap().is_redacted());
    // a later removal proof whose apply is missing: the dropped records need it
    let apply = records
        .iter()
        .position(|r| r.key().kind == kinds::REDACTION_APPLY)
        .unwrap();
    let apply_key = records[apply].key();
    let mut no_apply = records.clone();
    no_apply.remove(apply);
    let e = render(&rebatch(repo, s.v1, &no_apply), inputs(&s.r)).unwrap_err();
    assert_eq!(e.code, E_UNRESOLVED_REF, "{e}");
    let (_, stripped) = records_of(&rebatch(repo, s.v1, &no_apply));
    let header = StreamReader::open(Cursor::new(rebatch(repo, s.v1, &no_apply)))
        .unwrap()
        .1;
    assert_eq!(stripped.len(), no_apply.len());
    assert_eq!(
        e.message,
        format!("stream prerequisite {} is missing", header.prerequisites[0])
    );
    assert!(header.prerequisites.contains(&apply_key));
    // the removal's landing alone is missing
    let removal_landing = records
        .iter()
        .find_map(|r| match r {
            StreamRecord::Present { key, payload, .. } if key.kind == kinds::REDACTION_APPLY => {
                Some(
                    rdy_store::bounded::decode::<rdy_model::redaction::RedactionApply>(payload)
                        .unwrap()
                        .landing,
                )
            }
            _ => None,
        })
        .unwrap();
    let mut no_landing = records.clone();
    no_landing.retain(|r| r.key().hash != removal_landing);
    let e = render(&rebatch(repo, s.v1, &no_landing), inputs(&s.r)).unwrap_err();
    assert_eq!(e.code, E_UNRESOLVED_REF, "{e}");
    // a valid nonempty prerequisite list: the first sorted missing key
    let blocks: Vec<usize> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| matches!(r, StreamRecord::Present { key, .. } if key.kind == kinds::BLOCK))
        .map(|(i, _)| i)
        .take(2)
        .collect();
    let mut two = records.clone();
    let mut gone: Vec<ObjectKey> = blocks.iter().map(|i| records[*i].key()).collect();
    for i in blocks.iter().rev() {
        two.remove(*i);
    }
    gone.sort();
    let bytes = rebatch(repo, s.v1, &two);
    let header = StreamReader::open(Cursor::new(bytes.clone())).unwrap().1;
    assert!(gone.iter().all(|k| header.prerequisites.contains(k)));
    let e = render(&bytes, inputs(&s.r)).unwrap_err();
    assert_eq!(
        (e.code, e.message.clone(), e.fix.clone()),
        (
            E_UNRESOLVED_REF,
            format!("stream prerequisite {} is missing", header.prerequisites[0]),
            "send the missing object with its dependencies".to_string()
        )
    );
    assert_eq!(
        e.issues[0].message,
        format!("record 0 at byte 0; object {}", header.prerequisites[0])
    );
    // an unproved drop: its apply approves another tuple
    let mut unproved = records.clone();
    let d = unproved
        .iter()
        .position(|r| matches!(r, StreamRecord::Dropped { key, .. } if key.kind == kinds::BLOCK))
        .unwrap();
    let dropped_key = unproved[d].key();
    if let StreamRecord::Dropped { original_len, .. } = &mut unproved[d] {
        *original_len -= 1;
    }
    let e = render(&rebatch(repo, s.v1, &unproved), inputs(&s.r)).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            E_BAD_RECORD,
            "stream record 0 at byte 0: invalid removal proof"
        ),
        "{e}"
    );
    assert_eq!(
        e.issues[0].message,
        format!("record 0 at byte 0; object {dropped_key}")
    );
    // a removed block's stale bytes beside the approval that removes them: never read
    let (_, before) = records_of(&s.v1_before);
    let originals: BTreeMap<ObjectKey, StreamRecord> =
        before.into_iter().map(|r| (r.key(), r)).collect();
    let stale: Vec<StreamRecord> = records
        .iter()
        .map(|r| match r {
            StreamRecord::Dropped { key, .. } if originals.contains_key(key) => {
                originals[key].clone()
            }
            other => other.clone(),
        })
        .collect();
    assert!(
        stale
            .iter()
            .all(|r| !matches!(r, StreamRecord::Dropped { key, .. } if key.kind == kinds::BLOCK))
    );
    let rs = render(&rebatch(repo, s.v1, &stale), inputs(&s.r)).unwrap();
    let stub = rs.materialized().get(&p1).unwrap();
    assert!(stub.is_redacted());
    assert_eq!(stub.version_hash(), s.canary_version);
    for b in rs.materialized().iter() {
        assert!(!b.body.contains(CANARY), "{}", b.id);
    }
    assert!(rs.tag_redacted());
    // an orphan object
    let mut orphan = records.clone();
    let stray = StreamRecord::Present {
        encoding: 0,
        key: ObjectKey {
            kind: kinds::BLOB_CHUNK,
            hash: Hash::of(b"stray"),
        },
        payload: b"stray".to_vec(),
    };
    orphan.push(stray.clone());
    let e = render(&rebatch(repo, s.v1, &orphan), inputs(&s.r)).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            E_BAD_RECORD,
            "stream record 0 at byte 0: object is outside the tag closure"
        )
    );
    assert_eq!(
        e.issues[0].message,
        format!("record 0 at byte 0; object {}", stray.key())
    );
}

// the marker rule over a source the test holds in memory

/// A tag history in memory: landings, changes, removed objects and manifests.
#[derive(Default)]
struct History {
    landings: BTreeMap<Hash, LandingRecord>,
    changes: BTreeMap<Hash, Change>,
    removed: BTreeSet<(u8, Hash)>,
    manifests: BTreeMap<Hash, BlobManifest>,
}

impl TagHistorySource for History {
    fn landing(&mut self, h: Hash) -> Result<LandingRecord, RdyError> {
        self.landings
            .get(&h)
            .cloned()
            .ok_or_else(|| RdyError::new(E_NOT_FOUND, format!("no landing {h}"), "none"))
    }

    fn change(&mut self, h: Hash) -> Result<Change, RdyError> {
        assert!(!self.removed.contains(&(kinds::CHANGE, h)), "removed first");
        self.changes
            .get(&h)
            .cloned()
            .ok_or_else(|| RdyError::new(E_NOT_FOUND, format!("no change {h}"), "none"))
    }

    fn removed(&mut self, kind: u8, h: Hash) -> Result<bool, RdyError> {
        Ok(self.removed.contains(&(kind, h)))
    }

    fn manifest(&mut self, h: Hash) -> Result<Option<BlobManifest>, RdyError> {
        Ok(self.manifests.get(&h).cloned())
    }
}

fn prov() -> Provenance {
    Provenance {
        actor: "human:t".into(),
        run: None,
        role: Role::Approver,
        at_ms: 1,
    }
}

fn landing_rec(prev: Option<Hash>, changes: Vec<Hash>, n: u8) -> LandingRecord {
    LandingRecord {
        proposal: Hash([n; 32]),
        prev,
        state: Hash([n.wrapping_add(1); 32]),
        approved_by: prov(),
        changes,
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

fn blk(id: &str, ty: &str, blob: Option<&Hash>) -> Block {
    let mut fields = BTreeMap::new();
    if let Some(b) = blob {
        fields.insert("blob".into(), FieldValue::Str(b.to_string()));
    }
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: ty.into(),
        parent: None,
        order: "V".into(),
        fields,
        body: "text".into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn chg(ops: Vec<Op>) -> Change {
    Change {
        view: "work".into(),
        ops,
        deps: vec![],
        provenance: prov(),
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// A chain of two landings: the head carries `head_ops` in one change, the first one change
/// with an ordinary block. Returns the history and the head.
fn chain(head_ops: Vec<Op>) -> (History, Hash, Hash, Hash) {
    let mut h = History::default();
    let first_change = chg(vec![Op::Create {
        block: blk("b-00001", "prose", None),
    }]);
    let fc = first_change.id();
    h.changes.insert(fc, first_change);
    let first = landing_rec(None, vec![fc], 1);
    let fl = first.id();
    h.landings.insert(fl, first);
    let head_change = chg(head_ops);
    let hc = head_change.id();
    h.changes.insert(hc, head_change);
    let head = landing_rec(Some(fl), vec![hc], 3);
    let hl = head.id();
    h.landings.insert(hl, head);
    (h, hl, fc, hc)
}

#[test]
fn hpb5_tag_marker_matches_source_without_holder_canon() {
    let pack: Pack = Rdy::init(tempfile::tempdir().unwrap().path(), "rdy-standard")
        .unwrap()
        .read()
        .pack()
        .clone();
    assert!(pack.is_asset("figure"));
    assert!(!pack.is_asset("prose"));
    let blob = Hash::of(b"a blob");
    let chunk = Hash::of(b"a chunk");
    let manifest = BlobManifest {
        len: 7,
        chunks: vec![chunk],
    };
    let figure = blk("fg-00001", "figure", Some(&blob));
    let prose_with_blob = blk("b-00002", "prose", Some(&blob));
    let ops = |b: &Block| vec![Op::Create { block: b.clone() }];

    // nothing removed: false only after the whole chain is read
    let (mut h, head, _, _) = chain(ops(&figure));
    h.manifests.insert(blob, manifest.clone());
    assert!(!tag_is_redacted(&mut h, head, &pack).unwrap());
    // a removed change, carried by the head or by an earlier landing
    for which in [0, 1] {
        let (mut h, head, first, carried) = chain(ops(&figure));
        h.removed
            .insert((kinds::CHANGE, if which == 0 { carried } else { first }));
        assert!(tag_is_redacted(&mut h, head, &pack).unwrap());
    }
    // an old block version a change wrote, removed while the change is intact
    let (mut h, head, _, _) = chain(ops(&figure));
    h.removed.insert((kinds::BLOCK, figure.version_hash()));
    assert!(tag_is_redacted(&mut h, head, &pack).unwrap());
    let updated = blk("fg-00001", "figure", None);
    let (mut h, head, _, _) = chain(vec![Op::Update {
        block: updated.clone(),
        base: figure.version_hash(),
    }]);
    h.removed.insert((kinds::BLOCK, updated.version_hash()));
    assert!(tag_is_redacted(&mut h, head, &pack).unwrap());
    // an asset's removed manifest, or a removed chunk of its manifest
    let (mut h, head, _, _) = chain(ops(&figure));
    h.manifests.insert(blob, manifest.clone());
    h.removed.insert((kinds::BLOB_MANIFEST, blob));
    assert!(tag_is_redacted(&mut h, head, &pack).unwrap());
    let (mut h, head, _, _) = chain(ops(&figure));
    h.manifests.insert(blob, manifest.clone());
    h.removed.insert((kinds::BLOB_CHUNK, chunk));
    assert!(tag_is_redacted(&mut h, head, &pack).unwrap());
    // a missing manifest contributes no mark
    let (mut h, head, _, _) = chain(ops(&figure));
    h.removed.insert((kinds::BLOB_CHUNK, chunk));
    assert!(!tag_is_redacted(&mut h, head, &pack).unwrap());
    // a `blob` field of a type that is no asset is plain text
    let (mut h, head, _, _) = chain(ops(&prose_with_blob));
    h.manifests.insert(blob, manifest.clone());
    h.removed.insert((kinds::BLOB_MANIFEST, blob));
    h.removed.insert((kinds::BLOB_CHUNK, chunk));
    assert!(!tag_is_redacted(&mut h, head, &pack).unwrap());
    // a delete writes no version
    let (mut h, head, _, _) = chain(vec![Op::Delete {
        base: figure.version_hash(),
        id: figure.id.clone(),
    }]);
    h.removed.insert((kinds::BLOCK, figure.version_hash()));
    assert!(!tag_is_redacted(&mut h, head, &pack).unwrap());
    // a removal proof's landing outside the chain marks nothing
    let (mut h, head, _, _) = chain(ops(&figure));
    let proof_change = chg(vec![]);
    let pc = proof_change.id();
    h.changes.insert(pc, proof_change);
    h.removed.insert((kinds::CHANGE, pc));
    let outside = landing_rec(Some(head), vec![pc], 9);
    h.landings.insert(outside.id(), outside);
    assert!(!tag_is_redacted(&mut h, head, &pack).unwrap());
    // a cycle and a chain past 100000 landings refuse, as the holder check does
    let mut h = History::default();
    let a = landing_rec(Some(Hash([0xAB; 32])), vec![], 1);
    let ah = a.id();
    let b = landing_rec(Some(ah), vec![], 2);
    let bh = b.id();
    h.landings.insert(ah, a);
    h.landings.insert(bh, b);
    // `a` points to a landing that is `b` only by name: re-link it to close the loop
    let a2 = LandingRecord {
        prev: Some(bh),
        ..h.landings[&ah].clone()
    };
    h.landings.insert(ah, a2);
    let e = tag_is_redacted(&mut h, bh, &pack).unwrap_err();
    assert_eq!(e.code, E_BAD_RECORD, "{e}");
    assert!(e.message.contains("is on its own chain twice"), "{e}");
    let mut h = History::default();
    let mut prev = None;
    for i in 0..100_001u32 {
        let mut l = landing_rec(prev, vec![], 1);
        l.state = Hash::of(&i.to_le_bytes());
        let id = l.id();
        h.landings.insert(id, l);
        prev = Some(id);
    }
    let e = tag_is_redacted(&mut h, prev.unwrap(), &pack).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_LIMIT, "holder check exceeds 100000 landing depth")
    );

    // the project rule, the stream rule and a holder with no canon agree tag by tag
    let s = seeded();
    let read = s.r.read();
    let dir = tempfile::tempdir().unwrap();
    for (name, tag) in [("v0.1.0", s.v0), ("v1.0.0", s.v1), ("v2.0.0", s.v2)] {
        let want = read
            .versions()
            .unwrap()
            .into_iter()
            .find(|v| v.name == name)
            .unwrap()
            .redacted;
        let landing = landing_of(&s.r, tag);
        let mut project = ReadTagHistorySource::new(&read);
        assert_eq!(
            tag_is_redacted(&mut project, landing, read.pack()).unwrap(),
            want,
            "{name}: project"
        );
        let mut rs = render(&export(&s.r, tag), inputs(&s.r)).unwrap();
        assert_eq!(rs.tag_redacted(), want, "{name}: stream");
        assert_eq!(
            tag_is_redacted(&mut rs, landing, read.pack()).unwrap(),
            want,
            "{name}: stream source"
        );
        let target = UploadTarget {
            tag: name.into(),
            landing,
        };
        let holder =
            take_into_holder(dir.path(), name, repo(&s.r), &export(&s.r, tag), &target).unwrap();
        let held = holder.read();
        let mut source = ReadTagHistorySource::new(&held);
        assert_eq!(
            tag_is_redacted(&mut source, landing, read.pack()).unwrap(),
            want,
            "{name}: holder"
        );
    }
    let flags: Vec<bool> = read
        .versions()
        .unwrap()
        .iter()
        .map(|v| v.redacted)
        .collect();
    assert_eq!(flags, [false, true, true]);
}

#[test]
fn hpb5_stream_limits_precede_payload_allocation() {
    let s = seeded();
    let good = export(&s.r, s.v0);
    let given = Arc::new(AtomicU64::new(0));
    // total bytes: 2147483648 pass the copy, one more is refused once it is read, and the
    // scratch never holds it
    for (len, refused) in [(2_147_483_648u64, false), (2_147_483_649, true)] {
        given.store(0, Ordering::SeqCst);
        let seen = Arc::new(Seen::default());
        let input = Counting {
            prefix: good.clone(),
            len,
            given: Arc::clone(&given),
        };
        let e = RenderSource::read_with(input, inputs(&s.r), &mut scratch(&seen, false, true))
            .expect_err("refused");
        assert_eq!(given.load(Ordering::SeqCst), len);
        if refused {
            assert_eq!(
                refusal(&e),
                (
                    E_LIMIT,
                    "stream exceeds 2147483648 total bytes",
                    "reduce the stream to the stated limit",
                    vec!["record none at byte 0; object none".to_string()]
                )
            );
            assert!(Seen::get(&seen.written) <= 2_147_483_648);
        } else {
            assert_ne!(e.code, E_LIMIT, "{e}");
        }
        assert_eq!(Seen::get(&seen.closed), Seen::get(&seen.opened));
    }
    // a record that claims more than one record's bytes: refused by its frame header, before
    // its payload is read back from the scratch
    let (header, _) = records_of(&good);
    let mut claim = good[..good.len()].to_vec();
    let start = {
        let mut b = b"RDYSYNC\n".to_vec();
        b.extend_from_slice(&2u32.to_le_bytes());
        let hp = 4
            + rdy_store::format::baseline_features().to_json().len()
            + 16
            + 1
            + 32
            + 4
            + header.prerequisites.len() * 33;
        b.len() + 37 + hp
    };
    claim.truncate(start);
    claim.extend_from_slice(&16_777_217u32.to_le_bytes());
    claim.push(kinds::BLOCK);
    claim.extend_from_slice(&[0u8; 32]);
    claim.extend(std::iter::repeat_n(0u8, 1 << 20));
    let seen = Arc::new(Seen::default());
    let e = RenderSource::read_with(
        Cursor::new(claim.clone()),
        inputs(&s.r),
        &mut scratch(&seen, false, false),
    )
    .expect_err("refused");
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_LIMIT, "stream exceeds 16777216 record bytes")
    );
    assert!(
        Seen::get(&seen.read) < (start + 37 + 4096) as u64,
        "{}",
        Seen::get(&seen.read)
    );
    // object records: 1000001 is refused at the record past the bound
    let mut many = good[..start].to_vec();
    for i in 0..1_000_001u32 {
        let p = i.to_le_bytes();
        many.extend_from_slice(&4u32.to_le_bytes());
        many.push(kinds::BLOB_CHUNK);
        many.extend_from_slice(&Hash::of(&p).0);
        many.extend_from_slice(&p);
    }
    let e = render(&many, inputs(&s.r)).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_LIMIT, "stream exceeds 1000000 object records")
    );
    assert!(
        e.issues[0].message.starts_with("record 1000001 at byte "),
        "{e:?}"
    );
    // the root's name: 1048576 bytes reach the version parser, one more is refused first
    let (_, records) = records_of(&good);
    let root = records
        .iter()
        .find_map(|r| match r {
            StreamRecord::Present { key, payload, .. } if key.kind == kinds::TAG => {
                Some(rdy_store::bounded::decode::<Tag>(payload).unwrap())
            }
            _ => None,
        })
        .unwrap();
    for (len, limit) in [(1_048_576usize, false), (1_048_577, true)] {
        let named = Tag {
            name: "v".repeat(len),
            ..root.clone()
        };
        let bytes = enc(&named);
        let k = ObjectKey {
            kind: kinds::TAG,
            hash: Hash::of(&bytes),
        };
        let mut recs: Vec<StreamRecord> = records
            .iter()
            .filter(|r| r.key().kind != kinds::TAG)
            .filter(|r| !matches!(r, StreamRecord::Present { key, .. } if key.kind == kinds::ANSWER || key.kind == kinds::SIGNATURE))
            .cloned()
            .collect();
        recs.push(StreamRecord::Present {
            encoding: 0,
            key: k,
            payload: bytes,
        });
        let e = render(&rebatch(repo(&s.r), k.hash, &recs), inputs(&s.r)).unwrap_err();
        if limit {
            // the record's own text bound, the same as the take's tag bound, refuses it first
            assert_eq!(
                (e.code, e.message.as_str()),
                (E_LIMIT, "stream exceeds 1048576 text bytes")
            );
        } else {
            assert_eq!(
                refusal(&e),
                refusal(&parse_version(&named.name).unwrap_err()),
                "the version parser judges it"
            );
        }
    }
}

/// Every file under `dir` with its bytes.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            let rel = p.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
            if e.file_type().unwrap().is_dir() {
                out.insert(format!("{rel}/"), vec![]);
                stack.push(p);
            } else {
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

#[test]
fn hpb5_render_source_never_opens_a_store_or_config() {
    let s = seeded();
    let stream = export(&s.r, s.v1);
    let captured = inputs(&s.r);
    // a working directory with a malformed project config, a malformed local config and a
    // damaged project directory
    let here = tempfile::tempdir().unwrap();
    std::fs::write(here.path().join(".rdy.yaml"), "pack: [unclosed\n  - {").unwrap();
    std::fs::create_dir_all(here.path().join(".rdy/store")).unwrap();
    std::fs::write(here.path().join(".rdy/store/FORMAT"), "not a format").unwrap();
    // control: a settings read there fails on the project level, then, once the local file is
    // planted, on the local level (read first), so each file level a read could take is invalid
    let e = config_levels::effective(here.path(), None).unwrap_err();
    assert_eq!(e.code, E_CONFIG, "{e}");
    assert!(e.message.ends_with(" in .rdy.yaml"), "{e}");
    let local = here.path().join(config_levels::LOCAL_FILE);
    std::fs::write(&local, "serve: [unclosed\n  - {").unwrap();
    let e = config_levels::effective(here.path(), None).unwrap_err();
    assert_eq!(e.code, E_CONFIG, "{e}");
    assert!(e.message.ends_with(" in .rdy/local.yaml"), "{e}");
    let before = tree(here.path());
    assert!(before.keys().any(|k| k.ends_with("local.yaml")));
    let back = std::env::current_dir().unwrap();
    std::env::set_current_dir(here.path()).unwrap();
    let got = render(&stream, captured.clone());
    std::env::set_current_dir(&back).unwrap();
    let rs = got.unwrap();
    // nothing was read as a project, created or changed there
    assert_eq!(tree(here.path()), before);
    assert_eq!(rs.inputs(), &captured);
    assert_eq!(rs.title(), captured.title);
    assert_eq!(rs.pack(), &captured.pack);
    // the inputs are the caller's, checked; none comes from a config
    let mut other = captured.clone();
    other.title = "Another title".into();
    let rs = render(&stream, other).unwrap();
    assert_eq!(rs.title(), "Another title");
    let mut wrong = captured.clone();
    wrong.theme = "another".into();
    let e = render(&stream, wrong).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_BAD_INPUT, "render inputs do not match this renderer")
    );
}

#[test]
fn hpb5_scratch_closes_on_every_exit() {
    let s = seeded();
    let stream = export(&s.r, s.v1);
    // success: open while the source lives, closed when it drops
    let seen = Arc::new(Seen::default());
    let rs = RenderSource::read_with(
        Cursor::new(stream.clone()),
        inputs(&s.r),
        &mut scratch(&seen, false, false),
    )
    .unwrap();
    assert_eq!((Seen::get(&seen.opened), Seen::get(&seen.closed)), (1, 0));
    assert_eq!(Seen::get(&seen.written), stream.len() as u64);
    // the lazy blob read goes through the scratch
    let read_before = Seen::get(&seen.read);
    assert_eq!(rs.blob(&s.kept).unwrap(), b"kept figure bytes");
    assert!(Seen::get(&seen.read) > read_before);
    drop(rs);
    assert_eq!((Seen::get(&seen.opened), Seen::get(&seen.closed)), (1, 1));
    // a refusal closes it before it returns
    let mut cut = stream.clone();
    cut.pop();
    let seen = Arc::new(Seen::default());
    let e = RenderSource::read_with(
        Cursor::new(cut),
        inputs(&s.r),
        &mut scratch(&seen, false, false),
    )
    .expect_err("refused");
    assert_eq!(e.code, E_STREAM_TRUNCATED);
    assert_eq!((Seen::get(&seen.opened), Seen::get(&seen.closed)), (1, 1));
    // an output that fails early drops the source with its error
    let seen = Arc::new(Seen::default());
    let out = (|| -> Result<(), RdyError> {
        let rs = RenderSource::read_with(
            Cursor::new(stream.clone()),
            inputs(&s.r),
            &mut scratch(&seen, false, false),
        )?;
        let _first = rs.blob(&s.kept)?;
        Err(RdyError::new(E_IO, "output closed", "none"))
    })();
    assert_eq!(out.unwrap_err().message, "output closed");
    assert_eq!((Seen::get(&seen.opened), Seen::get(&seen.closed)), (1, 1));
    // a scratch file that cannot be made, or written: E_IO, nothing left open
    let scratch_error = (
        E_IO,
        "the render scratch file could not be used",
        "check the temporary directory and available disk space",
    );
    let e = RenderSource::read_with(Cursor::new(stream.clone()), inputs(&s.r), &mut || {
        Err(io::Error::other("no temporary directory"))
    })
    .expect_err("refused");
    assert_eq!((e.code, e.message.as_str(), e.fix.as_str()), scratch_error);
    let seen = Arc::new(Seen::default());
    let e = RenderSource::read_with(
        Cursor::new(stream.clone()),
        inputs(&s.r),
        &mut scratch(&seen, true, false),
    )
    .expect_err("refused");
    assert_eq!((e.code, e.message.as_str(), e.fix.as_str()), scratch_error);
    assert!(e.issues.is_empty());
    assert_eq!((Seen::get(&seen.opened), Seen::get(&seen.closed)), (1, 1));
    // invalid inputs refuse before a scratch file is made
    let seen = Arc::new(Seen::default());
    let mut wrong = inputs(&s.r);
    wrong.rdy = "0.0.0".into();
    let e = RenderSource::read_with(
        Cursor::new(stream.clone()),
        wrong,
        &mut scratch(&seen, false, false),
    )
    .expect_err("refused");
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(Seen::get(&seen.opened), 0);
    // the default scratch is an anonymous file: the source reads the same through it
    let rs = render(&stream, inputs(&s.r)).unwrap();
    assert_eq!(rs.blob(&s.kept).unwrap(), b"kept figure bytes");
}

#[test]
fn hpb5_scratch_read_failure_is_the_scratch_error() {
    let s = seeded();
    let stream = export(&s.r, s.v1);
    let scratch_error = (
        E_IO,
        "the render scratch file could not be used",
        "check the temporary directory and available disk space",
    );
    // a scratch file whose reads fail once the source is made: a lazy blob read is E_IO
    let seen = Arc::new(Seen::default());
    let rs = RenderSource::read_with(
        Cursor::new(stream.clone()),
        inputs(&s.r),
        &mut scratch(&seen, false, false),
    )
    .unwrap();
    let seeks = Seen::get(&seen.seeks);
    seen.fail_reads.store(true, Ordering::SeqCst);
    let e = rs.blob(&s.kept).expect_err("refused");
    assert_eq!((e.code, e.message.as_str(), e.fix.as_str()), scratch_error);
    assert!(e.issues.is_empty());
    drop(rs);
    assert_eq!((Seen::get(&seen.opened), Seen::get(&seen.closed)), (1, 1));
    // reads that fail from any seek of the read on, whichever check is reading: E_IO
    for at in 1..=seeks {
        let seen = Arc::new(Seen::default());
        seen.fail_at_seek.store(at, Ordering::SeqCst);
        let e = RenderSource::read_with(
            Cursor::new(stream.clone()),
            inputs(&s.r),
            &mut scratch(&seen, false, false),
        )
        .expect_err("refused");
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            scratch_error,
            "failing from seek {at} of {seeks}"
        );
        assert_eq!((Seen::get(&seen.opened), Seen::get(&seen.closed)), (1, 1));
    }
}
