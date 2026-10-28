#![cfg(unix)]
// Unix only: it makes symlinks. FW-XP1 Phase 4 ports it.
//! The write and open path of one flush per write (plan M2.0): batch markers (mark1), segment
//! rolls at batch boundaries (roll1), index rollback versus damage (kill1), checkpoints (ckpt1)
//! and the streaming catch-up (cat1).
#![allow(unused_imports, dead_code)]
use rdy_model::Hash;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::{McpClient, Reply};
use rdy_store::CHECKPOINT_IDLE;
use rdy_store::{Store, StoreError, kinds, segment::Location};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn seg(store: &Path, n: u32) -> PathBuf {
    store.join("segments").join(format!("seg-{n:06}.log"))
}

/// Every whole frame in a segment file: (offset, kind, payload).
fn frames(bytes: &[u8]) -> Vec<(u64, u8, Vec<u8>)> {
    let mut out = vec![];
    let mut at = 0usize;
    while at + 37 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        if at + 37 + len > bytes.len() {
            break;
        }
        out.push((
            at as u64,
            bytes[at + 4],
            bytes[at + 37..at + 37 + len].to_vec(),
        ));
        at += 37 + len;
    }
    out
}

/// Copies a store directory as a crash would leave it on disk (without the lock file).
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

#[test]
fn mark1_every_store_write_ends_with_a_marker_that_matches_its_batch() {
    let (_d, mut r) = project();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "doc", "fields": {"title": "Marked", "doc_type": "page"}}]),
    )
    .unwrap();
    let bytes = std::fs::read(seg(r.store().dir(), 0)).unwrap();
    let fs = frames(&bytes);
    let (last_off, last_kind, last_payload) = fs.last().unwrap();
    assert_eq!(*last_kind, kinds::BATCH_COMMIT, "the log ends at a marker");
    assert_eq!(
        last_off + 37 + last_payload.len() as u64,
        bytes.len() as u64
    );
    let mut since: Vec<(u64, Hash)> = vec![];
    let mut markers = 0;
    for (off, kind, payload) in &fs {
        if *kind == kinds::BATCH_COMMIT {
            let m: kinds::BatchCommit = postcard::from_bytes(payload).unwrap();
            let i = since
                .iter()
                .position(|(o, _)| *o == m.start.offset)
                .expect("a marker's start is a frame of its batch");
            assert_eq!(since.len() - i, m.frames as usize);
            assert_eq!(
                m.digest,
                kinds::batch_digest(since[i..].iter().map(|(_, h)| h))
            );
            since.clear();
            markers += 1;
        } else {
            since.push((*off, Hash::of(payload)));
        }
    }
    assert!(markers >= 1);
    assert_eq!(
        r.fsck().unwrap().frames,
        fs.len() - markers,
        "fsck counts data frames"
    );
}

/// Review Focus 1: the M1 rule would treat a lagging index's end as the acknowledged boundary and
/// silently truncate the acknowledged batches after the damage.
#[test]
fn mark1_damage_inside_an_acknowledged_batch_is_refused_even_when_the_index_lags() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    Store::create(&dir).unwrap().put_blob(b"first").unwrap();
    let early_index = std::fs::read(dir.join("index.redb")).unwrap();
    let end1 = {
        let mut s = Store::create(&dir).unwrap();
        let end1 = s.log_end();
        s.put_blob(b"second").unwrap();
        s.put_blob(b"third").unwrap();
        end1
    };
    let path = seg(&dir, 0);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[end1.offset as usize + 40] ^= 0xff; // inside the second write's first frame
    std::fs::write(&path, &bytes).unwrap();
    std::fs::write(dir.join("index.redb"), &early_index).unwrap(); // the index lags the log
    let e = Store::create(&dir)
        .err()
        .expect("an acknowledged batch follows the damage");
    assert!(
        matches!(&e, StoreError::Io(io) if io.kind() == std::io::ErrorKind::InvalidData),
        "{e}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes, "nothing truncated");
}

