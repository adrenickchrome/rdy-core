use rdy_model::Hash;
use rdy_store::kinds::{self, BatchCommit};
use rdy_store::segment::{Location, SegmentLog};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

#[test]
fn append_read_round_trip_and_hash_is_object_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = SegmentLog::open(dir.path(), 1 << 20).unwrap();
    let (loc, h) = log.append(1, b"hello").unwrap();
    log.sync().unwrap();
    assert_eq!(h, Hash::of(b"hello"));
    assert_eq!(log.read(loc).unwrap(), (1, b"hello".to_vec()));
}

#[test]
fn rolls_to_a_new_segment_when_full() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = SegmentLog::open(dir.path(), 128).unwrap();
    let mut locs = vec![];
    for i in 0..10u8 {
        locs.push(log.append(2, &[i; 60]).unwrap().0);
    }
    log.sync().unwrap();
    assert!(log.end().segment >= 4);
    for (i, loc) in locs.iter().enumerate() {
        assert_eq!(log.read(*loc).unwrap().1, vec![i as u8; 60]);
    }
    drop(log);
    let log = SegmentLog::open(dir.path(), 128).unwrap();
    assert_eq!(
        log.scan_from(Location {
            segment: 0,
            offset: 0
        })
        .unwrap()
        .len(),
        10
    );
}

#[test]
fn torn_tail_is_truncated_and_acknowledged_writes_survive() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = SegmentLog::open(dir.path(), 1 << 20).unwrap();
    let (a, _) = log.append(1, b"first").unwrap();
    let (b, _) = log.append(1, b"second").unwrap();
    log.sync().unwrap();
    let end_before = log.end();
    drop(log);

    // simulate a crash mid-frame: a header claiming 100 bytes, then only 3 bytes of payload
    let mut f = OpenOptions::new()
        .append(true)
        .open(dir.path().join("seg-000000.log"))
        .unwrap();
    f.write_all(&100u32.to_le_bytes()).unwrap();
    f.write_all(&[1u8]).unwrap();
    f.write_all(&[0u8; 32]).unwrap();
    f.write_all(b"abc").unwrap();
    drop(f);

    let mut log = SegmentLog::open(dir.path(), 1 << 20).unwrap();
    assert_eq!(log.end(), end_before);
    assert_eq!(log.read(a).unwrap().1, b"first");
    assert_eq!(log.read(b).unwrap().1, b"second");
    let (c, _) = log.append(1, b"third").unwrap();
    assert_eq!(c, end_before);
    log.sync().unwrap();
    drop(log);

    // reopen and verify all three frames are intact
    let log = SegmentLog::open(dir.path(), 1 << 20).unwrap();
    let frames = log
        .scan_from(Location {
            segment: 0,
            offset: 0,
        })
        .unwrap();
    assert_eq!(frames.len(), 3);
    assert_eq!(log.read(a).unwrap().1, b"first");
    assert_eq!(log.read(b).unwrap().1, b"second");
    assert_eq!(log.read(c).unwrap().1, b"third");
}

#[test]
fn corrupt_last_frame_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = SegmentLog::open(dir.path(), 1 << 20).unwrap();
    let (a, _) = log.append(1, b"good").unwrap();
    let (b, _) = log.append(1, b"flipme").unwrap();
    log.sync().unwrap();
    drop(log);
    let path = dir.path().join("seg-000000.log");
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&path, bytes).unwrap();

    let mut log = SegmentLog::open(dir.path(), 1 << 20).unwrap();
    assert_eq!(log.end(), b);
    assert_eq!(log.read(a).unwrap().1, b"good");
    let (c, _) = log.append(1, b"new").unwrap();
    log.sync().unwrap();
    drop(log);

    // reopen and verify the new frame persists
    let log = SegmentLog::open(dir.path(), 1 << 20).unwrap();
    assert_eq!(log.read(a).unwrap().1, b"good");
    assert_eq!(log.read(c).unwrap().1, b"new");
}

