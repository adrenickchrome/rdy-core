//! dl4: the ingest writes each dumped batch where the source log held it, so the new log is the
//! source log byte for byte; the first open builds the index from it.
//!
//! ec2 (RULING F2): the upgrade batch of a log with no repo id gives it one.
use rdy_store::dump::{DumpBatch, DumpItem, DumpReader, Frame};
use rdy_store::format::{
    self, DumpStore, REF_STORE_FEATURES, REF_STORE_FORMAT, baseline_features, encode_u32,
};
use rdy_store::ingest::Ingest;
use rdy_store::kinds::{self, BatchCommit, RefUpdate};
use rdy_store::meta::REF_REPO_ID;
use rdy_store::segment::Location;
use rdy_store::{Store, StoreError};
use std::path::Path;

fn batches(s: &Store) -> Vec<DumpBatch> {
    let mut out = vec![];
    s.reader()
        .for_each_batch(s.log_end(), |b| {
            out.push(b);
            Ok(())
        })
        .unwrap();
    out
}

fn ingest(to: &Path, bs: &[DumpBatch]) -> Result<(), StoreError> {
    let mut i = Ingest::create(to)?;
    for (n, b) in bs.iter().enumerate() {
        i.batch(n as u64, b)?;
    }
    i.finish().map(|_| ())
}

fn segments(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<_> = std::fs::read_dir(dir.join("segments"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&p).unwrap(),
            )
        })
        .collect();
    v.sort();
    v
}

/// A store whose small segments roll several times, with held batches that pass the soft limit.
fn rolling_store(base: &Path) -> Store {
    let mut s = Store::create(&base.join("src")).unwrap();
    s.set_segment_max(4096);
    for i in 0..20 {
        s.put_blob(format!("blob {i} {}", "x".repeat(300)).as_bytes())
            .unwrap();
    }
    s.set_held(true).unwrap();
    for i in 0..6 {
        s.put_blob(format!("held {i} {}", "y".repeat(900)).as_bytes())
            .unwrap();
    }
    s.seal(vec![]).unwrap();
    s.set_held(false).unwrap();
    s.set_refs(vec![("view/v".into(), b"head".to_vec())])
        .unwrap();
    s
}

#[test]
fn dl4_an_ingested_log_is_the_source_log_byte_for_byte() {
    let base = tempfile::tempdir().unwrap();
    let s = rolling_store(base.path());
    let to = base.path().join("dst");
    ingest(&to, &batches(&s)).unwrap();
    assert_eq!(segments(&to), segments(&base.path().join("src")));
    assert_eq!(
        std::fs::read(to.join("FORMAT")).unwrap(),
        std::fs::read(base.path().join("src/FORMAT")).unwrap()
    );
}

#[test]
fn dl4_ingest_rolls_where_the_source_rolled() {
    let base = tempfile::tempdir().unwrap();
    let s = rolling_store(base.path());
    assert!(
        segments(&base.path().join("src")).len() >= 3,
        "the source rolled"
    );
    let to = base.path().join("dst");
    ingest(&to, &batches(&s)).unwrap();
    assert_eq!(
        segments(&to).len(),
        segments(&base.path().join("src")).len()
    );
}

#[test]
fn dl4_the_first_open_builds_the_index_from_the_log() {
    let base = tempfile::tempdir().unwrap();
    let s = rolling_store(base.path());
    let want = s.index_dump_for_tests().unwrap();
    let to = base.path().join("dst");
    ingest(&to, &batches(&s)).unwrap();
    let mut t = Store::open(&to).unwrap();
    assert!(t.open_report().index_rebuilt || t.open_report().caught_up_frames > 0);
    assert_eq!(t.index_dump_for_tests().unwrap(), want, "the open's index");
    t.reindex().unwrap();
    assert_eq!(
        t.index_dump_for_tests().unwrap(),
        want,
        "a reindex changes nothing"
    );
    assert_eq!(t.verify().unwrap(), s.verify().unwrap());
}

#[test]
fn dl4_a_batch_out_of_place_is_refused() {
    let base = tempfile::tempdir().unwrap();
    let s = rolling_store(base.path());
    let mut bs = batches(&s);
    bs.swap(1, 2);
    let e = ingest(&base.path().join("dst"), &bs).unwrap_err();
    assert!(
        matches!(&e, StoreError::Dump { what, .. } if what.starts_with("batch 1 starts at segment")),
        "{e}"
    );
}

