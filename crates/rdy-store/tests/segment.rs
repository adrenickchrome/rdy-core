use rdy_model::Hash;
use rdy_store::segment::{Location, SegmentLog};
use std::fs::OpenOptions;
use std::io::Write;

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
    assert_eq!(log.scan_from(Location { segment: 0, offset: 0 }).unwrap().len(), 10);
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
    let mut f = OpenOptions::new().append(true).open(dir.path().join("seg-000000.log")).unwrap();
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
    let frames = log.scan_from(Location { segment: 0, offset: 0 }).unwrap();
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
    let result = log.scan_from(Location { segment: 0, offset: 0 });
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

    let err = SegmentLog::open(dir.path(), 1 << 20).err().expect("open must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    let msg = err.to_string();
    assert!(msg.contains("seg-000000.log") && msg.contains("offset 0"), "{msg}");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}
