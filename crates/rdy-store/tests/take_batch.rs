//! The take of an upload into a holder (plan HA1 Task 11; D-BPYN8: closure and tag in one batch;
//! D-D2NPN: verify on receive; hub p-97JVE, f-NE0Z4; O11): every batch file of one upload makes
//! one sealed batch, whatever their order or repeats; the root must name the publication target;
//! nothing is written on a refusal; changed input rolls back; a repeat is a no-op; the relay-tag
//! answer ref and SIG's subject rule hold at the take.
#[path = "support/carried.rs"]
mod carried_fixture;
#[path = "support/redaction.rs"]
mod redaction_fixture;

use rdy_model::signature::Signature;
use rdy_model::*;
use rdy_store::closure::{END_OF_LOG, StoreSource, closure, write_batch};
use rdy_store::holder::answer_tag_object_ref;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::redaction::StoredFrame;
use rdy_store::stream_error::StreamError;
use rdy_store::sync_proof::ObjectSource;
use rdy_store::sync_stream::{StreamHeader, StreamReader, StreamRecord};
use rdy_store::take_batch::{
    BatchTaken, TagChoice, TakeError, UploadFacts, UploadTarget, check_input_bounds,
};
use rdy_store::{Store, StoreReader};
use redaction_fixture::{ORIGIN, enc, segment_bytes};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

fn key(kind: u8, hash: Hash) -> ObjectKey {
    ObjectKey { kind, hash }
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

fn answer(item: &str, targets: Vec<Hash>, note: Option<&str>) -> Answer {
    Answer {
        item: item.into(),
        act: AnswerAct::Tag,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets,
        person: prov("human:alice", Role::Approver),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: note.map(String::from),
    }
}

fn sig(subject: Hash, namespace: &str, n: u8) -> Signature {
    Signature {
        subject,
        namespace: namespace.into(),
        statement: format!("statement {n}").into_bytes(),
        sig: vec![n],
        signed_by: prov("human:alice", Role::Approver),
    }
}

/// A source project: one change of two blocks, landed, tagged `v1` with its relay answer
/// (`answer/tag:v1` in the source, O11), with a signature of the landing and one of the tag.
struct Src {
    dir: tempfile::TempDir,
    path: PathBuf,
    repo: RepoId,
    landing: Hash,
    tag: Hash,
    relay: Hash,
    alpha: Block,
}

fn source() -> Src {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("src");
    let mut s = Store::create(&path).unwrap();
    let alpha = block("b-00001", "alpha");
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![
                Op::Create {
                    block: alpha.clone(),
                },
                Op::Create {
                    block: block("b-00002", "beta"),
                },
            ],
        ),
    )
    .unwrap();
    let proposal = s
        .propose("work", "first", prov("agent:w", Role::Writer))
        .unwrap();
    let landing = s
        .land(&proposal, prov("human:alice", Role::Approver))
        .unwrap();
    let (tag, relay) = s
        .put_tag_answered(
            &Tag {
                name: "v1".into(),
                landing,
                message: None,
                tagged_by: prov("human:alice", Role::Approver),
            },
            answer("tag:v1", vec![], None),
        )
        .unwrap();
    s.put_signature(&sig(landing, "rdy-approval-v1", 1))
        .unwrap();
    s.put_signature(&sig(tag, "rdy-tag-v1", 2)).unwrap();
    let repo = s.repo_id().unwrap().unwrap();
    drop(s);
    Src {
        dir,
        path,
        repo,
        landing,
        tag,
        relay,
        alpha,
    }
}

impl Src {
    fn reader(&self) -> (Store, StoreReader) {
        let s = Store::open(&self.path).unwrap();
        let r = s.reader();
        (s, r)
    }

    fn target(&self) -> UploadTarget {
        UploadTarget {
            tag: "v1".into(),
            landing: self.landing,
        }
    }

    /// The whole closure of `tag`.
    fn closure(&self, tag: Hash) -> Vec<ObjectKey> {
        let (_s, r) = self.reader();
        closure(&r, tag, &BTreeSet::new()).unwrap().records
    }

    /// One batch file per part of `tag`'s closure: each part's records (in dependency order)
    /// with its exact prerequisites, as the closure gives them when the rest is known.
    fn batches(&self, tag: Hash, parts: &[BTreeSet<ObjectKey>], name: &str) -> Vec<PathBuf> {
        let (_s, r) = self.reader();
        let all: BTreeSet<ObjectKey> = closure(&r, tag, &BTreeSet::new())
            .unwrap()
            .records
            .into_iter()
            .collect();
        parts
            .iter()
            .enumerate()
            .map(|(i, part)| {
                let known: BTreeSet<ObjectKey> = all.difference(part).copied().collect();
                let c = closure(&r, tag, &known).unwrap();
                assert_eq!(c.records.iter().copied().collect::<BTreeSet<_>>(), *part);
                let path = self.dir.path().join(format!("{name}-{i}.rdysync"));
                write(&r, &self.repo, tag, &c.records, c.prerequisites, &path);
                path
            })
            .collect()
    }

    /// The closure of `tag` cut into `n` contiguous parts.
    fn split(&self, tag: Hash, n: usize, name: &str) -> Vec<PathBuf> {
        let records = self.closure(tag);
        let size = records.len().div_ceil(n);
        let parts: Vec<BTreeSet<ObjectKey>> = records
            .chunks(size)
            .map(|c| c.iter().copied().collect())
            .collect();
        self.batches(tag, &parts, name)
    }
}

fn write(
    r: &StoreReader,
    repo: &RepoId,
    tag: Hash,
    records: &[ObjectKey],
    prerequisites: Vec<ObjectKey>,
    path: &Path,
) {
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        *repo,
        tag,
        prerequisites,
    );
    let mut source = StoreSource {
        reader: r,
        end: END_OF_LOG,
    };
    write_batch(&mut source, &header, records, File::create(path).unwrap()).unwrap();
}

/// A batch of the records `extra` (kind and bytes, in that order) with `prerequisites`, rooted
/// at `tag`.
fn write_extra(
    repo: &RepoId,
    tag: Hash,
    extra: &[(u8, Vec<u8>)],
    prerequisites: Vec<ObjectKey>,
    path: &Path,
) {
    let mut keys = vec![];
    let mut source: BTreeMap<ObjectKey, StreamRecord> = BTreeMap::new();
    for (kind, payload) in extra {
        let k = key(*kind, Hash::of(payload));
        keys.push(k);
        source.insert(
            k,
            StreamRecord::Present {
                encoding: 0,
                key: k,
                payload: payload.clone(),
            },
        );
    }
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        *repo,
        tag,
        prerequisites,
    );
    write_batch(&mut source, &header, &keys, File::create(path).unwrap()).unwrap();
}

fn open(paths: &[PathBuf]) -> Vec<File> {
    paths.iter().map(|p| File::open(p).unwrap()).collect()
}

fn holder(src: &Src, name: &str) -> (PathBuf, Store) {
    let path = src.dir.path().join(name);
    let h = Store::create_holder(&path, src.repo).unwrap();
    (path, h)
}

fn incoming(_: &mut dyn ObjectSource, _: &UploadFacts) -> Result<TagChoice, StreamError> {
    Ok(TagChoice::Incoming)
}

fn take(
    h: &mut Store,
    files: &[File],
    target: &UploadTarget,
) -> Result<BatchTaken, TakeError<StreamError>> {
    h.take_batches(files, target, incoming)
}

