//! dl1: the dump stream (format 1): header, project files, sealed batches copied byte for byte,
//! trailer; every record verified on read.
use rdy_model::Hash;
use rdy_store::dump::*;
use rdy_store::format::STORE_FORMAT;
use rdy_store::segment::Location;
use rdy_store::{Store, StoreError};

/// A store with three batches after its format batch, and its sealed batches.
fn store_and_batches() -> (tempfile::TempDir, Store, Vec<DumpBatch>) {
    let base = tempfile::tempdir().unwrap();
    let mut s = Store::create(&base.path().join("s")).unwrap();
    s.put_blob(b"one").unwrap();
    s.set_refs(vec![("view/a".into(), b"x".to_vec())]).unwrap();
    s.put_blob(&vec![7u8; 3 << 20]).unwrap(); // three chunks and a manifest in one batch
    let mut batches = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |b| {
            batches.push(b);
            Ok(())
        })
        .unwrap();
    (base, s, batches)
}

fn header(s: &Store) -> DumpHeader {
    DumpHeader {
        store_format: STORE_FORMAT,
        log_end: s.log_end(),
        rdy: "test".into(),
    }
}

fn stream(s: &Store, batches: &[DumpBatch], files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = DumpWriter::new(Vec::new(), &header(s)).unwrap();
    for (p, b) in files {
        w.file(p, b).unwrap();
    }
    for b in batches {
        w.batch(b).unwrap();
    }
    w.finish().unwrap().0
}

fn read_all(bytes: &[u8]) -> Result<Vec<DumpItem>, StoreError> {
    let (mut r, _, _) = DumpReader::open(bytes)?;
    let mut out = vec![];
    while let Some(i) = r.next()? {
        out.push(i);
    }
    Ok(out)
}

fn damaged(e: StoreError) -> String {
    match e {
        StoreError::Dump { what, .. } => what,
        other => panic!("not a damaged dump: {other}"),
    }
}

#[test]
fn dl1_for_each_batch_walks_every_sealed_batch_in_order() {
    let (_b, s, batches) = store_and_batches();
    assert_eq!(batches.len(), 4, "the format batch and three writes");
    assert_eq!(
        batches[0].start,
        Location {
            segment: 0,
            offset: 0
        }
    );
    let frames: usize = batches.iter().map(|b| b.frames.len()).sum();
    assert_eq!(frames, s.verify().unwrap(), "every data frame, no marker");
    for b in &batches {
        assert!(
            b.frames
                .iter()
                .all(|f| f.kind != rdy_store::kinds::BATCH_COMMIT)
        );
        assert!(b.frames.iter().all(|f| f.hash == Hash::of(&f.payload)));
    }
    assert!(batches.windows(2).all(|w| w[0].start < w[1].start));
}

#[test]
fn dl1_a_stream_round_trips_header_files_and_batches() {
    let (_b, s, batches) = store_and_batches();
    let files: [(&str, &[u8]); 3] = [
        (".rdy.yaml", b"pack: rdy-standard\n"),
        (".rdy/packs/x.yaml", b"name: x\n"),
        (".rdyignore", b"*.draft.yaml\n"),
    ];
    let bytes = stream(&s, &batches, &files);
    assert!(bytes.starts_with(b"RDYDUMP\n\x01\x00\x00\x00"));
    let (mut r, format, h) = DumpReader::open(&bytes[..]).unwrap();
    assert_eq!(
        (format, h.store_format, h.log_end),
        (1, STORE_FORMAT, s.log_end())
    );
    let mut got_files = vec![];
    let mut got_batches = vec![];
    while let Some(i) = r.next().unwrap() {
        match i {
            DumpItem::File { path, bytes } => got_files.push((path, bytes)),
            DumpItem::Batch(b) => got_batches.push(b),
        }
    }
    assert_eq!(
        got_files
            .iter()
            .map(|(p, _)| p.as_str())
            .collect::<Vec<_>>(),
        [".rdy.yaml", ".rdy/packs/x.yaml", ".rdyignore"]
    );
    assert_eq!(got_batches.len(), batches.len());
    for (a, b) in got_batches.iter().zip(&batches) {
        assert_eq!((a.start, a.digest), (b.start, b.digest));
        assert!(
            a.frames
                .iter()
                .zip(&b.frames)
                .all(|(x, y)| x.kind == y.kind && x.payload == y.payload)
        );
    }
    let t = r.totals();
    assert_eq!((t.batches, t.files), (batches.len() as u64, 3));
    assert_eq!(t.bytes, bytes.len() as u64);
}