#[test]
fn dl4_a_batch_that_does_not_match_its_digest_is_refused() {
    let base = tempfile::tempdir().unwrap();
    let s = rolling_store(base.path());
    let mut bs = batches(&s);
    bs[3].digest = rdy_model::Hash::of(b"another");
    let e = ingest(&base.path().join("dst"), &bs).unwrap_err();
    assert!(
        matches!(&e, StoreError::Dump { what, .. } if what == "batch 3 does not match its digest"),
        "{e}"
    );
}

#[test]
fn dl4_ingest_never_writes_into_an_existing_directory() {
    let base = tempfile::tempdir().unwrap();
    let to = base.path().join("dst");
    std::fs::create_dir(&to).unwrap();
    assert!(Ingest::create(&to).is_err());
    assert_eq!(
        std::fs::read_dir(&to).unwrap().count(),
        0,
        "nothing written"
    );
}

#[test]
fn dl4_a_refused_batch_in_the_next_segment_writes_no_segment() {
    // A batch that starts in the next segment but not at its offset 0 is refused before the log
    // rolls, so the refusal leaves no new segment file.
    let base = tempfile::tempdir().unwrap();
    let s = rolling_store(base.path());
    let bs = batches(&s);
    let to = base.path().join("dst");
    let mut i = Ingest::create(&to).unwrap();
    i.batch(0, &bs[0]).unwrap();
    let before = segments(&to);
    assert_eq!(before.len(), 1, "batch 0 is in segment 0");
    let mut b = bs[1].clone();
    b.start = Location {
        segment: 1,
        offset: 5,
    };
    let e = i.batch(1, &b).unwrap_err();
    assert!(
        matches!(&e, StoreError::Dump { what, .. } if what.starts_with("batch 1 starts at segment 1 offset 5")),
        "{e}"
    );
    let after: Vec<String> = segments(&to).into_iter().map(|(n, _)| n).collect();
    let before: Vec<String> = before.into_iter().map(|(n, _)| n).collect();
    assert_eq!(after, before, "no new segment file");
}

/// The source log `src` is a prefix of the ingested log `dst`, byte for byte: every segment but
/// the last is equal, and the last one starts with the source's.
fn is_prefix(src: &[(String, Vec<u8>)], dst: &[(String, Vec<u8>)]) -> bool {
    src.len() == dst.len()
        && src.iter().zip(dst).enumerate().all(|(i, (s, d))| {
            s.0 == d.0
                && if i + 1 == src.len() {
                    d.1.starts_with(&s.1)
                } else {
                    d.1 == s.1
                }
        })
}

/// Every frame of `s` from `from` to its log end: (kind, payload).
fn frames_after(s: &Store, from: Location) -> Vec<(u8, Vec<u8>)> {
    let mut out = vec![];
    s.reader()
        .for_each_frame_from(from, s.log_end(), |_, kind, _, payload, _| {
            out.push((kind, payload.to_vec()));
            Ok(())
        })
        .unwrap();
    out
}

#[test]
fn dl8_a_log_of_an_older_store_format_gets_one_upgrade_batch() {
    // FW-UP1 in its smallest form: a log of a scalar store format (the frozen (2, 15) pair)
    // keeps every byte, and one logged batch after it sets `meta/store_features` to the
    // baseline features; its historical `meta/store_format` stays
    let base = tempfile::tempdir().unwrap();
    let (bs, src_end) = fixture_batches("v2-s15.rdydump");
    // without the upgrade batch, the log names only the scalar format and an open refuses it
    let plain = base.path().join("plain");
    ingest(&plain, &bs).unwrap();
    let e = Store::open(&plain).err().expect("an older log is refused");
    assert!(
        matches!(&e, StoreError::Format { message, .. } if message == "store format 15 predates feature-based stores"),
        "{e}"
    );
    let src = segments(&plain);
    // with it, the store opens as a feature store
    let to = base.path().join("dst");
    let mut i = Ingest::create(&to).unwrap();
    for (n, b) in bs.iter().enumerate() {
        i.batch(n as u64, b).unwrap();
    }
    assert_eq!(
        i.end(),
        src_end,
        "the dump's batches end where the source ends"
    );
    assert!(
        i.upgrade(&DumpStore::Legacy(15)).unwrap(),
        "an upgrade batch is appended"
    );
    let report = i.finish().unwrap();
    assert_eq!(report.batches, bs.len() as u64, "the dump's batches only");
    assert!(
        is_prefix(&src, &segments(&to)),
        "the source log is kept byte for byte"
    );
    assert_eq!(
        std::fs::read(to.join("FORMAT")).unwrap(),
        baseline_features().to_text().into_bytes()
    );
    let t = Store::open(&to).unwrap();
    assert_eq!(report.end, t.log_end());
    let after = frames_after(&t, src_end);
    assert_eq!(after.len(), 2, "one ref frame and its marker: {after:?}");
    assert_eq!(after[0].0, kinds::REF_UPDATE);
    let u: RefUpdate = postcard::from_bytes(&after[0].1).unwrap();
    assert_eq!(
        u,
        RefUpdate {
            name: REF_STORE_FEATURES.into(),
            value: baseline_features().to_json(),
            at: src_end,
        }
    );
    assert_eq!(after[1].0, kinds::BATCH_COMMIT);
    assert_eq!(
        t.reader().refs_with_prefix(REF_STORE_FORMAT).unwrap(),
        vec![(REF_STORE_FORMAT.to_string(), encode_u32(15))],
        "the historical scalar ref stays"
    );
}

