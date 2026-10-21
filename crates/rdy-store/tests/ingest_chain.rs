//! hc7: the ingest of a chain load (D-2FMRR: one ingest; D-QF2HQ: one carried batch). One
//! `Ingest` takes the retained batches of every file of a chain in order, then at most one
//! carried batch, then at most one upgrade batch. Its pending-drop bounds hold for the whole
//! chain: a file's end resets neither the count of dropped frames waiting for their check nor
//! the zeros not yet validated.
use rdy_model::Hash;
use rdy_store::Store;
use rdy_store::dump::*;
use rdy_store::dump_chain::{BatchMarker, DumpChainHeader};
use rdy_store::dump_scan::{ChainSource, ScanPlan, scan};
use rdy_store::format::{REF_STORE_FORMAT, STORE_FORMAT, decode_u32};
use rdy_store::ingest::{Ingest, MAX_PENDING_DROPS, MAX_UNAUTHORIZED_BYTES};
use rdy_store::kinds::{self, RefUpdate};
use rdy_store::segment::Location;
use std::io::BufReader;
use std::path::{Path, PathBuf};

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../rdy-qa/fixtures/dump/{name}"))
}

/// Every batch of the dump `file`, its header and its dump format.
fn batches_of(file: &Path) -> (u32, DumpHeader, Vec<DumpBatch>) {
    let f = BufReader::new(std::fs::File::open(file).unwrap());
    let (mut r, format, header) = DumpReader::open(f).unwrap();
    let mut out = vec![];
    while let Some(item) = r.next().unwrap() {
        if let DumpItem::Batch(b) = item {
            out.push(b);
        }
    }
    (format, header, out)
}

/// A full dump (format 3) of the new store `s`, with no project file.
fn full_dump(s: &Store) -> Vec<u8> {
    let r = s.reader();
    let end = s.log_end();
    let end_batch = r.batch_marker_at(end).unwrap().unwrap_or(BatchMarker::ZERO);
    let repo = r.repo_id().unwrap().unwrap();
    let header = DumpHeader {
        store_format: STORE_FORMAT,
        log_end: end,
        rdy: "test".into(),
        chain: Some(DumpChainHeader::full(repo, end_batch)),
    };
    let mut w = DumpWriter::new(Vec::new(), &header).unwrap();
    let mut tally = RemovalTally::default();
    r.for_each_batch_after(ORIGIN, end, &mut |b| {
        tally.observe(&b)?;
        w.batch(&b)
    })
    .unwrap();
    let facts = tally.resolved(&r, end).unwrap();
    w.finish(facts).unwrap().0
}

/// A small valid chain of one full dump with no removal, its verified plan, and its batches.
fn small_chain(dir: &Path) -> (ScanPlan, Vec<DumpBatch>) {
    let s = Store::create(&dir.join("src")).unwrap();
    let bytes = full_dump(&s);
    drop(s);
    let file = dir.join("full.rdydump");
    std::fs::write(&file, &bytes).unwrap();
    let mut src = ChainSource::open(std::slice::from_ref(&file)).unwrap();
    let plan = scan(&mut src, None).unwrap();
    let (_, _, batches) = batches_of(&file);
    (plan, batches)
}

/// A batch at `start` of `n` dropped blocks, each claiming `len` zero bytes, named by an apply
/// no log holds: the ingest keeps each one for its final check.
fn drops(start: Location, n: usize, len: u32, salt: u64) -> DumpBatch {
    let frames: Vec<Frame> = (0..n as u64)
        .map(|i| Frame::Dropped {
            kind: kinds::BLOCK,
            hash: Hash::of(&[salt.to_le_bytes(), i.to_le_bytes()].concat()),
            len,
            apply: Hash::of(b"no such apply"),
            residue: Some(Hash::of(b"no such residue")),
        })
        .collect();
    DumpBatch {
        start,
        digest: kinds::batch_digest(frames.iter().map(|f| f.hash())),
        frames,
    }
}

/// The text of a `StoreError::Dump`.
fn what(e: rdy_store::StoreError) -> String {
    match e {
        rdy_store::StoreError::Dump { what, .. } => what,
        other => panic!("not a dump refusal: {other:?}"),
    }
}