#[test]
fn dl1_a_stream_is_the_same_bytes_every_time() {
    let (_b, s, batches) = store_and_batches();
    assert_eq!(stream(&s, &batches, &[]), stream(&s, &batches, &[]));
}

#[test]
fn dl1_every_record_is_verified_on_read() {
    let (_b, s, batches) = store_and_batches();
    let good = stream(&s, &batches, &[(".rdy.yaml", b"pack: rdy-standard\n")]);
    // flip one payload byte of every record in turn: header, file, batch, frames, trailer
    let mut at = 12usize;
    let mut records = 0;
    while at < good.len() {
        let len = u32::from_le_bytes(good[at..at + 4].try_into().unwrap()) as usize;
        if len > 0 {
            let mut bad = good.clone();
            bad[at + 37] ^= 0x01;
            assert_eq!(
                damaged(read_all(&bad).unwrap_err()),
                "a record's hash does not match its bytes",
                "record at byte {at}"
            );
        }
        at += 37 + len;
        records += 1;
    }
    assert!(
        records >= 6,
        "header, file, batch records, frames and trailer"
    );
}

#[test]
fn dl1_a_stream_must_end_at_its_trailer() {
    let (_b, s, batches) = store_and_batches();
    let good = stream(&s, &batches, &[]);
    let cut = &good[..good.len() - 10];
    assert_eq!(
        damaged(read_all(cut).unwrap_err()),
        "the file ends before its trailer"
    );
    let mut extra = good.clone();
    extra.push(0);
    assert_eq!(
        damaged(read_all(&extra).unwrap_err()),
        "bytes follow the trailer"
    );
    // the stream of one batch fewer, closed with the full stream's trailer: counts and digest differ
    const TRAILER: usize = 37 + 8 + 8 + 4 + 32;
    let short = stream(&s, &batches[..batches.len() - 1], &[]);
    let mut spliced = short[..short.len() - TRAILER].to_vec();
    spliced.extend_from_slice(&good[good.len() - TRAILER..]);
    assert_eq!(
        damaged(read_all(&spliced).unwrap_err()),
        "the trailer's counts or digest do not match the dump"
    );
}

#[test]
fn dl1_an_unknown_magic_or_format_is_refused() {
    let (_b, s, batches) = store_and_batches();
    let good = stream(&s, &batches, &[]);
    let mut bad = good.clone();
    bad[0] = b'X';
    assert_eq!(
        damaged(read_all(&bad).unwrap_err()),
        "it does not start with RDYDUMP"
    );
    let mut v2 = good.clone();
    v2[8] = 2;
    assert_eq!(
        damaged(read_all(&v2).unwrap_err()),
        "dump format 2 is not one this rdy reads"
    );
    let other = DumpHeader {
        store_format: STORE_FORMAT + 1,
        ..header(&s)
    };
    let bytes = DumpWriter::new(Vec::new(), &other)
        .unwrap()
        .finish()
        .unwrap()
        .0;
    assert_eq!(
        damaged(read_all(&bytes).unwrap_err()),
        format!(
            "store format {} in a dump of format 1 is not one this rdy reads",
            STORE_FORMAT + 1
        )
    );
}

#[test]
fn dl1_project_files_are_a_closed_set() {
    for ok in [
        ".rdy.yaml",
        ".rdyignore",
        ".rdy/packs/rdy-design.yaml",
        ".rdy/packs/a_b-1.yml",
    ] {
        assert!(is_project_file(ok), "{ok}");
    }
    for no in [
        ".rdy/local.yaml",
        ".rdy/run/x",
        ".rdy/packs/.hidden.yaml",
        ".rdy/packs/a/b.yaml",
        ".rdy/packs/a b.yaml",
        ".rdy/packs/x.json",
        "../.rdy.yaml",
        "/etc/passwd",
        ".rdy/store/FORMAT",
    ] {
        assert!(!is_project_file(no), "{no}");
    }
    let (_b, s, _) = store_and_batches();
    let mut w = DumpWriter::new(Vec::new(), &header(&s)).unwrap();
    assert!(
        w.file(".rdy/local.yaml", b"actor: human:x\n").is_err(),
        "the writer refuses it"
    );
}

