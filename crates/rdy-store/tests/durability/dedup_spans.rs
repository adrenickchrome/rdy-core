//! The span map: a CHANGE's copy of a block it creates maps to the BLOCK frame's retained bytes,
//! the mapped range is punched only after the map is durable, every read fills the hole before
//! the hash check, and the map's damage, torn tail, fullness and failures are recovered or
//! reported as the store's other metadata are.
use crate::carried_fixture::{find, framed_at};
use rdy_model::*;
use rdy_store::compact::on_allocation;
use rdy_store::dedup::{self, MapEvent, MapOpen, SPANMAP_FILE, SpanMap};
use rdy_store::format::{self, FEATURE_DEDUP_SPANS};
use rdy_store::pio::{self, Punch};
use rdy_store::segment::seg_path;
use rdy_store::upgrade::{self, DEDUP_SPANS_STEP, SNAPSHOT_DELTA_STEP};
use rdy_store::{OpenPolicy, Store, StoreError, kinds};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

const HEADER: u64 = 37;
const NOT_SUPPORTED: &str = "hole punching is not supported for";
const FAILED: &str = "hole punching failed for";

fn block(id: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn change(ops: Vec<Op>) -> Change {
    Change {
        view: "work".into(),
        ops,
        deps: vec![],
        provenance: Provenance {
            actor: "agent:w".into(),
            run: Some("w1".into()),
            role: Role::Writer,
            at_ms: 10,
        },
        justification: None,
        message: String::new(),
        links: ChangeLinks::default(),
        read_identity: None,
    }
}

/// `n` bytes of text that repeat no 4,096-byte window, from `seed`.
fn text(n: usize, seed: u64) -> String {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            char::from(b'a' + (x % 26) as u8)
        })
        .collect()
}

/// A store with one change that creates a 40,000-byte block: its CHANGE holds a copy of the
/// BLOCK frame's payload. Returns the change's hash and the block's version hash.
fn one_big_change(dir: &Path) -> (Hash, Hash) {
    let mut s = Store::create(dir).unwrap();
    let b = block("b-00001", &text(40_000, 7));
    let v = b.version_hash();
    let id = s
        .commit_to_view("work", change(vec![Op::Create { block: b }]))
        .unwrap();
    s.close().unwrap();
    (id, v)
}

/// The raw bytes of the frame's payload at `f` in its segment file.
fn raw_payload(dir: &Path, f: &crate::carried_fixture::Framed) -> Vec<u8> {
    let bytes = std::fs::read(seg_path(&dir.join("segments"), f.loc.segment)).unwrap();
    let start = (f.loc.offset + HEADER) as usize;
    bytes[start..start + f.len as usize].to_vec()
}

fn map_of(dir: &Path) -> SpanMap {
    SpanMap::load(dir, MapOpen::ReadOnly).unwrap()
}

/// Records every map event and punch call on this thread, in order.
fn record_events() -> Rc<RefCell<Vec<String>>> {
    let log = Rc::new(RefCell::new(vec![]));
    let l = log.clone();
    dedup::on_map_event(Some(Box::new(move |e: &MapEvent| {
        l.borrow_mut().push(match e {
            MapEvent::Append { maps, through } => format!("append {maps} {through}"),
            MapEvent::Sync => "sync".into(),
            MapEvent::Publish => "publish".into(),
            MapEvent::Punch(_) => "punch".into(),
            MapEvent::Scan => "scan".into(),
            MapEvent::Threshold => "threshold".into(),
        });
        Ok(())
    })));
    log
}

fn clear_hooks() {
    dedup::on_map_event(None);
    pio::on_punch(None);
    pio::on_dedup_alignment(None);
    on_allocation(None);
}

#[test]
fn r5902_new_store_lists_the_feature_and_an_empty_map() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let s = Store::create(&path).unwrap();
    assert!(s.has_feature_now(FEATURE_DEDUP_SPANS));
    assert!(s.features().has(FEATURE_DEDUP_SPANS));
    drop(s);
    let bytes = std::fs::read(path.join(SPANMAP_FILE)).unwrap();
    assert_eq!(
        bytes.len(),
        12,
        "the header alone: no row, no candidate scan"
    );
    assert_eq!(&bytes[..8], b"RDYSPANS");
    assert_eq!(map_of(&path).rows(), 0);
}

