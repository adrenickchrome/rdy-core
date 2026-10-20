//! hc1: dump format 3 and the carried removal record (D-82K89: rdy writes dump format 3 for every
//! dump; D-69AY3: every released dump format stays readable; D-F0904: no existing kind is
//! re-encoded). The header and trailer layouts, the frozen kind sets of the released pairs, the
//! physical boundaries of full dumps and increments, the envelope reader, the bounds, and the
//! new record's encoding.
use rdy_model::redaction::{DropSpan, RedactTarget, RedactionApply};
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
    let mut facts = RemovalFacts::default();
    for b in &batches {
        facts.observe_full(b).unwrap();
    }
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
    let mut f = RemovalFacts::default();
    let dropped = DumpBatch {
        start: at(0, u64::MAX - 10),
        digest: kinds::batch_digest([Hash([1; 32])].iter()),
        frames: vec![Frame::Dropped {
            kind: kinds::BLOB_CHUNK,
            hash: Hash([1; 32]),
            len: 5,
            apply: Hash([2; 32]),
            residue: None,
        }],
    };
    f.observe_full(&dropped).unwrap();
    assert_eq!(f.first_removed, Some(at(0, u64::MAX - 10)));
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