#[test]
fn dl1_a_record_out_of_place_is_refused() {
    let (_b, s, batches) = store_and_batches();
    // a file record after a batch
    let mut w = DumpWriter::new(Vec::new(), &header(&s)).unwrap();
    w.batch(&batches[0]).unwrap();
    assert!(w.file(".rdy.yaml", b"x").is_err());
    // a batch marker frame inside a batch
    let mut b = batches[1].clone();
    b.frames.push(Frame {
        kind: rdy_store::kinds::BATCH_COMMIT,
        hash: Hash::of(b""),
        payload: vec![],
    });
    let mut w = DumpWriter::new(Vec::new(), &header(&s)).unwrap();
    assert!(w.batch(&b).is_err(), "the writer refuses a marker frame");
}

#[test]
fn dl1_a_damaged_dump_is_e_dump() {
    let e = rdy_engine::RdyError::from(StoreError::Dump {
        at: 9,
        what: "x".into(),
    });
    assert_eq!(e.code, rdy_engine::E_DUMP);
    assert_eq!(e.message, "the dump is damaged at byte 9: x");
    assert_eq!(e.fix, "dump the project again; rdy never repairs a dump");
}

/// One record framed as a store frame, with its true hash.
fn record(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut r = (payload.len() as u32).to_le_bytes().to_vec();
    r.push(kind);
    r.extend_from_slice(&Hash::of(payload).0);
    r.extend_from_slice(payload);
    r
}

/// The records of `good` after its prelude, as (kind, payload).
fn records(good: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut at = 12usize;
    let mut out = vec![];
    while at < good.len() {
        let len = u32::from_le_bytes(good[at..at + 4].try_into().unwrap()) as usize;
        out.push((good[at + 4], good[at + 37..at + 37 + len].to_vec()));
        at += 37 + len;
    }
    out
}

/// A stream of `recs` with hash-valid records, the prelude in front.
fn forge(recs: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut out = b"RDYDUMP\n\x01\x00\x00\x00".to_vec();
    for (k, p) in recs {
        out.extend(record(*k, p));
    }
    out
}

#[test]
fn dl1_a_hash_valid_record_out_of_order_or_count_is_refused() {
    let (_b, s, batches) = store_and_batches();
    let good = stream(
        &s,
        &batches[..2],
        &[(".rdy.yaml", b"a\n"), (".rdyignore", b"b\n")],
    );
    let recs = records(&good);
    // header, two files, batch 1 and its frames, batch 2 and its frames, trailer
    let file_a = recs[1].clone();
    let file_b = recs[2].clone();
    let batch1 = 3;
    let n1 = batches[0].frames.len();
    let batch2 = batch1 + 1 + n1;
    // files out of order
    let mut r = recs.clone();
    r.swap(1, 2);
    assert_eq!(
        damaged(read_all(&forge(&r)).unwrap_err()),
        "a record of kind 241 cannot stand here"
    );
    // a file after a batch
    let mut r = recs.clone();
    r.remove(2);
    r.insert(batch2 - 1, file_b.clone());
    assert_eq!(
        damaged(read_all(&forge(&r)).unwrap_err()),
        "a record of kind 241 cannot stand here"
    );
    // the same file twice
    let mut r = recs.clone();
    r.insert(2, file_a.clone());
    assert_eq!(
        damaged(read_all(&forge(&r)).unwrap_err()),
        "a record of kind 241 cannot stand here"
    );
    // a second header
    let mut r = recs.clone();
    r.insert(1, recs[0].clone());
    assert_eq!(
        damaged(read_all(&forge(&r)).unwrap_err()),
        "a record of kind 240 cannot stand here"
    );
    // a batch that says one frame more than it holds
    let mut r = recs.clone();
    let mut p = r[batch1].1.clone();
    p[12..16].copy_from_slice(&(n1 as u32 + 1).to_le_bytes());
    r[batch1].1 = p;
    assert_eq!(
        damaged(read_all(&forge(&r)).unwrap_err()),
        format!("batch 1 holds {n1} frames, and its record says {}", n1 + 1)
    );
    // a batch whose digest is not its frames'
    let mut r = recs.clone();
    r[batch1].1[16] ^= 0x01;
    assert_eq!(
        damaged(read_all(&forge(&r)).unwrap_err()),
        "batch 1 does not match its digest"
    );
    // a batch marker frame among the frames
    let mut r = recs.clone();
    r[batch1 + 1].0 = rdy_store::kinds::BATCH_COMMIT;
    assert_eq!(
        damaged(read_all(&forge(&r)).unwrap_err()),
        "a record of kind 14 cannot stand here"
    );
    // a header that does not parse
    let mut r = recs.clone();
    r[0].1.push(0);
    assert_eq!(
        damaged(read_all(&forge(&r)).unwrap_err()),
        "the header does not parse"
    );
    // a file path outside the closed set
    let mut r = recs.clone();
    let mut p = (15u16).to_le_bytes().to_vec();
    p.extend_from_slice(b".rdy/local.yaml");
    r[1].1 = p;
    assert_eq!(
        damaged(read_all(&forge(&r)).unwrap_err()),
        "the project file path \".rdy/local.yaml\" is not one rdy carries"
    );
    // and the forged copy of the good stream reads
    assert_eq!(forge(&recs), good);
    assert!(read_all(&good).is_ok());
}