/// A kill can leave a complete, marked batch that was never fsynced. The open accepts it, so it
/// must fsync the segment before the catch-up (and a checkpoint) can make the index durable past it.
#[test]
fn mark1_open_fsyncs_the_last_segment_it_accepts() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    Store::create(&dir).unwrap().put_blob(b"accepted").unwrap();
    let (s, synced) = rdy_store::fsutil::record_syncs(|| Store::create(&dir));
    s.unwrap();
    assert!(synced.contains(&seg(&dir, 0)), "{synced:?}");
}

/// A write whose bytes embed a copy of an rdy segment, torn after the copy: the embedded markers
/// are hash-valid but belong to another file, so the tail is an ordinary unacknowledged one and
/// the store opens (it used to refuse, stuck behind a "damaged acknowledged data" error).
#[test]
fn mark1_a_torn_write_that_embeds_a_copy_of_a_segment_opens() {
    let base = tempfile::tempdir().unwrap();
    let a = base.path().join("a");
    {
        let mut s = Store::create(&a).unwrap();
        for i in 0..5u8 {
            s.put_blob(&[i; 40]).unwrap();
        }
    }
    let mut chunk = std::fs::read(seg(&a, 0)).unwrap();
    chunk.extend_from_slice(&[9u8; 2000]);
    let b = base.path().join("b");
    Store::create(&b).unwrap().put_blob(b"first").unwrap();
    let early_index = std::fs::read(b.join("index.redb")).unwrap();
    let before = std::fs::read(seg(&b, 0)).unwrap();
    Store::create(&b).unwrap().put_blob(&chunk).unwrap();
    let path = seg(&b, 0);
    let mut bytes = std::fs::read(&path).unwrap();
    let embedded_end = before.len() + 37 + 37 + a_len(&a);
    bytes.truncate(embedded_end + 500); // a short write, after the embedded copy
    std::fs::write(&path, &bytes).unwrap();
    std::fs::write(b.join("index.redb"), &early_index).unwrap();
    let s = Store::create(&b).expect("a torn unacknowledged write is quarantined, not refused");
    assert_eq!(
        s.open_report().quarantined.len(),
        1,
        "{:?}",
        s.open_report()
    );
}

fn a_len(store: &Path) -> usize {
    std::fs::metadata(seg(store, 0)).unwrap().len() as usize
}

fn damaged_files(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("index.redb.damaged-")
        })
        .count()
}

/// Review Focus 3: the on-disk state of a store whose process was killed now.
#[test]
fn kill1_a_copy_of_a_live_store_opens_by_rollback_not_rebuild() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    let hs: Vec<_> = (0..3u8).map(|i| s.put_blob(&[i; 5]).unwrap()).collect();
    assert!(dir.join(rdy_store::INDEX_OPEN_SENTINEL).exists());
    let copy = base.path().join("copy");
    copy_store(&dir, &copy);
    let c = Store::create(&copy).unwrap();
    let r = c.open_report().clone();
    assert!(r.unclean_shutdown, "{r:?}");
    assert!(!r.index_rebuilt, "a kill is not damage: {r:?}");
    for (i, h) in hs.iter().enumerate() {
        assert_eq!(c.get_blob(h).unwrap(), vec![i as u8; 5]);
    }
    assert_eq!(damaged_files(&copy), 0);
}

#[test]
fn kill1_a_clean_close_removes_the_open_sentinel() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    Store::create(&dir).unwrap().put_blob(b"x").unwrap();
    assert!(!dir.join(rdy_store::INDEX_OPEN_SENTINEL).exists());
    assert!(!Store::create(&dir).unwrap().open_report().unclean_shutdown);
}

/// A child process spawned by another thread while the store is open holds a duplicate of the
/// lock file's descriptor until it execs. Closing ours must still release the lock: a lock left to
/// the last descriptor's close made a reopen in this process fail as `Locked` (`store_flush`
/// flaked this way next to the MCP test's spawn).
#[test]
fn kill1_a_clean_close_releases_the_lock_a_spawned_child_still_shares() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    s.put_blob(b"x").unwrap();
    let child_copy = s.share_lock_descriptor_for_tests().unwrap();
    drop(s);
    let r = Store::create(&dir).map(|s| s.open_report().clone());
    drop(child_copy);
    let r = r.expect("a closed store reopens while a spawned child still holds its descriptor");
    assert!(!r.unclean_shutdown, "{r:?}");
}

