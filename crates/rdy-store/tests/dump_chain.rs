//! hc1: dump format 3 and the carried removal record (D-82K89: rdy writes dump format 3 for every
//! dump; D-69AY3: every released dump format stays readable; D-F0904: no existing kind is
//! re-encoded). The header and trailer layouts, the frozen kind sets of the released pairs, the
//! physical boundaries of full dumps and increments, the envelope reader, the bounds, and the
//! new record's encoding.
use rdy_model::redaction::{
    AssessmentSupport, DropSpan, RedactTarget, Redaction, RedactionApply, RedactionManifest,
};
use rdy_model::*;
use rdy_store::bounded::{self, RecordError};
use rdy_store::dump::*;
use rdy_store::dump_chain::*;
use rdy_store::format::STORE_FORMAT;
use rdy_store::kinds;
use rdy_store::segment::Location;
use rdy_store::{Store, StoreError};
use std::io::{Cursor, Read, Seek, SeekFrom};

/// The length of a container record's frame header.
const HEAD: usize = 37;

fn at(segment: u32, offset: u64) -> Location {
    Location { segment, offset }
}

fn marker(segment: u32, offset: u64, frames: u32, d: u8) -> BatchMarker {
    BatchMarker {
        start: at(segment, offset),
        frames,
        digest: Hash([d; 32]),
    }
}

fn damaged(e: StoreError) -> String {
    match e {
        StoreError::Dump { what, .. } => what,
        other => panic!("not a damaged dump: {other}"),
    }
}

fn refused(e: StoreError) -> String {
    match e {
        StoreError::Refused(what) => what,
        other => panic!("not a refusal: {other}"),
    }
}

/// Reads a whole dump; Ok with its header, batches and trailer.
fn read_all(bytes: &[u8]) -> Result<(DumpHeader, Vec<DumpBatch>, DumpTrailer), StoreError> {
    let (mut r, format, h) = DumpReader::open(bytes)?;
    assert_eq!(format, DUMP_FORMAT);
    assert!(r.trailer().is_none(), "no trailer before the end");
    let mut out = vec![];
    while let Some(item) = r.next()? {
        if let DumpItem::Batch(b) = item {
            out.push(b);
        }
    }
    let t = *r.trailer().expect("a verified trailer");
    assert_eq!(r.totals().digest, t.digest);
    Ok((h, out, t))
}

/// Writes a dump of `header` holding `batches`, with the removal facts `facts`.
fn write(header: &DumpHeader, batches: &[DumpBatch], facts: RemovalFacts) -> Vec<u8> {
    let mut w = DumpWriter::new(Vec::new(), header).unwrap();
    for b in batches {
        w.batch(b).unwrap();
    }
    w.finish(facts).unwrap().0
}

fn header(log_end: Location, chain: DumpChainHeader) -> DumpHeader {
    DumpHeader {
        store_format: STORE_FORMAT,
        log_end,
        rdy: "test".into(),
        chain: Some(chain),
    }
}

/// Replaces the payload of the record at `at` in `bytes` with `payload`, and its hash. The
/// trailer's digest covers every record but the trailer, so a changed record before it also
/// breaks the digest unless the reader refuses it first.
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

/// The offset of the trailer record of a format-3 dump.
fn trailer_at(bytes: &[u8]) -> usize {
    bytes.len() - (HEAD + TRAILER_LEN)
}

/// A store whose small segments roll between batches, with its repo id and sealed batches.
fn rolled_store() -> (tempfile::TempDir, Store, RepoId, Vec<DumpBatch>) {
    let base = tempfile::tempdir().unwrap();
    let mut s = Store::create(&base.path().join("s")).unwrap();
    s.set_segment_max(4096);
    for i in 0..6 {
        s.put_blob(format!("blob {i} {}", "q".repeat(1500)).as_bytes())
            .unwrap();
    }
    s.set_refs(vec![("view/a".into(), b"x".to_vec())]).unwrap();
    let id = s.reader().repo_id().unwrap().expect("a repo id");
    let mut batches = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |b| {
            batches.push(b);
            Ok(())
        })
        .unwrap();
    (base, s, id, batches)
}