/// The digest a `BATCH` record carries over its frames' kinds: `Hash::of` over each frame's
/// `kind byte || hash`, in order.
fn kinds_digest(frames: &[(u8, Vec<u8>)]) -> Hash {
    let mut b = vec![];
    for (k, p) in frames {
        b.push(*k);
        b.extend_from_slice(&Hash::of(p).0);
    }
    Hash::of(&b)
}

/// The trailer digest of dump format 1 over `recs` (the trailer left out): `Hash::of` over each
/// record's `kind byte || 8-byte payload length || hash`.
fn trailer_digest(recs: &[(u8, Vec<u8>)]) -> Hash {
    let mut b = vec![];
    for (k, p) in recs {
        b.push(*k);
        b.extend_from_slice(&(p.len() as u64).to_le_bytes());
        b.extend_from_slice(&Hash::of(p).0);
    }
    Hash::of(&b)
}

/// The trailer payload that closes `recs` (the trailer left out), with `digest`.
fn trailer_with(recs: &[(u8, Vec<u8>)], digest: Hash) -> Vec<u8> {
    let batches = recs.iter().filter(|(k, _)| *k == rec::BATCH).count() as u64;
    let frames = recs.iter().filter(|(k, _)| *k < rec::HEADER).count() as u64;
    let files = recs.iter().filter(|(k, _)| *k == rec::FILE).count() as u32;
    let mut p = batches.to_le_bytes().to_vec();
    p.extend_from_slice(&frames.to_le_bytes());
    p.extend_from_slice(&files.to_le_bytes());
    p.extend_from_slice(&digest.0);
    p
}

/// `recs` with its last record (the trailer) made true again.
fn reseal(recs: &mut [(u8, Vec<u8>)]) {
    let n = recs.len() - 1;
    let digest = trailer_digest(&recs[..n]);
    recs[n].1 = trailer_with(&recs[..n], digest);
}

/// The index of the `BATCH` record whose frames hold record `i`, and its number (from 1).
fn batch_of(recs: &[(u8, Vec<u8>)], i: usize) -> (usize, usize) {
    let j = (0..i).rev().find(|j| recs[*j].0 == rec::BATCH).unwrap();
    let n = recs[..=j].iter().filter(|(k, _)| *k == rec::BATCH).count();
    (j, n)
}

/// The frames of the `BATCH` record at `j`.
fn frames_of(recs: &[(u8, Vec<u8>)], j: usize) -> Vec<(u8, Vec<u8>)> {
    let n = u32::from_le_bytes(recs[j].1[12..16].try_into().unwrap()) as usize;
    recs[j + 1..=j + n].to_vec()
}