/// An open that fails after building its `Store` (here: the log names another store format) must
/// not erase a previous kill's sentinel on drop.
#[test]
fn kill1_a_refused_open_keeps_a_kills_sentinel() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    // the features ref, set to a manifest with a feature this rdy does not know, of class
    // incompatible, in place of min_reader
    let other = rdy_store::format::StoreFeatures::of(vec![rdy_store::format::StoreFeature {
        name: "future_x".into(),
        class: rdy_store::format::FeatureClass::Incompatible,
        required_rdy: "99.0.0".into(),
    }]);
    s.set_refs_unchecked_for_tests(vec![(
        rdy_store::format::REF_STORE_FEATURES.into(),
        other.to_json(),
    )])
    .unwrap();
    let copy = base.path().join("copy");
    copy_store(&dir, &copy); // what a kill leaves, sentinel included
    drop(s);
    let e = Store::create(&copy)
        .err()
        .expect("the log says another rdy build wrote it");
    assert!(matches!(e, StoreError::Format { .. }), "{e}");
    assert!(
        copy.join(rdy_store::INDEX_OPEN_SENTINEL).exists(),
        "the kill's evidence is kept"
    );
}

/// Guard: damage to a cleanly closed index is still damage.
#[test]
fn kill1_a_damaged_index_after_a_clean_close_is_still_rebuilt() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let h = Store::create(&dir).unwrap().put_blob(b"kept").unwrap();
    let idx = dir.join("index.redb");
    let mut bytes = std::fs::read(&idx).unwrap();
    bytes[..512].fill(0xAB);
    std::fs::write(&idx, bytes).unwrap();
    let s = Store::create(&dir).unwrap();
    assert!(s.open_report().index_rebuilt);
    assert_eq!(s.get_blob(&h).unwrap(), b"kept");
    assert_eq!(damaged_files(&dir), 1);
}

#[test]
fn ckpt1_a_write_leaves_the_durable_index_behind_the_log() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::create(&dir).unwrap(); // a new store: format batch, then a checkpoint
    for i in 0..5u8 {
        s.put_blob(&[i; 7]).unwrap();
    }
    let copy = base.path().join("copy");
    copy_store(&dir, &copy);
    let c = Store::create(&copy).unwrap();
    // 5 writes x (chunk, manifest, marker): acknowledged by the log flush alone
    assert_eq!(
        c.open_report().caught_up_frames,
        15,
        "{:?}",
        c.open_report()
    );
}

#[test]
fn ckpt1_a_checkpoint_makes_the_index_durable() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    for i in 0..5u8 {
        s.put_blob(&[i; 7]).unwrap();
    }
    s.checkpoint().unwrap();
    let copy = base.path().join("copy");
    copy_store(&dir, &copy);
    assert_eq!(
        Store::create(&copy).unwrap().open_report().caught_up_frames,
        0
    );
}

#[test]
fn ckpt1_the_index_checkpoints_every_1000_frames() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    drop(Store::create(&dir).unwrap());
    let mut s = Store::create(&dir).unwrap();
    assert_eq!(s.checkpoint_count(), 0);
    for i in 0..400u32 {
        s.put_blob(&i.to_le_bytes()).unwrap(); // 3 frames each
    }
    // the checkpoint fires after write 334 (1,002 frames); 66 writes (198 frames) follow it
    assert_eq!(s.checkpoint_count(), 1);
    assert_eq!(s.pending_checkpoint_frames(), 198);
}

#[test]
fn ckpt1_an_idle_store_checkpoints_after_two_seconds() {
    let base = tempfile::tempdir().unwrap();
    let mut s = Store::create(&base.path().join("s")).unwrap();
    s.put_blob(b"idle").unwrap();
    assert!(!s.checkpoint_if_idle(Instant::now()).unwrap());
    assert!(
        s.checkpoint_if_idle(Instant::now() + CHECKPOINT_IDLE)
            .unwrap()
    );
    assert_eq!(s.pending_checkpoint_frames(), 0);
    assert!(
        !s.checkpoint_if_idle(Instant::now() + CHECKPOINT_IDLE * 2)
            .unwrap(),
        "nothing new"
    );
}

