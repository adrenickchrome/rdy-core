//! The write and open path of one flush per write (plan M2.0): batch markers (mark1), segment
//! rolls at batch boundaries (roll1), index rollback versus damage (kill1), checkpoints (ckpt1)
//! and the streaming catch-up (cat1).
#![allow(unused_imports, dead_code)]
use rdy_model::Hash;
use rdy_qa::fixtures::*;
use rdy_store::{Store, StoreError, kinds, segment::Location};
use serde_json::json;
use std::path::{Path, PathBuf};

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
        json!([{"op": "create", "type": "doc", "fields": {"title": "Marked", "doc_type": "glossary"}}]),
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

#[test]
fn mark1_a_reopened_store_uses_the_marker_rule() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    Store::open(&dir).unwrap().put_blob(b"marked").unwrap();
    let s = Store::open(&dir).unwrap();
    assert!(s.open_report().marker_rule, "{:?}", s.open_report());
}

/// Review Focus 1: the M1 rule would treat a lagging index's end as the acknowledged boundary and
/// silently truncate the acknowledged batches after the damage.
#[test]
fn mark1_damage_inside_an_acknowledged_batch_is_refused_even_when_the_index_lags() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    Store::open(&dir).unwrap().put_blob(b"first").unwrap();
    let early_index = std::fs::read(dir.join("index.redb")).unwrap();
    let end1 = {
        let mut s = Store::open(&dir).unwrap();
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
    let e = Store::open(&dir)
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
    Store::open(&dir).unwrap().put_blob(b"accepted").unwrap();
    let (s, synced) = rdy_store::fsutil::record_syncs(|| Store::open(&dir));
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
        let mut s = Store::open(&a).unwrap();
        for i in 0..5u8 {
            s.put_blob(&[i; 40]).unwrap();
        }
    }
    let mut chunk = std::fs::read(seg(&a, 0)).unwrap();
    chunk.extend_from_slice(&[9u8; 2000]);
    let b = base.path().join("b");
    Store::open(&b).unwrap().put_blob(b"first").unwrap();
    let early_index = std::fs::read(b.join("index.redb")).unwrap();
    let before = std::fs::read(seg(&b, 0)).unwrap();
    Store::open(&b).unwrap().put_blob(&chunk).unwrap();
    let path = seg(&b, 0);
    let mut bytes = std::fs::read(&path).unwrap();
    let embedded_end = before.len() + 37 + 37 + a_len(&a);
    bytes.truncate(embedded_end + 500); // a short write, after the embedded copy
    std::fs::write(&path, &bytes).unwrap();
    std::fs::write(b.join("index.redb"), &early_index).unwrap();
    let s = Store::open(&b).expect("a torn unacknowledged write is quarantined, not refused");
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
    let mut s = Store::open(&dir).unwrap();
    let hs: Vec<_> = (0..3u8).map(|i| s.put_blob(&[i; 5]).unwrap()).collect();
    assert!(dir.join(rdy_store::INDEX_OPEN_SENTINEL).exists());
    let copy = base.path().join("copy");
    copy_store(&dir, &copy);
    let c = Store::open(&copy).unwrap();
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
    Store::open(&dir).unwrap().put_blob(b"x").unwrap();
    assert!(!dir.join(rdy_store::INDEX_OPEN_SENTINEL).exists());
    assert!(!Store::open(&dir).unwrap().open_report().unclean_shutdown);
}

/// An open that fails after building its `Store` (here: the log names a newer reader) must not
/// erase a previous kill's sentinel on drop.
#[test]
fn kill1_a_refused_open_keeps_a_kills_sentinel() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::open(&dir).unwrap();
    s.set_refs(vec![(
        rdy_store::format::REF_MIN_READER.into(),
        b"3".to_vec(),
    )])
    .unwrap();
    let copy = base.path().join("copy");
    copy_store(&dir, &copy); // what a kill leaves, sentinel included
    drop(s);
    let e = Store::open(&copy)
        .err()
        .expect("the log says a newer rdy wrote it");
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
    let h = Store::open(&dir).unwrap().put_blob(b"kept").unwrap();
    let idx = dir.join("index.redb");
    let mut bytes = std::fs::read(&idx).unwrap();
    bytes[..512].fill(0xAB);
    std::fs::write(&idx, bytes).unwrap();
    let s = Store::open(&dir).unwrap();
    assert!(s.open_report().index_rebuilt);
    assert_eq!(s.get_blob(&h).unwrap(), b"kept");
    assert_eq!(damaged_files(&dir), 1);
}
