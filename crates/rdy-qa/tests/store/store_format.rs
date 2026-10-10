//! Store format (plan M2.0 fmt1, plan M2a1 fmt2): one number, in the log and mirrored in FORMAT;
//! a store of any other format is refused before anything is written; the store is .rdy/store.
use rdy_engine::{Rdy, RdyError};
use rdy_qa::fixtures::*;
use rdy_store::format::{REF_STORE_FORMAT, STORE_FORMAT};
use rdy_store::index::Index;
use rdy_store::segment::SegmentLog;
use rdy_store::{Store, StoreError, kinds};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The FORMAT text of this binary's format.
fn current_text() -> String {
    format!("rdy store format\nstore_format {STORE_FORMAT}\n")
}

/// Every path under `dir` with its contents (None for directories).
fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.insert(p.clone(), None);
            out.extend(tree(&p));
        } else {
            out.insert(p.clone(), Some(std::fs::read(&p).unwrap()));
        }
    }
    out
}

#[test]
fn fmt1_a_new_store_records_its_format_in_the_log_and_in_format() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let s = Store::create(&dir).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("FORMAT")).unwrap(),
        current_text()
    );
    assert_eq!(
        s.get_ref(REF_STORE_FORMAT).unwrap(),
        Some(STORE_FORMAT.to_string().into_bytes())
    );
    assert_eq!(s.verify().unwrap(), 1, "one format batch of one ref");
    drop(s);
    let s = Store::create(&dir).unwrap();
    assert_eq!(s.verify().unwrap(), 1, "no second format batch");
    assert!(s.open_report().quarantined.is_empty());
}

#[test]
fn fmt1_format_refs_survive_reindex() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let mut s = Store::create(&dir).unwrap();
    s.put_blob(b"x").unwrap();
    s.reindex().unwrap();
    assert_eq!(
        s.get_ref(REF_STORE_FORMAT).unwrap(),
        Some(STORE_FORMAT.to_string().into_bytes())
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("FORMAT")).unwrap(),
        current_text()
    );
}

/// fmt3 replaces fmt1's rewrite: a store whose FORMAT is lost is refused, and FORMAT is not
/// rewritten from the log (that would mean opening the index before the format is known).
#[test]
fn fmt3_a_lost_format_file_is_refused_and_not_rewritten() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    {
        let mut s = Store::create(&dir).unwrap();
        s.put_blob(b"one").unwrap();
    }
    std::fs::remove_file(dir.join("FORMAT")).unwrap();
    let before = tree(&dir);
    refused(&dir, "has no FORMAT file");
    assert_eq!(tree(&dir), before);
}

#[test]
fn fmt1_an_unreadable_format_file_names_its_fix() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    drop(Store::create(&dir).unwrap());
    std::fs::write(dir.join("FORMAT"), "not a format file\n").unwrap();
    let e = RdyError::from(Store::open(&dir).err().expect("unreadable"));
    assert_eq!(e.code, "E_STORE_FORMAT", "{e}");
    assert!(e.message.contains("is unreadable"), "{e}");
    assert_eq!(
        e.fix, "restore FORMAT from backup, or re-create it with this rdy",
        "{e}"
    );
}

/// Review Focus 3: the one refusal, whatever told us the store is another build's.
fn refused(dir: &Path, what: &str) -> RdyError {
    let e = Store::open(dir)
        .err()
        .expect("a store of another format is refused");
    assert!(matches!(&e, StoreError::Format { .. }), "{e}");
    let e = RdyError::from(e);
    assert_eq!(e.code, "E_STORE_FORMAT", "{e}");
    assert_eq!(
        e.message,
        format!(
            "this store was written by a different rdy build: {} {what}, and this rdy uses store format {STORE_FORMAT}",
            dir.display()
        )
    );
    assert_eq!(e.fix, "re-create it with this rdy");
    e
}

#[test]
fn fmt2_a_store_of_a_newer_format_is_refused_before_anything_is_opened() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    Store::create(&dir).unwrap().put_blob(b"x").unwrap();
    let newer = STORE_FORMAT + 1;
    std::fs::write(
        dir.join("FORMAT"),
        format!("rdy store format\nstore_format {newer}\n"),
    )
    .unwrap();
    let before = tree(&dir);
    refused(&dir, &format!("has store format {newer}"));
    assert_eq!(tree(&dir), before, "nothing under the store changed");
}

