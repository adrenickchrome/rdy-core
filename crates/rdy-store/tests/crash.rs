//! Crash consistency of one flush per write (kill1, roll1). This test binary re-runs itself as a
//! child (`crash_child`) that makes acknowledged writes and is aborted at a chosen point of a later
//! one; the parent reopens the store and checks it recovered every acknowledged write, with the
//! index caught up from the log rather than rebuilt.
use rdy_store::segment::{CrashPoint, Location};
use rdy_store::{OpenReport, Store};
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn payload(i: usize, pad: usize) -> Vec<u8> {
    let mut v = format!("crash write {i:05} ").into_bytes();
    v.resize(v.len().max(pad), b'.');
    v
}

#[test]
#[ignore = "the crash tests run this in a child process"]
fn crash_child() {
    let Ok(dir) = std::env::var("RDY_CRASH_DIR") else {
        return;
    };
    let var = |k: &str| std::env::var(k).ok();
    let acked: usize = var("RDY_CRASH_ACKED").unwrap().parse().unwrap();
    let pad: usize = var("RDY_CRASH_PAD").map_or(0, |v| v.parse().unwrap());
    let point = CrashPoint::parse(&var("RDY_CRASH_POINT").unwrap()).expect("a crash point");
    let mut s = Store::open(Path::new(&dir)).unwrap();
    if let Some(m) = var("RDY_CRASH_SEGMENT_MAX") {
        s.set_segment_max(m.parse().unwrap());
    }
    if let Some(n) = var("RDY_CRASH_CHECKPOINT_FRAMES") {
        s.set_checkpoint_frames(n.parse().unwrap());
    }
    let mut out = std::io::stdout().lock();
    for i in 0..acked + 1000 {
        if i == acked {
            s.crash_at(point);
        }
        s.put_blob(&payload(i, pad)).unwrap();
        let end = s.log_end();
        writeln!(out, "ACK {i} {} {}", end.segment, end.offset).unwrap();
        out.flush().unwrap();
    }
    std::process::exit(3); // the crash point was never reached
}

/// Runs `crash_child` on `dir`; returns the log's end after each acknowledged write.
fn crash(dir: &Path, point: CrashPoint, acked: usize, env: &[(&str, &str)]) -> Vec<Location> {
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "crash_child",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env("RDY_CRASH_DIR", dir)
        .env("RDY_CRASH_ACKED", acked.to_string())
        .env("RDY_CRASH_POINT", point.name())
        .envs(env.iter().copied())
        .output()
        .unwrap();
    assert_eq!(
        out.status.signal(),
        Some(6),
        "the child must abort at {point:?}: {:?}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            // libtest prints "test crash_child ... " before the first ACK on the same line
            let mut w = l[l.find("ACK ")?..].strip_prefix("ACK ")?.split(' ');
            w.next()?;
            Some(Location {
                segment: w.next()?.parse().ok()?,
                offset: w.next()?.parse().ok()?,
            })
        })
        .collect()
}

fn copy_store(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let t = to.join(p.file_name().unwrap());
        if p.is_dir() {
            copy_store(&p, &t)
        } else if p.file_name().unwrap() != "lock" {
            std::fs::copy(&p, &t).unwrap();
        }
    }
}

/// Reopens `dir` after a crash: every acknowledged write reads back, the interrupted write
/// (`acked`) is present exactly when `kept`, the index was caught up rather than rebuilt, verify
/// passes, and a new write survives a clean reopen.
fn recovered(dir: &Path, acked: usize, kept: bool, pad: usize) -> OpenReport {
    let mut s = Store::open(dir).unwrap_or_else(|e| panic!("reopen: {e}"));
    let report = s.open_report().clone();
    assert!(!report.index_rebuilt, "a kill is not damage: {report:?}");
    for i in 0..acked {
        let want = payload(i, pad);
        assert_eq!(
            s.get_blob(&Store::blob_hash(&want)).unwrap(),
            want,
            "write {i}"
        );
    }
    assert_eq!(
        s.has_blob(&Store::blob_hash(&payload(acked, pad))).unwrap(),
        kept,
        "the interrupted write {acked}"
    );
    s.verify().unwrap();
    let after = b"after the crash".to_vec();
    s.put_blob(&after).unwrap();
    drop(s);
    let s = Store::open(dir).unwrap();
    assert_eq!(s.get_blob(&Store::blob_hash(&after)).unwrap(), after);
    s.verify().unwrap();
    report
}

