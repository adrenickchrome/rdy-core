//! rpl35 (repo links; D-FK58A, D-DAPR8): a hostile link delivery through the adapter's one
//! writer. Each bound of the stream is checked before the store reads or allocates for it: a
//! stream over 2147483648 bytes, a record over 16777216 bytes, a selection over 100000 versions
//! (stated or real), a text over 1048576 bytes, and a stream cut short. Each is refused and
//! writes nothing. A take reads the file the caller opened: a file put in its place at the same
//! path before the take is not read.
use rdy_adapter::{BlockId, DeliverySource, Hash, RdyError, RepoId, StoreHandle};
use rdy_model::Block;
use rdy_model::foreign::{ForeignDerived, ForeignPublication, ForeignVersion, selection_identity};
use rdy_store::codec;
use rdy_store::format::baseline_features;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::{
    DeliveryRecord, MAGIC, StreamHeader, StreamRecord, StreamWriter, rec,
};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

/// The repo the deliveries cite.
const CITED: RepoId = RepoId([7; 16]);
const PROFILE: &str = "pp-00001";
/// The bytes of a frame header: length, kind and hash.
const HEAD: usize = 37;
/// The bytes of the prelude: the magic and the stream format.
const PRELUDE: usize = 12;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// An empty project on disk, closed again.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    rdy_engine::Rdy::init(dir.path(), "rdy-standard")
        .unwrap()
        .close()
        .unwrap();
    dir
}

fn block(id_: &str, body: &str) -> Block {
    Block {
        id: id(id_),
        block_type: "decision".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn version(b: &Block) -> ForeignVersion {
    ForeignVersion {
        id: b.id.clone(),
        version: b.version_hash(),
        content: b.content_hash(),
        encoding: 0,
        derived: ForeignDerived::default(),
    }
}

fn delivery_of(tag: &str, versions: Vec<ForeignVersion>) -> DeliveryRecord {
    DeliveryRecord {
        publication: ForeignPublication {
            tag: tag.into(),
            profile: PROFILE.into(),
            profile_version: None,
        },
        selection: selection_identity(versions.iter().map(|v| (&v.id, &v.version))).unwrap(),
        versions,
        shells: vec![],
    }
}

fn header() -> StreamHeader {
    StreamHeader::link_delivery(baseline_features(), CITED, Hash::of(b"v1"))
}

/// A whole delivery stream of `blocks` (in id order), with tag `tag`.
fn stream(tag: &str, blocks: &[&Block]) -> Vec<u8> {
    let d = delivery_of(tag, blocks.iter().map(|b| version(b)).collect());
    let mut w = StreamWriter::new_delivery(Vec::new(), &header(), &d).unwrap();
    for b in blocks {
        let o = codec::encode_new(kinds::BLOCK, *b);
        w.record(&StreamRecord::Present {
            key: ObjectKey {
                kind: kinds::BLOCK,
                hash: Hash::of(&o.payload),
            },
            encoding: o.version,
            payload: o.payload,
        })
        .unwrap();
    }
    w.finish().unwrap().0
}

/// The frames of a whole stream after its prelude, the trailer left out: (kind, payload).
fn frames_of(bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut out = vec![];
    let mut at = PRELUDE;
    while at + HEAD <= bytes.len() {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let kind = bytes[at + 4];
        if kind != rec::TRAILER {
            out.push((kind, bytes[at + HEAD..at + HEAD + len].to_vec()));
        }
        at += HEAD + len;
    }
    assert_eq!(at, bytes.len(), "whole frames");
    out
}

fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut f = (payload.len() as u32).to_le_bytes().to_vec();
    f.push(kind);
    f.extend_from_slice(&Hash::of(payload).0);
    f.extend_from_slice(payload);
    f
}

/// A stream of format 3 built by hand from `frames` (the header first) and a trailer that
/// states them, so only the bound under test is broken.
fn craft(frames: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&3u32.to_le_bytes());
    let mut chain = vec![];
    for (kind, p) in frames {
        out.extend(frame(*kind, p));
        chain.push(*kind);
        chain.extend_from_slice(&(p.len() as u64).to_le_bytes());
        chain.extend_from_slice(&Hash::of(p).0);
    }
    let mut trailer = ((frames.len() - 1) as u64).to_le_bytes().to_vec();
    trailer.extend_from_slice(&(out.len() as u64).to_le_bytes());
    trailer.extend_from_slice(&Hash::of(&chain).0);
    out.extend(frame(rec::TRAILER, &trailer));
    out
}