#[test]
fn fmt2_an_m2_0_store_is_refused() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    Store::create(&dir).unwrap().put_blob(b"x").unwrap();
    std::fs::write(
        dir.join("FORMAT"),
        "rdy store format\nlog_format 2\nmin_reader 2\n",
    )
    .unwrap();
    let before = tree(&dir);
    refused(&dir, "has a FORMAT file with no store_format line");
    assert_eq!(tree(&dir), before);
}

#[test]
fn fmt2_a_log_that_says_another_format_is_refused() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    Store::create(&dir)
        .unwrap()
        .set_refs(vec![(REF_STORE_FORMAT.into(), b"2".to_vec())])
        .unwrap();
    let seg = std::fs::read(dir.join("segments/seg-000000.log")).unwrap();
    refused(&dir, "has store format 2");
    assert_eq!(
        std::fs::read(dir.join("segments/seg-000000.log")).unwrap(),
        seg
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("FORMAT")).unwrap(),
        current_text()
    );
}

/// A marked log with no format ref under a FORMAT that names the current format: the index is built to find that
/// out; the log is untouched.
#[test]
fn fmt2_a_log_with_frames_and_no_store_format_is_refused() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    std::fs::create_dir_all(dir.join("segments")).unwrap();
    {
        let mut log = SegmentLog::open(&dir.join("segments"), u64::MAX).unwrap();
        log.append_batch(&[(
            kinds::BLOB_CHUNK,
            b"written before the format existed".to_vec(),
        )])
        .unwrap();
        log.sync().unwrap();
    }
    std::fs::write(dir.join("FORMAT"), current_text()).unwrap();
    let seg = std::fs::read(dir.join("segments/seg-000000.log")).unwrap();
    refused(&dir, "has a log with no store format");
    assert_eq!(
        std::fs::read(dir.join("segments/seg-000000.log")).unwrap(),
        seg
    );
}

#[test]
fn fmt2_a_project_whose_store_is_not_a_directory_is_refused() {
    let (dir, r) = project();
    drop(r);
    let rdy = dir.path().join(".rdy");
    std::fs::remove_dir_all(rdy.join("store")).unwrap();
    // what an M2.0 project holds at .rdy/store
    std::fs::write(
        rdy.join("store"),
        "this store was upgraded by a newer rdy (M2 or later); upgrade rdy\n",
    )
    .unwrap();
    let before = tree(&rdy);
    let e = Rdy::open(dir.path())
        .err()
        .expect("the stub is another build's store");
    assert_eq!(e.code, "E_STORE_FORMAT", "{e}");
    assert!(
        e.message.contains(&format!(
            "{} is not a directory",
            rdy.join("store").display()
        )),
        "{e}"
    );
    assert_eq!(e.fix, "re-create it with this rdy");
    assert_eq!(tree(&rdy), before, "nothing under .rdy changed");
}

#[test]
fn fmt1_a_new_project_keeps_its_store_in_dot_rdy_store() {
    let (dir, r) = project();
    let rdy = dir.path().join(".rdy");
    for f in ["lock", "FORMAT", "index.redb", "segments"] {
        assert!(rdy.join("store").join(f).exists(), "{f}");
    }
    assert!(!rdy.join("store2").exists());
    assert_eq!(r.store().dir(), rdy.join("store").as_path());
    let e = Rdy::open(dir.path()).err().expect("the store is locked");
    assert_eq!(e.code, "E_LOCKED", "{e}");
}

/// A missing store directory under an intact .rdy is refused, not created: only init creates a
/// store (fmt3). Init creates it durably (dur1).
#[test]
fn fmt3_a_missing_store_directory_is_refused_not_created() {
    let (dir, r) = project();
    drop(r);
    let rdy = dir.path().join(".rdy");
    std::fs::remove_dir_all(rdy.join("store")).unwrap();
    let before = tree(&rdy);
    let e = Rdy::open(dir.path()).err().expect("no store");
    assert_eq!(e.code, "E_STORE_FORMAT", "{e}");
    assert!(
        e.message.starts_with(&format!(
            "there is no store at {}",
            rdy.join("store").display()
        )),
        "{e}"
    );
    assert_eq!(e.fix, "re-create it with this rdy");
    assert_eq!(tree(&rdy), before);
    let fresh = tempfile::tempdir().unwrap();
    let (r, synced) = rdy_store::fsutil::record_syncs(|| Rdy::init(fresh.path(), "rdy-standard"));
    r.unwrap();
    let rdy = fresh.path().join(".rdy");
    assert!(synced.contains(&rdy), "{synced:?}");
    assert!(synced.contains(&rdy.join("store")), "{synced:?}");
}