/// The characterisation §4.1 asks for: what redb does after a kill with unflushed index commits.
/// The printed report goes in the ledger.
#[test]
fn kill1_a_kill_rolls_the_index_back_and_the_open_catches_it_up() {
    let dir = tempfile::tempdir().unwrap();
    let acks = crash(
        dir.path(),
        CrashPoint::AfterIndex,
        20,
        &[("RDY_CRASH_CHECKPOINT_FRAMES", "1000000")],
    );
    assert_eq!(acks.len(), 20);
    assert!(dir.path().join(rdy_store::INDEX_OPEN_SENTINEL).exists());
    let r = recovered(dir.path(), 20, true, 0);
    eprintln!("redb after a kill with unflushed index commits: {r:?}");
    assert!(r.unclean_shutdown, "{r:?}");
    // the creation checkpoint covered only the format batch: 21 writes x (chunk, manifest, marker)
    assert_eq!(r.caught_up_frames, 63, "{r:?}");
}

/// Review Focus 3.
#[test]
fn kill1_every_write_step_recovers_to_the_acknowledged_writes() {
    for (point, kept) in [
        (CrashPoint::BeforeMarker, false),
        (CrashPoint::BeforeSync, true), // a kill keeps unsynced bytes; power loss is the tear test
        (CrashPoint::AfterSync, true),
        (CrashPoint::AfterIndex, true),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let acks = crash(dir.path(), point, 5, &[]);
        assert_eq!(acks.len(), 5, "{point:?}");
        recovered(dir.path(), 5, kept, 0);
    }
}

#[test]
fn kill1_a_kill_around_a_checkpoint_recovers() {
    for point in [CrashPoint::BeforeCheckpoint, CrashPoint::AfterCheckpoint] {
        let dir = tempfile::tempdir().unwrap();
        // checkpoint every 9 frames (3 writes); armed at write 4, it fires inside a later write
        // whose batch the log already holds
        let acks = crash(
            dir.path(),
            point,
            4,
            &[("RDY_CRASH_CHECKPOINT_FRAMES", "9")],
        );
        assert!(acks.len() >= 4, "{point:?}: {acks:?}");
        recovered(dir.path(), acks.len(), true, 0);
    }
}

#[test]
fn roll1_a_kill_after_a_roll_before_the_marker_loses_only_that_batch() {
    let dir = tempfile::tempdir().unwrap();
    let acks = crash(
        dir.path(),
        CrashPoint::RolledBeforeMarker,
        1,
        &[("RDY_CRASH_SEGMENT_MAX", "4096"), ("RDY_CRASH_PAD", "1000")],
    );
    let last = *acks.last().unwrap();
    let rolled = rdy_store::segment::seg_path(&dir.path().join("segments"), last.segment + 1);
    assert!(rolled.exists(), "the crash came after the roll");
    let r = recovered(dir.path(), acks.len(), false, 1000);
    assert!(r.marker_rule, "{r:?}");
    assert_eq!(
        r.quarantined,
        vec![PathBuf::from(format!("{}.torn-0", rolled.display()))]
    );
}