#[test]
fn dl8_a_log_of_this_store_format_gets_no_upgrade_batch() {
    let base = tempfile::tempdir().unwrap();
    let s = rolling_store(base.path());
    let to = base.path().join("dst");
    let mut i = Ingest::create(&to).unwrap();
    for (n, b) in batches(&s).iter().enumerate() {
        i.batch(n as u64, b).unwrap();
    }
    assert!(
        !i.upgrade(&DumpStore::Features(baseline_features()))
            .unwrap(),
        "nothing is appended"
    );
    assert_eq!(i.finish().unwrap().end, s.log_end());
    assert_eq!(segments(&to), segments(&base.path().join("src")));
}

#[test]
fn dl8_a_log_of_a_newer_store_format_is_refused() {
    let base = tempfile::tempdir().unwrap();
    let s = rolling_store(base.path());
    let to = base.path().join("dst");
    let mut i = Ingest::create(&to).unwrap();
    for (n, b) in batches(&s).iter().enumerate() {
        i.batch(n as u64, b).unwrap();
    }
    let e = i.upgrade(&DumpStore::Legacy(19)).unwrap_err();
    assert!(
        matches!(&e, StoreError::Dump { what, .. } if what == "store format 19 is newer than the last scalar store format 18"),
        "{e}"
    );
    assert_eq!(i.end(), s.log_end(), "nothing is appended");
}

/// The batches of a dump fixture (`fixtures/dump/<name>`), read with `DumpReader`, and the source
/// log end its header names.
fn fixture_batches(name: &str) -> (Vec<DumpBatch>, Location) {
    let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/dump")
        .join(name);
    let (mut rd, _, h) = DumpReader::open(std::fs::File::open(p).unwrap()).unwrap();
    let mut out = vec![];
    while let Some(item) = rd.next().unwrap() {
        if let DumpItem::Batch(b) = item {
            out.push(b);
        }
    }
    (out, h.log_end)
}

/// The v1-s11 fixture's batches ingested into `to`, then `upgrade(11)`; returns the fixture's
/// log end.
fn upgraded_s11(to: &Path) -> Location {
    let (bs, log_end) = fixture_batches("v1-s11.rdydump");
    let mut i = Ingest::create(to).unwrap();
    for (n, b) in bs.iter().enumerate() {
        i.batch(n as u64, b).unwrap();
    }
    assert_eq!(i.end(), log_end, "the fixture's batches end at its log end");
    assert!(
        i.upgrade(&DumpStore::Legacy(11)).unwrap(),
        "an upgrade batch is appended"
    );
    i.finish().unwrap();
    log_end
}

#[test]
fn ec2_an_upgrade_of_a_log_with_no_repo_id_writes_the_format_and_a_new_repo_id_in_one_batch() {
    // RULING F2: a log of a store format before 12 has no repo id; its upgrade batch gives it one
    let base = tempfile::tempdir().unwrap();
    let to = base.path().join("dst");
    let log_end = upgraded_s11(&to);
    let t = Store::open(&to).unwrap();
    let after = frames_after(&t, log_end);
    assert_eq!(after.len(), 3, "two ref frames and one marker: {after:?}");
    assert_eq!(after[0].0, kinds::REF_UPDATE);
    let f: RefUpdate = postcard::from_bytes(&after[0].1).unwrap();
    assert_eq!(f.name, REF_STORE_FEATURES);
    assert_eq!(f.value, baseline_features().to_json());
    assert_eq!(after[1].0, kinds::REF_UPDATE);
    let r: RefUpdate = postcard::from_bytes(&after[1].1).unwrap();
    assert_eq!(r.name, REF_REPO_ID);
    assert_eq!(r.value.len(), 16, "a repo id is 16 bytes");
    assert_eq!(after[2].0, kinds::BATCH_COMMIT);
    let m: BatchCommit = postcard::from_bytes(&after[2].1).unwrap();
    assert_eq!(m.start, log_end, "one batch, from the fixture's log end");
    assert_eq!(m.frames, 2, "one batch of both refs");
    assert_eq!(t.reader().get_ref(REF_REPO_ID).unwrap(), Some(r.value));
    t.check_objects().unwrap();
}