/// Every frame of the log as (location, kind, hash, dropped).
fn log_frames(h: &Store) -> Vec<(rdy_store::segment::Location, u8, Hash, bool)> {
    let mut out = vec![];
    h.reader()
        .for_each_record_from(ORIGIN, h.log_end(), |loc, f, _| {
            out.push(match f {
                StoredFrame::Present { kind, hash, .. } => (loc, kind, hash, false),
                StoredFrame::Dropped { kind, hash, .. } => (loc, kind, hash, true),
            });
            Ok(())
        })
        .unwrap();
    out
}

fn markers(h: &Store) -> usize {
    log_frames(h)
        .iter()
        .filter(|(_, k, _, _)| *k == kinds::BATCH_COMMIT)
        .count()
}

fn stream_err(e: TakeError<StreamError>) -> StreamError {
    match e {
        TakeError::Stream(e) => e,
        other => panic!("not a stream refusal: {other:?}"),
    }
}

#[test]
fn ha11_many_files_make_one_sealed_take() {
    let src = source();
    let records = src.closure(src.tag);
    let paths = src.split(src.tag, 3, "three");
    assert_eq!(paths.len(), 3);
    let (_, mut h) = holder(&src, "h");
    let before = markers(&h);
    let end = h.log_end();
    let got = take(&mut h, &open(&paths), &src.target()).unwrap();
    // one marker over every record and the two refs
    assert_eq!(markers(&h), before + 1);
    assert_eq!(got.new_objects, records.len() as u64);
    assert_eq!(got.frames, records.len() as u64 + 2);
    assert_eq!(got.redacted_objects, 0);
    assert_eq!(got.end, h.log_end());
    assert_eq!(got.tag_hash, src.tag);
    assert_eq!(got.tag.name, "v1");
    let data: Vec<_> = log_frames(&h)
        .into_iter()
        .filter(|(l, k, _, _)| *l >= end && *k != kinds::BATCH_COMMIT)
        .collect();
    assert_eq!(data.len() as u64, got.frames);
    for k in &records {
        assert!(h.has_kind(k.kind, &k.hash).unwrap(), "{k} is held");
    }
    // the records in the closure's dependency order, then the tag ref and the relay answer ref,
    // named by the tag object in a holder (ruling task-13)
    let written: Vec<ObjectKey> = data
        .iter()
        .filter(|(_, k, _, _)| *k != kinds::REF_UPDATE)
        .map(|(_, k, h, _)| key(rdy_store::codec::logical_kind(*k), *h))
        .collect();
    assert_eq!(written, records);
    assert_eq!(h.get_ref("tag/v1").unwrap(), Some(src.tag.0.to_vec()));
    assert_eq!(
        h.get_ref(&answer_tag_object_ref(&src.tag)).unwrap(),
        Some(src.relay.0.to_vec())
    );
    assert_eq!(h.get_ref("answer/tag:v1").unwrap(), None);
    h.check_meta().unwrap();
    // the sealed take reopens
    let path = h.dir().to_path_buf();
    drop(h);
    let h = Store::open(&path).unwrap();
    assert_eq!(h.get_ref("tag/v1").unwrap(), Some(src.tag.0.to_vec()));
}

#[test]
fn ha11_batch_order_and_repeats_do_not_change_the_take() {
    let src = source();
    let p = src.split(src.tag, 3, "three");
    let (a_path, mut a) = holder(&src, "a");
    let (b_path, mut b) = holder(&src, "b");
    let in_order = take(&mut a, &open(&p), &src.target()).unwrap();
    let shuffled = [
        p[2].clone(),
        p[0].clone(),
        p[1].clone(),
        p[0].clone(),
        p[2].clone(),
    ];
    let repeated = take(&mut b, &open(&shuffled), &src.target()).unwrap();
    assert_eq!(in_order, repeated);
    drop((a, b));
    assert_eq!(segment_bytes(&a_path), segment_bytes(&b_path));
    // one whole batch and its parts make the same take too
    let whole = src.split(src.tag, 1, "one");
    let (c_path, mut c) = holder(&src, "c");
    let mixed = [p[1].clone(), whole[0].clone(), p[1].clone()];
    take(&mut c, &open(&mixed), &src.target()).unwrap();
    drop(c);
    assert_eq!(segment_bytes(&a_path), segment_bytes(&c_path));
}