#[test]
fn ckpt1_a_clean_close_leaves_a_durable_index() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    {
        let mut s = Store::create(&dir).unwrap();
        for i in 0..3u8 {
            s.put_blob(&[i; 3]).unwrap();
        }
    }
    let r = Store::create(&dir).unwrap().open_report().clone();
    assert_eq!(r.caught_up_frames, 0, "{r:?}");
    assert!(!r.unclean_shutdown);
}

/// Review Focus 5.
#[test]
fn ckpt1_a_failed_checkpoint_never_fails_the_write() {
    let base = tempfile::tempdir().unwrap();
    let mut s = Store::create(&base.path().join("s")).unwrap();
    s.set_checkpoint_frames(1);
    s.fail_next_checkpoint();
    let h = s.put_blob(b"durable in the log").unwrap();
    assert!(s.last_checkpoint_error().unwrap().contains("injected"));
    s.put_blob(b"next").unwrap();
    assert!(
        s.last_checkpoint_error().is_none(),
        "the next checkpoint retried"
    );
    assert_eq!(s.get_blob(&h).unwrap(), b"durable in the log");
}

/// Final review (Important 3): an idle checkpoint that fails is recorded like a write's, so a
/// disk that refuses it is visible before the 1,000-frame checkpoint or the close also fails.
#[test]
fn ckpt1_a_failed_idle_checkpoint_is_recorded_until_one_succeeds() {
    let base = tempfile::tempdir().unwrap();
    let mut s = Store::create(&base.path().join("s")).unwrap();
    s.put_blob(b"idle").unwrap();
    s.fail_next_checkpoint();
    let later = Instant::now() + CHECKPOINT_IDLE;
    let e = s.checkpoint_if_idle(later).unwrap_err();
    assert!(e.to_string().contains("injected"), "{e}");
    assert!(
        s.last_checkpoint_error()
            .is_some_and(|m| m.contains("injected")),
        "{:?}",
        s.last_checkpoint_error()
    );
    assert!(
        s.checkpoint_if_idle(later).unwrap(),
        "the next tick retries"
    );
    assert!(s.last_checkpoint_error().is_none());
}

/// Final review (Important 4): `close` makes the clean close's checkpoint and returns its error,
/// keeping the sentinel when it fails, so the next open still catches up and the failure is seen.
#[test]
fn ckpt1_close_returns_a_failed_checkpoint_and_keeps_the_sentinel() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    let h = s.put_blob(b"durable in the log").unwrap();
    s.fail_next_checkpoint();
    let e = s.close().unwrap_err();
    assert!(e.to_string().contains("injected"), "{e}");
    assert!(dir.join(rdy_store::INDEX_OPEN_SENTINEL).exists());
    let s = Store::create(&dir).unwrap();
    assert!(s.open_report().unclean_shutdown, "{:?}", s.open_report());
    assert!(!s.open_report().index_rebuilt, "{:?}", s.open_report());
    assert_eq!(s.get_blob(&h).unwrap(), b"durable in the log");
}

#[test]
fn ckpt1_close_checkpoints_and_removes_the_sentinel() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    s.put_blob(b"x").unwrap();
    s.close().unwrap();
    assert!(!dir.join(rdy_store::INDEX_OPEN_SENTINEL).exists());
    let r = Store::create(&dir).unwrap().open_report().clone();
    assert!(!r.unclean_shutdown, "{r:?}");
    assert_eq!(r.caught_up_frames, 0, "{r:?}");
}

/// Final review (Minor 12): the sentinel is never opened through a symlink, so a link planted at
/// `<store>/index.open` cannot make every open truncate the file it points to.
#[test]
fn kill1_the_sentinel_never_truncates_through_a_symlink() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    drop(Store::create(&dir).unwrap());
    let target = base.path().join("precious");
    std::fs::write(&target, b"keep me").unwrap();
    std::os::unix::fs::symlink(&target, dir.join(rdy_store::INDEX_OPEN_SENTINEL)).unwrap();
    let s = Store::create(&dir).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"keep me");
    drop(s);
    assert_eq!(std::fs::read(&target).unwrap(), b"keep me");
}