#[test]
fn corrupt_frame_in_sealed_segment_detected_on_scan() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = SegmentLog::open(dir.path(), 128).unwrap();
    let _a = log.append(1, &[0; 50]).unwrap().0;
    let _b = log.append(1, &[1; 50]).unwrap().0;
    let _c = log.append(1, &[2; 50]).unwrap().0;
    log.sync().unwrap();
    drop(log);

    // corrupt a byte in the middle frame (seg-000001.log)
    let path = dir.path().join("seg-000001.log");
    let mut bytes = std::fs::read(&path).unwrap();
    // flip a byte in the middle of the payload (37-byte header + 25 bytes into 50-byte payload)
    let corruption_point = 37 + 25;
    bytes[corruption_point] ^= 0xff;
    std::fs::write(&path, bytes).unwrap();

    // scan should detect the corruption in the sealed segment
    let log = SegmentLog::open(dir.path(), 128).unwrap();
    let result = log.scan_from(Location {
        segment: 0,
        offset: 0,
    });
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn mid_segment_corruption_refuses_to_open_and_leaves_file_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = SegmentLog::open(dir.path(), 1 << 20).unwrap();
    log.append(1, b"first-frame").unwrap();
    log.append(1, b"second-frame").unwrap();
    log.append(1, b"third-frame").unwrap();
    log.sync().unwrap();
    drop(log);
    let path = dir.path().join("seg-000000.log");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[37 + 3] ^= 0xff; // inside the first frame's payload
    std::fs::write(&path, &bytes).unwrap();

    let err = SegmentLog::open(dir.path(), 1 << 20)
        .err()
        .expect("open must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    let msg = err.to_string();
    assert!(
        msg.contains("seg-000000.log") && msg.contains("offset 0"),
        "{msg}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn corrupted_length_field_with_valid_frames_after_refuses_to_open() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = SegmentLog::open(dir.path(), 1 << 20).unwrap();
    log.append(1, b"first-frame").unwrap();
    log.append(1, b"second-frame").unwrap();
    log.append(1, b"third-frame").unwrap();
    log.sync().unwrap();
    drop(log);
    let path = dir.path().join("seg-000000.log");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[3] ^= 0xff; // high byte of the first frame's length
    std::fs::write(&path, &bytes).unwrap();

    let err = SegmentLog::open(dir.path(), 1 << 20)
        .err()
        .expect("open must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    let msg = err.to_string();
    assert!(
        msg.contains("seg-000000.log") && msg.contains("offset 0"),
        "{msg}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn torn_tail_is_quarantined_before_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = SegmentLog::open(dir.path(), 1 << 20).unwrap();
    log.append(1, b"keep-me").unwrap();
    let (b, _) = log.append(1, b"torn-payload-here").unwrap();
    log.sync().unwrap();
    drop(log);
    let path = dir.path().join("seg-000000.log");
    let bytes = std::fs::read(&path).unwrap();
    let cut = bytes.len() - 5;
    std::fs::write(&path, &bytes[..cut]).unwrap();

    let log = SegmentLog::open(dir.path(), 1 << 20).unwrap();
    assert_eq!(log.end(), b);
    let q = dir.path().join(format!("seg-000000.log.torn-{}", b.offset));
    assert_eq!(std::fs::read(q).unwrap(), &bytes[b.offset as usize..cut]);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), b.offset);
}

#[test]
fn truncate_to_discards_frames_and_segments_created_after_the_point() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = SegmentLog::open(dir.path(), 128).unwrap();
    let (a, _) = log.append(1, &[1; 40]).unwrap();
    log.sync().unwrap();
    let start = log.end();
    for i in 0..4u8 {
        log.append(2, &[i; 60]).unwrap();
    }
    assert!(log.end().segment > start.segment);
    log.truncate_to(start).unwrap();
    assert_eq!(log.end(), start);
    assert!(!dir.path().join("seg-000001.log").exists());
    assert_eq!(log.read(a).unwrap().1, vec![1; 40]);
    let (b, _) = log.append(3, b"after").unwrap();
    assert_eq!(b, start);
    log.sync().unwrap();
    drop(log);
    let log = SegmentLog::open(dir.path(), 128).unwrap();
    assert_eq!(
        log.scan_from(Location {
            segment: 0,
            offset: 0
        })
        .unwrap()
        .len(),
        2
    );
}