#[test]
fn ec2_the_upgrade_salts_each_ref_as_the_store_does() {
    let base = tempfile::tempdir().unwrap();
    let to = base.path().join("dst");
    let log_end = upgraded_s11(&to);
    let t = Store::open(&to).unwrap();
    let after = frames_after(&t, log_end);
    let ats: Vec<Location> = after
        .iter()
        .filter(|(k, _)| *k == kinds::REF_UPDATE)
        .map(|(_, p)| postcard::from_bytes::<RefUpdate>(p).unwrap().at)
        .collect();
    assert_eq!(
        ats,
        vec![
            log_end,
            Location {
                segment: log_end.segment,
                offset: log_end.offset + 1,
            },
        ],
        "the i-th ref of a batch is salted with the log end plus i"
    );
}

#[test]
fn ec2_an_upgrade_of_a_log_that_has_a_repo_id_writes_only_the_format() {
    // guard: a store made by this rdy holds its repo id; a forced older format upgrades only the
    // format, and the repo id stays
    let base = tempfile::tempdir().unwrap();
    let mut s = rolling_store(base.path());
    let id = s.reader().get_ref(REF_REPO_ID).unwrap().expect("a repo id");
    let older = format::LAST_SCALAR_STORE_FORMAT;
    s.set_refs_unchecked_for_tests(vec![(REF_STORE_FORMAT.into(), encode_u32(older))])
        .unwrap();
    let (src_end, bs) = (s.log_end(), batches(&s));
    let to = base.path().join("dst");
    let mut i = Ingest::create(&to).unwrap();
    for (n, b) in bs.iter().enumerate() {
        i.batch(n as u64, b).unwrap();
    }
    assert!(
        i.upgrade(&DumpStore::Legacy(older)).unwrap(),
        "an upgrade batch is appended"
    );
    i.finish().unwrap();
    let t = Store::open(&to).unwrap();
    let after = frames_after(&t, src_end);
    assert_eq!(after.len(), 2, "one ref frame and its marker: {after:?}");
    let u: RefUpdate = postcard::from_bytes(&after[0].1).unwrap();
    assert_eq!(
        u,
        RefUpdate {
            name: REF_STORE_FEATURES.into(),
            value: baseline_features().to_json(),
            at: src_end,
        }
    );
    assert_eq!(after[1].0, kinds::BATCH_COMMIT);
    // the rolling store forges a view ref, so fsck is not run here
    assert_eq!(t.reader().get_ref(REF_REPO_ID).unwrap(), Some(id));
}

#[test]
fn ec2_check_refuses_a_store_with_no_repo_id() {
    // The v1-s11 batches with no upgrade batch, the log's features forced to the baseline by
    // one hand-made batch of `meta/store_features` alone, and `FORMAT` forced to them: a feature
    // store log with no repo id.
    let base = tempfile::tempdir().unwrap();
    let to = base.path().join("dst");
    let (bs, log_end) = fixture_batches("v1-s11.rdydump");
    let mut i = Ingest::create(&to).unwrap();
    for (n, b) in bs.iter().enumerate() {
        i.batch(n as u64, b).unwrap();
    }
    let payload = postcard::to_allocvec(&RefUpdate {
        name: REF_STORE_FEATURES.into(),
        value: baseline_features().to_json(),
        at: log_end,
    })
    .unwrap();
    let hash = rdy_model::Hash::of(&payload);
    let forced = DumpBatch {
        start: log_end,
        digest: kinds::batch_digest([&hash]),
        frames: vec![Frame::Present {
            kind: kinds::REF_UPDATE,
            hash,
            payload,
        }],
    };
    i.batch(bs.len() as u64, &forced).unwrap();
    i.finish().unwrap();
    format::write_format_file(&to, &baseline_features()).unwrap();
    let t = Store::open(&to).unwrap();
    assert_eq!(t.reader().get_ref(REF_REPO_ID).unwrap(), None);
    let e = t.check_objects().unwrap_err();
    assert!(
        matches!(&e, StoreError::Decode(m) if m == "meta/repo_id is missing; load the project from a dump with this rdy"),
        "{e}"
    );
}
