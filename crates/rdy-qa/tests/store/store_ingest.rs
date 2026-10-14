//! dl4: the ingest writes each dumped batch where the source log held it, so the new log is the
//! source log byte for byte; the first open builds the index from it.
use rdy_store::dump::DumpBatch;
use rdy_store::format::{Format, REF_STORE_FORMAT, STORE_FORMAT, encode_u32};
use rdy_store::ingest::Ingest;
use rdy_store::kinds::{self, RefUpdate};
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
    // FW-UP1 in its smallest form: a log of an older store format keeps every byte, and one
    // logged batch after it sets `meta/store_format` to this rdy's. The older number is forced:
    // no older dump format exists yet.
    let base = tempfile::tempdir().unwrap();
    let mut s = rolling_store(base.path());
    let older = STORE_FORMAT - 1;
    s.set_refs_unchecked_for_tests(vec![(REF_STORE_FORMAT.into(), encode_u32(older))])
        .unwrap();
    let (src_end, bs) = (s.log_end(), batches(&s));
    let src = segments(&base.path().join("src"));
    // without the upgrade batch, the log names the older format and an open refuses it
    let plain = base.path().join("plain");
    ingest(&plain, &bs).unwrap();
    let e = Store::open(&plain).err().expect("an older log is refused");
    assert!(
        matches!(&e, StoreError::Format { message, .. } if message.contains(&format!("has store format {older}"))),
        "{e}"
    );
    // with it, the store opens at this rdy's format
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
    assert!(i.upgrade(older).unwrap(), "an upgrade batch is appended");
    let report = i.finish().unwrap();
    assert_eq!(report.batches, bs.len() as u64, "the dump's batches only");
    assert!(
        is_prefix(&src, &segments(&to)),
        "the source log is kept byte for byte"
    );
    assert_eq!(
        std::fs::read(to.join("FORMAT")).unwrap(),
        Format::CURRENT.to_text().into_bytes()
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
            name: REF_STORE_FORMAT.into(),
            value: encode_u32(STORE_FORMAT),
            at: src_end,
        }
    );
    assert_eq!(after[1].0, kinds::BATCH_COMMIT);
    assert_eq!(
        t.reader().refs_with_prefix(REF_STORE_FORMAT).unwrap(),
        vec![(REF_STORE_FORMAT.to_string(), encode_u32(STORE_FORMAT))]
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
    assert!(!i.upgrade(STORE_FORMAT).unwrap(), "nothing is appended");
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
    let newer = STORE_FORMAT + 1;
    let e = i.upgrade(newer).unwrap_err();
    assert!(
        matches!(&e, StoreError::Dump { what, .. } if *what == format!("store format {newer} is newer than this rdy's store format {STORE_FORMAT}")),
        "{e}"
    );
    assert_eq!(i.end(), s.log_end(), "nothing is appended");
}