#[test]
fn dl1_a_flipped_frame_kind_is_refused() {
    let (_b, s, batches) = store_and_batches();
    let good = stream(&s, &batches, &[]);
    let recs = records(&good);
    use rdy_store::kinds::{BLOB_CHUNK, BLOB_MANIFEST, REF_UPDATE, TAG};
    for (from, to) in [(REF_UPDATE, TAG), (BLOB_MANIFEST, BLOB_CHUNK)] {
        let i = recs.iter().rposition(|(k, _)| *k == from).unwrap();
        let (j, n) = batch_of(&recs, i);
        let mut r = recs.clone();
        r[i].0 = to;
        // the frame's hash covers its payload only, so the record itself still verifies
        assert_eq!(
            damaged(read_all(&forge(&r)).unwrap_err()),
            format!("batch {n} does not match its digest"),
            "{from} to {to}"
        );
        // with the batch's kinds digest forced to match, the trailer refuses it
        let d = kinds_digest(&frames_of(&r, j));
        r[j].1[48..80].copy_from_slice(&d.0);
        assert_eq!(
            damaged(read_all(&forge(&r)).unwrap_err()),
            "the trailer's counts or digest do not match the dump",
            "{from} to {to}, at the trailer"
        );
        // and a trailer digest over the record hashes alone (no kinds) is refused too
        let last = r.len() - 1;
        let mut blind = vec![];
        for (_, p) in &r[..last] {
            blind.extend_from_slice(&Hash::of(p).0);
        }
        r[last].1 = trailer_with(&r[..last], Hash::of(&blind));
        assert_eq!(
            damaged(read_all(&forge(&r)).unwrap_err()),
            "the trailer's counts or digest do not match the dump",
            "{from} to {to}, a kind-blind trailer"
        );
        // the same stream, sealed with the format's own digest, reads: only the kind changed
        reseal(&mut r);
        assert!(read_all(&forge(&r)).is_ok(), "{from} to {to}, resealed");
    }
}

#[test]
fn dl1_an_unassigned_kind_is_refused() {
    let (_b, s, batches) = store_and_batches();
    assert_eq!(
        frame_kinds(DUMP_FORMAT, STORE_FORMAT),
        Some(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 13, 15, 16, 17, 18, 19][..]),
        "exactly the kinds store format 10 assigns"
    );
    for (d, sf) in READS {
        assert!(frame_kinds(*d, *sf).is_some(), "({d}, {sf}) has its kinds");
    }
    let good = stream(&s, &batches, &[]);
    let recs = records(&good);
    let first = recs.iter().position(|(k, _)| *k < rec::HEADER).unwrap();
    for k in [10u8, 11, 12, 14] {
        // the reader
        let mut r = recs.clone();
        r[first].0 = k;
        assert_eq!(
            damaged(read_all(&forge(&r)).unwrap_err()),
            format!("a record of kind {k} cannot stand here"),
            "the reader, kind {k}"
        );
        // the writer
        let mut b = batches[1].clone();
        b.frames[0].kind = k;
        let mut w = DumpWriter::new(Vec::new(), &header(&s)).unwrap();
        match w.batch(&b) {
            Err(StoreError::Refused(m)) => {
                assert_eq!(m, format!("a record of kind {k} cannot stand here"))
            }
            other => panic!("the writer, kind {k}: {other:?}"),
        }
    }
}

#[test]
fn dl1_a_reordered_record_is_refused() {
    let (_b, s, batches) = store_and_batches();
    let good = stream(&s, &batches, &[]);
    let recs = records(&good);
    let starts: Vec<usize> = (0..recs.len())
        .filter(|i| recs[*i].0 == rec::BATCH)
        .collect();
    // two whole batches swapped, each with its frames: every record verifies, the order does not
    let (a, b) = (starts[1], starts[2]);
    let mut r = recs[..a].to_vec();
    r.extend_from_slice(&recs[b..starts[3]]);
    r.extend_from_slice(&recs[a..b]);
    r.extend_from_slice(&recs[starts[3]..]);
    assert_eq!(
        damaged(read_all(&forge(&r)).unwrap_err()),
        "the trailer's counts or digest do not match the dump"
    );
    // two frames of one batch swapped (the blob's chunks and manifest)
    let big = starts[3];
    assert!(frames_of(&recs, big).len() >= 2);
    let mut r = recs.clone();
    r.swap(big + 1, big + 2);
    assert_eq!(
        damaged(read_all(&forge(&r)).unwrap_err()),
        "batch 4 does not match its digest"
    );
}