/// Review Focus 2: power loss tears the unsynced batch anywhere; truncation at every offset and
/// zero-filled tails (every 7th offset) must each recover to the five acknowledged writes.
#[test]
fn kill1_a_power_loss_tear_of_the_unsynced_batch_recovers_at_every_offset() {
    let src = tempfile::tempdir().unwrap();
    let acks = crash(src.path(), CrashPoint::BeforeSync, 5, &[]);
    let acked_end = acks[4];
    let seg = rdy_store::segment::seg_path(&src.path().join("segments"), acked_end.segment);
    let full = std::fs::read(&seg).unwrap();
    assert!(
        full.len() as u64 > acked_end.offset,
        "the interrupted batch is in the log"
    );
    for cut in acked_end.offset + 1..full.len() as u64 {
        for zero in [false, true] {
            if zero && cut % 7 != 0 {
                continue;
            }
            let copy = tempfile::tempdir().unwrap();
            copy_store(src.path(), copy.path());
            let mut bytes = full.clone();
            if zero {
                bytes[cut as usize..].fill(0);
            } else {
                bytes.truncate(cut as usize);
            }
            std::fs::write(
                rdy_store::segment::seg_path(&copy.path().join("segments"), acked_end.segment),
                &bytes,
            )
            .unwrap();
            let r = recovered(copy.path(), 5, false, 0);
            assert_eq!(r.quarantined.len(), 1, "cut {cut} zero {zero}: {r:?}");
        }
    }
}

/// Review Focus 2: power loss can lose a middle page of the unsynced batch while its marker page
/// survives. That batch is still the unacknowledged tail: quarantined, never refused.
#[test]
fn kill1_a_power_loss_tear_under_an_intact_marker_is_the_unacknowledged_tail() {
    let src = tempfile::tempdir().unwrap();
    let acks = crash(src.path(), CrashPoint::BeforeSync, 5, &[]);
    let acked_end = acks[4];
    let seg = rdy_store::segment::seg_path(&src.path().join("segments"), acked_end.segment);
    let mut bytes = std::fs::read(&seg).unwrap();
    let at = acked_end.offset as usize + 40; // inside the interrupted batch's first payload
    bytes[at..at + 8].fill(0);
    std::fs::write(&seg, &bytes).unwrap();
    let r = recovered(src.path(), 5, false, 0);
    assert_eq!(r.quarantined.len(), 1, "{r:?}");
}

/// A marker-shaped copy inside a torn payload: the unacknowledged tail embeds a hash-valid copy of
/// the store's own segment (every batch marker in it is genuine, for other offsets) and is cut
/// after it. That is a torn write to quarantine, never "an acknowledged batch follows the damage".
#[test]
fn kill1_marker_shaped_bytes_in_a_torn_tail_are_quarantined_not_refused() {
    let dir = tempfile::tempdir().unwrap();
    let acks = crash(dir.path(), CrashPoint::BeforeSync, 5, &[]);
    let acked_end = acks[4];
    let seg = rdy_store::segment::seg_path(&dir.path().join("segments"), acked_end.segment);
    let mut bytes = std::fs::read(&seg).unwrap();
    bytes.truncate(acked_end.offset as usize); // drop the kept batch: the tail is crafted below
    let mut payload = bytes.clone();
    payload.extend_from_slice(&[7u8; 2000]);
    let mut torn = (payload.len() as u32).to_le_bytes().to_vec();
    torn.push(rdy_store::kinds::BLOB_CHUNK);
    torn.extend_from_slice(&rdy_model::Hash::of(&payload).0);
    torn.extend_from_slice(&payload);
    torn.truncate(37 + bytes.len() + 500); // cut after the embedded copy
    let mut file = bytes.clone();
    file.extend_from_slice(&torn);
    std::fs::write(&seg, &file).unwrap();
    let r = recovered(dir.path(), 5, false, 0);
    assert_eq!(r.quarantined.len(), 1, "{r:?}");
    assert_eq!(std::fs::read(&r.quarantined[0]).unwrap(), torn);
}