#[test]
fn quarantine_never_overwrites_an_earlier_quarantine_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = SegmentLog::open(dir.path(), 1 << 20).unwrap();
    let (b, _) = log.append(1, b"keep-me").unwrap();
    log.sync().unwrap();
    drop(log);
    let path = dir.path().join("seg-000000.log");
    let q = dir
        .path()
        .join(format!("seg-000000.log.torn-{}", b.offset + 44));
    std::fs::write(&q, b"earlier evidence").unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(&[9u8; 20]);
    std::fs::write(&path, &bytes).unwrap();

    SegmentLog::open(dir.path(), 1 << 20).unwrap();
    assert_eq!(std::fs::read(&q).unwrap(), b"earlier evidence");
    let q1 = dir
        .path()
        .join(format!("seg-000000.log.torn-{}.1", b.offset + 44));
    assert_eq!(std::fs::read(q1).unwrap(), vec![9u8; 20]);
}

#[test]
fn a_failed_roll_leaves_the_log_unchanged_and_the_next_append_rolls_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    // one 60-byte payload makes a 97-byte frame, so the second append must roll
    let mut log = SegmentLog::open(dir.path(), 128).unwrap();
    let (a, _) = log.append(2, &[1; 60]).unwrap();
    let before = log.end();
    log.fail_next_roll();
    assert!(log.append(2, &[2; 60]).is_err());
    assert_eq!(log.end(), before);
    assert!(
        !dir.path().join("seg-000001.log").exists(),
        "the failed roll's file is removed"
    );
    let (b, _) = log.append(2, &[3; 60]).unwrap();
    log.sync().unwrap();
    assert_eq!(b.segment, 1);
    assert_eq!(log.read(a).unwrap(), (2, vec![1; 60]));
    assert_eq!(log.read(b).unwrap(), (2, vec![3; 60]));
    let end = log.end();
    drop(log);
    let log = SegmentLog::open_acked(dir.path(), 128, Some(end)).unwrap();
    assert_eq!(log.read(a).unwrap(), (2, vec![1; 60]));
    assert_eq!(log.read(b).unwrap(), (2, vec![3; 60]));
}

#[test]
fn truncate_to_removes_a_stray_next_segment() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = SegmentLog::open(dir.path(), 1 << 20).unwrap();
    log.append(1, b"one").unwrap();
    log.sync().unwrap();
    let end = log.end();
    std::fs::write(dir.path().join("seg-000001.log"), b"").unwrap();
    log.truncate_to(end).unwrap();
    assert!(!dir.path().join("seg-000001.log").exists());
    assert_eq!(log.end(), end);
}

#[test]
fn reading_at_an_absurd_offset_is_an_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = SegmentLog::open(dir.path(), 1 << 20).unwrap();
    log.append(1, b"one").unwrap();
    log.sync().unwrap();
    assert!(
        log.read(Location {
            segment: 0,
            offset: u64::MAX - 3
        })
        .is_err()
    );
}

fn batch(n: u8, size: usize) -> Vec<(u8, Vec<u8>)> {
    (0..n).map(|i| (kinds::BLOB_CHUNK, vec![i; size])).collect()
}

#[test]
fn mark1_append_batch_ends_with_a_marker_naming_its_frames() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = SegmentLog::open(dir.path(), 1 << 20).unwrap();
    let first = log.append_batch(&batch(2, 10)).unwrap();
    let placed = log.append_batch(&batch(3, 20)).unwrap();
    log.sync().unwrap();
    assert_eq!(
        first[0].0,
        Location {
            segment: 0,
            offset: 0
        }
    );
    assert_eq!(placed.len(), 3);
    let all = log
        .scan_from(Location {
            segment: 0,
            offset: 0,
        })
        .unwrap();
    assert_eq!(all.len(), 2 + 1 + 3 + 1);
    assert_eq!(all[2].1, kinds::BATCH_COMMIT);
    let (mloc, kind, _) = all[6];
    assert_eq!(kind, kinds::BATCH_COMMIT);
    let m: BatchCommit = postcard::from_bytes(&log.read(mloc).unwrap().1).unwrap();
    assert_eq!(m.start, placed[0].0);
    assert_eq!(m.frames, 3);
    assert_eq!(m.digest, kinds::batch_digest(placed.iter().map(|(_, h)| h)));
    assert_eq!(log.verify_frames().unwrap(), 5, "markers are not counted");
}

