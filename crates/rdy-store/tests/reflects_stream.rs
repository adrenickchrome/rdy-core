//! rf4 (D-4WC6R, D-D2NPN verify-on-receive, quality-gate tb-A4DAV): a change with the appended
//! `Justification::Reflects` travels in a sync batch of store format 18 under its own hash, and a
//! hostile one is refused by the one bounded decoder: an oversized source count before anything
//! is built for it, an unassigned variant tag and a truncated payload as records that do not
//! decode. Nothing panics.
use rdy_model::*;
use rdy_store::bounded::{self, COLLECTION_ELEMENTS, RecordError};
use rdy_store::format::LAST_SCALAR_STORE_FORMAT;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::stream_error::StreamError;
use rdy_store::sync_stream::{StreamHeader, StreamRecord, StreamWriter, check_batch};
use std::collections::BTreeMap;

const REPO: RepoId = RepoId([7; 16]);

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// postcard's varint of `n`.
fn varint(mut n: u64) -> Vec<u8> {
    let mut out = vec![];
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

fn claim() -> Block {
    Block {
        id: id("c-00001"),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: "The claim reflects its source.".into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn change(j: Justification) -> Change {
    Change {
        view: "work".into(),
        ops: vec![Op::Create { block: claim() }],
        deps: vec![],
        provenance: Provenance {
            actor: "agent:a".into(),
            run: None,
            role: Role::Writer,
            at_ms: 1,
        },
        justification: Some(j),
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

fn reflects() -> Justification {
    Justification::Reflects {
        sources: vec![id("S-00001")],
        code: true,
    }
}

fn raw(kind: u8, payload: &[u8]) -> StreamRecord {
    StreamRecord::Present {
        encoding: 0,
        key: ObjectKey {
            kind,
            hash: Hash::of(payload),
        },
        payload: payload.to_vec(),
    }
}

/// A batch of the claim's block, then the change with `payload`, rooted at a held tag.
fn batch(payload: &[u8]) -> Vec<u8> {
    let root = Hash([4; 32]);
    let header = StreamHeader::tag_history(
        rdy_store::format::baseline_features(),
        REPO,
        root,
        vec![ObjectKey {
            kind: kinds::TAG,
            hash: root,
        }],
    );
    let mut w = StreamWriter::new(Vec::new(), &header).unwrap();
    w.record(&raw(
        kinds::BLOCK,
        &postcard::to_allocvec(&claim()).unwrap(),
    ))
    .unwrap();
    w.record(&raw(kinds::CHANGE, payload)).unwrap();
    w.finish().unwrap().0
}

/// The bytes of `change(reflects())` with its source count replaced by `count`, and nothing
/// after it: the count's hint is what a decoder sees before it reads any source.
fn hostile_change(count: u64) -> Vec<u8> {
    let good = postcard::to_allocvec(&change(reflects())).unwrap();
    // the justification's bytes are 01 (Some), 02 (Reflects), 01 (one source), the id, 01 (code)
    let source = postcard::to_allocvec(&id("S-00001")).unwrap();
    let mut tail = vec![1u8, 2, 1];
    tail.extend_from_slice(&source);
    tail.push(1);
    let at = good
        .windows(tail.len())
        .position(|w| w == tail.as_slice())
        .expect("the justification's bytes");
    let mut out = good[..at + 2].to_vec();
    out.extend(varint(count));
    out
}

fn stream_refusal(bytes: &[u8]) -> StreamError {
    check_batch(bytes).unwrap_err()
}

#[test]
fn rf4_hostile_reflects_decode_is_bounded() {
    assert_eq!(LAST_SCALAR_STORE_FORMAT, 18);
    let good = postcard::to_allocvec(&change(reflects())).unwrap();
    assert_eq!(
        bounded::decode::<Change>(&good).unwrap(),
        change(reflects())
    );
    // a valid batch: the change checks under its own hash
    let ok = check_batch(batch(&good).as_slice()).unwrap();
    assert_eq!(ok.keys.len(), 2);
    assert_eq!(ok.keys[1].hash, change(reflects()).id());
    // an oversized source count: refused at the hint. A visitor that preallocated for 2^40
    // block ids (24 TiB) would abort the process; the call returns the limit instead.
    let limit = RecordError::Limit {
        name: COLLECTION_ELEMENTS,
        maximum: 100_000,
    };
    assert_eq!(limit.to_string(), "100000 collection elements");
    for count in [100_001u64, 1 << 40, u64::from(u32::MAX)] {
        let mut j = vec![2u8];
        j.extend(varint(count));
        assert_eq!(bounded::decode::<Justification>(&j), Err(limit), "{count}");
        assert_eq!(
            bounded::decode::<Change>(&hostile_change(count)),
            Err(limit),
            "{count}"
        );
        let e = stream_refusal(&batch(&hostile_change(count)));
        assert!(
            e.message.ends_with("100000 collection elements"),
            "{count}: {e:?}"
        );
    }
    // within the bound but past the bytes left: no source is there, so the record does not
    // decode
    assert_eq!(
        bounded::decode::<Change>(&hostile_change(100_000)),
        Err(RecordError::Decode)
    );
    // an unassigned variant tag (3) and a truncated payload do not decode
    let mut tag = good.clone();
    let source = postcard::to_allocvec(&id("S-00001")).unwrap();
    let mut j = vec![1u8, 2, 1];
    j.extend_from_slice(&source);
    let at = tag
        .windows(j.len())
        .position(|w| w == j.as_slice())
        .unwrap();
    tag[at + 1] = 3;
    assert_eq!(
        bounded::decode::<Change>(&tag),
        Err(RecordError::Decode),
        "an unassigned variant"
    );
    assert_eq!(RecordError::Decode.to_string(), "record does not decode");
    let truncated = &good[..good.len() - 1];
    assert_eq!(
        bounded::decode::<Change>(truncated),
        Err(RecordError::Decode)
    );
    let mut just = postcard::to_allocvec(&reflects()).unwrap();
    just.pop();
    assert_eq!(
        bounded::decode::<Justification>(&just),
        Err(RecordError::Decode)
    );
    for bad in [&tag[..], truncated] {
        let e = stream_refusal(&batch(bad));
        assert_eq!(e.code, "E_BAD_RECORD", "{e:?}");
        assert!(e.message.ends_with(": record does not decode"), "{e:?}");
    }
}