/// Opens a store directory the way a crash left it and checks the open finished the format batch:
/// the log holds a marker before `FORMAT` exists (never `FORMAT` over a log with no marker), the
/// refs and `FORMAT` read back, every `blobs` entry survives, and a second open is a plain one.
fn finished_format(dir: &Path, blobs: &[Vec<u8>], what: &str) {
    let segments = dir.join("segments");
    let s = Store::open(dir).unwrap_or_else(|e| panic!("{what}: {e}"));
    assert!(
        !s.open_report().index_rebuilt,
        "{what}: {:?}",
        s.open_report()
    );
    assert!(
        rdy_store::segment::log_has_marker(&segments).unwrap(),
        "{what}: no marker"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("FORMAT")).unwrap(),
        "rdy store format\nlog_format 2\nmin_reader 2\n",
        "{what}"
    );
    assert_eq!(
        s.get_ref("meta/log_format").unwrap(),
        Some(b"2".to_vec()),
        "{what}"
    );
    assert_eq!(
        s.get_ref("meta/min_reader").unwrap(),
        Some(b"2".to_vec()),
        "{what}"
    );
    drop(s);
    let mut s = Store::open(dir).unwrap_or_else(|e| panic!("{what}, second open: {e}"));
    let r = s.open_report().clone();
    assert!(
        r.marker_rule && !r.upgraded && !r.index_rebuilt,
        "{what}: {r:?}"
    );
    assert!(r.quarantined.is_empty(), "{what}: {r:?}");
    for b in blobs {
        assert_eq!(s.get_blob(&Store::blob_hash(b)).unwrap(), *b, "{what}");
    }
    s.verify().unwrap();
    s.put_blob(b"after the format batch").unwrap();
    s.verify().unwrap();
}

/// A kill between the format batch's frames and its marker (or anywhere inside the batch, or
/// before `FORMAT` is written) on a brand-new store: the next open finishes the format batch.
/// The crash points are armed after open, so each kill is the log cut at an offset of the batch.
#[test]
fn kill1_a_kill_inside_the_first_format_batch_of_a_new_store_is_finished_by_the_next_open() {
    let base = tempfile::tempdir().unwrap();
    drop(Store::open_legacy_for_tests(base.path()).unwrap()); // an empty store with its index
    let done = tempfile::tempdir().unwrap();
    copy_store(base.path(), done.path());
    drop(Store::open(done.path()).unwrap());
    let seg = rdy_store::segment::seg_path(&done.path().join("segments"), 0);
    let full = std::fs::read(&seg).unwrap();
    assert!(!full.is_empty(), "the first open wrote the format batch");
    for cut in 0..=full.len() {
        let copy = tempfile::tempdir().unwrap();
        copy_store(base.path(), copy.path());
        let seg = rdy_store::segment::seg_path(&copy.path().join("segments"), 0);
        std::fs::write(&seg, &full[..cut]).unwrap();
        finished_format(
            copy.path(),
            &[],
            &format!("new store, log cut at {cut} of {}", full.len()),
        );
    }
}

/// The same for an M1 store being upgraded: a kill anywhere in the upgrade's format batch leaves
/// the M1 frames acknowledged and the next open finishes the upgrade with a marker.
#[test]
fn kill1_a_kill_inside_the_upgrade_batch_of_an_m1_store_is_finished_by_the_next_open() {
    let blobs: Vec<Vec<u8>> = (0..4).map(|i| payload(i, 0)).collect();
    let m1 = tempfile::tempdir().unwrap();
    let mut s = Store::open_legacy_for_tests(m1.path()).unwrap();
    for b in &blobs {
        s.put_blob(b).unwrap();
    }
    let m1_end = s.log_end();
    drop(s);
    let done = tempfile::tempdir().unwrap();
    copy_store(m1.path(), done.path());
    assert!(Store::open(done.path()).unwrap().open_report().upgraded);
    let seg = rdy_store::segment::seg_path(&done.path().join("segments"), 0);
    let full = std::fs::read(&seg).unwrap();
    assert!(
        full.len() as u64 > m1_end.offset,
        "the upgrade appended its batch"
    );
    for cut in m1_end.offset as usize..=full.len() {
        let copy = tempfile::tempdir().unwrap();
        copy_store(m1.path(), copy.path());
        let seg = rdy_store::segment::seg_path(&copy.path().join("segments"), 0);
        std::fs::write(&seg, &full[..cut]).unwrap();
        finished_format(
            copy.path(),
            &blobs,
            &format!("upgrade, log cut at {cut} of {}", full.len()),
        );
    }
}