#[test]
fn r5902_compact_maps_exact_runs_and_reads_them_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let (id, v) = one_big_change(&path);
    let frames = framed_at(&path);
    let c = find(&frames, kinds::CHANGE, id);
    let raw_before = raw_payload(&path, &c);

    let s = Store::open(&path).unwrap();
    let before = s.change(&id).unwrap();
    let r = s.compact(false).unwrap();
    assert!(r.deduplicated_bytes >= 4096 * 8, "{}", r.deduplicated_bytes);
    assert_eq!(r.deduplicated_bytes % 4096, 0, "whole blocks only");
    assert!(
        !r.notes.iter().any(|n| n.starts_with(FAILED)),
        "{:?}",
        r.notes
    );
    // the logical read is the same, through this handle and a reopened one
    assert_eq!(s.change(&id).unwrap(), before);
    s.close().unwrap();
    let map = map_of(&path);
    assert!(map.rows() >= 2, "Map rows and one Through");
    assert!(map.through().is_some());
    for m in map.mappings() {
        assert_eq!(m.target.place, c.loc);
        assert_eq!(m.source.place, find(&frames, kinds::BLOCK, v).loc);
        assert_eq!(m.target.len, m.source.len);
        assert_eq!(m.target.len % 4096, 0);
        assert_eq!(
            (c.loc.offset + HEADER + u64::from(m.target.start)) % 4096,
            0
        );
    }
    let s = Store::open(&path).unwrap();
    assert_eq!(s.change(&id).unwrap(), before);
    s.check_objects().unwrap();
    assert_eq!(
        s.log_bytes().unwrap().deduplicated_bytes,
        r.deduplicated_bytes
    );
    // a punched range reads as zeros in the file, where the platform punched it
    let raw_after = raw_payload(&path, &c);
    assert_eq!(raw_after.len(), raw_before.len(), "the file length stays");
    for m in map.mappings() {
        let (a, b) = (
            m.target.start as usize,
            (m.target.start + m.target.len) as usize,
        );
        let punched = raw_after[a..b].iter().all(|&x| x == 0);
        assert!(punched || raw_after[a..b] == raw_before[a..b]);
    }
    // bytes outside the mapped ranges are untouched
    let mut outside = raw_before.clone();
    let mut got = raw_after.clone();
    for m in map.mappings() {
        let (a, b) = (
            m.target.start as usize,
            (m.target.start + m.target.len) as usize,
        );
        outside[a..b].fill(0);
        got[a..b].fill(0);
    }
    assert_eq!(got, outside);

    // a second compact adds no mapping and no row
    let rows = map.rows();
    let again = s.compact(false).unwrap();
    assert_eq!(again.deduplicated_bytes, r.deduplicated_bytes);
    drop(s);
    assert_eq!(map_of(&path).rows(), rows);
}

#[test]
fn r5902_dry_run_reports_candidates_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let (id, _) = one_big_change(&path);
    let c = find(&framed_at(&path), kinds::CHANGE, id);
    let raw = raw_payload(&path, &c);
    let s = Store::open(&path).unwrap();
    let r = s.compact(true).unwrap();
    assert!(r.deduplicated_bytes >= 4096 * 8);
    assert_eq!(r.punched_bytes, 0);
    assert_eq!(
        s.log_bytes().unwrap().deduplicated_bytes,
        0,
        "no durable mapping"
    );
    drop(s);
    assert_eq!(map_of(&path).rows(), 0);
    assert_eq!(std::fs::read(path.join(SPANMAP_FILE)).unwrap().len(), 12);
    assert_eq!(raw_payload(&path, &c), raw);
}

#[test]
fn r5902_map_sync_precedes_every_punch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    one_big_change(&path);
    let s = Store::open(&path).unwrap();
    let log = record_events();
    let punches = Rc::new(RefCell::new(0usize));
    let p = punches.clone();
    pio::on_punch(Some(Box::new(move |_, _| {
        *p.borrow_mut() += 1;
        None
    })));
    let r = s.compact(false);
    clear_hooks();
    r.unwrap();
    let events = log.borrow().clone();
    let first_punch = events.iter().position(|e| e == "punch").expect("a punch");
    let sync = events.iter().position(|e| e == "sync").unwrap();
    let append = events.iter().position(|e| e.starts_with("append")).unwrap();
    let publish = events.iter().position(|e| e == "publish").unwrap();
    assert!(
        append < sync && sync < publish && publish < first_punch,
        "{events:?}"
    );
    assert_eq!(
        events.iter().filter(|e| *e == "sync").count(),
        1,
        "one sync a pass"
    );
    assert!(*punches.borrow() >= 1);
}