#[test]
fn hc1_header_and_trailer_match_literal_bytes() {
    assert_eq!(HEADER_FIXED, 175, "the fixed part of the format-3 header");
    assert_eq!(TRAILER_LEN, 69, "the format-3 trailer payload");
    let repo = RepoId([0xA5; 16]);
    let full = header(
        at(2, 0x0102_0304_0506),
        DumpChainHeader::full(repo, marker(2, 0x0100, 7, 0xEE)),
    );
    let facts = RemovalFacts {
        removals: 3,
        first_removed: Some(at(1, 0x4455)),
    };
    let (bytes, totals) = {
        let w = DumpWriter::new(Vec::new(), &full).unwrap();
        w.finish(facts).unwrap()
    };
    assert_eq!(&bytes[..12], b"RDYDUMP\n\x03\x00\x00\x00");
    // the header record: 175 bytes and the rdy text
    let p = &bytes[12 + HEAD..12 + HEAD + 179];
    assert_eq!(&bytes[12..16], &179u32.to_le_bytes());
    assert_eq!(bytes[16], rec::HEADER);
    assert_eq!(&bytes[17..49], &Hash::of(p).0);
    let mut want = vec![0u8]; // kind: full
    want.extend_from_slice(&16u32.to_le_bytes());
    want.extend_from_slice(&[0xA5; 16]);
    want.extend_from_slice(&[0u8; 12]); // base 0.0
    want.extend_from_slice(&[0u8; 48]); // base marker zero
    want.extend_from_slice(&[0u8; 32]); // prev zero
    want.extend_from_slice(&2u32.to_le_bytes());
    want.extend_from_slice(&0x0102_0304_0506u64.to_le_bytes());
    want.extend_from_slice(&2u32.to_le_bytes());
    want.extend_from_slice(&0x0100u64.to_le_bytes());
    want.extend_from_slice(&7u32.to_le_bytes());
    want.extend_from_slice(&[0xEE; 32]);
    want.extend_from_slice(&4u16.to_le_bytes());
    want.extend_from_slice(b"test");
    assert_eq!(p, &want[..]);
    // the trailer record: 106 bytes framed, 69 of payload
    let t = trailer_at(&bytes);
    assert_eq!(bytes.len() - t, 106);
    assert_eq!(t, 12 + HEAD + 179, "the trailer follows the header");
    assert_eq!(&bytes[t..t + 4], &69u32.to_le_bytes());
    assert_eq!(bytes[t + 4], rec::TRAILER);
    let tp = &bytes[t + HEAD..];
    assert_eq!(&bytes[t + 5..t + HEAD], &Hash::of(tp).0);
    // the digest covers the header record: kind, length and hash
    let mut link = vec![rec::HEADER];
    link.extend_from_slice(&179u64.to_le_bytes());
    link.extend_from_slice(&Hash::of(p).0);
    let mut want = vec![];
    want.extend_from_slice(&0u64.to_le_bytes()); // batches
    want.extend_from_slice(&0u64.to_le_bytes()); // frames
    want.extend_from_slice(&0u32.to_le_bytes()); // files
    want.extend_from_slice(&3u32.to_le_bytes()); // removals
    want.push(1);
    want.extend_from_slice(&1u32.to_le_bytes());
    want.extend_from_slice(&0x4455u64.to_le_bytes());
    want.extend_from_slice(&Hash::of(&link).0);
    assert_eq!(tp, &want[..]);
    assert_eq!(totals.digest, Hash::of(&link));
    assert_eq!(
        (totals.removals, totals.first_removed),
        (3, Some(at(1, 0x4455)))
    );

    // an increment: kind 1 and its base facts, in field order
    let prev = ChainEnd {
        repo_id: repo,
        store_format: STORE_FORMAT,
        end: at(1, 0x0A0B),
        end_batch: marker(1, 0x0909, 2, 0x11),
        digest: Hash([0x22; 32]),
    };
    let inc = header(
        at(3, 0x30),
        DumpChainHeader::after(&prev, marker(3, 0x10, 1, 0x33)),
    );
    let bytes = DumpWriter::new(Vec::new(), &inc)
        .unwrap()
        .finish(RemovalFacts::default())
        .unwrap()
        .0;
    let p = &bytes[12 + HEAD..12 + HEAD + 179];
    let mut want = vec![1u8];
    want.extend_from_slice(&16u32.to_le_bytes());
    want.extend_from_slice(&[0xA5; 16]);
    want.extend_from_slice(&1u32.to_le_bytes());
    want.extend_from_slice(&0x0A0Bu64.to_le_bytes());
    want.extend_from_slice(&1u32.to_le_bytes());
    want.extend_from_slice(&0x0909u64.to_le_bytes());
    want.extend_from_slice(&2u32.to_le_bytes());
    want.extend_from_slice(&[0x11; 32]);
    want.extend_from_slice(&[0x22; 32]);
    want.extend_from_slice(&3u32.to_le_bytes());
    want.extend_from_slice(&0x30u64.to_le_bytes());
    want.extend_from_slice(&3u32.to_le_bytes());
    want.extend_from_slice(&0x10u64.to_le_bytes());
    want.extend_from_slice(&1u32.to_le_bytes());
    want.extend_from_slice(&[0x33; 32]);
    want.extend_from_slice(&4u16.to_le_bytes());
    want.extend_from_slice(b"test");
    assert_eq!(p, &want[..]);
    // no removal facts: a zero flag and a zero position
    let t = trailer_at(&bytes);
    assert_eq!(&bytes[t + HEAD + 20..t + HEAD + 37], &[0u8; 17]);
    // both kinds read back their headers exactly (the batch checks come at the trailer)
    let (_, _, h) = DumpReader::open(&bytes[..]).unwrap();
    assert_eq!(h, inc);
    let (_, _, h) = DumpReader::open(&write(&full, &[], facts)[..]).unwrap();
    assert_eq!(h, full);
    // a kind byte of 2 to 255 does not parse, in the reader and the envelope
    for kind in 2..=255u8 {
        let mut b = bytes.clone();
        let mut p = b[12 + HEAD..12 + HEAD + 179].to_vec();
        p[0] = kind;
        set_record(&mut b, 12, &p);
        let e = DumpReader::open(&b[..]).err().expect("refused");
        assert_eq!(damaged(e), "the header does not parse", "kind {kind}");
        let e = read_envelope(&mut Cursor::new(&b)).unwrap_err();
        assert_eq!(damaged(e), "the header does not parse", "kind {kind}");
    }
    // the writer needs chain facts: it writes dump format 3 only
    let mut legacy = full.clone();
    legacy.chain = None;
    let e = DumpWriter::new(Vec::new(), &legacy).err().expect("refused");
    assert_eq!(refused(e), "the header does not parse");
}

#[test]
fn hc1_released_pairs_keep_frozen_kind_sets() {
    assert_eq!((DUMP_FORMAT, STORE_FORMAT), (3, 16));
    assert_eq!(kinds::REDACTION_CARRIED, 23);
    assert_eq!(
        READS,
        &[
            (1, 10),
            (1, 11),
            (1, 12),
            (2, 13),
            (2, 14),
            (2, 15),
            (3, 16)
        ]
    );
    let f10: Vec<u8> = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 13, 15, 16, 17, 18, 19];
    let f11: Vec<u8> = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 13, 15, 16, 17, 18, 19];
    let f13: Vec<u8> = vec![
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 13, 15, 16, 17, 18, 19, 20, 21, 22,
    ];
    let f14: Vec<u8> = vec![
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 16, 17, 18, 19, 20, 21, 22,
    ];
    let f16: Vec<u8> = (1..=11).chain([13]).chain(15..=23).collect();
    let want: [((u32, u32), &[u8]); 7] = [
        ((1, 10), &f10),
        ((1, 11), &f11),
        ((1, 12), &f11),
        ((2, 13), &f13),
        ((2, 14), &f14),
        ((2, 15), &f14),
        ((3, 16), &f16),
    ];
    for ((d, s), kinds) in want {
        assert_eq!(frame_kinds(d, s), Some(kinds), "pair ({d}, {s})");
        // the batch marker and code 12 never stand as dump data
        assert!(
            !kinds.contains(&14) && !kinds.contains(&12),
            "pair ({d}, {s})"
        );
    }
    // no pair moves to another dump format
    for (d, s) in [(1, 13), (2, 16), (3, 15), (2, 12), (4, 16)] {
        assert_eq!(frame_kinds(d, s), None, "pair ({d}, {s})");
    }
    // a carried record is dump data only at (3, 16)
    for (d, s) in READS.iter().take(6) {
        assert!(!frame_kinds(*d, *s).unwrap().contains(&23), "({d}, {s})");
    }
}

