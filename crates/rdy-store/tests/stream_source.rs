//! hpb5 (D-D2NPN, D-48NYM; hub r-7GS51): a checked record source without a store. One sync batch
//! is checked whole by the take's own completeness checks, with nothing held, and then reads as
//! a record source: the take and the checked stream refuse the same malformed batches with the
//! same errors, and see the same facts of a valid one.
use rdy_model::signature::Signature;
use rdy_model::*;
use rdy_store::closure::{END_OF_LOG, StoreSource, closure, write_batch};
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::stream_error::StreamError;
use rdy_store::stream_source::{CheckedStream, check_complete};
use rdy_store::sync_proof::ObjectSource;
use rdy_store::sync_stream::{StreamHeader, StreamReader, StreamRecord};
use rdy_store::take_batch::{TagChoice, TakeError, UploadFacts, UploadTarget};
use rdy_store::{Store, StoreReader};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Cursor, Seek, SeekFrom, Write};
use std::path::PathBuf;

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

fn answer(item: &str) -> Answer {
    Answer {
        item: item.into(),
        act: AnswerAct::Tag,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets: vec![],
        person: prov("human:alice", Role::Approver),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: None,
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

/// A source project: one change of two blocks, landed, tagged `v1` with its relay answer, with
/// a signature of the landing and one of the tag.
struct Src {
    dir: tempfile::TempDir,
    path: PathBuf,
    repo: RepoId,
    landing: Hash,
    tag: Hash,
}

fn source() -> Src {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("src");
    let mut s = Store::create(&path).unwrap();
    s.commit_to_view(
        "work",
        change(
            "work",
            vec![
                Op::Create {
                    block: block("b-00001", "alpha"),
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
    let (tag, _relay) = s
        .put_tag_answered(
            &Tag {
                name: "v1".into(),
                landing,
                message: None,
                tagged_by: prov("human:alice", Role::Approver),
            },
            answer("tag:v1"),
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

    /// Every record of the tag's closure, by key, and the closure's order.
    fn records(&self) -> (BTreeMap<ObjectKey, StreamRecord>, Vec<ObjectKey>) {
        let (_s, r) = self.reader();
        let c = closure(&r, self.tag, &BTreeSet::new()).unwrap();
        let mut source = StoreSource {
            reader: &r,
            end: END_OF_LOG,
        };
        let map = c
            .records
            .iter()
            .map(|k| (*k, source.record(*k).unwrap().unwrap()))
            .collect();
        (map, c.records)
    }

    /// The tag's closure as one batch less the keys of `known`, with its exact prerequisites.
    fn batch_without(&self, known: &BTreeSet<ObjectKey>) -> Vec<u8> {
        let (_s, r) = self.reader();
        let c = closure(&r, self.tag, known).unwrap();
        let header = StreamHeader::tag_history(
            rdy_store::format::baseline_features(),
            self.repo,
            self.tag,
            c.prerequisites,
        );
        let mut source = StoreSource {
            reader: &r,
            end: END_OF_LOG,
        };
        write_batch(&mut source, &header, &c.records, Vec::new())
            .unwrap()
            .0
    }

    fn batch(&self) -> Vec<u8> {
        self.batch_without(&BTreeSet::new())
    }

    /// `records` (in that order) as one batch rooted at the tag, with no prerequisites.
    fn batch_of(
        &self,
        records: &BTreeMap<ObjectKey, StreamRecord>,
        order: &[ObjectKey],
    ) -> Vec<u8> {
        let header = StreamHeader::tag_history(
            rdy_store::format::baseline_features(),
            self.repo,
            self.tag,
            vec![],
        );
        let mut source = records.clone();
        write_batch(&mut source, &header, order, Vec::new())
            .unwrap()
            .0
    }

    /// `bytes` as a file of the test directory, opened for reading.
    fn file(&self, name: &str, bytes: &[u8]) -> File {
        let p = self.dir.path().join(format!("{name}.rdysync"));
        std::fs::write(&p, bytes).unwrap();
        File::open(&p).unwrap()
    }
}

/// The take of `bytes` into a new holder: the facts its check saw, or its stream refusal.
fn take(src: &Src, name: &str, bytes: &[u8]) -> Result<UploadFacts, StreamError> {
    let path = src.dir.path().join(format!("holder-{name}"));
    let mut h = Store::create_holder(&path, src.repo).unwrap();
    let seen: RefCell<Option<UploadFacts>> = RefCell::new(None);
    let file = src.file(name, bytes);
    let got = h.take_batches(&[file], &src.target(), |_, facts| {
        *seen.borrow_mut() = Some(facts.clone());
        Ok::<_, StreamError>(TagChoice::Incoming)
    });
    match got {
        Ok(_) => Ok(seen.into_inner().expect("the check ran")),
        Err(TakeError::Stream(e)) => Err(e),
        Err(other) => panic!("{name}: not a stream refusal: {other:?}"),
    }
}

/// The checked stream of `bytes`: its facts, or its refusal.
fn open(bytes: &[u8]) -> Result<UploadFacts, StreamError> {
    CheckedStream::open(Cursor::new(bytes.to_vec())).map(|s| s.facts().clone())
}

/// Frames `frames` (kind and payload) under a header of `prerequisites`, rooted at `root`, as raw
/// bytes with a correct trailer, whatever the batch rules say of them.
fn raw(
    repo: &RepoId,
    root: Hash,
    prerequisites: &[ObjectKey],
    frames: &[(u8, Vec<u8>)],
) -> Vec<u8> {
    let mut out = b"RDYSYNC\n".to_vec();
    out.extend_from_slice(&2u32.to_le_bytes());
    let json = rdy_store::format::baseline_features().to_json();
    let mut header = (json.len() as u32).to_le_bytes().to_vec();
    header.extend_from_slice(&json);
    header.extend_from_slice(&repo.0);
    header.push(0);
    header.extend_from_slice(&root.0);
    header.extend_from_slice(&(prerequisites.len() as u32).to_le_bytes());
    for k in prerequisites {
        header.push(k.kind);
        header.extend_from_slice(&k.hash.0);
    }
    let mut chain = Vec::new();
    fn frame(out: &mut Vec<u8>, chain: Option<&mut Vec<u8>>, kind: u8, p: &[u8]) {
        let h = Hash::of(p);
        if let Some(chain) = chain {
            chain.push(kind);
            chain.extend_from_slice(&(p.len() as u64).to_le_bytes());
            chain.extend_from_slice(&h.0);
        }
        out.extend_from_slice(&(p.len() as u32).to_le_bytes());
        out.push(kind);
        out.extend_from_slice(&h.0);
        out.extend_from_slice(p);
    }
    frame(&mut out, Some(&mut chain), 0xF0, &header);
    for (kind, p) in frames {
        frame(&mut out, Some(&mut chain), *kind, p);
    }
    let mut trailer = (frames.len() as u64).to_le_bytes().to_vec();
    trailer.extend_from_slice(&(out.len() as u64).to_le_bytes());
    trailer.extend_from_slice(&Hash::of(&chain).0);
    frame(&mut out, None, 0xFF, &trailer);
    out
}

#[test]
fn hpb5_checked_stream_refuses_what_a_take_refuses() {
    let src = source();
    let (records, order) = src.records();
    // a valid batch: the same facts through both entry points
    let good = src.batch();
    let taken = take(&src, "good", &good).unwrap();
    let checked = open(&good).unwrap();
    assert_eq!(checked, taken);
    assert_eq!(checked.root_hash, src.tag);
    assert_eq!(checked.root.name, "v1");
    assert_eq!(
        checked.objects,
        order
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    );
    assert_eq!(checked.relay_answers.len(), 1);

    let block_key = *order.iter().find(|k| k.kind == kinds::BLOCK).unwrap();
    let landing_key = key(kinds::LANDING, src.landing);
    let mut corpus: Vec<(&str, Vec<u8>)> = vec![];
    // a missing dependency: a record the others need is only a prerequisite
    corpus.push((
        "missing block",
        src.batch_without(&BTreeSet::from([block_key])),
    ));
    corpus.push((
        "missing two",
        src.batch_without(&BTreeSet::from([block_key, landing_key])),
    ));
    // an unused record: an object no link or attachment reaches
    let mut orphaned = records.clone();
    let stray = StreamRecord::Present {
        encoding: 0,
        key: key(kinds::BLOB_CHUNK, Hash::of(b"stray chunk")),
        payload: b"stray chunk".to_vec(),
    };
    orphaned.insert(stray.key(), stray.clone());
    let mut with_stray = order.clone();
    with_stray.push(stray.key());
    corpus.push(("orphan", src.batch_of(&orphaned, &with_stray)));
    // a wrong hash: one payload byte flipped (the last record's, before the trailer)
    let mut flipped = good.clone();
    let at = flipped.len() - 86;
    flipped[at] ^= 1;
    corpus.push(("hash", flipped));
    // a bad trailer, a cut file and trailing bytes
    let mut cut = good.clone();
    cut.pop();
    corpus.push(("cut", cut));
    let mut trailing = good.clone();
    trailing.push(0);
    corpus.push(("trailing", trailing));
    let mut bad_trailer = good.clone();
    let n = bad_trailer.len();
    bad_trailer[n - 40] ^= 1;
    corpus.push(("trailer", bad_trailer));
    // a repeated key with other bytes cannot be framed by the writer; framed raw, it is refused
    let frames: Vec<(u8, Vec<u8>)> = order
        .iter()
        .map(|k| match &records[k] {
            StreamRecord::Present { payload, .. } => (k.kind, payload.clone()),
            other => panic!("a present record: {other:?}"),
        })
        .collect();
    let mut repeated = frames.clone();
    repeated.push(frames[0].clone());
    corpus.push(("repeated", raw(&src.repo, src.tag, &[], &repeated)));
    // an absent root that is no prerequisite
    let no_root: Vec<(u8, Vec<u8>)> = frames
        .iter()
        .filter(|(k, _)| *k != kinds::TAG && *k != kinds::ANSWER)
        .cloned()
        .collect();
    corpus.push(("no root", raw(&src.repo, src.tag, &[], &no_root)));
    for (name, bytes) in &corpus {
        let by_take = take(&src, name, bytes).unwrap_err();
        let by_stream = open(bytes).unwrap_err();
        assert_eq!(by_stream, by_take, "{name}");
    }
    // the refusals name what they say they do
    let missing = open(&corpus[0].1).unwrap_err();
    assert_eq!(
        (missing.code, missing.message.clone()),
        (
            "E_UNRESOLVED_REF",
            format!("stream prerequisite {block_key} is missing")
        )
    );
    // two missing: the first in key order
    let first = block_key.min(landing_key);
    let two = open(&corpus[1].1).unwrap_err();
    assert_eq!(
        two.message,
        format!("stream prerequisite {first} is missing")
    );
    let orphan = open(&corpus[2].1).unwrap_err();
    assert_eq!(
        (orphan.code, orphan.message.as_str(), orphan.object),
        (
            "E_BAD_RECORD",
            "stream record 0 at byte 0: object is outside the tag closure",
            Some(stray.key())
        )
    );
}

#[test]
fn hpb5_check_complete_reads_any_source() {
    let src = source();
    let (records, order) = src.records();
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        src.repo,
        src.tag,
        vec![],
    );
    let mut map = records.clone();
    let facts = check_complete(&mut map, std::slice::from_ref(&header), &order).unwrap();
    assert_eq!(facts, open(&src.batch()).unwrap());
    // a record the source holds that is not incoming is never required to be reached
    let held = StreamRecord::Present {
        encoding: 0,
        key: key(kinds::BLOB_CHUNK, Hash::of(b"held chunk")),
        payload: b"held chunk".to_vec(),
    };
    map.insert(held.key(), held.clone());
    assert_eq!(
        check_complete(&mut map, std::slice::from_ref(&header), &order).unwrap(),
        facts
    );
    // incoming, it is outside the closure
    let mut incoming = order.clone();
    incoming.push(held.key());
    let e = check_complete(&mut map, std::slice::from_ref(&header), &incoming).unwrap_err();
    assert_eq!(
        e.message,
        "stream record 0 at byte 0: object is outside the tag closure"
    );
    assert_eq!(e.object, Some(held.key()));
    // a prerequisite the source holds is met; one it lacks is missing
    let block_key = *order.iter().find(|k| k.kind == kinds::BLOCK).unwrap();
    let with_pre = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        src.repo,
        src.tag,
        vec![block_key],
    );
    let rest: Vec<ObjectKey> = order.iter().filter(|k| **k != block_key).copied().collect();
    let got = check_complete(&mut map, std::slice::from_ref(&with_pre), &rest).unwrap();
    assert_eq!(got.objects, {
        let mut v = rest.clone();
        v.sort();
        v
    });
    let mut lacking = records.clone();
    lacking.remove(&block_key);
    let e = check_complete(&mut lacking, std::slice::from_ref(&with_pre), &rest).unwrap_err();
    assert_eq!(
        e.message,
        format!("stream prerequisite {block_key} is missing")
    );
    // no header is no upload
    let e = check_complete(&mut map, &[], &order).unwrap_err();
    assert_eq!(e.message, "stream record 0 at byte 0: no batch files");
}

#[test]
fn hpb5_checked_stream_reads_each_record_again() {
    let src = source();
    let (records, order) = src.records();
    let bytes = src.batch();
    let mut stream = CheckedStream::open(Cursor::new(bytes.clone())).unwrap();
    for k in &order {
        assert!(stream.holds(k));
        assert_eq!(stream.read(*k).unwrap().as_ref(), Some(&records[k]), "{k}");
        assert_eq!(
            stream.record(*k).unwrap().as_ref(),
            Some(&records[k]),
            "{k}"
        );
        assert_eq!(stream.redaction_of(k), None);
    }
    let absent = key(kinds::BLOCK, Hash::of(b"absent"));
    assert!(!stream.holds(&absent));
    assert_eq!(stream.read(absent).unwrap(), None);
    // the tagged state: the landing's snapshot and its two blocks
    let (snapshot, blocks) = stream.materialize_tag().unwrap();
    assert_eq!(snapshot.blocks.len(), 2);
    let ids: Vec<&str> = blocks.keys().map(|b| b.as_str()).collect();
    assert_eq!(ids, ["b-00001", "b-00002"]);
    assert_eq!(blocks[&BlockId::parse("b-00001").unwrap()].body, "alpha");
    for (id, v) in &snapshot.blocks {
        assert_eq!(blocks[id].version_hash(), *v);
    }
    // input changed after the check is refused at the read, naming the record
    let path = src.dir.path().join("changed.rdysync");
    std::fs::write(&path, &bytes).unwrap();
    let stream = CheckedStream::open(File::open(&path).unwrap()).unwrap();
    let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    let at = bytes.len() as u64 - 86;
    f.seek(SeekFrom::Start(at)).unwrap();
    f.write_all(&[bytes[at as usize] ^ 1]).unwrap();
    drop(f);
    let last = {
        let (mut r, _) = StreamReader::open(Cursor::new(bytes.clone())).unwrap();
        let mut last = None;
        while let Some(rec) = r.next().unwrap() {
            last = Some(rec.key());
        }
        last.unwrap()
    };
    let e = stream.read(last).unwrap_err();
    assert!(
        e.message.ends_with("input changed during take"),
        "{}",
        e.message
    );
    assert_eq!(e.object, Some(last));
}