fn save(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn source() -> DeliverySource {
    DeliverySource {
        link: id("rl-00001"),
        site: "hub.example".into(),
        at_ms: 1_760_000_000_000,
        set: None,
    }
}

fn refused(e: RdyError, code: &str, message: &str, what: &str) {
    assert_eq!((e.code, &e.message[..]), (code, message), "{what}: {e:?}");
}

#[test]
fn rpl35_hostile_stream_stays_within_bounds() {
    let dir = project();
    let files = tempfile::tempdir().unwrap();
    let (a, b, c) = (
        block("d-00001", "A"),
        block("d-00002", "B"),
        block("d-00003", "C"),
    );
    let handle = StoreHandle::open(dir.path()).unwrap();
    let base = save(files.path(), "base", &stream("v1", &[&a]));
    let first = handle
        .take_delivery(&File::open(&base).unwrap(), source())
        .unwrap();
    assert!(!first.unchanged);
    let take = |p: &Path| handle.take_delivery(&File::open(p).unwrap(), source());

    // a stream over 2147483648 bytes: refused by its length, before a byte is read
    let huge = files.path().join("huge");
    File::create(&huge)
        .unwrap()
        .set_len(2_147_483_648 + 1)
        .unwrap();
    refused(
        take(&huge).unwrap_err(),
        "E_LIMIT",
        "stream exceeds 2147483648 total bytes",
        "a stream over the byte bound",
    );
    std::fs::remove_file(&huge).unwrap();

    let whole = frames_of(&stream("v2", &[&a, &b]));
    let head = whole[0].clone();
    assert_eq!(head.0, rec::HEADER);
    assert_eq!(whole[1].0, rec::DELIVERY);

    // a DELIVERY frame that states one byte over the record bound: refused by its header
    let mut long = MAGIC.to_vec();
    long.extend_from_slice(&3u32.to_le_bytes());
    long.extend(frame(rec::HEADER, &head.1));
    long.extend_from_slice(&(16_777_216u32 + 1).to_le_bytes());
    long.push(rec::DELIVERY);
    long.extend_from_slice(&[0u8; 32]);
    refused(
        take(&save(files.path(), "long", &long)).unwrap_err(),
        "E_LIMIT",
        "stream exceeds 16777216 record bytes",
        "a record over the byte bound",
    );

    // 100001 selected versions, each real: refused by the count, before any other rule
    let many: Vec<ForeignVersion> = (0..100_001u32).map(|_| version(&a)).collect();
    let mut d = delivery_of("v2", vec![]);
    d.versions = many;
    let crafted = craft(&[
        head.clone(),
        (rec::DELIVERY, postcard::to_allocvec(&d).unwrap()),
    ]);
    refused(
        take(&save(files.path(), "many", &crafted)).unwrap_err(),
        "E_LIMIT",
        "stream exceeds 100000 collection elements",
        "100001 versions",
    );

    // a count of 4294967295 versions stated in a record of a few hundred bytes: refused by the
    // count, before the decoder allocates for it
    let mut d = delivery_of("v2", vec![version(&a)]);
    let one = postcard::to_allocvec(&d).unwrap();
    d.versions.clear();
    let none = postcard::to_allocvec(&d).unwrap();
    let at = none
        .iter()
        .zip(&one)
        .position(|(x, y)| x != y)
        .expect("the count differs");
    assert_eq!((none[at], one[at]), (0, 1), "the versions count");
    let mut stated = none[..at].to_vec();
    stated.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0x0F]);
    stated.extend_from_slice(&one[at + 1..]);
    assert!(stated.len() < 1024);
    let crafted = craft(&[head.clone(), (rec::DELIVERY, stated)]);
    refused(
        take(&save(files.path(), "stated", &crafted)).unwrap_err(),
        "E_LIMIT",
        "stream exceeds 100000 collection elements",
        "a stated count",
    );

    // a tag over the text bound
    let long_tag = "t".repeat(1_048_577);
    let d = delivery_of(&long_tag, vec![version(&a)]);
    let crafted = craft(&[
        head.clone(),
        (rec::DELIVERY, postcard::to_allocvec(&d).unwrap()),
        whole[2].clone(),
    ]);
    refused(
        take(&save(files.path(), "tag", &crafted)).unwrap_err(),
        "E_LIMIT",
        "stream exceeds 1048576 text bytes",
        "a tag over the text bound",
    );

    // a stream cut short, at every frame boundary and inside each frame
    let full = stream("v2", &[&a, &b]);
    let mut cuts = vec![PRELUDE - 1];
    let mut at = PRELUDE;
    while at + HEAD <= full.len() {
        let len = u32::from_le_bytes(full[at..at + 4].try_into().unwrap()) as usize;
        cuts.extend([at, at + HEAD - 1, at + HEAD + len / 2]);
        at += HEAD + len;
    }
    cuts.push(full.len() - 1);
    for cut in cuts {
        let e = take(&save(files.path(), "cut", &full[..cut])).unwrap_err();
        refused(
            e,
            "E_STREAM_TRUNCATED",
            "stream trailer is missing or does not match",
            &format!("a cut at byte {cut}"),
        );
    }

    // none of these wrote anything: the first delivery is still the source's last, at its end
    let same = take(&base).unwrap();
    assert!(same.unchanged);
    assert_eq!((same.entry, same.end), (first.entry, first.end));

    // a file put at the stream's path after the caller opened it is not what the take reads
    let path = save(files.path(), "next", &stream("v3", &[&a, &b, &c]));
    let opened = File::open(&path).unwrap();
    let other = save(files.path(), "other", &stream("v4", &[&b]));
    std::fs::rename(&other, &path).unwrap();
    let taken = handle.take_delivery(&opened, source()).unwrap();
    drop(opened);
    assert_eq!(
        (taken.added, taken.removed, taken.blocks_taken),
        (2, 0, 2),
        "the opened file's delivery: {taken:?}"
    );
    let view = handle
        .snapshot()
        .unwrap()
        .foreign_source_for_tests(&id("rl-00001"))
        .unwrap()
        .unwrap();
    assert_eq!(
        view.members.keys().cloned().collect::<Vec<_>>(),
        [a.id.clone(), b.id.clone(), c.id.clone()]
    );
    // the file now at the path is a delivery of its own
    let replaced = take(&path).unwrap();
    assert_eq!((replaced.added, replaced.removed), (0, 2), "{replaced:?}");
    handle.close().unwrap();
}