#[test]
fn hc1_full_and_increment_boundaries_are_exact() {
    let (_dir, s, repo, batches) = rolled_store();
    let end = s.log_end();
    let r = s.reader();
    let n = batches.len();
    assert!(n >= 4, "{n} batches");
    let last = BatchMarker::of(&batches[n - 1]);
    assert_eq!(r.batch_marker_at(end).unwrap(), Some(last));
    assert_eq!(batch_end(&batches[n - 1]), Some(end));
    // each batch ends where the next starts, or the log rolled to the next segment
    for w in batches.windows(2) {
        let e = batch_end(&w[0]).unwrap();
        assert!(
            w[1].start == e || (w[1].start.segment == e.segment + 1 && w[1].start.offset == 0),
            "{e:?} then {:?}",
            w[1].start
        );
        assert_eq!(r.batch_marker_at(e).unwrap(), Some(BatchMarker::of(&w[0])));
    }
    assert_eq!(r.batch_marker_at(at(0, 0)).unwrap(), None, "the origin");

    // a full dump: zero base, every batch, the end marker of the last
    let full = header(end, DumpChainHeader::full(repo, last));
    let mut tally = RemovalTally::default();
    for b in &batches {
        tally.observe(b).unwrap();
    }
    let facts = tally.facts().unwrap();
    assert_eq!(
        facts,
        RemovalFacts::default(),
        "no applies, no dropped frame"
    );
    let (h, read, t) = read_all(&write(&full, &batches, facts)).unwrap();
    assert_eq!(h, full);
    assert_eq!(read, batches);
    assert_eq!(
        (t.batches, t.removals, t.first_removed),
        (n as u64, 0, None)
    );
    // a full dump with any nonzero base fact
    let mut bad_base = vec![];
    let mut c = DumpChainHeader::full(repo, last);
    c.base = at(0, 1);
    bad_base.push(c);
    let mut c = DumpChainHeader::full(repo, last);
    c.base_batch.frames = 1;
    bad_base.push(c);
    let mut c = DumpChainHeader::full(repo, last);
    c.prev.0[31] = 1;
    bad_base.push(c);
    for c in bad_base {
        let e = DumpReader::open(&write(&header(end, c), &batches, facts)[..])
            .err()
            .expect("refused");
        assert_eq!(damaged(e), "a full dump has nonzero base facts");
    }

    // one batch: the increment of the last batch after the end of the one before it
    let base = batch_end(&batches[n - 2]).unwrap();
    let prev = ChainEnd {
        repo_id: repo,
        store_format: STORE_FORMAT,
        end: base,
        end_batch: BatchMarker::of(&batches[n - 2]),
        digest: Hash([9; 32]),
    };
    let inc = header(end, DumpChainHeader::after(&prev, last));
    let (h, read, _) = read_all(&write(&inc, &batches[n - 1..], facts)).unwrap();
    assert_eq!(h.chain.unwrap().kind, DumpKind::Increment);
    assert_eq!(read, &batches[n - 1..]);
    let mut suffix = vec![];
    r.for_each_batch_after(base, end, &mut |b| {
        suffix.push(b);
        Ok(())
    })
    .unwrap();
    assert_eq!(suffix, &batches[n - 1..]);
    // an increment of no batches
    let empty = header(base, DumpChainHeader::after(&prev, prev.end_batch));
    let e = read_all(&write(&empty, &[], facts)).unwrap_err();
    assert_eq!(damaged(e), "an increment holds no batches");
    // a first batch that does not start at the base
    let mut off = prev;
    off.end.offset -= 1;
    let skewed = header(end, DumpChainHeader::after(&off, last));
    let e = read_all(&write(&skewed, &batches[n - 1..], facts)).unwrap_err();
    assert_eq!(damaged(e), "the first batch does not follow the base");
    // a mismatched end, or end marker
    let mut long = full.clone();
    long.log_end.offset += 1;
    let e = read_all(&write(&long, &batches, facts)).unwrap_err();
    assert_eq!(damaged(e), "the last batch does not match the header");
    let mut other = DumpChainHeader::full(repo, last);
    other.end_batch.digest.0[0] ^= 1;
    let e = read_all(&write(&header(end, other), &batches, facts)).unwrap_err();
    assert_eq!(damaged(e), "the last batch does not match the header");
    // missing the last batch: the header's end is not the batches' end
    let e = read_all(&write(&full, &batches[..n - 1], facts)).unwrap_err();
    assert_eq!(damaged(e), "the last batch does not match the header");

    // rollover: the first batch of a segment follows the end of the segment before it
    let i = (1..n)
        .find(|i| batches[*i].start.offset == 0)
        .expect("a rolled segment");
    let base = batch_end(&batches[i - 1]).unwrap();
    assert_eq!(base.segment + 1, batches[i].start.segment);
    let prev = ChainEnd {
        repo_id: repo,
        store_format: STORE_FORMAT,
        end: base,
        end_batch: BatchMarker::of(&batches[i - 1]),
        digest: Hash([9; 32]),
    };
    let inc = header(end, DumpChainHeader::after(&prev, last));
    let (_, read, _) = read_all(&write(&inc, &batches[i..], facts)).unwrap();
    assert_eq!(read, &batches[i..]);
    let mut suffix = vec![];
    r.for_each_batch_after(base, end, &mut |b| {
        suffix.push(b);
        Ok(())
    })
    .unwrap();
    assert_eq!(suffix, &batches[i..]);
    // a rollover to any later segment, or to a later offset, does not follow
    for start in [
        at(base.segment + 2, 0),
        at(base.segment + 1, 1),
        at(base.segment, base.offset + 1),
    ] {
        let mut b = batches[i].clone();
        b.start = start;
        let mut moved = prev;
        moved.end = base;
        let h = header(
            batch_end(&b).unwrap(),
            DumpChainHeader::after(&moved, BatchMarker::of(&b)),
        );
        let e = read_all(&write(&h, &[b], facts)).unwrap_err();
        assert_eq!(
            damaged(e),
            "the first batch does not follow the base",
            "{start:?}"
        );
    }

    // the store walk: a base or an end that is not a batch end is refused
    let mid = batches.iter().find(|b| b.frames.len() >= 2).unwrap();
    let inside = at(
        mid.start.segment,
        mid.start.offset + HEAD as u64 + u64::from(mid.frames[0].original_len()),
    );
    let e = r
        .for_each_batch_after(inside, end, &mut |_| Ok(()))
        .unwrap_err();
    assert!(matches!(e, StoreError::Decode(_)), "{e}");
    let e = r.batch_marker_at(inside).unwrap_err();
    assert!(
        matches!(&e, StoreError::Decode(m) if m.contains("does not end at a batch marker")),
        "{e}"
    );
    let e = r
        .batch_marker_at(at(end.segment, end.offset - 1))
        .unwrap_err();
    assert!(!matches!(e, StoreError::Dump { .. }), "{e}");
}

/// A reader that counts the bytes it hands out and the seeks it makes.
struct Counting<R> {
    inner: R,
    read: u64,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        Ok(n)
    }
}

impl<R: Seek> Seek for Counting<R> {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(to)
    }
}

#[test]
fn hc1_envelope_reads_only_header_and_trailer() {
    let (_dir, s, repo, batches) = rolled_store();
    let end = s.log_end();
    let last = BatchMarker::of(batches.last().unwrap());
    let full = header(end, DumpChainHeader::full(repo, last));
    let mut w = DumpWriter::new(Vec::new(), &full).unwrap();
    w.file(".rdy.yaml", b"project: t\n").unwrap();
    for b in &batches {
        w.batch(b).unwrap();
    }
    let (mut bytes, totals) = w.finish(RemovalFacts::default()).unwrap();
    // damage the body: the whole reader refuses it, the envelope never reads it
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    assert!(read_all(&bytes).is_err());
    let mut r = Counting {
        inner: Cursor::new(&bytes),
        read: 0,
    };
    let (format, h, t, len) = read_envelope(&mut r).unwrap();
    assert_eq!((format, len), (3, bytes.len() as u64));
    assert_eq!(h, full);
    assert_eq!(
        t,
        DumpTrailer {
            batches: totals.batches,
            frames: totals.frames,
            files: 1,
            removals: 0,
            first_removed: None,
            digest: totals.digest,
        }
    );
    assert_eq!(
        r.read,
        12 + (HEAD + 179) as u64 + 106,
        "prelude, header, trailer"
    );
    // a damaged trailer or header record is refused by its own hash
    let mut b = bytes.clone();
    let tail = b.len() - 1;
    b[tail] ^= 1;
    let e = read_envelope(&mut Cursor::new(&b)).unwrap_err();
    assert_eq!(damaged(e), "a record's hash does not match its bytes");
    let mut b = bytes.clone();
    b[12 + HEAD + 2] ^= 1;
    let e = read_envelope(&mut Cursor::new(&b)).unwrap_err();
    assert_eq!(damaged(e), "a record's hash does not match its bytes");
    // a file shorter than its envelope
    let e = read_envelope(&mut Cursor::new(&bytes[..12 + HEAD + 179 + 50])).unwrap_err();
    assert_eq!(damaged(e), "the file ends before its trailer");
    // a last record that is not a trailer of its fixed length
    let mut b = bytes.clone();
    let t = trailer_at(&b);
    let p = b[t + HEAD..].to_vec();
    set_record(&mut b, t, &p[..68]);
    let e = read_envelope(&mut Cursor::new(&b)).unwrap_err();
    assert_eq!(damaged(e), "the trailer does not parse");
    // not a dump
    let e = read_envelope(&mut Cursor::new(b"RDYDUMQ\n\x03\x00\x00\x00")).unwrap_err();
    assert_eq!(damaged(e), "it does not start with RDYDUMP");
}