/// The byte offset of record `k` of `good` (0 is the header).
fn offset_of(good: &[u8], k: usize) -> usize {
    let mut at = 12usize;
    for _ in 0..k {
        let len = u32::from_le_bytes(good[at..at + 4].try_into().unwrap()) as usize;
        at += 37 + len;
    }
    at
}

/// Reads `bytes` to its first error, then calls `next` twice more: each call gives that error
/// again, never an item or the end.
fn stays_failed(bytes: &[u8], want: &str) {
    let (mut r, _, _) = DumpReader::open(bytes).unwrap();
    let first = loop {
        match r.next() {
            Ok(Some(_)) => continue,
            Ok(None) => panic!("no error before the end"),
            Err(e) => break e,
        }
    };
    assert_eq!(damaged(first), want);
    for i in 0..2 {
        assert_eq!(
            damaged(r.next().unwrap_err()),
            want,
            "call {i} after the error"
        );
    }
}

#[test]
fn dl1_a_reader_stays_failed_after_an_error() {
    let (_b, s, batches) = store_and_batches();
    let good = stream(&s, &batches, &[]);
    const TRAILER: usize = 37 + 8 + 8 + 4 + 32;
    // one junk byte after the trailer, then a copy of the trailer: a retry must not read it
    let mut extra = good.clone();
    extra.push(0);
    extra.extend_from_slice(&good[good.len() - TRAILER..]);
    stays_failed(&extra, "bytes follow the trailer");
    // a hash error at the first batch record: a retry must not read on from its frames
    let mut bad = good.clone();
    bad[offset_of(&good, 1) + 37] ^= 0x01;
    stays_failed(&bad, "a record's hash does not match its bytes");
}

#[test]
fn dl1_a_path_longer_than_its_length_field_is_refused() {
    let (_b, s, _) = store_and_batches();
    let path = format!(".rdy/packs/{}.yaml", "a".repeat(65536));
    assert!(is_project_file(&path));
    let mut w = DumpWriter::new(Vec::new(), &header(&s)).unwrap();
    match w.file(&path, b"x") {
        Err(StoreError::Refused(m)) => assert_eq!(
            m,
            format!("the project file path {path:?} is not one rdy carries")
        ),
        other => panic!("not refused: {other:?}"),
    }
    // nothing was written: the writer still finishes a dump that reads
    let bytes = w.finish().unwrap().0;
    assert!(read_all(&bytes).unwrap().is_empty());
}

#[test]
fn dl1_a_batch_of_no_frames_is_refused() {
    let (_b, s, batches) = store_and_batches();
    let end = s.log_end();
    // the writer
    let empty = DumpBatch {
        start: end,
        digest: Hash::of(b""),
        frames: vec![],
    };
    let mut w = DumpWriter::new(Vec::new(), &header(&s)).unwrap();
    match w.batch(&empty) {
        Err(StoreError::Refused(m)) => assert_eq!(m, "a record of kind 242 cannot stand here"),
        other => panic!("not refused: {other:?}"),
    }
    // the reader: a hash-valid BATCH record of no frames, with true digests and a true trailer
    let mut r = records(&stream(&s, &batches, &[]));
    let mut p = end.segment.to_le_bytes().to_vec();
    p.extend_from_slice(&end.offset.to_le_bytes());
    p.extend_from_slice(&0u32.to_le_bytes());
    p.extend_from_slice(&Hash::of(b"").0);
    p.extend_from_slice(&kinds_digest(&[]).0);
    let last = r.len() - 1;
    r.insert(last, (rec::BATCH, p));
    reseal(&mut r);
    assert_eq!(
        damaged(read_all(&forge(&r)).unwrap_err()),
        "a record of kind 242 cannot stand here"
    );
}

/// The batch markers of `s`'s log: each one's location and payload.
fn markers(s: &Store) -> Vec<(Location, Vec<u8>)> {
    let mut out = vec![];
    s.reader()
        .for_each_frame_from(
            Location {
                segment: 0,
                offset: 0,
            },
            s.log_end(),
            |loc, kind, _, payload, _| {
                if kind == rdy_store::kinds::BATCH_COMMIT {
                    out.push((loc, payload.to_vec()));
                }
                Ok(())
            },
        )
        .unwrap();
    out
}