#[test]
fn ha11_publication_target_mismatch_writes_nothing() {
    let src = source();
    let paths = src.split(src.tag, 2, "two");
    let (path, mut h) = holder(&src, "h");
    let before = segment_bytes(&path);
    let wrong = [
        // the exact name, byte for byte: no case fold, no trim, no optional `v`
        UploadTarget {
            tag: "V1".into(),
            landing: src.landing,
        },
        UploadTarget {
            tag: "v1 ".into(),
            landing: src.landing,
        },
        UploadTarget {
            tag: "1".into(),
            landing: src.landing,
        },
        // the right name at another landing
        UploadTarget {
            tag: "v1".into(),
            landing: src.tag,
        },
    ];
    for t in &wrong {
        let e = stream_err(take(&mut h, &open(&paths), t).unwrap_err());
        assert_eq!(e.code, "E_BAD_RECORD");
        assert_eq!(e.message, "upload target does not match the batch root tag");
        assert_eq!(
            e.fix,
            "upload the history for the publication tag and landing"
        );
        assert_eq!(e.object, Some(key(kinds::TAG, src.tag)));
    }
    // the target names no hash: another TAG named v1 is refused at another landing too
    let mut s = Store::open(&src.path).unwrap();
    let other = s
        .put_tag(&Tag {
            name: "v1".into(),
            landing: src.landing,
            message: Some("another v1".into()),
            tagged_by: prov("human:alice", Role::Approver),
        })
        .unwrap();
    drop(s);
    let other_paths = src.split(other, 1, "other");
    let e = stream_err(
        take(
            &mut h,
            &open(&other_paths),
            &UploadTarget {
                tag: "v1".into(),
                landing: src.tag,
            },
        )
        .unwrap_err(),
    );
    assert_eq!(e.message, "upload target does not match the batch root tag");
    assert_eq!(h.get_ref("tag/v1").unwrap(), None);
    drop(h);
    assert_eq!(segment_bytes(&path), before);
    // the bounds of the inputs, before anything is read
    let e = check_input_bounds(0, &src.target()).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        ("E_BAD_RECORD", "stream record 0 at byte 0: no batch files")
    );
    let e = check_input_bounds(1025, &src.target()).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_LIMIT",
            "stream exceeds 1024 batch files",
            "reduce the stream to the stated limit"
        )
    );
    check_input_bounds(1024, &src.target()).unwrap();
    let long = UploadTarget {
        tag: "v".repeat(1_048_577),
        landing: src.landing,
    };
    let e = check_input_bounds(1, &long).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_LIMIT",
            "upload tag exceeds 1048576 bytes",
            "reduce the tag name to the stated limit"
        )
    );
    check_input_bounds(
        1,
        &UploadTarget {
            tag: "v".repeat(1_048_576),
            landing: src.landing,
        },
    )
    .unwrap();
    // the target is a strict input
    let json = format!(r#"{{"tag":"v1","landing":"{}"}}"#, src.landing);
    let t: UploadTarget = serde_json::from_str(&json).unwrap();
    assert_eq!(t, src.target());
    let json = format!(r#"{{"tag":"v1","landing":"{}","tga":"x"}}"#, src.landing);
    assert!(serde_json::from_str::<UploadTarget>(&json).is_err());
}

#[test]
fn ha11_missing_root_or_dependency_changes_nothing() {
    let src = source();
    let root = key(kinds::TAG, src.tag);
    let landing = key(kinds::LANDING, src.landing);
    let all: BTreeSet<ObjectKey> = src.closure(src.tag).into_iter().collect();
    let only = |k: ObjectKey| BTreeSet::from([k]);
    let rest = |out: &[ObjectKey]| -> BTreeSet<ObjectKey> {
        all.iter().filter(|k| !out.contains(k)).copied().collect()
    };
    let parts = src.batches(
        src.tag,
        &[only(root), only(landing), rest(&[root, landing])],
        "cut",
    );
    let (path, mut h) = holder(&src, "h");
    let before = (
        segment_bytes(&path),
        h.refs_with_prefix("").unwrap(),
        h.log_end(),
    );
    let refused = |h: &mut Store, files: &[PathBuf]| {
        stream_err(take(h, &open(files), &src.target()).unwrap_err())
    };
    // the root missing: every other file names it
    let e = refused(&mut h, &[parts[1].clone(), parts[2].clone()]);
    assert_eq!(e.code, "E_UNRESOLVED_REF");
    assert_eq!(e.message, format!("stream prerequisite {root} is missing"));
    assert_eq!(e.fix, "send the missing object with its dependencies");
    assert_eq!(e.object, Some(root));
    // a dependency missing: the smallest missing key of the union is named
    let e = refused(&mut h, &[parts[0].clone(), parts[2].clone()]);
    assert_eq!(
        e.message,
        format!("stream prerequisite {landing} is missing")
    );
    let e = refused(&mut h, &[parts[2].clone()]);
    assert_eq!(
        e.message,
        format!("stream prerequisite {landing} is missing")
    );
    // a batch of another repo is refused before its records count
    let other = Store::create(&src.dir.path().join("other")).unwrap();
    let other_repo = other.repo_id().unwrap().unwrap();
    drop(other);
    let foreign = src.dir.path().join("foreign.rdysync");
    {
        let (_s, r) = src.reader();
        let c = closure(&r, src.tag, &BTreeSet::new()).unwrap();
        write(
            &r,
            &other_repo,
            src.tag,
            &c.records,
            c.prerequisites,
            &foreign,
        );
    }
    match take(&mut h, &open(std::slice::from_ref(&foreign)), &src.target()).unwrap_err() {
        TakeError::RepoMismatch { source, target } => {
            assert_eq!((source, target), (other_repo, src.repo));
        }
        other => panic!("{other:?}"),
    }
    // two files of different headers
    let e = refused(&mut h, &[parts[0].clone(), foreign]);
    assert_eq!(
        e.message,
        "stream record 0 at byte 12: batch header differs"
    );
    assert_eq!(
        (
            segment_bytes(&path),
            h.refs_with_prefix("").unwrap(),
            h.log_end()
        ),
        before
    );
    // and with every part, the take passes
    take(&mut h, &open(&parts), &src.target()).unwrap();
    // a project refuses the take with its structured variant
    let mut p = Store::open(&src.path).unwrap();
    assert!(matches!(
        p.take_batches(&open(&parts), &src.target(), incoming),
        Err(TakeError::ModeMismatch)
    ));
}

#[test]
fn ha11_changed_input_during_append_rolls_back() {
    let src = source();
    let paths = src.split(src.tag, 1, "one");
    // the last record of the file: its place and a byte inside its payload
    let (n, offset) = {
        let (mut r, _) = StreamReader::open(File::open(&paths[0]).unwrap()).unwrap();
        while r.next().unwrap().is_some() {}
        r.position()
    };
    let (path, h) = holder(&src, "h");
    let before = (segment_bytes(&path), h.log_end());
    let files = open(&paths);
    let barrier = Arc::new(Barrier::new(2));
    let mut h = h;
    let b = Arc::clone(&barrier);
    let last = src.closure(src.tag).len() as u64 - 1;
    h.on_take_append_for_tests(move |appended| {
        if appended == last {
            b.wait();
            b.wait();
        }
    });
    let target = src.target();
    let worker = std::thread::spawn(move || {
        let got = take(&mut h, &files, &target);
        (h, got)
    });
    // the take has appended every record but the last; change that one's bytes
    barrier.wait();
    let mut bytes = std::fs::read(&paths[0]).unwrap();
    let at = offset as usize + 37;
    bytes[at] ^= 0xFF;
    std::fs::write(&paths[0], &bytes).unwrap();
    barrier.wait();
    let (h, got) = worker.join().unwrap();
    let e = stream_err(got.unwrap_err());
    assert_eq!(e.code, "E_BAD_RECORD");
    assert_eq!(
        e.message,
        format!("stream record {n} at byte {offset}: input changed during take")
    );
    assert_eq!(h.log_end(), before.1);
    assert_eq!(h.get_ref("tag/v1").unwrap(), None);
    drop(h);
    assert_eq!(segment_bytes(&path), before.0);
    // put the bytes back: a path that names another file now cannot redirect an opened handle
    bytes[at] ^= 0xFF;
    std::fs::write(&paths[0], &bytes).unwrap();
    let mut h = Store::open(&path).unwrap();
    let files = open(&paths);
    let moved = src.dir.path().join("moved.rdysync");
    let p0 = paths[0].clone();
    let barrier = Arc::new(Barrier::new(2));
    let b = Arc::clone(&barrier);
    h.on_take_append_for_tests(move |appended| {
        if appended == 1 {
            b.wait();
            b.wait();
        }
    });
    let target = src.target();
    let worker = std::thread::spawn(move || {
        let got = take(&mut h, &files, &target);
        (h, got)
    });
    barrier.wait();
    std::fs::rename(&p0, &moved).unwrap();
    std::fs::write(&p0, b"RDYSYNC\nnot the batch").unwrap();
    barrier.wait();
    let (h, got) = worker.join().unwrap();
    got.unwrap();
    assert_eq!(h.get_ref("tag/v1").unwrap(), Some(src.tag.0.to_vec()));
}

#[test]
fn ha11_repeated_take_appends_nothing() {
    let src = source();
    let paths = src.split(src.tag, 2, "two");
    let (path, mut h) = holder(&src, "h");
    take(&mut h, &open(&paths), &src.target()).unwrap();
    let end = h.log_end();
    let bytes = segment_bytes(&path);
    let again = take(&mut h, &open(&paths), &src.target()).unwrap();
    assert_eq!(
        again,
        BatchTaken {
            new_objects: 0,
            frames: 0,
            redacted_objects: 0,
            end,
            tag_hash: src.tag,
            tag: again.tag.clone(),
        }
    );
    assert_eq!(again.tag.name, "v1");
    // the existing tag chosen: nothing either
    let existing = h
        .take_batches(&open(&paths), &src.target(), |_, _| {
            Ok::<_, StreamError>(TagChoice::Existing {
                name: "v1".into(),
                hash: src.tag,
            })
        })
        .unwrap();
    assert_eq!(
        (existing.new_objects, existing.frames, existing.end),
        (0, 0, end)
    );
    // the caller's validation error passes through, and nothing is written
    let e = h
        .take_batches(&open(&paths), &src.target(), |_, facts| {
            assert_eq!(facts.root_hash, src.tag);
            assert_eq!(facts.repo_id, src.repo);
            assert!(facts.objects.windows(2).all(|w| w[0] < w[1]));
            Err::<TagChoice, _>("no")
        })
        .unwrap_err();
    assert!(matches!(e, TakeError::Validation("no")));
    drop(h);
    assert_eq!(segment_bytes(&path), bytes);
}

#[test]
fn ha11_take_writes_the_relay_tag_answer_ref_once() {
    let src = source();
    let paths = src.split(src.tag, 1, "one");
    // Incoming: the tag ref and the relay answer ref of the tag object, once
    let (_, mut h) = holder(&src, "h");
    take(&mut h, &open(&paths), &src.target()).unwrap();
    assert_eq!(
        h.get_ref(&answer_tag_object_ref(&src.tag)).unwrap(),
        Some(src.relay.0.to_vec())
    );
    let again = take(&mut h, &open(&paths), &src.target()).unwrap();
    assert_eq!(again.frames, 0);
    // Existing: another tag of the same landing keeps the held one as the version; its own tag
    // object is stored with the ref of its relay answer (ruling task-13)
    let mut s = Store::open(&src.path).unwrap();
    let (v10, relay10) = s
        .put_tag_answered(
            &Tag {
                name: "v1.0".into(),
                landing: src.landing,
                message: None,
                tagged_by: prov("human:alice", Role::Approver),
            },
            answer("tag:v1.0", vec![], None),
        )
        .unwrap();
    drop(s);
    let v10_paths = src.split(v10, 1, "v10");
    let target10 = UploadTarget {
        tag: "v1.0".into(),
        landing: src.landing,
    };
    let choose = |name: &str, hash: Hash| {
        let name = name.to_string();
        move |_: &mut dyn ObjectSource, _: &UploadFacts| {
            Ok::<_, StreamError>(TagChoice::Existing { name, hash })
        }
    };
    let got = h
        .take_batches(&open(&v10_paths), &target10, choose("v1", src.tag))
        .unwrap();
    assert_eq!((got.tag_hash, got.tag.name.as_str()), (src.tag, "v1"));
    assert!(h.has_kind(kinds::TAG, &v10).unwrap());
    assert!(h.has_kind(kinds::ANSWER, &relay10).unwrap());
    assert_eq!(h.get_ref("tag/v1.0").unwrap(), None);
    assert_eq!(
        h.get_ref(&answer_tag_object_ref(&v10)).unwrap(),
        Some(relay10.0.to_vec())
    );
    assert_eq!(
        h.get_ref(&answer_tag_object_ref(&src.tag)).unwrap(),
        Some(src.relay.0.to_vec())
    );
    h.check_holder().unwrap();
    // an Existing choice must name a held tag of that landing by its stored name and hash
    for (name, hash) in [("v1", v10), ("v2", src.tag), ("v1.0", v10)] {
        let e = stream_err(
            h.take_batches(&open(&paths), &src.target(), choose(name, hash))
                .unwrap_err(),
        );
        assert_eq!(
            e.message, "stream record 0 at byte 0: invalid header",
            "{name}"
        );
    }
    // two relay answers of the root (each targets the root TAG, as a relay answer does):
    // refused, whichever tag stays the version, as one tag object has one relay answer
    let second = enc(&answer("tag:v1", vec![src.tag], Some("again")));
    let extra = src.dir.path().join("second.rdysync");
    write_extra(
        &src.repo,
        src.tag,
        &[(kinds::ANSWER, second.clone())],
        vec![key(kinds::TAG, src.tag)],
        &extra,
    );
    let mut both = paths.clone();
    both.push(extra);
    let (_, mut fresh) = holder(&src, "fresh");
    let e = stream_err(take(&mut fresh, &open(&both), &src.target()).unwrap_err());
    assert_eq!(
        e.message,
        "stream record 0 at byte 0: object is outside the tag closure"
    );
    assert_eq!(fresh.get_ref("tag/v1").unwrap(), None);
    let end = h.log_end();
    let e = stream_err(
        h.take_batches(&open(&both), &src.target(), choose("v1", src.tag))
            .unwrap_err(),
    );
    assert_eq!(
        e.message,
        "stream record 0 at byte 0: object is outside the tag closure"
    );
    assert_eq!(h.log_end(), end);
    // one relay answer of the root, but not the one the holder holds for that tag object
    let only = vec![src.dir.path().join("second.rdysync")];
    let e = stream_err(take(&mut h, &open(&only), &src.target()).unwrap_err());
    assert_eq!(
        e.message,
        "stream record 0 at byte 0: object is outside the tag closure"
    );
    assert_eq!(h.log_end(), end);
    assert!(!h.has_kind(kinds::ANSWER, &Hash::of(&second)).unwrap());
    assert_eq!(
        h.get_ref(&answer_tag_object_ref(&src.tag)).unwrap(),
        Some(src.relay.0.to_vec())
    );
}

#[test]
fn ha11_signature_subjects_are_checked_at_take() {
    let src = source();
    let paths = src.split(src.tag, 1, "one");
    let root = key(kinds::TAG, src.tag);
    let alpha = key(kinds::BLOCK, src.alpha.version_hash());
    let landing = key(kinds::LANDING, src.landing);
    // a signature of a block: SIG allows only a landing or a tag
    let of_block = postcard::to_allocvec(&sig(alpha.hash, "rdy-approval-v1", 9)).unwrap();
    // a key binding: never stored
    let keybind = postcard::to_allocvec(&sig(src.landing, "rdy-keybind-v1", 8)).unwrap();
    for (bytes, prerequisites, text) in [
        (
            of_block,
            vec![alpha, root],
            format!(
                "signature subject {} is not a present landing or tag",
                alpha.hash
            ),
        ),
        (
            keybind,
            vec![landing, root],
            "key binding signatures are not stored".to_string(),
        ),
    ] {
        let extra = src.dir.path().join("sig.rdysync");
        write_extra(
            &src.repo,
            src.tag,
            &[(kinds::SIGNATURE, bytes)],
            prerequisites,
            &extra,
        );
        let mut files = paths.clone();
        files.push(extra);
        let (path, mut h) = holder(&src, &format!("h-{}", text.len()));
        let before = segment_bytes(&path);
        match take(&mut h, &open(&files), &src.target()).unwrap_err() {
            TakeError::Store(rdy_store::StoreError::Refused(m)) => assert_eq!(m, text),
            other => panic!("{other:?}"),
        }
        drop(h);
        assert_eq!(segment_bytes(&path), before);
    }
    // the source's own signatures of its landing and tag are taken
    let (_, mut h) = holder(&src, "ok");
    take(&mut h, &open(&paths), &src.target()).unwrap();
    assert_eq!(h.signatures_of(&src.landing).unwrap().len(), 1);
    assert_eq!(h.signatures_of(&src.tag).unwrap().len(), 1);
}

#[test]
fn ha11_changed_relay_answer_refuses_before_append() {
    let src = source();
    let paths = src.split(src.tag, 1, "one");
    let relay = key(kinds::ANSWER, src.relay);
    // the relay answer's place in the file
    let (n, offset) = {
        let (mut r, _) = StreamReader::open(File::open(&paths[0]).unwrap()).unwrap();
        loop {
            let rec = r.next().unwrap().expect("the relay answer is in the file");
            if rec.key() == relay {
                break r.position();
            }
        }
    };
    // a holder that holds every immutable record of the upload, the relay answer included, and
    // no tag or answer ref
    let (path, mut h) = holder(&src, "h");
    let items: Vec<(u8, Vec<u8>)> = {
        let records = src.closure(src.tag);
        let (_s, r) = src.reader();
        let mut source = StoreSource {
            reader: &r,
            end: END_OF_LOG,
        };
        records
            .into_iter()
            .map(|k| match source.record(k).unwrap().unwrap() {
                // the raw frame: its raw kind and payload
                r @ StreamRecord::Present { .. } => {
                    let raw = r.raw_kind();
                    let StreamRecord::Present { payload, .. } = r else {
                        unreachable!("a present record")
                    };
                    (raw, payload)
                }
                other => panic!("{other:?}"),
            })
            .collect()
    };
    h.write(items).unwrap();
    assert!(h.has_kind(kinds::ANSWER, &src.relay).unwrap());
    assert_eq!(h.get_ref("tag/v1").unwrap(), None);
    let before = (segment_bytes(&path), h.log_end());
    // the caller's check changes a byte of the answer's payload, then chooses the incoming tag
    let p0 = paths[0].clone();
    let got = h.take_batches(&open(&paths), &src.target(), move |_, _| {
        let mut bytes = std::fs::read(&p0).unwrap();
        bytes[offset as usize + 37] ^= 0xFF;
        std::fs::write(&p0, &bytes).unwrap();
        Ok::<_, StreamError>(TagChoice::Incoming)
    });
    let e = stream_err(got.unwrap_err());
    assert_eq!(
        (e.code, e.message),
        (
            "E_BAD_RECORD",
            format!("stream record {n} at byte {offset}: input changed during take")
        )
    );
    assert_eq!(h.log_end(), before.1);
    assert_eq!(h.get_ref("tag/v1").unwrap(), None);
    assert_eq!(h.get_ref(&answer_tag_object_ref(&src.tag)).unwrap(), None);
    drop(h);
    assert_eq!(segment_bytes(&path), before.0);
}

/// Fix round 2: the take installs `answer/tag-object:<root>` only for an answer that targets
/// exactly the root TAG. Two tag objects of one name at one landing (A held, B uploaded): an
/// answer to `tag:v1` that targets A, or no object, is not B's relay answer, so an upload of B
/// that carries it is refused before any append; one that targets B and A is an answer of B,
/// taken with no ref; and B's own relay answer gets B's ref.
#[test]
fn ha13_take_names_only_the_relay_answer_of_its_own_tag_object() {
    let src = source();
    let paths = src.split(src.tag, 1, "one");
    let (path, mut h) = holder(&src, "h");
    take(&mut h, &open(&paths), &src.target()).unwrap();
    // B: a second tag object named v1 at the same landing, as a clone tags it
    let b = Tag {
        name: "v1".into(),
        landing: src.landing,
        message: Some("B".into()),
        tagged_by: prov("human:alice", Role::Approver),
    };
    let b_bytes = enc(&b);
    let b_hash = Hash::of(&b_bytes);
    assert_ne!(b_hash, src.tag);
    let existing = |_: &mut dyn ObjectSource, _: &UploadFacts| {
        Ok::<_, StreamError>(TagChoice::Existing {
            name: "v1".into(),
            hash: src.tag,
        })
    };
    // an upload of B and an answer to tag:v1 with `targets`: the answer's hash and the file
    let upload = |name: &str, targets: Vec<Hash>| {
        let a = enc(&answer("tag:v1", targets.clone(), Some(name)));
        let file = src.dir.path().join(format!("{name}.rdysync"));
        write_extra(
            &src.repo,
            b_hash,
            &[(kinds::TAG, b_bytes.clone()), (kinds::ANSWER, a.clone())],
            // the exact prerequisites: the landing, and A when the answer targets it
            std::iter::once(key(kinds::LANDING, src.landing))
                .chain(targets.contains(&src.tag).then(|| key(kinds::TAG, src.tag)))
                .collect(),
            &file,
        );
        (Hash::of(&a), file)
    };
    // crossed: an answer that targets A (the held tag of the same name), and one with no target
    for (name, targets) in [("crossed", vec![src.tag]), ("none", vec![])] {
        let (a, file) = upload(name, targets);
        let before = (segment_bytes(&path), h.log_end());
        let e = stream_err(
            h.take_batches(&open(&[file]), &src.target(), existing)
                .unwrap_err(),
        );
        assert_eq!(
            e.message, "stream record 0 at byte 0: object is outside the tag closure",
            "{name}"
        );
        assert_eq!(h.log_end(), before.1);
        assert_eq!(segment_bytes(&path), before.0);
        assert!(!h.has_kind(kinds::TAG, &b_hash).unwrap());
        assert!(!h.has_kind(kinds::ANSWER, &a).unwrap());
        assert_eq!(h.get_ref(&answer_tag_object_ref(&b_hash)).unwrap(), None);
        h.check_holder().unwrap();
    }
    // an answer that targets B and A: attached to B by its target, taken, but not B's relay
    // answer
    let (loose, file) = upload("loose", vec![b_hash, src.tag]);
    h.take_batches(&open(&[file]), &src.target(), existing)
        .unwrap();
    assert!(h.has_kind(kinds::TAG, &b_hash).unwrap());
    assert!(h.has_kind(kinds::ANSWER, &loose).unwrap());
    assert_eq!(h.get_ref(&answer_tag_object_ref(&b_hash)).unwrap(), None);
    h.check_holder().unwrap();
    // B with its own relay answer: taken, and named by B's tag object
    let (own, file) = upload("own", vec![b_hash]);
    let got = h
        .take_batches(&open(&[file]), &src.target(), existing)
        .unwrap();
    assert_eq!(got.tag_hash, src.tag);
    assert_eq!(
        h.get_ref(&answer_tag_object_ref(&b_hash)).unwrap(),
        Some(own.0.to_vec())
    );
    assert_eq!(
        h.get_ref(&answer_tag_object_ref(&src.tag)).unwrap(),
        Some(src.relay.0.to_vec())
    );
    h.check_holder().unwrap();
}

/// The source copied to `removed` and then a person's removal of alpha applied there (the
/// shared raw fixture), its erasure finished: the copy's path, the request and the apply.
struct Removed {
    path: PathBuf,
    request: Hash,
    apply: Hash,
}

fn removed_copy(src: &Src) -> Removed {
    let path = src.dir.path().join("removed");
    carried_fixture::copy_store(&src.path, &path);
    let fixture = redaction_fixture::authorized_batch_fixture(
        &path,
        &[redaction_fixture::RequestSpec::new(
            "work",
            vec![rdy_model::redaction::RedactTarget::Version {
                id: src.alpha.id.clone(),
                version: src.alpha.version_hash(),
            }],
        )
        .at(60)],
        &redaction_fixture::FixtureOptions::approved(redaction_fixture::Proof::local_human()),
    );
    drop(Store::open(&path).unwrap());
    Removed {
        path,
        request: fixture.requests[0].request,
        apply: fixture.applies[0].0,
    }
}

/// The whole history of `tag` in the store at `path`, as one batch file `name`.
fn upload_of(src: &Src, path: &Path, tag: Hash, name: &str) -> PathBuf {
    let s = Store::open(path).unwrap();
    let r = s.reader();
    let c = closure(&r, tag, &BTreeSet::new()).unwrap();
    let file = src.dir.path().join(format!("{name}.rdysync"));
    write(&r, &src.repo, tag, &c.records, c.prerequisites, &file);
    file
}

/// The change that wrote alpha (and beta) in the source.
fn alpha_change(src: &Src) -> Change {
    let s = Store::open(&src.path).unwrap();
    let id = s.producers(&src.alpha.version_hash()).unwrap()[0];
    s.change(&id).unwrap()
}

/// A holder that took v1 from the source before the removal, loaded into `name` with one
/// carried record of `request` and `apply` over its own frames of alpha (block and change) and,
/// with `beta`, beta's block, each `held`. Returns the holder's path and the record.
fn carried_holder(
    src: &Src,
    name: &str,
    request: Hash,
    apply: Hash,
    held: bool,
    beta: bool,
) -> (PathBuf, rdy_model::redaction::RedactionCarried) {
    use carried_fixture::{block_residue, change_residue, find, framed_at, span};
    let pre = src.dir.path().join(format!("{name}-pre"));
    {
        let mut h = Store::create_holder(&pre, src.repo).unwrap();
        take(&mut h, &open(&src.split(src.tag, 1, name)), &src.target()).unwrap();
    }
    let frames = framed_at(&pre);
    let c = alpha_change(src);
    let beta_block = block("b-00002", "beta");
    let mut residues = vec![block_residue(&src.alpha), change_residue(&c)];
    let mut spans = vec![
        span(
            &find(&frames, kinds::BLOCK, src.alpha.version_hash()),
            Some(&residues[0]),
            held,
        ),
        span(
            &find(&frames, kinds::CHANGE, c.id()),
            Some(&residues[1]),
            held,
        ),
    ];
    if beta {
        residues.push(block_residue(&beta_block));
        spans.push(span(
            &find(&frames, kinds::BLOCK, beta_block.version_hash()),
            Some(&residues[2]),
            held,
        ));
    }
    spans.sort_by_key(|s| s.place);
    let mut rec = carried_fixture::record(
        vec![rdy_model::redaction::RedactTarget::Version {
            id: src.alpha.id.clone(),
            version: src.alpha.version_hash(),
        }],
        spans,
    );
    rec.request = request;
    rec.apply = apply;
    let to = src.dir.path().join(name);
    carried_fixture::load(&pre, &to, &residues, std::slice::from_ref(&rec)).unwrap();
    (to, rec)
}

/// The proof the record walker names for the dropped frame of `kind` and `hash`.
fn proof_of(h: &Store, kind: u8, hash: Hash) -> Option<Hash> {
    let mut out = None;
    h.reader()
        .for_each_record_from(ORIGIN, h.log_end(), |_, f, _| {
            if let StoredFrame::Dropped {
                kind: k,
                hash: x,
                apply,
                ..
            } = f
                && (rdy_store::codec::logical_kind(k), x) == (kind, hash)
            {
                out = Some(apply);
            }
            Ok(())
        })
        .unwrap();
    out
}

#[test]
fn hc3_take_of_any_matching_apply_removes_held_payloads() {
    let src = source();
    let removed = removed_copy(&src);
    let upload = upload_of(&src, &removed.path, src.tag, "removed");
    let alpha = src.alpha.version_hash();
    let c = alpha_change(&src).id();
    // the source's own apply removes the held payloads its carried record names
    let (path, rec) = carried_holder(&src, "own", removed.request, removed.apply, true, false);
    let mut h = Store::open(&path).unwrap();
    assert_eq!(h.reader().carried_counts(&rec).unwrap(), (2, 0));
    take(&mut h, &open(std::slice::from_ref(&upload)), &src.target()).unwrap();
    assert_eq!(h.reader().carried_counts(&rec).unwrap(), (0, 2));
    assert_eq!(
        h.get_block(&alpha).unwrap().body,
        rdy_model::redaction::CONTENT_REMOVED
    );
    assert_eq!(proof_of(&h, kinds::BLOCK, alpha), Some(removed.apply));
    drop(h);
    // another request's apply counts too; a partial take erases only the spans it covers
    let (path, rec) = carried_holder(
        &src,
        "other",
        Hash::of(b"another request"),
        Hash::of(b"another apply"),
        true,
        true,
    );
    let mut h = Store::open(&path).unwrap();
    assert_eq!(h.reader().carried_counts(&rec).unwrap(), (3, 0));
    take(&mut h, &open(std::slice::from_ref(&upload)), &src.target()).unwrap();
    assert_eq!(h.reader().carried_counts(&rec).unwrap(), (1, 2));
    let beta = block("b-00002", "beta");
    assert_eq!(h.get_block(&beta.version_hash()).unwrap(), beta);
    assert_eq!(proof_of(&h, kinds::CHANGE, c), Some(removed.apply));
    drop(h);
    // an apply over zeros a carried record explained becomes their proof
    let (path, rec) = carried_holder(
        &src,
        "zeros",
        Hash::of(b"another request"),
        Hash::of(b"another apply"),
        false,
        false,
    );
    let carried = carried_fixture::carried_hash(&rec);
    let mut h = Store::open(&path).unwrap();
    assert_eq!(proof_of(&h, kinds::BLOCK, alpha), Some(carried));
    take(&mut h, &open(std::slice::from_ref(&upload)), &src.target()).unwrap();
    assert_eq!(proof_of(&h, kinds::BLOCK, alpha), Some(removed.apply));
    assert_eq!(proof_of(&h, kinds::CHANGE, c), Some(removed.apply));
    assert_eq!(h.reader().carried_counts(&rec).unwrap(), (0, 2));
    let path = h.dir().to_path_buf();
    drop(h);
    // the reopened holder derives the same proofs
    let h = Store::open(&path).unwrap();
    assert_eq!(proof_of(&h, kinds::BLOCK, alpha), Some(removed.apply));
}

/// The upload of `removed` with every placeholder's proof replaced by `proof`, which the header
/// lists as a prerequisite.
fn forged_upload(src: &Src, removed: &Removed, proof: Hash, name: &str) -> (PathBuf, ObjectKey) {
    let honest = upload_of(src, &removed.path, src.tag, &format!("{name}-honest"));
    let bytes = std::fs::read(&honest).unwrap();
    let (mut r, header) = StreamReader::open(&bytes[..]).unwrap();
    let mut keys = vec![];
    let mut records: BTreeMap<ObjectKey, StreamRecord> = BTreeMap::new();
    let mut placeholder = None;
    while let Some(rec) = r.next().unwrap() {
        let rec = match rec {
            StreamRecord::Dropped {
                key,
                original_len,
                residue,
                ..
            } => {
                placeholder.get_or_insert(key);
                StreamRecord::Dropped {
                    encoding: 0,
                    key,
                    original_len,
                    apply: proof,
                    residue,
                }
            }
            other => other,
        };
        keys.push(rec.key());
        records.insert(rec.key(), rec);
    }
    r.finish().unwrap();
    let mut prerequisites = header.prerequisites.clone();
    prerequisites.push(key(kinds::REDACTION_APPLY, proof));
    prerequisites.sort();
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        src.repo,
        src.tag,
        prerequisites,
    );
    let file = src.dir.path().join(format!("{name}.rdysync"));
    write_batch(&mut records, &header, &keys, File::create(&file).unwrap()).unwrap();
    (file, placeholder.expect("a placeholder"))
}