#[test]
fn hc1_large_lengths_flags_and_offsets_refuse() {
    let repo = RepoId([1; 16]);
    let empty = header(at(0, 0), DumpChainHeader::full(repo, BatchMarker::ZERO));
    let none = RemovalFacts::default();
    // the rdy text: 256 bytes, never 257
    let mut h = empty.clone();
    h.rdy = "r".repeat(256);
    let bytes = write(&h, &[], none);
    assert_eq!(read_all(&bytes).unwrap().0, h);
    h.rdy.push('r');
    let e = DumpWriter::new(Vec::new(), &h).err().expect("refused");
    assert_eq!(refused(e), "the header does not parse");
    let mut p = bytes[12 + HEAD..12 + HEAD + 175 + 256].to_vec();
    let n = p.len();
    p[n - 258..n - 256].copy_from_slice(&257u16.to_le_bytes());
    p.push(b'r');
    let mut b = bytes.clone();
    set_record(&mut b, 12, &p);
    assert_eq!(
        damaged(DumpReader::open(&b[..]).err().unwrap()),
        "the header does not parse"
    );
    // a header payload one byte short or long
    let bytes = write(&empty, &[], none);
    let p = bytes[12 + HEAD..12 + HEAD + 179].to_vec();
    for p in [p[..178].to_vec(), [&p[..], &[0]].concat()] {
        let mut b = bytes.clone();
        set_record(&mut b, 12, &p);
        assert_eq!(
            damaged(DumpReader::open(&b[..]).err().unwrap()),
            "the header does not parse"
        );
    }
    // the trailer: 69 bytes, a flag of 0 (zero position) or 1
    let t = trailer_at(&bytes);
    let tp = bytes[t + HEAD..].to_vec();
    assert_eq!(tp.len(), 69);
    let trailer_bad = |p: &[u8]| {
        let mut b = bytes.clone();
        set_record(&mut b, t, p);
        damaged(read_all(&b).unwrap_err())
    };
    assert_eq!(trailer_bad(&tp[..68]), "the trailer does not parse");
    assert_eq!(
        trailer_bad(&[&tp[..], &[0]].concat()),
        "the trailer does not parse"
    );
    let mut p = tp.clone();
    p[24] = 2;
    assert_eq!(trailer_bad(&p), "the trailer does not parse");
    let mut p = tp.clone();
    p[25] = 1; // flag 0 with a nonzero segment
    assert_eq!(trailer_bad(&p), "the trailer does not parse");
    let mut p = tp.clone();
    p[36] = 1; // flag 0 with a nonzero offset
    assert_eq!(trailer_bad(&p), "the trailer does not parse");
    // the removal facts: the dump's apply count, and no place without an apply
    let mut p = tp.clone();
    p[20] = 1;
    assert_eq!(
        trailer_bad(&p),
        "the removal facts do not match the batches"
    );
    let mut p = tp.clone();
    p[24] = 1;
    assert_eq!(
        trailer_bad(&p),
        "the removal facts do not match the batches"
    );
    let facts = |removals, first_removed| RemovalFacts {
        removals,
        first_removed,
    };
    let e = read_all(&write(&empty, &[], facts(1, None))).unwrap_err();
    assert_eq!(damaged(e), "the removal facts do not match the batches");
    let e = read_all(&write(&empty, &[], facts(0, Some(at(0, 0))))).unwrap_err();
    assert_eq!(damaged(e), "the removal facts do not match the batches");

    // a batch's physical end: exactly u64::MAX is a place, one more overflows
    let frame = |payload: &[u8]| Frame::Present {
        kind: kinds::BLOB_CHUNK,
        hash: Hash::of(payload),
        payload: payload.to_vec(),
    };
    let batch_at = |offset: u64, payload: &[u8]| {
        let frames = vec![frame(payload)];
        DumpBatch {
            start: at(0, offset),
            digest: kinds::batch_digest(frames.iter().map(|f| f.hash())),
            frames,
        }
    };
    let probe = batch_at(1 << 63, b"x");
    let span = batch_end(&probe).unwrap().offset - (1 << 63);
    let ok = batch_at(u64::MAX - span, b"x");
    assert_eq!(batch_end(&ok), Some(at(0, u64::MAX)));
    let over = batch_at(u64::MAX - span + 1, b"x");
    assert_eq!(batch_end(&over), None);
    let inc = |b: &DumpBatch, end: Location| {
        let prev = ChainEnd {
            repo_id: repo,
            store_format: STORE_FORMAT,
            end: b.start,
            end_batch: marker(0, 0, 1, 7),
            digest: Hash([3; 32]),
        };
        header(end, DumpChainHeader::after(&prev, BatchMarker::of(b)))
    };
    let (_, read, _) = read_all(&write(
        &inc(&ok, at(0, u64::MAX)),
        std::slice::from_ref(&ok),
        none,
    ))
    .unwrap();
    assert_eq!(read, [ok]);
    let e = read_all(&write(
        &inc(&over, at(0, u64::MAX)),
        std::slice::from_ref(&over),
        none,
    ))
    .unwrap_err();
    assert_eq!(damaged(e), "dump position overflows");
    // the tally: a dropped place near u64::MAX is a place, one past it overflows
    let request = request_bytes(vec![chunk_span(at(0, u64::MAX - 42), 1)]);
    let apply = apply_bytes(&request);
    let near = |offset: u64, len: u32| DumpBatch {
        start: at(0, offset),
        digest: kinds::batch_digest([Hash([1; 32])].iter()),
        frames: vec![Frame::Dropped {
            kind: kinds::BLOB_CHUNK,
            hash: Hash([1; 32]),
            len,
            apply: Hash::of(&apply),
            residue: None,
        }],
    };
    let mut f = RemovalTally::default();
    f.observe(&batch_of(
        at(0, 0),
        vec![
            present(kinds::REDACTION, &request),
            present(kinds::REDACTION_APPLY, &apply),
        ],
    ))
    .unwrap();
    f.observe(&near(u64::MAX - 42, 5)).unwrap();
    assert_eq!(f.facts().unwrap().first_removed, Some(at(0, u64::MAX - 42)));
    let e = RemovalTally::default()
        .observe(&near(u64::MAX - 41, 5))
        .unwrap_err();
    assert_eq!(refused(e), "dump position overflows");
    // the first base segment has no successor past u32::MAX
    let mut b = batch_at(0, b"x");
    b.start = at(0, 0);
    let mut h = inc(&b, batch_end(&b).unwrap());
    if let Some(c) = h.chain.as_mut() {
        c.base = at(u32::MAX, 5);
    }
    let e = read_all(&write(&h, &[b], none)).unwrap_err();
    assert_eq!(damaged(e), "the first batch does not follow the base");

    // the carried record's bounds are HA1's decoder's
    let carried = |actor: String, spans: usize| RedactionCarried {
        request: Hash([1; 32]),
        apply: Hash([2; 32]),
        landing: Hash([3; 32]),
        approved_by: Provenance {
            actor,
            run: None,
            role: Role::Approver,
            at_ms: 0,
        },
        reason_class: ReasonClass::Secret,
        targets: vec![],
        spans: (0..spans)
            .map(|i| CarriedSpan {
                place: FramePlace {
                    segment: 0,
                    offset: i as u64,
                },
                kind: kinds::BLOB_CHUNK,
                hash: Hash([4; 32]),
                len: 1,
                residue: None,
                held: false,
            })
            .collect(),
    };
    let decode = |c: &RedactionCarried| {
        bounded::decode::<RedactionCarried>(&postcard::to_allocvec(c).unwrap())
    };
    let c = carried("a".repeat(bounded::MAX_TEXT_BYTES), 0);
    assert_eq!(decode(&c), Ok(c));
    assert_eq!(
        decode(&carried("a".repeat(bounded::MAX_TEXT_BYTES + 1), 0)),
        Err(RecordError::Limit {
            name: bounded::TEXT_BYTES,
            maximum: 1_048_576
        })
    );
    let c = carried("p".into(), bounded::MAX_ELEMENTS);
    assert_eq!(decode(&c), Ok(c));
    assert_eq!(
        decode(&carried("p".into(), bounded::MAX_ELEMENTS + 1)),
        Err(RecordError::Limit {
            name: bounded::COLLECTION_ELEMENTS,
            maximum: 100_000
        })
    );
    let max = vec![0u8; bounded::MAX_RECORD_BYTES];
    assert_eq!(
        bounded::decode::<RedactionCarried>(&max),
        Err(RecordError::Decode),
        "16777216 bytes are read, and do not decode"
    );
    let over = vec![0u8; bounded::MAX_RECORD_BYTES + 1];
    assert_eq!(
        bounded::decode::<RedactionCarried>(&over),
        Err(RecordError::Limit {
            name: bounded::RECORD_BYTES,
            maximum: 16_777_216
        })
    );
    // decoded values: a target is 3 (an enum and a hash), a span with a residue 7
    let mut c = carried("p".into(), 0);
    c.targets = vec![RedactTarget::Blob(Hash([5; 32])); bounded::MAX_ELEMENTS];
    let span = CarriedSpan {
        place: FramePlace {
            segment: 0,
            offset: 0,
        },
        kind: kinds::BLOCK,
        hash: Hash([6; 32]),
        len: 1,
        residue: Some(Hash([7; 32])),
        held: true,
    };
    let fits = |n: usize| {
        let mut c = c.clone();
        c.spans = vec![span.clone(); n];
        decode(&c)
    };
    // 100000 targets are 300000 values and each span 7, so 99998 spans fit and 99999 do not
    assert!(fits(99_998).is_ok());
    assert_eq!(
        fits(99_999),
        Err(RecordError::Limit {
            name: bounded::DECODED_VALUES,
            maximum: 1_000_000
        })
    );
}