/// The M1 store layout (M1 at a22b24b): `lock`, a redb `index.redb` and `segments/`, and no
/// `FORMAT`. `frames` are written to `seg-000000.log` in M1's frame layout (u32 length, kind,
/// 32-byte hash, payload); none is the M1 `init` store, which holds an empty segment.
fn m1_store(dir: &Path, frames: &[(u8, &[u8])]) {
    std::fs::create_dir_all(dir.join("segments")).unwrap();
    drop(rdy_store::index::Index::open(&dir.join("index.redb")).unwrap());
    std::fs::write(dir.join("lock"), b"").unwrap();
    let mut seg = Vec::new();
    for (kind, payload) in frames {
        seg.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        seg.push(*kind);
        seg.extend_from_slice(&Store::blob_hash(payload).0);
        seg.extend_from_slice(payload);
    }
    std::fs::write(dir.join("segments/seg-000000.log"), seg).unwrap();
}

/// An M1 project: `.rdy.yaml`, `.rdy/packs/` and an M1 store at `.rdy/store`.
fn m1_project(frames: &[(u8, &[u8])]) -> tempfile::TempDir {
    let p = tempfile::tempdir().unwrap();
    let rdy = p.path().join(".rdy");
    std::fs::create_dir_all(rdy.join("packs")).unwrap();
    std::fs::write(p.path().join(".rdy.yaml"), "pack: rdy-standard\n").unwrap();
    m1_store(&rdy.join("store"), frames);
    p
}

const M1_FRAME: (u8, &[u8]) = (kinds::BLOB_CHUNK, b"an M1 doc: wallet, components");

fn assert_refused_as_another_build(e: RdyError, store: &Path) {
    assert_eq!(e.code, "E_STORE_FORMAT", "{e}");
    assert_eq!(
        e.message,
        format!(
            "this store was written by a different rdy build: {} has no FORMAT file, and this rdy uses store format {STORE_FORMAT}",
            store.display()
        )
    );
    assert_eq!(e.fix, "re-create it with this rdy");
}

#[test]
fn fmt3_an_m1_store_with_data_is_refused_and_nothing_is_written() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    m1_store(&dir, &[M1_FRAME]);
    let before = tree(&dir);
    let e = Store::open(&dir).err().expect("an M1 store is refused");
    assert_refused_as_another_build(RdyError::from(e), &dir);
    assert_eq!(tree(&dir), before, "every file is byte-identical");
}

#[test]
fn fmt3_an_m1_project_with_data_is_refused_and_nothing_is_written() {
    let p = m1_project(&[M1_FRAME]);
    let rdy = p.path().join(".rdy");
    let before = tree(&rdy);
    let e = Rdy::open(p.path()).err().expect("an M1 project is refused");
    assert_refused_as_another_build(e, &rdy.join("store"));
    assert_eq!(
        tree(&rdy),
        before,
        "every file under .rdy is byte-identical"
    );
}

#[test]
fn fmt3_an_empty_m1_store_is_refused_and_gains_no_format_file() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    m1_store(&dir, &[]);
    let before = tree(&dir);
    let e = Store::open(&dir)
        .err()
        .expect("an empty M1 store is refused");
    assert_refused_as_another_build(RdyError::from(e), &dir);
    assert!(!dir.join("FORMAT").exists());
    assert_eq!(tree(&dir), before);
    // init does not claim it either: it is not empty
    let e = Store::create(&dir).err().expect("create does not claim it");
    assert_refused_as_another_build(RdyError::from(e), &dir);
    assert_eq!(tree(&dir), before);
}