#[test]
fn hc3_take_refuses_a_placeholder_naming_a_carried_record() {
    let src = source();
    let removed = removed_copy(&src);
    let (path, rec) = carried_holder(&src, "h", removed.request, removed.apply, true, false);
    let carried = carried_fixture::carried_hash(&rec);
    let (forged, placeholder) = forged_upload(&src, &removed, carried, "forged");
    let mut h = Store::open(&path).unwrap();
    let before = (segment_bytes(&path), h.log_end());
    let e = stream_err(take(&mut h, &open(&[forged]), &src.target()).unwrap_err());
    assert_eq!(e.code, "E_BAD_RECORD");
    assert!(
        e.message.ends_with("invalid removal proof"),
        "{}",
        e.message
    );
    assert_eq!(e.object, Some(placeholder));
    // nothing was written, and the held payloads are still held
    assert_eq!(h.reader().carried_counts(&rec).unwrap(), (2, 0));
    let end = h.log_end();
    drop(h);
    assert_eq!((segment_bytes(&path), end), before);
}

#[test]
fn hc3_a_placeholder_naming_an_invalid_carried_record_is_refused() {
    let src = source();
    let removed = removed_copy(&src);
    // the holder's carried record explains zeros; it is still no removal proof for a take
    let (path, rec) = carried_holder(
        &src,
        "h",
        Hash::of(b"another request"),
        Hash::of(b"another apply"),
        false,
        false,
    );
    let carried = carried_fixture::carried_hash(&rec);
    let (forged, placeholder) = forged_upload(&src, &removed, carried, "forged");
    let mut h = Store::open(&path).unwrap();
    let before = segment_bytes(&path);
    let e = stream_err(take(&mut h, &open(&[forged]), &src.target()).unwrap_err());
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_BAD_RECORD",
            "stream record 0 at byte 0: invalid removal proof",
            "request this stream again; run rdy fsck at its source"
        )
    );
    assert_eq!(e.object, Some(placeholder));
    drop(h);
    assert_eq!(segment_bytes(&path), before);
}