#[test]
fn hc1_carried_encoding_adds_no_existing_bytes() {
    let c = RedactionCarried {
        request: Hash([1; 32]),
        apply: Hash([2; 32]),
        landing: Hash([3; 32]),
        approved_by: Provenance {
            actor: "person:ann".into(),
            run: None,
            role: Role::Approver,
            at_ms: 5,
        },
        reason_class: ReasonClass::Personal,
        targets: vec![RedactTarget::Blob(Hash([4; 32]))],
        spans: vec![
            CarriedSpan {
                place: FramePlace {
                    segment: 1,
                    offset: 300,
                },
                kind: kinds::BLOB_CHUNK,
                hash: Hash([5; 32]),
                len: 1000,
                residue: None,
                held: true,
            },
            CarriedSpan {
                place: FramePlace {
                    segment: 0,
                    offset: 5,
                },
                kind: kinds::BLOCK,
                hash: Hash([6; 32]),
                len: 7,
                residue: Some(Hash([7; 32])),
                held: false,
            },
        ],
    };
    // postcard keeps the plain field bytes, in declaration order
    let mut want = vec![];
    want.extend_from_slice(&[1; 32]);
    want.extend_from_slice(&[2; 32]);
    want.extend_from_slice(&[3; 32]);
    want.push(10);
    want.extend_from_slice(b"person:ann");
    want.push(0); // run: none
    want.push(3); // Role::Approver
    want.push(10); // at_ms 5, zigzag
    want.push(1); // ReasonClass::Personal
    want.push(1); // one target
    want.push(2); // RedactTarget::Blob
    want.extend_from_slice(&[4; 32]);
    want.push(2); // two spans
    want.extend_from_slice(&[1, 0xAC, 0x02, 6]);
    want.extend_from_slice(&[5; 32]);
    want.extend_from_slice(&[0xE8, 0x07, 0, 1]);
    want.extend_from_slice(&[0, 5, 1]);
    want.extend_from_slice(&[6; 32]);
    want.extend_from_slice(&[7, 1]);
    want.extend_from_slice(&[7; 32]);
    want.push(0);
    let bytes = postcard::to_allocvec(&c).unwrap();
    assert_eq!(bytes, want);
    assert_eq!(bounded::decode::<RedactionCarried>(&bytes), Ok(c.clone()));
    // JSON: full lowercase hashes, a missing residue is null
    let j = serde_json::to_value(&c).unwrap();
    assert_eq!(j["request"], "01".repeat(32));
    assert_eq!(j["apply"], "02".repeat(32));
    assert_eq!(j["landing"], "03".repeat(32));
    assert_eq!(j["reason_class"], "personal");
    assert_eq!(j["targets"][0]["blob"], "04".repeat(32));
    assert_eq!(j["spans"][0]["hash"], "05".repeat(32));
    assert_eq!(j["spans"][0]["residue"], serde_json::Value::Null);
    assert_eq!(j["spans"][1]["residue"], "07".repeat(32));
    assert_eq!(j["spans"][1]["held"], false);
    assert_eq!(
        serde_json::from_value::<RedactionCarried>(j.clone()).unwrap(),
        c
    );
    // no reason text and no time of its own; unknown fields are refused
    assert!(j.get("reason").is_none() && j.get("at_ms").is_none());
    let mut extra = j.clone();
    extra["reason"] = "x".into();
    assert!(serde_json::from_value::<RedactionCarried>(extra).is_err());
    let mut extra = j["spans"][0].clone();
    extra["apply"] = "x".into();
    assert!(serde_json::from_value::<CarriedSpan>(extra).is_err());

    // the existing redaction records keep their bytes
    assert_eq!(
        (
            kinds::REDACTION,
            kinds::REDACTION_RESIDUE,
            kinds::REDACTION_APPLY
        ),
        (20, 21, 22)
    );
    let apply = RedactionApply {
        request: Hash([1; 32]),
        landing: Hash([2; 32]),
        report_hash: Hash([3; 32]),
    };
    assert_eq!(
        postcard::to_allocvec(&apply).unwrap(),
        [[1u8; 32], [2; 32], [3; 32]].concat()
    );
    let span = DropSpan {
        place: FramePlace {
            segment: 1,
            offset: 300,
        },
        kind: kinds::BLOCK,
        hash: Hash([5; 32]),
        len: 1000,
        residue: Some(Hash([6; 32])),
    };
    let mut want = vec![1, 0xAC, 0x02, 1];
    want.extend_from_slice(&[5; 32]);
    want.extend_from_slice(&[0xE8, 0x07, 1]);
    want.extend_from_slice(&[6; 32]);
    assert_eq!(postcard::to_allocvec(&span).unwrap(), want);

    // the chain facts in JSON: lowercase hashes, a 32-digit repo id, a lowercase kind
    let prev = ChainEnd {
        repo_id: RepoId([0xAB; 16]),
        store_format: STORE_FORMAT,
        end: at(1, 2),
        end_batch: marker(1, 0, 3, 0xCD),
        digest: Hash([0xEF; 32]),
    };
    let j = serde_json::to_value(prev).unwrap();
    assert_eq!(j["repo_id"], "ab".repeat(16));
    assert_eq!(j["end_batch"]["digest"], "cd".repeat(32));
    assert_eq!(j["digest"], "ef".repeat(32));
    assert_eq!(serde_json::from_value::<ChainEnd>(j).unwrap(), prev);
    let h = DumpChainHeader::after(&prev, marker(2, 0, 1, 1));
    let j = serde_json::to_value(h).unwrap();
    assert_eq!(j["kind"], "increment");
    assert_eq!(j["prev"], "ef".repeat(32));
    assert_eq!(
        serde_json::to_value(DumpKind::Full).unwrap(),
        serde_json::json!("full")
    );
}