#[test]
fn r5902_failed_map_sync_punches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let (id, _) = one_big_change(&path);
    let c = find(&framed_at(&path), kinds::CHANGE, id);
    let raw = raw_payload(&path, &c);
    let s = Store::open(&path).unwrap();
    let before = s.change(&id).unwrap();
    let punched = Rc::new(RefCell::new(false));
    let p = punched.clone();
    dedup::on_map_event(Some(Box::new(move |e: &MapEvent| match e {
        MapEvent::Sync => Err(std::io::Error::other("injected")),
        MapEvent::Punch(_) => {
            *p.borrow_mut() = true;
            Ok(())
        }
        _ => Ok(()),
    })));
    let r = s.compact(false);
    clear_hooks();
    assert!(r.is_err(), "a compact's map failure is an ordinary error");
    assert!(!*punched.borrow());
    assert_eq!(
        s.change(&id).unwrap(),
        before,
        "no acknowledged data rolls back"
    );
    drop(s);
    assert_eq!(raw_payload(&path, &c), raw);
}

#[test]
fn r5902_threshold_below_the_limit_scans_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let log = record_events();
    let b = block("b-00001", &text(40_000, 9));
    s.commit_to_view("work", change(vec![Op::Create { block: b }]))
        .unwrap();
    clear_hooks();
    assert!(
        log.borrow().is_empty(),
        "an ordinary write below the threshold: {:?}",
        log.borrow()
    );
    drop(s);
    assert_eq!(map_of(&path).rows(), 0);
}

#[test]
fn r5902_unsupported_punch_keeps_bytes_and_notes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let (id, _) = one_big_change(&path);
    let c = find(&framed_at(&path), kinds::CHANGE, id);
    let raw = raw_payload(&path, &c);
    let s = Store::open(&path).unwrap();
    pio::on_punch(Some(Box::new(|_, _| Some(Ok(Punch::Unsupported)))));
    let r = s.compact(false);
    clear_hooks();
    let r = r.unwrap();
    let n = map_of(&path).mappings().count();
    assert!(n > 0);
    assert!(
        r.notes.contains(&format!(
            "{NOT_SUPPORTED} {n} mapped spans; they keep their disk space"
        )),
        "{:?}",
        r.notes
    );
    drop(s);
    assert_eq!(raw_payload(&path, &c), raw, "no zero fallback");

    let s = Store::open(&path).unwrap();
    pio::on_punch(Some(Box::new(|_, _| {
        Some(Err(std::io::Error::other("injected")))
    })));
    let r = s.compact(false);
    clear_hooks();
    let r = r.unwrap();
    assert!(
        r.notes.contains(&format!(
            "{FAILED} {n} mapped spans; they keep their disk space"
        )),
        "{:?}",
        r.notes
    );
}

#[test]
fn r5902_unsupported_alignment_maps_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    one_big_change(&path);
    let s = Store::open(&path).unwrap();
    pio::on_dedup_alignment(Some(Box::new(|| Some(Ok(None)))));
    let r = s.compact(false);
    clear_hooks();
    assert_eq!(r.unwrap().deduplicated_bytes, 0);
    drop(s);
    assert_eq!(map_of(&path).mappings().count(), 0);
}

