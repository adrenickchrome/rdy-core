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