/// A present frame of `kind` holding `payload`, with its hash.
fn present(kind: u8, payload: &[u8]) -> Frame {
    Frame::Present {
        kind,
        hash: Hash::of(payload),
        payload: payload.to_vec(),
    }
}

/// A batch at `start` holding `frames`, with its digest.
fn batch_of(start: Location, frames: Vec<Frame>) -> DumpBatch {
    DumpBatch {
        start,
        digest: kinds::batch_digest(frames.iter().map(|f| f.hash())),
        frames,
    }
}

/// The header of a full dump of `batches`: it ends where the last one ends, with its marker.
fn full_of(batches: &[DumpBatch]) -> DumpHeader {
    let last = batches.last().expect("a batch");
    header(
        batch_end(last).unwrap(),
        DumpChainHeader::full(RepoId([4; 16]), BatchMarker::of(last)),
    )
}

/// The increment of `batches` after `base`, the end of a batch of `base_batch`.
fn increment_of(base: Location, base_batch: BatchMarker, batches: &[DumpBatch]) -> DumpHeader {
    let last = batches.last().expect("a batch");
    let prev = ChainEnd {
        repo_id: RepoId([4; 16]),
        store_format: STORE_FORMAT,
        end: base,
        end_batch: base_batch,
        digest: Hash([8; 32]),
    };
    header(
        batch_end(last).unwrap(),
        DumpChainHeader::after(&prev, BatchMarker::of(last)),
    )
}

/// A dropped chunk of 10 bytes whose authority (the DROPPED record's apply field) is `authority`.
fn hole(n: u8, authority: Hash) -> Frame {
    Frame::Dropped {
        kind: kinds::BLOB_CHUNK,
        hash: Hash([n; 32]),
        len: 10,
        apply: authority,
        residue: None,
    }
}

/// A carried record's bytes: approved by `actor`, accounting for one zero span.
fn carried_bytes(actor: String) -> Vec<u8> {
    postcard::to_allocvec(&RedactionCarried {
        request: Hash([1; 32]),
        apply: Hash([2; 32]),
        landing: Hash([3; 32]),
        approved_by: Provenance {
            actor,
            run: None,
            role: Role::Approver,
            at_ms: 0,
        },
        reason_class: ReasonClass::Secret,
        targets: vec![RedactTarget::Blob(Hash([5; 32]))],
        spans: vec![CarriedSpan {
            place: FramePlace {
                segment: 0,
                offset: 0,
            },
            kind: kinds::BLOB_CHUNK,
            hash: Hash([1; 32]),
            len: 10,
            residue: None,
            held: false,
        }],
    })
    .unwrap()
}

/// A full dump built by hand, record by record, of one batch at the origin whose one frame is a
/// present frame of `kind` holding `payload`; its trailer names no removal. The writer refuses
/// some of what this forges.
fn forge_one(kind: u8, payload: &[u8]) -> Vec<u8> {
    let b = batch_of(at(0, 0), vec![present(kind, payload)]);
    let empty = write(
        &full_of(std::slice::from_ref(&b)),
        &[],
        RemovalFacts::default(),
    );
    let mut out = empty[..trailer_at(&empty)].to_vec();
    let mut chain = vec![];
    let mut add = |out: &mut Vec<u8>, kind: u8, p: &[u8]| {
        chain.push(kind);
        chain.extend_from_slice(&(p.len() as u64).to_le_bytes());
        chain.extend_from_slice(&Hash::of(p).0);
        out.extend_from_slice(&(p.len() as u32).to_le_bytes());
        out.push(kind);
        out.extend_from_slice(&Hash::of(p).0);
        out.extend_from_slice(p);
    };
    let head = out[12 + HEAD..].to_vec();
    out.truncate(12);
    add(&mut out, rec::HEADER, &head);
    let mut kinds_of = vec![kind];
    kinds_of.extend_from_slice(&Hash::of(payload).0);
    let mut p = 0u32.to_le_bytes().to_vec();
    p.extend_from_slice(&0u64.to_le_bytes());
    p.extend_from_slice(&1u32.to_le_bytes());
    p.extend_from_slice(&b.digest.0);
    p.extend_from_slice(&Hash::of(&kinds_of).0);
    add(&mut out, rec::BATCH, &p);
    add(&mut out, kind, payload);
    let mut t = 1u64.to_le_bytes().to_vec();
    t.extend_from_slice(&1u64.to_le_bytes());
    t.extend_from_slice(&[0u8; 4 + 4 + 1 + 12]);
    t.extend_from_slice(&Hash::of(&chain).0);
    out.extend_from_slice(&(t.len() as u32).to_le_bytes());
    out.push(rec::TRAILER);
    out.extend_from_slice(&Hash::of(&t).0);
    out.extend_from_slice(&t);
    out
}

/// A request's bytes whose manifest lists `spans` (it removes a blob's chunks).
fn request_bytes(spans: Vec<DropSpan>) -> Vec<u8> {
    postcard::to_allocvec(&Redaction {
        carrier: Hash([6; 32]),
        actor: Provenance {
            actor: "agent:w".into(),
            run: None,
            role: Role::Writer,
            at_ms: 0,
        },
        targets: vec![RedactTarget::Blob(Hash([5; 32]))],
        reason_class: ReasonClass::Secret,
        reason: "a test".into(),
        manifest: RedactionManifest {
            versions: vec![],
            blobs: vec![Hash([5; 32])],
            shared_blobs: vec![],
            spans,
            inside: vec![],
            outside: vec![],
            quarantine: vec![],
            assessment: AssessmentSupport::NotImplemented,
        },
    })
    .unwrap()
}

/// The bytes of an apply of the request whose bytes are `request`.
fn apply_bytes(request: &[u8]) -> Vec<u8> {
    postcard::to_allocvec(&RedactionApply {
        request: Hash::of(request),
        landing: Hash([2; 32]),
        report_hash: Hash([3; 32]),
    })
    .unwrap()
}

/// A manifest span at `place` of the 10-byte chunk `[n; 32]` (as [`hole`] drops it).
fn chunk_span(place: Location, n: u8) -> DropSpan {
    DropSpan {
        place: FramePlace {
            segment: place.segment,
            offset: place.offset,
        },
        kind: kinds::BLOB_CHUNK,
        hash: Hash([n; 32]),
        len: 10,
        residue: None,
    }
}

/// The ref frame that makes a log a holder (in its first batch).
fn holder_ref() -> Frame {
    present(
        kinds::REF_UPDATE,
        &postcard::to_allocvec(&kinds::RefUpdate {
            name: "meta/store_mode".into(),
            value: b"holder".to_vec(),
            at: at(0, 0),
        })
        .unwrap(),
    )
}