/// A project whose one landing is tagged `v1` by a TAG written through `Store::put` (so in the
/// TAG kind's latest encoding), and one batch file of the tag's whole closure whose header
/// carries `features`. Returns the base directory, the project path, its repo, the landing, the
/// tag and the batch file.
fn put_tag_upload(
    features: rdy_store::format::StoreFeatures,
) -> (tempfile::TempDir, PathBuf, RepoId, Hash, Hash, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("src");
    let mut s = Store::create(&path).unwrap();
    let cid = s
        .commit_to_view(
            "work",
            change(
                "work",
                vec![Op::Create {
                    block: block("b-00001", "alpha"),
                }],
            ),
        )
        .unwrap();
    let proposal = s
        .propose_changes(
            "work",
            "first",
            Some("the first version"),
            prov("agent:w", Role::Writer),
            vec![cid],
            None,
        )
        .unwrap();
    let landing = s
        .land(&proposal, prov("human:alice", Role::Approver))
        .unwrap();
    let tag = s
        .put(
            kinds::TAG,
            &Tag {
                name: "v1".into(),
                landing,
                message: None,
                tagged_by: prov("human:alice", Role::Approver),
            },
        )
        .unwrap();
    let repo = s.repo_id().unwrap().unwrap();
    let r = s.reader();
    let c = closure(&r, tag, &BTreeSet::new()).unwrap();
    let file = dir.path().join("v1.rdysync");
    let header = StreamHeader::tag_history(features, repo, tag, c.prerequisites);
    let mut source = StoreSource {
        reader: &r,
        end: END_OF_LOG,
    };
    write_batch(
        &mut source,
        &header,
        &c.records,
        File::create(&file).unwrap(),
    )
    .unwrap();
    drop((r, s));
    (dir, path, repo, landing, tag, file)
}