#[test]
fn hc7_older_ingest_gets_one_upgrade_after_all_batches() {
    // the frozen (2, 15) pair: its batches go to one ingest in three slices, as three files of a
    // chain would, and the one upgrade comes after the last of them
    let file = fixture("v2-s15.rdydump");
    let (format, header, batches) = batches_of(&file);
    assert_eq!((format, header.store_format), (2, 15));
    assert!(batches.len() >= 3, "the fixture holds batches");
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let mut ingest = Ingest::create(&store).unwrap();
    let third = batches.len() / 3;
    let slices = [
        &batches[..third],
        &batches[third..2 * third],
        &batches[2 * third..],
    ];
    let mut n = 0;
    for slice in slices {
        assert!(!slice.is_empty());
        for b in slice {
            n += 1;
            ingest.batch(n, b).unwrap();
        }
        // a slice's end is no upgrade point: nothing but the source batches so far
        assert_eq!(
            ingest.end(),
            rdy_store::dump_chain::batch_end(slice.last().unwrap()).unwrap()
        );
    }
    assert_eq!(ingest.end(), header.log_end);
    ingest.append_carried(&[], &[]).unwrap();
    assert!(ingest.upgrade(header.store_format).unwrap());
    let report = ingest.finish().unwrap();
    assert_eq!(report.batches, batches.len() as u64);
    assert_eq!(
        report.frames,
        batches.iter().map(|b| b.frames.len() as u64).sum::<u64>()
    );
    // the tail after the source log: exactly one batch, which names this rdy's store format
    let s = Store::open(&store).unwrap();
    let mut tail = vec![];
    s.reader()
        .for_each_batch_after(header.log_end, s.log_end(), &mut |b| {
            tail.push(b);
            Ok(())
        })
        .unwrap();
    assert_eq!(tail.len(), 1, "{tail:?}");
    let refs: Vec<RefUpdate> = tail[0]
        .frames
        .iter()
        .map(|f| {
            assert_eq!(f.kind(), kinds::REF_UPDATE);
            postcard::from_bytes(f.present_payload().unwrap()).unwrap()
        })
        .collect();
    assert_eq!(refs.len(), 1, "the source holds its repo id: {refs:?}");
    assert_eq!(refs[0].name, REF_STORE_FORMAT);
    assert_eq!(decode_u32(&refs[0].value), Some(STORE_FORMAT));
    assert_eq!(refs[0].at, header.log_end);
    // the source log is the store's prefix, byte for byte
    let mut source = vec![];
    s.reader()
        .for_each_batch_after(ORIGIN, header.log_end, &mut |b| {
            source.push(b);
            Ok(())
        })
        .unwrap();
    assert_eq!(source, batches);
}

#[test]
fn hc7_chain_pending_limits_do_not_reset() {
    let dir = tempfile::tempdir().unwrap();
    let (plan, prefix) = small_chain(dir.path());

    // the count: dropped frames from three "files" of one chain, each slice well under the
    // bound, together at it; one more is refused
    let mut ingest = Ingest::create(&dir.path().join("count")).unwrap();
    let mut n = 0;
    for b in &prefix {
        n += 1;
        ingest.batch_with_plan(n, b, &plan).unwrap();
    }
    for (i, count) in [400_000usize, 400_000, MAX_PENDING_DROPS - 800_000]
        .into_iter()
        .enumerate()
    {
        n += 1;
        let b = drops(ingest.end(), count, 0, i as u64);
        ingest.batch_with_plan(n, &b, &plan).unwrap();
    }
    n += 1;
    let one_more = drops(ingest.end(), 1, 0, 9);
    let e = ingest.batch_with_plan(n, &one_more, &plan).unwrap_err();
    assert_eq!(what(e), "record allocation failed");
    drop(ingest);

    // the zeros not yet validated: 1 GiB over two "files" is the bound; one byte more is refused
    let mut ingest = Ingest::create(&dir.path().join("bytes")).unwrap();
    let mut n = 0;
    for b in &prefix {
        n += 1;
        ingest.batch_with_plan(n, b, &plan).unwrap();
    }
    let big = 64u32 << 20;
    let per_file = (MAX_UNAUTHORIZED_BYTES / u64::from(big) / 2) as usize;
    for i in 0..2u64 {
        n += 1;
        let b = drops(ingest.end(), per_file, big, 100 + i);
        ingest.batch_with_plan(n, &b, &plan).unwrap();
    }
    let before = ingest.end();
    n += 1;
    let one_byte = drops(ingest.end(), 1, 1, 200);
    let e = ingest.batch_with_plan(n, &one_byte, &plan).unwrap_err();
    assert_eq!(
        what(e),
        "dropped frames claim more than 1073741824 bytes before their authorization"
    );
    // the refused frame was never written
    assert_eq!(ingest.end(), before);
}
