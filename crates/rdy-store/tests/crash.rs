//! Crash consistency of one flush per write (kill1, roll1). This test binary re-runs itself as a
//! child (`crash_child`) that makes acknowledged writes and is aborted at a chosen point of a later
//! one; the parent reopens the store and checks it recovered every acknowledged write, with the
//! index caught up from the log rather than rebuilt.
use rdy_model::Hash;
use rdy_store::format::STORE_FORMAT;
use rdy_store::segment::{CrashPoint, Location};
use rdy_store::transition::{Cause, Side, TensionMove, Transitions, Why};
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

/// The second blob of held batch `i`.
fn second(i: usize, pad: usize) -> Vec<u8> {
    let mut v = format!("crash second {i:05} ").into_bytes();
    v.resize(v.len().max(pad), b'.');
    v
}

fn tension_id(i: usize) -> Hash {
    Hash::of(format!("crash tension {i}").as_bytes())
}

fn tension_move(i: usize, cause: Cause) -> TensionMove {
    let side = |c: &str, actor: &str| Side {
        change: Some(Hash::of(c.as_bytes())),
        actor: Some(actor.into()),
        run: None,
        landing: None,
    };
    TensionMove {
        tension: tension_id(i),
        kind: "edit_collision".into(),
        severity: "blocking".into(),
        sides: vec![side("c1", "agent:a"), side("c2", "agent:b")],
        rekey: None,
        why: Why {
            cause,
            id: Some(Hash::of(format!("crash cause {i}").as_bytes())),
            actor: None,
            run: None,
        },
    }
}