/// The place of frame `i` of `b`.
fn place_of(b: &DumpBatch, i: usize) -> Location {
    let off = b.frames[..i].iter().fold(b.start.offset, |a, f| {
        a + HEAD as u64 + u64::from(f.original_len())
    });
    at(b.start.segment, off)
}

#[test]
fn hc1_removal_facts_come_from_the_resolved_spans_of_the_files_applies() {
    let removals = "the removal facts do not match the batches";
    let facts = |removals, first_removed| RemovalFacts {
        removals,
        first_removed,
    };
    let tally = |batches: &[DumpBatch]| {
        let mut t = RemovalTally::default();
        for b in batches {
            t.observe(b).unwrap();
        }
        t.facts().unwrap()
    };
    let carried = carried_bytes("person:ann".into());
    // a hole that only a carried record explains, then that record: no apply, so no span
    let b1 = batch_of(at(0, 0), vec![hole(1, Hash::of(&carried))]);
    let b2 = batch_of(
        batch_end(&b1).unwrap(),
        vec![present(kinds::REDACTION_CARRIED, &carried)],
    );
    let carried_only = [b1.clone(), b2.clone()];
    let h = full_of(&carried_only);
    assert_eq!(tally(&carried_only), facts(0, None));
    let (_, read, t) = read_all(&write(&h, &carried_only, facts(0, None))).unwrap();
    assert_eq!(read, carried_only);
    assert_eq!((t.removals, t.first_removed), (0, None));
    let e = read_all(&write(&h, &carried_only, facts(0, Some(b1.start)))).unwrap_err();
    assert_eq!(damaged(e), removals, "a carried hole is no removal");

    // a project: hole 2 names another proof (an apply of no file here) and the request of the
    // file's apply lists it; hole 3 names the file's apply, which does not list it. The
    // request's resolved span counts; the DROPPED record's authority is not the rule.
    let s3 = batch_end(&b2).unwrap();
    let h2 = at(s3.segment, s3.offset);
    let h3 = at(s3.segment, s3.offset + HEAD as u64 + 10);
    let request = request_bytes(vec![chunk_span(h2, 2)]);
    let apply = apply_bytes(&request);
    let b3 = batch_of(s3, vec![hole(2, Hash([9; 32])), hole(3, Hash::of(&apply))]);
    assert_eq!((place_of(&b3, 0), place_of(&b3, 1)), (h2, h3));
    let b4 = batch_of(
        batch_end(&b3).unwrap(),
        vec![present(kinds::REDACTION, &request)],
    );
    let b5 = batch_of(
        batch_end(&b4).unwrap(),
        vec![present(kinds::REDACTION_APPLY, &apply)],
    );
    let project = [b1.clone(), b2.clone(), b3.clone(), b4.clone(), b5.clone()];
    let h = full_of(&project);
    assert_eq!(tally(&project), facts(1, Some(h2)));
    let (_, _, t) = read_all(&write(&h, &project, facts(1, Some(h2)))).unwrap();
    assert_eq!((t.removals, t.first_removed), (1, Some(h2)));
    for bad in [
        facts(1, Some(h3)),
        facts(1, Some(b1.start)),
        facts(1, None),
        facts(0, Some(h2)),
        facts(2, Some(h2)),
    ] {
        let e = read_all(&write(&h, &project, bad)).unwrap_err();
        assert_eq!(damaged(e), removals, "{bad:?}");
    }
    // each physical apply counts; the lowest resolved span is still the first removed
    let b6 = batch_of(
        batch_end(&b5).unwrap(),
        vec![present(kinds::REDACTION_APPLY, &apply)],
    );
    let twice = [
        b1.clone(),
        b2.clone(),
        b3.clone(),
        b4.clone(),
        b5.clone(),
        b6,
    ];
    assert_eq!(tally(&twice), facts(2, Some(h2)));
    read_all(&write(&full_of(&twice), &twice, facts(2, Some(h2)))).unwrap();
    // an apply whose request no file here holds resolves no span in a full dump
    let other = apply_bytes(&request_bytes(vec![chunk_span(h3, 3)]));
    let b5x = batch_of(
        batch_end(&b4).unwrap(),
        vec![present(kinds::REDACTION_APPLY, &other)],
    );
    let unknown = [b1.clone(), b2.clone(), b3.clone(), b4.clone(), b5x];
    assert_eq!(tally(&unknown), facts(1, None));
    read_all(&write(&full_of(&unknown), &unknown, facts(1, None))).unwrap();

    // a holder with overlapping authorities: its request lists the key of hole 4 at a source
    // place, which means nothing here; every local copy of that key is the apply's span (HA1's
    // logical rule), whatever proof each DROPPED record names. The carried hole stays out.
    let source_place = at(7, 99);
    let hrequest = request_bytes(vec![chunk_span(source_place, 4), chunk_span(at(7, 200), 8)]);
    let happly = apply_bytes(&hrequest);
    let hb0 = batch_of(at(0, 0), vec![holder_ref(), hole(1, Hash::of(&carried))]);
    let hb1 = batch_of(
        batch_end(&hb0).unwrap(),
        vec![present(kinds::REDACTION_CARRIED, &carried)],
    );
    // the first copy names another proof, an earlier apply of no file here; a later copy names
    // the file's apply; hole 5 names the file's apply, which does not approve it
    let hb2 = batch_of(
        batch_end(&hb1).unwrap(),
        vec![hole(5, Hash::of(&happly)), hole(4, Hash([9; 32]))],
    );
    let first_copy = place_of(&hb2, 1);
    let hb3 = batch_of(batch_end(&hb2).unwrap(), vec![hole(4, Hash::of(&happly))]);
    let hb4 = batch_of(
        batch_end(&hb3).unwrap(),
        vec![present(kinds::REDACTION, &hrequest)],
    );
    let hb5 = batch_of(
        batch_end(&hb4).unwrap(),
        vec![present(kinds::REDACTION_APPLY, &happly)],
    );
    let holder = [hb0.clone(), hb1, hb2.clone(), hb3, hb4, hb5];
    let h = full_of(&holder);
    assert_eq!(tally(&holder), facts(1, Some(first_copy)));
    read_all(&write(&h, &holder, facts(1, Some(first_copy)))).unwrap();
    for bad in [
        facts(1, Some(source_place)),
        facts(1, Some(place_of(&hb2, 0))),
        facts(1, Some(place_of(&hb0, 1))),
        facts(1, None),
    ] {
        let e = read_all(&write(&h, &holder, bad)).unwrap_err();
        assert_eq!(damaged(e), removals, "{bad:?}");
    }

    // an increment after b2: the resolved span, or a span before its base that an earlier file
    // holds
    let tail = [b3.clone(), b4.clone(), b5.clone()];
    let inc = increment_of(batch_end(&b2).unwrap(), BatchMarker::of(&b2), &tail);
    for ok in [facts(1, Some(h2)), facts(1, Some(b1.start))] {
        read_all(&write(&inc, &tail, ok)).unwrap();
    }
    for bad in [facts(1, Some(h3)), facts(1, None), facts(0, Some(b1.start))] {
        let e = read_all(&write(&inc, &tail, bad)).unwrap_err();
        assert_eq!(damaged(e), removals, "{bad:?}");
    }
    // an increment whose apply's request an earlier file holds: the chain resolves its spans,
    // this file checks its count
    let tail = [b5.clone()];
    let inc = increment_of(batch_end(&b4).unwrap(), BatchMarker::of(&b4), &tail);
    read_all(&write(&inc, &tail, facts(1, Some(h2)))).unwrap();
    let e = read_all(&write(&inc, &tail, facts(0, None))).unwrap_err();
    assert_eq!(damaged(e), removals);
}