#[test]
fn roll1_a_batch_that_does_not_fit_rolls_before_its_first_frame() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = SegmentLog::open(dir.path(), 400).unwrap();
    log.append_batch(&batch(2, 50)).unwrap();
    let second = log.append_batch(&batch(2, 50)).unwrap();
    assert!(second.iter().all(|(l, _)| l.segment == 1), "{second:?}");
    assert_eq!(second[0].0.offset, 0);
    let seg0: Vec<_> = log
        .scan_from(Location {
            segment: 0,
            offset: 0,
        })
        .unwrap()
        .into_iter()
        .filter(|(l, _, _)| l.segment == 0)
        .collect();
    assert_eq!(
        seg0.last().unwrap().1,
        kinds::BATCH_COMMIT,
        "segment 0 ends at a marker"
    );
}

#[test]
fn roll1_a_batch_larger_than_a_segment_gets_a_segment_of_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = SegmentLog::open(dir.path(), 128).unwrap();
    log.append_batch(&batch(1, 10)).unwrap();
    let big = log.append_batch(&batch(3, 100)).unwrap();
    assert!(big.iter().all(|(l, _)| l.segment == 1), "{big:?}");
    let next = log.append_batch(&batch(1, 10)).unwrap();
    assert_eq!(
        next[0].0,
        Location {
            segment: 2,
            offset: 0
        }
    );
}

#[test]
fn mark1_verify_refuses_a_marker_that_does_not_match_its_batch() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = SegmentLog::open(dir.path(), 1 << 20).unwrap();
    let placed = log.append_batch(&batch(2, 10)).unwrap();
    let bad = BatchCommit {
        start: placed[0].0,
        frames: 2,
        digest: Hash::of(b"not the batch"),
    };
    log.append(kinds::BATCH_COMMIT, &postcard::to_allocvec(&bad).unwrap())
        .unwrap();
    log.sync().unwrap();
    let e = log.verify_frames().unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
    assert!(e.to_string().contains("does not match its batch"), "{e}");
}

#[test]
fn mark1_crash_points_round_trip_by_name() {
    use rdy_store::segment::CrashPoint;
    for p in CrashPoint::ALL {
        assert_eq!(CrashPoint::parse(p.name()), Some(p));
    }
    assert_eq!(CrashPoint::parse("nowhere"), None);
}

fn seg_file(dir: &Path, n: u32) -> PathBuf {
    dir.join(format!("seg-{n:06}.log"))
}

/// A well-formed frame's bytes: length, kind, payload hash, payload.
fn frame_bytes(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = (payload.len() as u32).to_le_bytes().to_vec();
    out.push(kind);
    out.extend_from_slice(&Hash::of(payload).0);
    out.extend_from_slice(payload);
    out
}

/// Three synced two-frame batches in segment 0; returns the log's end after each.
fn three_batches(dir: &Path) -> Vec<Location> {
    let mut log = SegmentLog::open(dir, 1 << 20).unwrap();
    let mut ends = vec![];
    for b in 0..3u8 {
        log.append_batch(&[
            (kinds::BLOB_CHUNK, vec![b; 30]),
            (kinds::BLOB_CHUNK, vec![b + 100; 30]),
        ])
        .unwrap();
        ends.push(log.end());
    }
    log.sync().unwrap();
    ends
}

fn torn_files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".torn-"))
        .collect();
    v.sort();
    v
}