/// The record that seals held batch `i`: it opens tension `i` and resolves tension `i - 1`.
fn held_record(i: usize) -> Transitions {
    Transitions {
        at_ms: i as i64,
        resolved: if i == 0 {
            vec![]
        } else {
            vec![tension_move(i - 1, Cause::Landed)]
        },
        opened: vec![tension_move(i, Cause::Committed)],
        gone: vec![],
        new: vec![],
    }
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
    let mut s = Store::create(Path::new(&dir)).unwrap();
    let held = var("RDY_CRASH_HELD").is_some();
    if held {
        s.set_held(true).unwrap();
    }
    if let Some(m) = var("RDY_CRASH_SEGMENT_MAX") {
        s.set_segment_max(m.parse().unwrap());
    }
    if let Some(n) = var("RDY_CRASH_CHECKPOINT_FRAMES") {
        s.set_checkpoint_frames(n.parse().unwrap());
    }
    let fail_seal = var("RDY_CRASH_FAIL_SEAL").is_some();
    let mut out = std::io::stdout().lock();
    for i in 0..acked + 1000 {
        if i == acked {
            s.crash_at(point);
        }
        s.put_blob(&payload(i, pad)).unwrap();
        if held {
            // a held batch: two writes, then the seal logs the batch's record last
            s.put_blob(&second(i, pad)).unwrap();
            if i == acked && fail_seal {
                s.fail_next_index_apply();
                assert!(s.seal(vec![held_record(i).frame()]).is_err());
                drop(s); // the close of the store dies at the armed crash point
                std::process::exit(3);
            }
            s.seal(vec![held_record(i).frame()]).unwrap();
        }
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
    let mut s = Store::create(dir).unwrap_or_else(|e| panic!("reopen: {e}"));
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
    let s = Store::create(dir).unwrap();
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

/// Checks the held batches of `s` after a kill in batch `acked`: batches before it read back
/// whole, and batch `acked` (two blobs and its record) survives whole when `kept`, else not at
/// all. The open-tension table and fsck agree with the batches kept.
fn held_batches_kept(s: &Store, acked: usize, kept: bool, pad: usize) {
    for i in 0..acked {
        for want in [payload(i, pad), second(i, pad)] {
            assert_eq!(
                s.get_blob(&Store::blob_hash(&want)).unwrap(),
                want,
                "batch {i}"
            );
        }
    }
    for b in [payload(acked, pad), second(acked, pad)] {
        assert_eq!(
            s.has_blob(&Store::blob_hash(&b)).unwrap(),
            kept,
            "a blob of the interrupted batch {acked}"
        );
    }
    let record = Hash::of(&held_record(acked).frame().1);
    assert_eq!(
        s.reader().transitions(&record).is_ok(),
        kept,
        "the record of the interrupted batch {acked}"
    );
    let last = if kept { acked } else { acked - 1 };
    let open: Vec<Hash> = s
        .reader()
        .open_tensions()
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(open, vec![tension_id(last)], "the open tensions");
    assert_eq!(s.check_objects().unwrap().transitions, last + 1);
    s.verify().unwrap();
}

/// ib14 (Review Focus 6): a held batch is acknowledged only by its seal. Each batch is two
/// writes and a record that moves a tension. A kill before the seal's marker loses the whole
/// batch, record included, and the log ends where the last seal left it; a kill after the marker
/// keeps the whole batch. The same holds when the first write of each batch rolls the segment.
#[test]
fn ib14_a_held_batch_killed_before_its_seal_is_not_acknowledged() {
    for rolled in [false, true] {
        let env: &[(&str, &str)] = if rolled {
            &[("RDY_CRASH_HELD", "1"), ("RDY_CRASH_SEGMENT_MAX", "64")]
        } else {
            &[("RDY_CRASH_HELD", "1")]
        };
        for (point, kept) in [
            (CrashPoint::BeforeMarker, false),
            (CrashPoint::BeforeSync, true),
            (CrashPoint::AfterSync, true),
            (CrashPoint::AfterIndex, true),
        ] {
            let what = format!("{point:?}, rolled {rolled}");
            let dir = tempfile::tempdir().unwrap();
            let acks = crash(dir.path(), point, 5, env);
            assert_eq!(acks.len(), 5, "{what}");
            if rolled {
                assert!(
                    acks.windows(2).all(|w| w[1].segment > w[0].segment),
                    "{what}: each batch rolls: {acks:?}"
                );
            }
            let s = Store::create(dir.path()).unwrap_or_else(|e| panic!("{what}: reopen: {e}"));
            if !kept {
                assert_eq!(
                    s.log_end(),
                    acks[4],
                    "{what}: the log ends at the last acknowledged end"
                );
                assert_eq!(
                    s.open_report().quarantined.len(),
                    1,
                    "{what}: {:?}",
                    s.open_report()
                );
            }
            held_batches_kept(&s, 5, kept, 0);
            drop(s);
            recovered(dir.path(), 5, kept, 0);
        }
    }
}

/// ib14 fix round 1: a seal fails after its batch was indexed, so the close removes the stale
/// index. A kill right after the database closes must leave a store that opens, with the whole
/// batch lost and every acknowledged batch kept.
#[test]
fn ib14_a_kill_as_a_failed_seal_closes_its_index_leaves_a_store_that_opens() {
    let dir = tempfile::tempdir().unwrap();
    let acks = crash(
        dir.path(),
        CrashPoint::StaleIndexClosed,
        5,
        &[("RDY_CRASH_HELD", "1"), ("RDY_CRASH_FAIL_SEAL", "1")],
    );
    assert_eq!(acks.len(), 5);
    let s = Store::create(dir.path()).unwrap_or_else(|e| panic!("reopen: {e}"));
    assert_eq!(s.log_end(), acks[4]);
    held_batches_kept(&s, 5, false, 0);
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
/// a larger store's segment, whose batch markers are genuine but start past this store's
/// acknowledged end (so the boundary filter alone does not drop them), and is cut after the copy.
/// That is a torn write to quarantine, never "an acknowledged batch follows the damage". This pins
/// the `closes_a_real_batch` check of `acknowledged_batch_follows`: without it the open refuses.
#[test]
fn kill1_marker_shaped_bytes_in_a_torn_tail_are_quarantined_not_refused() {
    let other = tempfile::tempdir().unwrap();
    let mut o = Store::create(other.path()).unwrap();
    for i in 0..12 {
        o.put_blob(&payload(i, 0)).unwrap();
    }
    drop(o);
    let embedded = std::fs::read(rdy_store::segment::seg_path(
        &other.path().join("segments"),
        0,
    ))
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let acks = crash(dir.path(), CrashPoint::BeforeSync, 5, &[]);
    let acked_end = acks[4];
    let seg = rdy_store::segment::seg_path(&dir.path().join("segments"), acked_end.segment);
    let mut file = std::fs::read(&seg).unwrap();
    file.truncate(acked_end.offset as usize); // drop the kept batch: the tail is crafted below
    assert!(
        (embedded.len() as u64) > acked_end.offset + 500,
        "the embedded copy holds markers that start past the acknowledged end"
    );
    let mut payload = embedded.clone();
    payload.extend_from_slice(&[7u8; 2000]);
    let mut torn = (payload.len() as u32).to_le_bytes().to_vec();
    torn.push(rdy_store::kinds::BLOB_CHUNK);
    torn.extend_from_slice(&rdy_model::Hash::of(&payload).0);
    torn.extend_from_slice(&payload);
    torn.truncate(37 + embedded.len() + 500); // cut after the embedded copy
    file.extend_from_slice(&torn);
    std::fs::write(&seg, &file).unwrap();
    let r = recovered(dir.path(), 5, false, 0);
    assert_eq!(r.quarantined.len(), 1, "{r:?}");
    assert_eq!(std::fs::read(&r.quarantined[0]).unwrap(), torn);
}

/// Ordering pin for a new store (fmt3): `create` writes `FORMAT` before anything else, so a
/// directory without one is never a store this rdy made. A crash right after the `FORMAT` write
/// leaves `FORMAT` alone, and the next open finishes the format batch. A crash inside the
/// `FORMAT` write leaves no `FORMAT`: that directory is refused, untouched, by open and create.
#[test]
fn kill1_create_writes_format_before_anything_else() {
    let what = "a store holding only FORMAT";
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("FORMAT"),
        format!("rdy store format\nstore_format {STORE_FORMAT}\n"),
    )
    .unwrap();
    finished_format(dir.path(), &[], what);

    let torn = tempfile::tempdir().unwrap();
    std::fs::write(torn.path().join(".FORMAT.tmp"), "rdy store").unwrap();
    for e in [
        Store::open(torn.path()).err(),
        Store::create(torn.path()).err(),
    ] {
        let e = e.expect("a directory without FORMAT is refused");
        assert!(matches!(e, rdy_store::StoreError::Format { .. }), "{e}");
    }
    let names: Vec<_> = std::fs::read_dir(torn.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, [".FORMAT.tmp"], "nothing was written");
}

/// A new store after `create` wrote `FORMAT`, before its first format batch: `FORMAT`, an empty
/// log and an empty index.
fn empty_store(dir: &Path) {
    std::fs::create_dir_all(dir.join("segments")).unwrap();
    drop(rdy_store::index::Index::open(&dir.join("index.redb")).unwrap());
    std::fs::write(
        dir.join("FORMAT"),
        format!("rdy store format\nstore_format {STORE_FORMAT}\n"),
    )
    .unwrap();
}

/// Opens a store directory the way a crash left it and checks the open finished the format batch:
/// the log holds a marker, the ref and `FORMAT` read back, every `blobs` entry survives, and a second open is a plain one.
fn finished_format(dir: &Path, blobs: &[Vec<u8>], what: &str) {
    let segments = dir.join("segments");
    let s = Store::create(dir).unwrap_or_else(|e| panic!("{what}: {e}"));
    assert!(
        !s.open_report().index_rebuilt,
        "{what}: {:?}",
        s.open_report()
    );
    assert!(
        rdy_store::segment::log_has_marker(&segments).unwrap(),
        "{what}: no marker"
    );
    // M2a1 (fmt2): one format number, `store_format N`, in place of log_format/min_reader
    assert_eq!(
        std::fs::read_to_string(dir.join("FORMAT")).unwrap(),
        format!("rdy store format\nstore_format {STORE_FORMAT}\n"),
        "{what}"
    );
    assert_eq!(
        s.get_ref("meta/store_format").unwrap(),
        Some(STORE_FORMAT.to_string().into_bytes()),
        "{what}"
    );
    drop(s);
    let mut s = Store::create(dir).unwrap_or_else(|e| panic!("{what}, second open: {e}"));
    let r = s.open_report().clone();
    assert!(!r.index_rebuilt, "{what}: {r:?}");
    assert!(r.quarantined.is_empty(), "{what}: {r:?}");
    for b in blobs {
        assert_eq!(s.get_blob(&Store::blob_hash(b)).unwrap(), *b, "{what}");
    }
    s.verify().unwrap();
    s.put_blob(b"after the format batch").unwrap();
    s.verify().unwrap();
}

/// A kill between the format batch's frames and its marker (or anywhere inside the batch) on a
/// brand-new store, after `create` wrote `FORMAT`: the next open finishes the format batch.
/// The crash points are armed after open, so each kill is the log cut at an offset of the batch.
#[test]
fn kill1_a_kill_inside_the_first_format_batch_of_a_new_store_is_finished_by_the_next_open() {
    let base = tempfile::tempdir().unwrap();
    empty_store(base.path()); // an empty store with its index and FORMAT
    let done = tempfile::tempdir().unwrap();
    copy_store(base.path(), done.path());
    drop(Store::create(done.path()).unwrap());
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