#[test]
fn r5902_torn_map_tail_is_quarantined_on_a_writable_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let (id, _) = one_big_change(&path);
    let s = Store::open(&path).unwrap();
    let want = s.change(&id).unwrap();
    s.compact(false).unwrap();
    s.close().unwrap();
    let good = std::fs::read(path.join(SPANMAP_FILE)).unwrap();
    let rows = map_of(&path).rows();
    // a torn row: its length and part of its bytes
    let mut torn = good.clone();
    torn.extend_from_slice(&[40, 0, 0, 0, 1, 2, 3]);
    std::fs::write(path.join(SPANMAP_FILE), &torn).unwrap();

    // a read-only open ignores the torn suffix and reports an unclean shutdown
    let ro = Store::open_source_read_only(&path).unwrap();
    assert!(ro.open_report().unclean_shutdown);
    assert_eq!(ro.change(&id).unwrap(), want);
    drop(ro);
    assert_eq!(std::fs::read(path.join(SPANMAP_FILE)).unwrap(), torn);

    let s = Store::open(&path).unwrap();
    assert_eq!(s.change(&id).unwrap(), want);
    let names: Vec<String> = std::fs::read_dir(&path)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| dedup::is_quarantine_name(n))
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(
        s.open_report()
            .quarantined
            .iter()
            .any(|q| q.file_name().is_some_and(|n| n == names[0].as_str()))
    );
    drop(s);
    assert_eq!(std::fs::read(path.join(SPANMAP_FILE)).unwrap(), good);
    assert_eq!(map_of(&path).rows(), rows);
}

#[test]
fn r5902_damaged_map_row_refuses_the_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    one_big_change(&path);
    let s = Store::open(&path).unwrap();
    s.compact(false).unwrap();
    s.close().unwrap();
    let mut bytes = std::fs::read(path.join(SPANMAP_FILE)).unwrap();
    assert!(map_of(&path).rows() >= 2);
    // a byte of the first row's body: a valid row follows it
    bytes[12 + 4 + 2] ^= 0xFF;
    std::fs::write(path.join(SPANMAP_FILE), &bytes).unwrap();
    assert!(matches!(
        Store::open(&path).map(|_| ()),
        Err(StoreError::DedupCorrupt)
    ));
    // a damaged header too
    let mut bytes = std::fs::read(path.join(SPANMAP_FILE)).unwrap();
    bytes[0] = b'X';
    std::fs::write(path.join(SPANMAP_FILE), &bytes).unwrap();
    assert!(matches!(
        Store::open(&path).map(|_| ()),
        Err(StoreError::DedupCorrupt)
    ));
}

#[test]
fn r5902_a_missing_map_keeps_reads_and_maps_at_compact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let (id, _) = one_big_change(&path);
    std::fs::remove_file(path.join(SPANMAP_FILE)).unwrap();
    let s = Store::open(&path).unwrap();
    let want = s.change(&id).unwrap();
    let r = s.compact(false).unwrap();
    assert!(r.deduplicated_bytes > 0);
    s.close().unwrap();
    let s = Store::open(&path).unwrap();
    assert_eq!(s.change(&id).unwrap(), want);
}

#[test]
fn r5902_old_stores_open_for_readers_unmapped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    {
        let _legacy = format::legacy_writer_for_tests();
        let mut s = Store::create_legacy_for_tests(&path).unwrap();
        let b = block("b-00001", &text(40_000, 3));
        s.commit_to_view("work", change(vec![Op::Create { block: b }]))
            .unwrap();
        s.close().unwrap();
    }
    assert!(!path.join(SPANMAP_FILE).exists());
    let parent = path.parent().unwrap();
    let backups = |p: &Path| {
        std::fs::read_dir(p)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.starts_with(upgrade::BACKUP_PREFIX))
            .count()
    };
    // a RequireCurrent reader opens it as it is: no backup, no map
    let s = Store::open_with_policy(&path, OpenPolicy::RequireCurrent).unwrap();
    assert!(!s.has_feature_now(FEATURE_DEDUP_SPANS));
    drop(s);
    assert!(!path.join(SPANMAP_FILE).exists());
    assert_eq!(backups(parent), 0);

    // a dry-run plan lists both optional steps and creates no map
    let plan = upgrade::inspect(&path).unwrap();
    assert_eq!(plan.steps, [SNAPSHOT_DELTA_STEP, DEDUP_SPANS_STEP]);
    assert!(!plan.requires_upgrade);
    assert!(!path.join(SPANMAP_FILE).exists());
}