#[test]
fn ckpt1_the_mcp_server_checkpoints_when_idle() {
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    assert!(matches!(
        c.call(
            "rdy_session",
            json!({"actor": "agent:qa", "role": "writer"})
        ),
        Reply::Ok(_)
    ));
    c.call("rdy_write", json!({"ops": [{"op": "create", "type": "doc", "fields": {"title": "Idle", "doc_type": "page"}}]})).ok();
    // the checkpoint lands once the server has been idle for CHECKPOINT_IDLE: poll a copy of
    // the store until an open of it has nothing to catch up, with a generous deadline
    let t = std::time::Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(200));
        let copy = tempfile::tempdir().unwrap();
        // M2a1 (fmt2): the store is .rdy/store again
        copy_store(&dir.path().join(".rdy/store"), copy.path());
        // a copy taken mid-checkpoint may not open: not checkpointed yet, poll again
        let caught_up = Store::create(copy.path())
            .map(|s| s.open_report().caught_up_frames)
            .unwrap_or(u64::MAX);
        if caught_up == 0 {
            break;
        }
        assert!(
            t.elapsed() < Duration::from_secs(30),
            "the idle server never checkpointed: {caught_up} frames to catch up"
        );
    }
}

/// Carry-forward (Task 3 review): the marker rule must not depend on seeing a marker before the
/// first bad frame once the index lags. With the index at the start, the M1 rule would take the lagging `indexed_upto` as the acknowledged boundary and truncate acknowledged
/// batches after damage in the first batch.
#[test]
fn mark1_a_lagging_index_still_refuses_damage_in_the_first_batch() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    {
        let mut s = Store::create(&dir).unwrap();
        s.put_blob(b"second").unwrap();
        s.put_blob(b"third").unwrap();
    }
    // an index that never got past the start of the log (a store with no FORMAT is refused
    // before its log is read: fmt3)
    std::fs::remove_file(dir.join("index.redb")).unwrap();
    {
        let idx = rdy_store::index::Index::open(&dir.join("index.redb")).unwrap();
        idx.apply_durable(
            &[],
            Location {
                segment: 0,
                offset: 0,
            },
        )
        .unwrap();
    }
    let path = seg(&dir, 0);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[40] ^= 0xff; // inside the first batch's first frame
    std::fs::write(&path, &bytes).unwrap();
    let e = Store::create(&dir)
        .err()
        .expect("acknowledged batches follow the damage");
    assert!(
        matches!(&e, StoreError::Io(io) if io.kind() == std::io::ErrorKind::InvalidData),
        "{e}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes, "nothing truncated");
    let torn = std::fs::read_dir(dir.join("segments"))
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".torn-")
        })
        .count();
    assert_eq!(torn, 0, "nothing quarantined");
}

/// Carry-forward (Task 6 review): an index file that redb must repair (a pending non-durable
/// commit left by a kill) is trusted, and the open catches it up from the log: it is neither
/// rebuilt nor set aside. With redb 4.3 this goes through the trusted `Ok(clean)` arm, not the
/// sentinel-decided `Ok(clean == false)` arm: redb repairs the file at open and `check_integrity`
/// then reports it clean (final review, Important 2; pinned in the rdy-store index test
/// `kill1_redb_repairs_a_copy_of_a_live_index_at_open_and_then_reports_it_clean`). So
/// `redb_repaired` here is redb's repair at open.
#[test]
fn kill1_a_pending_non_durable_commit_is_a_rollback_that_catches_up() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    let hs: Vec<_> = (0..4u8).map(|i| s.put_blob(&[i; 9]).unwrap()).collect();
    let copy = base.path().join("copy");
    copy_store(&dir, &copy);
    let c = Store::create(&copy).unwrap();
    let r = c.open_report().clone();
    assert!(r.unclean_shutdown, "{r:?}");
    assert!(
        r.redb_repaired,
        "redb repaired the killed file at open (its repair callback fired): {r:?}"
    );
    assert!(!r.index_rebuilt, "{r:?}");
    assert_eq!(r.caught_up_frames, 12, "{r:?}");
    assert_eq!(damaged_files(&copy), 0);
    for (i, h) in hs.iter().enumerate() {
        assert_eq!(c.get_blob(h).unwrap(), vec![i as u8; 9]);
    }
}