fn manifest_of(s: &Store) -> rdy_store::format::StoreFeatures {
    rdy_store::format::StoreFeatures::parse(
        &s.get_ref(rdy_store::format::REF_STORE_FEATURES)
            .unwrap()
            .expect("the features ref"),
    )
    .unwrap()
}

#[test]
fn ok1_a_versioned_frame_is_taken_with_its_raw_kind() {
    use rdy_store::codec;
    let _v1 = codec::test_encoding(kinds::TAG, 1);
    let (dir, path, repo, landing, tag, file) =
        put_tag_upload(rdy_store::format::baseline_features());
    let (src_kind, src_payload) = Store::open(&path).unwrap().get_raw(&tag).unwrap();
    assert_eq!(
        src_kind,
        kinds::TAG | codec::VERSIONED,
        "the source's raw kind"
    );
    assert_eq!(&src_payload[..2], &[1, 0]);
    let hpath = dir.path().join("h");
    let mut h = Store::create_holder(&hpath, repo).unwrap();
    let target = UploadTarget {
        tag: "v1".into(),
        landing,
    };
    take(&mut h, &open(&[file]), &target).unwrap();
    // the holder keeps the frame's raw kind and exact bytes, and reads it as a TAG
    let check = |h: &Store| {
        assert_eq!(
            h.get_raw(&tag).unwrap(),
            (src_kind, src_payload.clone()),
            "the raw frame as the source stored it"
        );
        let t: Tag = h.get_typed(kinds::TAG, &tag).unwrap();
        assert_eq!((t.name.as_str(), t.landing), ("v1", landing));
        assert_eq!(h.get_ref("tag/v1").unwrap(), Some(tag.0.to_vec()));
        h.check_meta().unwrap();
        h.check_holder().unwrap();
    };
    check(&h);
    h.reindex().unwrap();
    check(&h);
    drop(h);
    let h = Store::open(&hpath).unwrap();
    check(&h);
    assert!(h.verify().unwrap() > 0);
}