#[test]
fn mark1_the_boundary_is_the_end_of_the_last_valid_marker() {
    let dir = tempfile::tempdir().unwrap();
    let ends = three_batches(dir.path());
    let tail = frame_bytes(kinds::BLOB_CHUNK, b"unmarked tail");
    let mut f = OpenOptions::new()
        .append(true)
        .open(seg_file(dir.path(), 0))
        .unwrap();
    f.write_all(&tail).unwrap();
    drop(f);
    let (log, q) = SegmentLog::open_marked(dir.path(), 1 << 20, None).unwrap();
    assert_eq!(log.end(), ends[2]);
    assert_eq!(
        q,
        vec![
            dir.path()
                .join(format!("seg-000000.log.torn-{}", ends[2].offset))
        ]
    );
    assert_eq!(std::fs::read(&q[0]).unwrap(), tail);
    assert_eq!(
        std::fs::metadata(seg_file(dir.path(), 0)).unwrap().len(),
        ends[2].offset
    );
}

#[test]
fn mark1_a_torn_marker_leaves_its_batch_unacknowledged() {
    let dir = tempfile::tempdir().unwrap();
    let ends = three_batches(dir.path());
    let f = OpenOptions::new()
        .write(true)
        .open(seg_file(dir.path(), 0))
        .unwrap();
    f.set_len(ends[2].offset - 5).unwrap();
    drop(f);
    let (log, q) = SegmentLog::open_marked(dir.path(), 1 << 20, None).unwrap();
    assert_eq!(log.end(), ends[1]);
    assert_eq!(q.len(), 1);
}

#[test]
fn mark1_a_torn_middle_frame_under_an_intact_marker_is_the_unacknowledged_tail() {
    let dir = tempfile::tempdir().unwrap();
    let ends = three_batches(dir.path());
    let path = seg_file(dir.path(), 0);
    let mut bytes = std::fs::read(&path).unwrap();
    let at = ends[1].offset as usize + 40; // inside the last batch's first payload
    bytes[at..at + 10].fill(0);
    std::fs::write(&path, &bytes).unwrap();
    let (log, q) = SegmentLog::open_marked(dir.path(), 1 << 20, None).unwrap();
    assert_eq!(log.end(), ends[1]);
    assert_eq!(q.len(), 1);
}

#[test]
fn mark1_corruption_inside_an_earlier_batch_is_refused_and_left_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let ends = three_batches(dir.path());
    let path = seg_file(dir.path(), 0);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[ends[0].offset as usize + 40] ^= 0xff; // the second batch; the third follows it
    std::fs::write(&path, &bytes).unwrap();
    let e = SegmentLog::open_marked(dir.path(), 1 << 20, None)
        .err()
        .expect("refuse");
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        e.to_string().contains("move the damaged segment aside"),
        "{e}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(torn_files(dir.path()).is_empty());
}

#[test]
fn mark1_a_floor_past_the_boundary_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let ends = three_batches(dir.path());
    let path = seg_file(dir.path(), 0);
    let f = OpenOptions::new().write(true).open(&path).unwrap();
    f.set_len(ends[2].offset - 5).unwrap();
    drop(f);
    let before = std::fs::read(&path).unwrap();
    let e = SegmentLog::open_marked(dir.path(), 1 << 20, Some(ends[2]))
        .err()
        .expect("refuse");
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn mark1_a_last_segment_without_a_marker_is_quarantined_and_removed() {
    let dir = tempfile::tempdir().unwrap();
    let ends = three_batches(dir.path());
    let unmarked = frame_bytes(kinds::BLOB_CHUNK, b"rolled, never marked");
    std::fs::write(seg_file(dir.path(), 1), &unmarked).unwrap();
    let (log, q) = SegmentLog::open_marked(dir.path(), 1 << 20, None).unwrap();
    assert_eq!(log.end(), ends[2]);
    assert!(!seg_file(dir.path(), 1).exists());
    assert_eq!(q, vec![dir.path().join("seg-000001.log.torn-0")]);
    assert_eq!(std::fs::read(&q[0]).unwrap(), unmarked);
}

#[test]
fn mark1_an_unmarked_last_segment_the_index_acknowledged_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    three_batches(dir.path());
    std::fs::write(
        seg_file(dir.path(), 1),
        frame_bytes(kinds::BLOB_CHUNK, b"x"),
    )
    .unwrap();
    let floor = Location {
        segment: 1,
        offset: 10,
    };
    assert!(SegmentLog::open_marked(dir.path(), 1 << 20, Some(floor)).is_err());
    assert!(seg_file(dir.path(), 1).exists());
}