/// Three batches of 1,000 frames each, with no checkpoint: a copy must catch up 3,003 frames.
fn lagging_copy(base: &Path) -> PathBuf {
    let dir = base.join("s");
    let mut s = Store::create(&dir).unwrap();
    s.set_checkpoint_frames(u64::MAX);
    for b in 0..3 {
        let items = (0..1000)
            .map(|i| (kinds::BLOB_CHUNK, format!("chunk {b}-{i}").into_bytes()))
            .collect();
        s.write(items).unwrap();
    }
    let copy = base.join("copy");
    copy_store(&dir, &copy);
    copy
}

#[test]
fn cat1_catch_up_streams_in_chunks_of_1000_frames() {
    let base = tempfile::tempdir().unwrap();
    let copy = lagging_copy(base.path());
    let s = Store::create(&copy).unwrap();
    let r = s.open_report().clone();
    assert_eq!(r.caught_up_frames, 3003, "{r:?}");
    assert_eq!(r.catch_up_commits, 4, "1000 + 1000 + 1000 + 3: {r:?}");
    assert!(s.has(&Hash::of(b"chunk 2-999")).unwrap());
}

#[test]
fn cat1_a_catch_up_ends_with_a_checkpoint() {
    let base = tempfile::tempdir().unwrap();
    let copy = lagging_copy(base.path());
    let _caught_up = Store::create(&copy).unwrap(); // still open: the index file is what a kill leaves
    let again = base.path().join("again");
    copy_store(&copy, &again);
    assert_eq!(
        Store::create(&again)
            .unwrap()
            .open_report()
            .caught_up_frames,
        0
    );
}

#[test]
fn cat1_an_index_point_inside_a_frame_rebuilds_from_the_whole_log() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let h = Store::create(&dir).unwrap().put_blob(b"kept").unwrap();
    {
        let idx = rdy_store::index::Index::open(&dir.join("index.redb")).unwrap();
        idx.apply(
            &[],
            Location {
                segment: 0,
                offset: 5,
            },
        )
        .unwrap();
        idx.checkpoint().unwrap();
    }
    let s = Store::create(&dir).unwrap_or_else(|e| panic!("{e}"));
    assert!(s.open_report().index_rebuilt, "{:?}", s.open_report());
    assert_eq!(s.get_blob(&h).unwrap(), b"kept");
}

/// Damage in a sealed segment after a valid index point is the log's fault, not the index's:
/// the open refuses and keeps the index, rather than setting it aside for a rebuild that would
/// hit the same frame.
#[test]
fn cat1_damage_in_a_sealed_segment_after_the_index_point_keeps_the_index() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    s.set_segment_max(4096);
    s.set_checkpoint_frames(u64::MAX);
    for i in 0..20u32 {
        s.put_blob(&vec![i as u8; 1000]).unwrap();
    }
    assert!(s.log_end().segment >= 2, "{:?}", s.log_end());
    let copy = base.path().join("copy");
    copy_store(&dir, &copy); // its durable index point is in segment 0, after the format batch
    drop(s);
    let path = seg(&copy, 1);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[40] ^= 0xff; // inside segment 1's first payload
    std::fs::write(&path, &bytes).unwrap();
    assert!(Store::create(&copy).is_err());
    assert_eq!(damaged_files(&copy), 0, "the index was not set aside");
}