#[test]
fn hc1_a_suffix_walk_starts_and_ends_at_frame_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    s.put_blob(b"before").unwrap();
    // a blob chunk whose payload holds a hash-valid frame and the marker that closes it
    let base = s.log_end();
    let pad = 16u64;
    let inner_at = at(base.segment, base.offset + HEAD as u64 + pad);
    let inner = b"a frame inside a payload";
    let marker = postcard::to_allocvec(&kinds::BatchCommit {
        start: inner_at,
        frames: 1,
        digest: kinds::batch_digest([&Hash::of(inner)]),
    })
    .unwrap();
    let mut payload = vec![0u8; pad as usize];
    for (kind, p) in [
        (kinds::BLOB_CHUNK, &inner[..]),
        (kinds::BATCH_COMMIT, &marker[..]),
    ] {
        payload.extend_from_slice(&(p.len() as u32).to_le_bytes());
        payload.push(kind);
        payload.extend_from_slice(&Hash::of(p).0);
        payload.extend_from_slice(p);
    }
    let inner_end = at(
        base.segment,
        inner_at.offset + 2 * HEAD as u64 + (inner.len() + marker.len()) as u64,
    );
    payload.extend_from_slice(&[0u8; 8]);
    s.write(vec![(kinds::BLOB_CHUNK, payload)]).unwrap();
    s.put_blob(b"after").unwrap();
    let end = s.log_end();
    let r = s.reader();
    let mut real = vec![];
    r.for_each_batch(end, |b| {
        real.push(b);
        Ok(())
    })
    .unwrap();
    assert!(
        real.iter().all(|b| b.start != inner_at),
        "no real batch there"
    );
    // the marker-shaped bytes are no batch end, and no batch starts inside the payload
    assert!(r.batch_marker_at(inner_end).is_err());
    assert!(r.batch_marker_at(inner_at).is_err());
    let cases = [(inner_at, inner_end), (inner_at, end), (base, inner_end)];
    for (from, to) in cases {
        let mut seen = 0;
        let e = r.for_each_batch_after(from, to, &mut |_| {
            seen += 1;
            Ok(())
        });
        assert!(e.is_err(), "{from:?}..{to:?}");
        assert_eq!(
            seen, 0,
            "{from:?}..{to:?}: nothing visited before the check"
        );
    }
    // the real boundaries still walk
    let mut suffix = vec![];
    r.for_each_batch_after(base, end, &mut |b| {
        suffix.push(b);
        Ok(())
    })
    .unwrap();
    let from = real.iter().position(|b| b.start == base).unwrap();
    assert_eq!(suffix, real[from..]);
    // an end before the base is refused, an equal one visits nothing
    assert!(r.for_each_batch_after(end, base, &mut |_| Ok(())).is_err());
    let mut seen = 0;
    r.for_each_batch_after(base, base, &mut |_| {
        seen += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(seen, 0);
}

#[test]
fn hc1_carried_records_are_decoded_within_bounds_on_read() {
    let none = RemovalFacts::default();
    // the approver's text: 1048576 bytes read, one more is refused at the frame
    let fits = carried_bytes("a".repeat(bounded::MAX_TEXT_BYTES));
    let b = batch_of(at(0, 0), vec![present(kinds::REDACTION_CARRIED, &fits)]);
    let bs = [b];
    let (_, read, _) = read_all(&write(&full_of(&bs), &bs, none)).unwrap();
    assert_eq!(read, bs);
    assert_eq!(
        read_all(&forge_one(kinds::REDACTION_CARRIED, &fits))
            .unwrap()
            .1,
        bs
    );
    let over = carried_bytes("a".repeat(bounded::MAX_TEXT_BYTES + 1));
    let e = read_all(&forge_one(kinds::REDACTION_CARRIED, &over)).unwrap_err();
    assert!(
        matches!(&e, StoreError::Dump { at, .. } if *at == (12 + HEAD + 179 + HEAD + 80) as u64),
        "blamed on the frame: {e:?}"
    );
    assert_eq!(damaged(e), "1048576 text bytes");
    // an empty or truncated record does not decode
    for p in [&[][..], &fits[..fits.len() - 1]] {
        let e = read_all(&forge_one(kinds::REDACTION_CARRIED, p)).unwrap_err();
        assert_eq!(damaged(e), "record does not decode", "{} bytes", p.len());
    }
    // the writer refuses what the reader refuses, before it writes a byte
    for (p, want) in [
        (over, "1048576 text bytes"),
        (vec![], "record does not decode"),
    ] {
        let b = batch_of(at(0, 0), vec![present(kinds::REDACTION_CARRIED, &p)]);
        let mut w = DumpWriter::new(Vec::new(), &full_of(std::slice::from_ref(&b))).unwrap();
        assert_eq!(refused(w.batch(&b).unwrap_err()), want);
        assert_eq!(w.finish(none).unwrap().1.batches, 0);
    }
    // the released kinds keep their limits: a record of another kind is not decoded on read
    let bs = [batch_of(
        at(0, 0),
        vec![present(kinds::REDACTION, b"not a request")],
    )];
    read_all(&write(&full_of(&bs), &bs, none)).unwrap();
}

#[test]
fn hc1_a_record_payload_holds_at_most_64_mib() {
    let ceiling = 67_108_864usize;
    let none = RemovalFacts::default();
    // exactly the ceiling: written and read back
    let max = vec![7u8; ceiling];
    let bs = [batch_of(at(0, 0), vec![present(kinds::BLOB_CHUNK, &max)])];
    let bytes = write(&full_of(&bs), &bs, none);
    let (_, read, _) = read_all(&bytes).unwrap();
    assert_eq!(read[0].frames[0].original_len() as usize, ceiling);
    drop((bytes, read, bs));
    // one more: the writer refuses it before it writes a byte
    let over = vec![7u8; ceiling + 1];
    let b = batch_of(at(0, 0), vec![present(kinds::BLOB_CHUNK, &over)]);
    let mut w = DumpWriter::new(Vec::new(), &full_of(std::slice::from_ref(&b))).unwrap();
    assert_eq!(
        refused(w.batch(&b).unwrap_err()),
        "a record of kind 6 cannot stand here"
    );
    assert_eq!(w.finish(none).unwrap().1.batches, 0);
    drop(b);
    // and the reader refuses its length before it reads the payload
    let forged = forge_one(kinds::BLOB_CHUNK, &over);
    let frame_at = 12 + HEAD + 179 + HEAD + 80;
    let e = read_all(&forged).unwrap_err();
    assert!(
        matches!(&e, StoreError::Dump { at, .. } if *at == frame_at as u64),
        "{e:?}"
    );
    assert_eq!(damaged(e), "record allocation failed");
    // a stream that claims more and ends there is refused by the claim, not by its end
    let e = read_all(&forged[..frame_at + HEAD]).unwrap_err();
    assert_eq!(damaged(e), "record allocation failed");
    let e = read_all(&forged[..frame_at + HEAD - 1]).unwrap_err();
    assert_eq!(damaged(e), "the file ends before its trailer");
}