#[test]
fn mark1_an_empty_last_segment_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let ends = three_batches(dir.path());
    std::fs::File::create(seg_file(dir.path(), 1)).unwrap();
    let (log, q) = SegmentLog::open_marked(dir.path(), 1 << 20, None).unwrap();
    assert_eq!(log.end(), ends[2]);
    assert!(q.is_empty());
    assert!(!seg_file(dir.path(), 1).exists());
}

#[test]
fn mark1_a_sealed_segment_that_does_not_end_at_a_marker_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    three_batches(dir.path());
    let mut f = OpenOptions::new()
        .append(true)
        .open(seg_file(dir.path(), 0))
        .unwrap();
    f.write_all(&frame_bytes(kinds::BLOB_CHUNK, b"after the marker"))
        .unwrap();
    drop(f);
    std::fs::write(
        seg_file(dir.path(), 1),
        frame_bytes(kinds::BLOB_CHUNK, b"unmarked"),
    )
    .unwrap();
    let e = SegmentLog::open_marked(dir.path(), 1 << 20, None)
        .err()
        .expect("refuse");
    assert!(
        e.to_string().contains("does not end at a batch marker"),
        "{e}"
    );
    assert!(
        seg_file(dir.path(), 1).exists(),
        "refused before touching either segment"
    );
    assert!(torn_files(dir.path()).is_empty());
}

#[test]
fn mark1_log_has_marker_tells_m2_logs_from_m1_logs() {
    let m1 = tempfile::tempdir().unwrap();
    let mut log = SegmentLog::open(m1.path(), 1 << 20).unwrap();
    log.append(kinds::BLOB_CHUNK, b"m1 frame").unwrap();
    log.sync().unwrap();
    drop(log);
    assert!(!rdy_store::segment::log_has_marker(m1.path()).unwrap());
    let m2 = tempfile::tempdir().unwrap();
    three_batches(m2.path());
    assert!(rdy_store::segment::log_has_marker(m2.path()).unwrap());
    let empty = tempfile::tempdir().unwrap();
    assert!(!rdy_store::segment::log_has_marker(empty.path()).unwrap());
}

#[test]
fn mark1_an_empty_batch_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = SegmentLog::open(dir.path(), 1 << 20).unwrap();
    assert!(log.append_batch(&[]).unwrap().is_empty());
    assert_eq!(
        log.end(),
        Location {
            segment: 0,
            offset: 0
        }
    );
}

/// A write whose payload carries a copy of an rdy segment (an imported fixture, or bytes an agent
/// crafted) and is then torn after the copy: the embedded markers are hash-valid but belong to
/// another file's frames, so they are not an acknowledged batch following the damage.
#[test]
fn mark1_marker_shaped_bytes_inside_a_torn_payload_are_not_an_acknowledged_batch() {
    let other = tempfile::tempdir().unwrap();
    three_batches(other.path());
    let embedded = std::fs::read(seg_file(other.path(), 0)).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut log = SegmentLog::open(dir.path(), 1 << 20).unwrap();
    log.append_batch(&[(kinds::BLOB_CHUNK, b"first".to_vec())])
        .unwrap();
    let b = log.end();
    log.sync().unwrap();
    drop(log);
    let mut payload = embedded.clone();
    payload.extend_from_slice(&[7u8; 2000]);
    let mut torn = frame_bytes(kinds::BLOB_CHUNK, &payload);
    torn.truncate(37 + embedded.len() + 500); // cut after the embedded copy
    let path = seg_file(dir.path(), 0);
    let mut f = OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(&torn).unwrap();
    drop(f);
    let (log, q) = SegmentLog::open_marked(dir.path(), 1 << 20, None)
        .expect("an ordinary torn write is quarantined, not refused");
    assert_eq!(log.end(), b);
    assert_eq!(q.len(), 1);
    assert_eq!(std::fs::read(&q[0]).unwrap(), torn);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), b.offset);
}