#[test]
fn ok1_take_refuses_a_stream_this_holder_may_only_read() {
    use rdy_store::format::{FeatureClass, StoreFeature, StoreFeatures};
    let read_only = StoreFeatures::of(vec![StoreFeature {
        name: "future_read_only".into(),
        class: FeatureClass::ReadOnlyCompatible,
        required_rdy: "99.0.0".into(),
    }]);
    let (dir, _path, repo, landing, _tag, file) = put_tag_upload(read_only);
    let hpath = dir.path().join("h");
    let mut h = Store::create_holder(&hpath, repo).unwrap();
    let end = h.log_end();
    let before = segment_bytes(&hpath);
    let format_before = std::fs::read(hpath.join(rdy_store::format::FEATURES_FILE)).unwrap();
    let target = UploadTarget {
        tag: "v1".into(),
        landing,
    };
    let e = stream_err(take(&mut h, &open(&[file]), &target).unwrap_err());
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_STORE_FORMAT",
            "store feature future_read_only needs rdy 99.0.0",
            "open the project with rdy 99.0.0 or later"
        )
    );
    assert_eq!(h.log_end(), end, "nothing appended");
    assert_eq!(manifest_of(&h), rdy_store::format::baseline_features());
    drop(h);
    assert_eq!(segment_bytes(&hpath), before);
    assert_eq!(
        std::fs::read(hpath.join(rdy_store::format::FEATURES_FILE)).unwrap(),
        format_before
    );
}