/// Writes `frame` at `loc` of the log of the store at `dir`, in place.
fn write_at(dir: &std::path::Path, loc: Location, frame: &[u8]) {
    use std::os::unix::fs::FileExt;
    let path = rdy_store::segment::seg_path(&dir.join("segments"), loc.segment);
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.write_all_at(frame, loc.offset).unwrap();
}

fn decode_text(e: StoreError) -> String {
    match e {
        StoreError::Decode(m) => m,
        other => panic!("not a decode error: {other}"),
    }
}

/// A change to a marker payload.
type Bend = fn(&[u8]) -> Vec<u8>;

/// The marker payload `p`, decoded, changed by `bend` and encoded again.
fn bend_marker(p: &[u8], bend: fn(&mut rdy_store::kinds::BatchCommit)) -> Vec<u8> {
    let mut m = rdy_store::kinds::BatchCommit::decode(p).unwrap();
    bend(&mut m);
    postcard::to_allocvec(&m).unwrap()
}

#[test]
fn dl1_for_each_batch_refuses_a_marker_that_does_not_match_its_batch() {
    use rdy_store::kinds::{BATCH_COMMIT, BatchCommit};
    let cases: [(&str, Bend); 4] = [
        ("digest", |p| bend_marker(p, |m| m.digest.0[0] ^= 1)),
        ("count", |p| bend_marker(p, |m| m.frames += 1)),
        ("start", |p| bend_marker(p, |m| m.start.segment += 1)),
        ("junk", |p| vec![0xFF; p.len()]),
    ];
    for (what, bend) in cases {
        let (base, s, batches) = store_and_batches();
        let ms = markers(&s);
        assert_eq!(ms.len(), batches.len(), "{what}");
        // the marker of the third batch (the ref write), with its true frame hash
        let (loc, p) = &ms[2];
        let q = bend(p);
        assert_eq!(q.len(), p.len(), "{what}: the marker keeps its length");
        assert!(
            BatchCommit::decode(&q).is_none() == (what == "junk"),
            "{what}"
        );
        write_at(&base.path().join("s"), *loc, &record(BATCH_COMMIT, &q));
        let start = batches[2].start;
        assert_eq!(
            decode_text(
                s.reader()
                    .for_each_batch(s.log_end(), |_| Ok(()))
                    .unwrap_err()
            ),
            format!(
                "batch at segment {} offset {} does not match its marker; run rdy fsck",
                start.segment, start.offset
            ),
            "{what}"
        );
    }
}

#[test]
fn dl1_for_each_batch_refuses_a_marker_of_no_frames() {
    use rdy_store::kinds::{BATCH_COMMIT, BatchCommit, batch_digest};
    let (base, s, _) = store_and_batches();
    let end = s.log_end();
    let m = BatchCommit {
        start: end,
        frames: 0,
        digest: batch_digest([]),
    };
    let frame = record(BATCH_COMMIT, &postcard::to_allocvec(&m).unwrap());
    write_at(&base.path().join("s"), end, &frame);
    let upto = Location {
        segment: end.segment,
        offset: end.offset + frame.len() as u64,
    };
    assert_eq!(
        decode_text(s.reader().for_each_batch(upto, |_| Ok(())).unwrap_err()),
        format!(
            "batch at segment {} offset {} does not match its marker; run rdy fsck",
            end.segment, end.offset
        )
    );
}

#[test]
fn dl1_for_each_batch_refuses_a_log_that_ends_inside_a_batch() {
    let (_b, mut s, _) = store_and_batches();
    s.set_held(true).unwrap();
    s.set_refs(vec![("view/b".into(), b"y".to_vec())]).unwrap();
    assert!(
        s.open_batch().is_some_and(|b| !b.is_empty()),
        "a held open batch"
    );
    let end = s.log_end();
    assert_eq!(
        decode_text(s.reader().for_each_batch(end, |_| Ok(())).unwrap_err()),
        format!(
            "the log does not end at a batch marker at segment {} offset {}; run rdy fsck",
            end.segment, end.offset
        )
    );
}