/// An index point past the end of a sealed segment means acknowledged frames are missing from the
/// log (seg1): the open refuses and keeps the index, rather than rebuilding it from what is left,
/// which would silently roll canon back.
#[test]
fn cat1_an_index_point_past_the_end_of_a_sealed_segment_refuses_and_keeps_the_index() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    s.set_segment_max(4096);
    s.set_checkpoint_frames(u64::MAX);
    s.set_refs(vec![("r".into(), b"v1".to_vec())]).unwrap();
    let mut i = 0u32;
    while s.log_end().segment < 1 {
        s.put_blob(&vec![i as u8; 1000]).unwrap();
        i += 1;
    }
    s.set_refs(vec![("r".into(), b"v2".to_vec())]).unwrap();
    let blob = s.put_blob(b"in segment one").unwrap();
    s.checkpoint().unwrap(); // the durable index point is inside segment 1
    let point = s.log_end();
    assert_eq!(point.segment, 1, "{point:?}");
    assert!(point.offset > 0, "{point:?}");
    while s.log_end().segment < 3 {
        s.put_blob(&vec![i as u8; 1000]).unwrap();
        i += 1;
    }
    let copy = base.path().join("copy");
    copy_store(&dir, &copy);
    drop(s);
    std::fs::write(seg(&copy, 1), b"").unwrap(); // segment 1 lost what the index acknowledged
    let e = Store::create(&copy)
        .err()
        .expect("open must refuse, not rebuild over the loss");
    assert!(matches!(&e, StoreError::MissingFrames { .. }), "{e}");
    assert!(e.to_string().contains("seg-000001.log"), "{e}");
    assert_eq!(damaged_files(&copy), 0, "the index was not set aside");
    // the index was kept: restoring the segment brings the writes back, with no rebuild
    std::fs::copy(seg(&dir, 1), seg(&copy, 1)).unwrap();
    let s = Store::create(&copy).unwrap();
    assert!(!s.open_report().index_rebuilt, "{:?}", s.open_report());
    assert_eq!(s.get_ref("r").unwrap().unwrap(), b"v2");
    assert_eq!(s.get_blob(&blob).unwrap(), b"in segment one");
}

/// Triage #1235: a sealed segment cut at a frame boundary inside its last batch (its marker lost)
/// has lost acknowledged frames even though every frame left is whole. The open refuses with
/// `MissingFrames` and keeps the index, rather than catching up past the loss.
#[test]
fn seal1_a_sealed_segment_cut_before_its_last_marker_refuses_and_keeps_the_index() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    s.set_segment_max(4096);
    s.set_checkpoint_frames(u64::MAX);
    s.set_refs(vec![("r".into(), b"v1".to_vec())]).unwrap();
    s.checkpoint().unwrap(); // the durable index point is in segment 0
    let mut i = 0u32;
    while s.log_end().segment < 3 {
        s.put_blob(&vec![i as u8; 1000]).unwrap();
        i += 1;
    }
    let copy = base.path().join("copy");
    copy_store(&dir, &copy);
    drop(s);
    let bytes = std::fs::read(seg(&copy, 1)).unwrap();
    let (marker_at, kind, _) = frames(&bytes).pop().unwrap();
    assert_eq!(
        kind,
        kinds::BATCH_COMMIT,
        "a sealed segment ends at a marker"
    );
    std::fs::write(seg(&copy, 1), &bytes[..marker_at as usize]).unwrap();
    let e = Store::create(&copy)
        .err()
        .expect("open must refuse, not catch up past the loss");
    assert!(matches!(&e, StoreError::MissingFrames { .. }), "{e}");
    assert!(e.to_string().contains("seg-000001.log"), "{e}");
    assert_eq!(damaged_files(&copy), 0, "the index was not set aside");
    std::fs::copy(seg(&dir, 1), seg(&copy, 1)).unwrap();
    Store::create(&copy).unwrap();
}

/// Segments roll only between batches: with a tiny segment limit, every sealed segment ends at a
/// batch marker and a store reopens and reads every blob back.
#[test]
fn roll1_every_sealed_segment_of_a_rolling_store_ends_at_a_marker() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    s.set_segment_max(400);
    let blobs: Vec<_> = (0..12u8)
        .map(|i| (s.put_blob(&[i; 60]).unwrap(), [i; 60]))
        .collect();
    drop(s);
    let n = (0..).take_while(|i| seg(&dir, *i).exists()).count();
    assert!(
        n >= 3,
        "the limit should have rolled the log, got {n} segments"
    );
    for i in 0..n as u32 {
        let fs = frames(&std::fs::read(seg(&dir, i)).unwrap());
        assert_eq!(
            fs.last().unwrap().1,
            kinds::BATCH_COMMIT,
            "segment {i} ends at a marker"
        );
    }
    let s = Store::create(&dir).unwrap();
    for (h, bytes) in blobs {
        assert_eq!(s.get_blob(&h).unwrap(), bytes);
    }
}