#[test]
fn ok1_take_keeps_the_stream_features_and_activates_first_use() {
    use rdy_store::format::{
        FEATURES_FILE, FeatureClass, StoreFeature, StoreFeatures, TEST_FEATURE, inject_test_feature,
    };
    // an unknown compatible feature of the source, and the test feature that a TAG uses
    let future = StoreFeature {
        name: "zz_future".into(),
        class: FeatureClass::Compatible,
        required_rdy: "99.1.0".into(),
    };
    let _known = inject_test_feature(FeatureClass::Compatible, kinds::TAG);
    let (dir, _path, repo, landing, tag, file) =
        put_tag_upload(StoreFeatures::of(vec![future.clone()]));
    let hpath = dir.path().join("h");
    let mut h = Store::create_holder(&hpath, repo).unwrap();
    let end = h.log_end();
    let target = UploadTarget {
        tag: "v1".into(),
        landing,
    };
    let got = take(&mut h, &open(&[file]), &target).unwrap();
    let want = rdy_store::format::baseline_features().merged(&[
        StoreFeature {
            name: TEST_FEATURE.into(),
            class: FeatureClass::Compatible,
            required_rdy: "99.0.0".into(),
        },
        future,
    ]);
    assert_eq!(
        manifest_of(&h),
        want,
        "the stream's and the first use's features"
    );
    assert_eq!(h.features(), &want);
    assert_eq!(
        std::fs::read_to_string(hpath.join(FEATURES_FILE)).unwrap(),
        want.to_text(),
        "FORMAT mirrors the log"
    );
    // one sealed batch: the closure, the tag ref and the features ref
    assert_eq!(markers_since(&h, end), 1);
    assert_eq!(got.frames, got.new_objects + 2);
    assert!(h.has_kind(kinds::TAG, &tag).unwrap());
    h.check_meta().unwrap();
    // a repeat activates nothing more
    drop(h);
    let h = Store::open(&hpath).unwrap();
    assert_eq!(manifest_of(&h), want);
}

/// Fix round 3: a take whose batch is synced and whose index update then fails is rolled back
/// only if the truncate works. When the truncate fails too, the marked batch stays in the log,
/// so `FORMAT` keeps the manifest the take published and the holder reopens with it.
#[test]
fn ok1_a_take_that_cannot_roll_back_keeps_format() {
    use rdy_store::format::{
        FEATURES_FILE, FeatureClass, StoreFeature, StoreFeatures, TEST_FEATURE, inject_test_feature,
    };
    let future = StoreFeature {
        name: "zz_future".into(),
        class: FeatureClass::Compatible,
        required_rdy: "99.1.0".into(),
    };
    let _known = inject_test_feature(FeatureClass::Compatible, kinds::TAG);
    let (dir, _path, repo, landing, _tag, file) =
        put_tag_upload(StoreFeatures::of(vec![future.clone()]));
    let hpath = dir.path().join("h");
    let mut h = Store::create_holder(&hpath, repo).unwrap();
    let target = UploadTarget {
        tag: "v1".into(),
        landing,
    };
    let want = rdy_store::format::baseline_features().merged(&[
        StoreFeature {
            name: TEST_FEATURE.into(),
            class: FeatureClass::Compatible,
            required_rdy: "99.0.0".into(),
        },
        future,
    ]);
    h.fail_next_index_apply();
    h.fail_next_rollback();
    assert!(take(&mut h, &open(&[file]), &target).is_err());
    assert_eq!(
        std::fs::read_to_string(hpath.join(FEATURES_FILE)).unwrap(),
        want.to_text(),
        "FORMAT keeps the published manifest"
    );
    drop(h);
    let h = Store::open(&hpath).expect("the holder reopens");
    assert_eq!(manifest_of(&h), want, "the marked batch is the log's");
    assert_eq!(h.features(), &want);
}

/// `n` unknown compatible features named `{prefix}000` on.
fn unknown_features(prefix: &str, n: usize) -> Vec<rdy_store::format::StoreFeature> {
    (0..n)
        .map(|i| rdy_store::format::StoreFeature {
            name: format!("{prefix}{i:03}"),
            class: rdy_store::format::FeatureClass::Compatible,
            required_rdy: "1.0.0".into(),
        })
        .collect()
}

/// D-WWAK7: the manifest a take would leave (the holder's, the stream header's and the take's
/// first uses) keeps the manifest's bounds. A union of more than 256 features is refused before
/// `FORMAT` or the log changes, whichever parts make it up; one that reaches the bound lands.
#[test]
fn ok1_take_refuses_a_merged_manifest_over_its_bounds() {
    use rdy_store::format::{
        FEATURES_FILE, FeatureClass, REF_STORE_FEATURES, inject_test_feature, write_format_file,
    };
    // the TAG of every upload first uses the test feature
    let _known = inject_test_feature(FeatureClass::Compatible, kinds::TAG);
    // (holder's unknown features, header's unknown features, refused): each manifest also
    // names the two baseline features (ok4), and the take first uses the test feature, so the
    // merged manifest holds 3 more than the two counts
    let cases: Vec<(usize, usize, bool)> = vec![
        (0, 254, true),   // a full header and the first use
        (200, 54, true),  // the holder and the header
        (253, 1, true),   // the holder, the header and the first use
        (128, 126, true), // half and half, and the first use
        (252, 1, false),  // exactly the bound
        (0, 253, false),  // exactly the bound
    ];
    for (held, carried, refused) in cases {
        let case = format!("holder {held}, header {carried}");
        let header = rdy_store::format::baseline_features().merged(&unknown_features("h", carried));
        let (dir, _path, repo, landing, tag, file) = put_tag_upload(header);
        let hpath = dir.path().join("h");
        let mut h = Store::create_holder(&hpath, repo).unwrap();
        let mine = rdy_store::format::baseline_features().merged(&unknown_features("a", held));
        if held > 0 {
            h.set_refs_unchecked_for_tests(vec![(REF_STORE_FEATURES.into(), mine.to_json())])
                .unwrap();
            write_format_file(&hpath, &mine).unwrap();
            drop(h);
            h = Store::open(&hpath).unwrap();
        }
        assert_eq!(h.features(), &mine, "{case}");
        let end = h.log_end();
        let before = segment_bytes(&hpath);
        let format_before = std::fs::read(hpath.join(FEATURES_FILE)).unwrap();
        let target = UploadTarget {
            tag: "v1".into(),
            landing,
        };
        let got = take(&mut h, &open(&[file]), &target);
        if !refused {
            got.unwrap();
            assert_eq!(manifest_of(&h).features.len(), 256, "{case}");
            assert_eq!(h.features(), &manifest_of(&h), "{case}");
            assert!(h.has_kind(kinds::TAG, &tag).unwrap(), "{case}");
            drop(h);
            Store::open(&hpath).unwrap().check_meta().unwrap();
            continue;
        }
        let e = stream_err(got.unwrap_err());
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (
                "E_STORE_FORMAT",
                "store feature manifest is invalid: more than 256 features",
                "restore the store from a backup"
            ),
            "{case}"
        );
        assert_eq!(h.log_end(), end, "{case}: nothing appended");
        assert_eq!(h.features(), &mine, "{case}");
        assert!(!h.has_kind(kinds::TAG, &tag).unwrap(), "{case}");
        drop(h);
        assert_eq!(segment_bytes(&hpath), before, "{case}");
        assert_eq!(
            std::fs::read(hpath.join(FEATURES_FILE)).unwrap(),
            format_before,
            "{case}"
        );
        let h = Store::open(&hpath).unwrap();
        assert_eq!(manifest_of(&h), mine, "{case}");
        h.check_meta().unwrap();
    }
}

/// The batch markers of `h`'s log from `from` on.
fn markers_since(h: &Store, from: rdy_store::segment::Location) -> usize {
    log_frames(h)
        .iter()
        .filter(|(l, k, _, _)| *l >= from && *k == kinds::BATCH_COMMIT)
        .count()
}