#[test]
fn fmt3_an_empty_m1_project_is_refused_and_gains_no_format_file() {
    let p = m1_project(&[]);
    let rdy = p.path().join(".rdy");
    let before = tree(&rdy);
    let e = Rdy::open(p.path())
        .err()
        .expect("an empty M1 project is refused");
    assert_refused_as_another_build(e, &rdy.join("store"));
    assert!(!rdy.join("store/FORMAT").exists());
    assert_eq!(tree(&rdy), before);
}

/// A directory that is not a store at all (it holds anything) is refused by open and by create.
#[test]
fn fmt3_a_directory_that_is_not_a_store_is_never_claimed() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), b"mine").unwrap();
    let before = tree(&dir);
    for e in [Store::open(&dir).err(), Store::create(&dir).err()] {
        assert_refused_as_another_build(RdyError::from(e.expect("refused")), &dir);
        assert_eq!(tree(&dir), before);
    }
    // an empty directory is a store only once init creates one: open does not
    let empty = base.path().join("e");
    std::fs::create_dir_all(&empty).unwrap();
    assert_refused_as_another_build(
        RdyError::from(Store::open(&empty).err().expect("open never creates")),
        &empty,
    );
    assert!(tree(&empty).is_empty());
    drop(Store::create(&empty).unwrap());
    assert_eq!(
        std::fs::read_to_string(empty.join("FORMAT")).unwrap(),
        current_text()
    );
    drop(Store::open(&empty).unwrap());
}

#[test]
fn fmt3_init_on_a_fresh_path_makes_a_format_3_store_that_reopens() {
    let p = tempfile::tempdir().unwrap();
    let proj = p.path().join("new/project");
    drop(Rdy::init(&proj, "rdy-standard").unwrap());
    let store = proj.join(".rdy/store");
    assert_eq!(
        std::fs::read_to_string(store.join("FORMAT")).unwrap(),
        current_text()
    );
    let r = Rdy::open(&proj).unwrap();
    assert_eq!(
        r.store().get_ref(REF_STORE_FORMAT).unwrap(),
        Some(STORE_FORMAT.to_string().into_bytes())
    );
}

/// `rdy init` in a directory whose `.rdy/store` holds something that is not a store is refused,
/// and nothing is written: no config, no FORMAT.
#[test]
fn fmt3_init_over_a_non_store_directory_is_refused() {
    let p = tempfile::tempdir().unwrap();
    let store = p.path().join(".rdy/store");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("index.redb"), b"not ours").unwrap();
    let e = Rdy::init(p.path(), "rdy-standard").err().expect("refused");
    assert_refused_as_another_build(e, &store);
    assert!(!p.path().join(".rdy.yaml").exists());
    assert!(!store.join("FORMAT").exists());
}

/// The CLI shows the human the refusal and its "re-create it" fix.
#[test]
fn fmt3_the_cli_shows_re_create_it_for_an_m1_project() {
    let p = m1_project(&[M1_FRAME]);
    let rdy = p.path().join(".rdy");
    let before = tree(&rdy);
    // the direct path: a daemon would add .rdy/run and report the refusal from its log
    let (_cfg, env) = human_env("human:qa");
    let out = std::process::Command::new(rdy_bin())
        .args(["--no-daemon", "status"])
        .current_dir(p.path())
        .env("RDY_ACTOR", "human:qa")
        .envs(env)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("error E_STORE_FORMAT"), "{err}");
    assert!(err.contains("has no FORMAT file"), "{err}");
    assert!(err.contains("fix: re-create it with this rdy"), "{err}");
    assert_eq!(tree(&rdy), before);
}

/// An index that lost the format ref over a log that has it (an old partial reindex): open
/// rebuilds the index from the log and finds it.
#[test]
fn hd2_an_index_without_the_format_ref_is_rebuilt_at_open() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("s");
    let (h, end) = {
        let mut s = Store::create(&dir).unwrap();
        let h = s.put_blob(b"x").unwrap();
        (h, s.log_end())
    };
    std::fs::remove_file(dir.join("index.redb")).unwrap();
    Index::open(&dir.join("index.redb"))
        .unwrap()
        .apply_durable(&[], end)
        .unwrap();
    let s = Store::open(&dir).expect("open rebuilds the index");
    assert_eq!(
        s.get_ref(REF_STORE_FORMAT).unwrap(),
        Some(STORE_FORMAT.to_string().into_bytes())
    );
    assert_eq!(s.get_blob(&h).unwrap(), b"x");
}