#[test]
fn r5902_first_open_maps_once_after_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let id = {
        let _legacy = format::legacy_writer_for_tests();
        let mut s = Store::create_legacy_for_tests(&path).unwrap();
        let b = block("b-00001", &text(40_000, 5));
        let id = s
            .commit_to_view("work", change(vec![Op::Create { block: b }]))
            .unwrap();
        s.close().unwrap();
        id
    };
    let parent = path.parent().unwrap().to_path_buf();
    let backup_seen = Rc::new(RefCell::new(None));
    let b = backup_seen.clone();
    let pp = parent.clone();
    dedup::on_map_event(Some(Box::new(move |e: &MapEvent| {
        if matches!(e, MapEvent::Scan) && b.borrow().is_none() {
            let n = std::fs::read_dir(&pp)
                .unwrap()
                .filter_map(|e| e.ok()?.file_name().into_string().ok())
                .filter(|n| n.starts_with(upgrade::BACKUP_PREFIX))
                .count();
            *b.borrow_mut() = Some(n);
        }
        Ok(())
    })));
    let opened = Store::open(&path);
    clear_hooks();
    let mut s = opened.unwrap();
    assert_eq!(
        *backup_seen.borrow(),
        Some(1),
        "the backup precedes the first scan"
    );
    assert!(s.has_feature_now(FEATURE_DEDUP_SPANS));
    let map = map_of(&path);
    assert!(map.through().is_some(), "the map reaches the source end");
    assert!(map.mappings().count() > 0);
    let want = s.change(&id).unwrap();
    upgrade::publish_checked(&mut s).unwrap();
    s.close().unwrap();

    // a second open scans nothing
    let log = record_events();
    let s = Store::open(&path).unwrap();
    clear_hooks();
    assert!(
        !log.borrow().iter().any(|e| e == "scan"),
        "{:?}",
        log.borrow()
    );
    assert_eq!(s.change(&id).unwrap(), want);
    s.check_objects().unwrap();
}

#[test]
fn r5902_an_empty_old_store_gets_an_empty_checked_map() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    {
        let _legacy = format::legacy_writer_for_tests();
        Store::create_legacy_for_tests(&path)
            .unwrap()
            .close()
            .unwrap();
    }
    let mut s = Store::open(&path).unwrap();
    upgrade::publish_checked(&mut s).unwrap();
    drop(s);
    let map = map_of(&path);
    assert_eq!(map.mappings().count(), 0);
    assert!(map.through().is_some(), "one Through row");
}

#[test]
fn r5902_stopped_mapping_note_text() {
    use dedup::StopReason;
    assert_eq!(
        StopReason::Io.note(),
        "span mapping stopped: map I/O failed; run rdy compact to retry"
    );
    assert_eq!(
        StopReason::Damaged.note(),
        "span mapping stopped: map data is damaged; run rdy compact to retry"
    );
    assert_eq!(
        StopReason::Limit.note(),
        "span mapping stopped: map allocation or row limit failed; run rdy compact to retry"
    );
    assert_eq!(
        dedup::FULL_NOTE,
        "the span map is full; new repeated byte runs keep their disk space"
    );
}

#[test]
fn r5902_threshold_at_the_limit_maps_once_after_the_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let log = record_events();
    let mut ids = vec![];
    // each write holds a 4 MiB block twice (its BLOCK frame and the CHANGE's copy)
    let mut i = 0;
    while !log.borrow().iter().any(|e| e == "threshold") {
        i += 1;
        assert!(i <= 12, "the threshold is 64 MiB of unmapped bytes");
        let b = block(&format!("b-{i:05}"), &text(4 << 20, i));
        let id = s
            .commit_to_view("work", change(vec![Op::Create { block: b }]))
            .unwrap();
        ids.push(id);
    }
    let events = log.borrow().clone();
    // the pass follows the write's own durable batch, in the same call
    let t = events.iter().position(|e| e == "threshold").unwrap();
    let sync = events.iter().position(|e| e == "sync").unwrap();
    assert!(t < sync, "{events:?}");
    assert_eq!(events.iter().filter(|e| *e == "sync").count(), 1);
    // the next write is below the threshold again
    log.borrow_mut().clear();
    let b = block("b-99999", "small");
    s.commit_to_view("work", change(vec![Op::Create { block: b }]))
        .unwrap();
    clear_hooks();
    assert!(log.borrow().is_empty(), "{:?}", log.borrow());
    let want: Vec<Change> = ids.iter().map(|id| s.change(id).unwrap()).collect();
    s.close().unwrap();
    let n = map_of(&path).mappings().count();
    assert!(n + 1 >= ids.len(), "{n} of {}", ids.len());
    let s = Store::open(&path).unwrap();
    for (id, w) in ids.iter().zip(&want) {
        assert_eq!(&s.change(id).unwrap(), w);
    }
}
