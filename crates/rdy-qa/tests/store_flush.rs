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
