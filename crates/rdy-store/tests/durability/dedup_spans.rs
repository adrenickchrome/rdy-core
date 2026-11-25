//! The span map: a CHANGE's copy of a block it creates maps to the BLOCK frame's retained bytes,
//! the mapped range is punched only after the map is durable, every read fills the hole before
//! the hash check, and the map's damage, torn tail, fullness and failures are recovered or
//! reported as the store's other metadata are.
use crate::carried_fixture::{find, framed_at};
use rdy_model::plan::{PlanAct, PlanAnswer};
use rdy_model::*;
use rdy_store::compact::on_allocation;
use rdy_store::dedup::{self, MapEvent, MapOpen, SPANMAP_FILE, SpanMap};
use rdy_store::dedup_candidates;
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

    // a threshold pass: the write's batch (its marker last) and its sync come first, then the
    // Map/Through append, one map sync, publication and the punches, with no segment write after
    let base = below_threshold(100_000);
    let work = tempfile::tempdir().unwrap();
    let path = copy_of(base, work.path(), "t");
    let mut s = Store::open(&path).unwrap();
    let log = record_events();
    let l = log.clone();
    rdy_store::segment::on_segment_write(Some(Box::new(move |_| {
        l.borrow_mut().push("segment".into());
        Ok(())
    })));
    let p = log.clone();
    pio::on_punch(Some(Box::new(move |_, _| {
        p.borrow_mut().push("platform punch".into());
        None
    })));
    let id = s
        .commit_to_view(
            "work",
            change(vec![Op::Create {
                block: block("b-00001", &text(200_000, 31)),
            }]),
        )
        .unwrap();
    rdy_store::segment::on_segment_write(None);
    clear_hooks();
    let events = log.borrow().clone();
    let at = |name: &str| events.iter().position(|e| e.starts_with(name)).unwrap();
    let last_segment = events.iter().rposition(|e| e == "segment").unwrap();
    assert!(last_segment < at("threshold"), "{events:?}");
    assert!(
        at("threshold") < at("append")
            && at("append") < at("sync")
            && at("sync") < at("publish")
            && at("publish") < at("punch"),
        "{events:?}"
    );
    assert_eq!(events.iter().filter(|e| *e == "sync").count(), 1);
    assert!(events.iter().any(|e| e == "platform punch"), "{events:?}");
    let want = s.change(&id).unwrap();
    s.close().unwrap();
    assert!(map_of(&path).mappings().count() > 0);
    assert_eq!(Store::open(&path).unwrap().change(&id).unwrap(), want);

    // a write whose own sync fails is not durable: no pass runs after it
    let path = copy_of(base, work.path(), "f");
    let mut s = Store::open(&path).unwrap();
    let log = record_events();
    s.fail_next_sync();
    let r = s.commit_to_view(
        "work",
        change(vec![Op::Create {
            block: block("b-00001", &text(200_000, 31)),
        }]),
    );
    clear_hooks();
    assert!(r.is_err());
    assert!(log.borrow().is_empty(), "{:?}", log.borrow());
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

/// A store with a small secret block and a 54,000-byte retained block in separate changes, the
/// secret's removal sealed but not yet erased. Returns the retained change's hash.
fn secret_and_retained(path: &Path) -> Hash {
    use crate::redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
    use rdy_model::redaction::RedactTarget;
    let mut s = Store::create(path).unwrap();
    let secret = block("b-00001", "canary alpha secret");
    s.commit_to_view(
        "work",
        change(vec![Op::Create {
            block: secret.clone(),
        }]),
    )
    .unwrap();
    let kept = s
        .commit_to_view(
            "work",
            change(vec![Op::Create {
                block: block("b-00002", &text(54_000, 11)),
            }]),
        )
        .unwrap();
    drop(s);
    let spec = RequestSpec::new(
        "work",
        vec![RedactTarget::Version {
            id: secret.id.clone(),
            version: secret.version_hash(),
        }],
    );
    authorized_batch_fixture(
        path,
        &[spec],
        &FixtureOptions::approved(Proof::local_human()),
    );
    kept
}

/// Every open's redaction bootstrap reads a mapped log through its map: after a removal and a
/// compact that punches a retained change, a port-source read-only open reads the log as the
/// writable open does.
#[test]
fn r5902_removal_bootstrap_reads_through_the_map() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let kept = secret_and_retained(&path);
    let s = Store::open(&path).unwrap();
    let before = s.change(&kept).unwrap();
    let r = s.compact(false).unwrap();
    assert!(r.deduplicated_bytes >= 4096, "{}", r.deduplicated_bytes);
    s.close().unwrap();

    let s = Store::open(&path).unwrap();
    assert_eq!(s.change(&kept).unwrap(), before);
    s.close().unwrap();
    let ro = Store::open_source_read_only(&path).unwrap();
    assert_eq!(ro.change(&kept).unwrap(), before);
    ro.check_objects().unwrap();
}

// ---------------------------------------------------------------------------------------------
// the candidate rule

fn frame(place: u64, raw_kind: u8, hash: Hash, payload: &[u8]) -> dedup_candidates::FrameRef<'_> {
    dedup_candidates::FrameRef {
        place: rdy_store::segment::Location {
            segment: 0,
            offset: place,
        },
        raw_kind,
        hash,
        payload,
    }
}

fn plan_answer(item: u8, reason: &str) -> PlanAnswer {
    PlanAnswer {
        item: Hash([item; 32]),
        act: PlanAct::Confirm,
        reason: Some(reason.into()),
        subject: None,
        cause: Hash([0x52; 32]),
        by: Provenance {
            actor: "agent:w".into(),
            run: None,
            role: Role::Writer,
            at_ms: 1,
        },
    }
}

/// An answer whose encoding is exactly `len` bytes.
fn answer_of_len(item: u8, len: usize) -> PlanAnswer {
    for n in 1..len {
        let a = plan_answer(item, &"r".repeat(n));
        if postcard::to_allocvec(&a).unwrap().len() == len {
            return a;
        }
    }
    panic!("no answer of {len} bytes");
}

fn plan_with(prev: Option<Hash>, answers: Vec<PlanAnswer>) -> Plan {
    Plan {
        stable: Some(Hash([0x11; 32])),
        prev,
        view: "work".into(),
        proposal: None,
        answers,
        heads: vec![],
        provenance: Provenance {
            actor: "agent:w".into(),
            run: None,
            role: Role::Writer,
            at_ms: 1,
        },
        seal: None,
    }
}

/// The runs of a PLAN target with `t` answers against its prev with `s` answers.
fn plan_pair_runs(s: Vec<PlanAnswer>, t: Vec<PlanAnswer>) -> Vec<dedup_candidates::Run> {
    let source = postcard::to_allocvec(&plan_with(None, s)).unwrap();
    let sh = Hash::of(&source);
    let target = postcard::to_allocvec(&plan_with(Some(sh), t)).unwrap();
    let th = Hash::of(&target);
    dedup_candidates::plan_runs(
        &frame(100_000, kinds::PLAN, th, &target),
        &frame(0, kinds::PLAN, sh, &source),
    )
}

fn snapshot_of(ids: &[&str]) -> Vec<u8> {
    let s = rdy_model::StateSnapshot {
        blocks: ids
            .iter()
            .map(|i| (BlockId::parse(i).unwrap(), Hash::of(i.as_bytes())))
            .collect(),
    };
    s.canonical_bytes()
}

/// r5902: only the three pairs map, only their exact equal runs, joined where adjacent in both
/// payloads and kept from 256 bytes; each run keeps only whole aligned blocks of the target's
/// physical place (4,096 bytes on Unix, or an injected 512-byte or 65,536-byte cluster). A
/// different version, unequal bytes or a later source map nothing, and whole-frame dedupe still
/// drops a repeated frame.
#[test]
fn r5902_exact_runs_cover_new_and_historical_targets() {
    // PLAN: the join floor is 256 bytes, one answer of 255 bytes is discarded
    let r = plan_pair_runs(vec![answer_of_len(1, 255)], vec![answer_of_len(1, 255)]);
    assert!(r.is_empty(), "255 bytes: {r:?}");
    let r = plan_pair_runs(vec![answer_of_len(1, 256)], vec![answer_of_len(1, 256)]);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].target.len, 256);
    assert_eq!(r[0].class, dedup_candidates::Class::Plan);
    // equal answers adjacent in both join; a different answer between them splits the run
    let a = |i| answer_of_len(i, 200);
    let r = plan_pair_runs(vec![a(1), a(2)], vec![a(1), a(2)]);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].target.len, 400);
    let r = plan_pair_runs(vec![a(1), a(2)], vec![a(1), a(9), a(2)]);
    assert!(r.is_empty(), "two runs of 200 bytes: {r:?}");
    // an unequal encoding under the same item maps nothing
    let r = plan_pair_runs(
        vec![answer_of_len(1, 300)],
        vec![plan_answer(1, &"x".repeat(230))],
    );
    assert!(r.is_empty(), "{r:?}");
    // a source that is not the target's prev, or not earlier, maps nothing
    let s = postcard::to_allocvec(&plan_with(None, vec![a(1), a(2)])).unwrap();
    let t = postcard::to_allocvec(&plan_with(Some(Hash([9; 32])), vec![a(1), a(2)])).unwrap();
    let not_prev = dedup_candidates::plan_runs(
        &frame(100_000, kinds::PLAN, Hash::of(&t), &t),
        &frame(0, kinds::PLAN, Hash::of(&s), &s),
    );
    assert!(not_prev.is_empty());
    let t = postcard::to_allocvec(&plan_with(Some(Hash::of(&s)), vec![a(1), a(2)])).unwrap();
    let later = dedup_candidates::plan_runs(
        &frame(0, kinds::PLAN, Hash::of(&t), &t),
        &frame(100_000, kinds::PLAN, Hash::of(&s), &s),
    );
    assert!(later.is_empty(), "the source ends before the target starts");

    // SNAPSHOT: equal (id, version) entries adjacent in both join; reordering splits them
    let ids: Vec<String> = (0..20).map(|i| format!("b-{i:05}")).collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let s = snapshot_of(&refs);
    let mut more = refs.clone();
    more.push("c-00001");
    let t = snapshot_of(&more);
    let r = dedup_candidates::snapshot_runs(
        &frame(100_000, kinds::SNAPSHOT, Hash::of(&t), &t),
        &frame(0, kinds::SNAPSHOT, Hash::of(&s), &s),
    );
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0].class, dedup_candidates::Class::Snapshot);
    assert_eq!(r[0].target.len as usize, 20 * 40, "twenty 40-byte entries");
    // every 6th entry changed: no run of adjacent equal entries reaches 256 bytes
    let changed: Vec<String> = (0..20)
        .map(|i| {
            if i % 6 == 5 {
                format!("d-{i:05}")
            } else {
                format!("b-{i:05}")
            }
        })
        .collect();
    let crefs: Vec<&str> = changed.iter().map(String::as_str).collect();
    let t = snapshot_of(&crefs);
    let r = dedup_candidates::snapshot_runs(
        &frame(100_000, kinds::SNAPSHOT, Hash::of(&t), &t),
        &frame(0, kinds::SNAPSHOT, Hash::of(&s), &s),
    );
    assert!(r.is_empty(), "runs of 5 entries, 200 bytes: {r:?}");

    // alignment: the Unix minimum and the injected Windows clusters
    let run = |t_off: u64, len: u32| dedup_candidates::Run {
        class: dedup_candidates::Class::Change,
        target: dedup::FrameSpan {
            place: rdy_store::segment::Location {
                segment: 0,
                offset: t_off,
            },
            raw_kind: kinds::CHANGE,
            payload_len: 1 << 20,
            hash: Hash([1; 32]),
            start: 0,
            len,
        },
        source: dedup::FrameSpan {
            place: rdy_store::segment::Location {
                segment: 0,
                offset: 0,
            },
            raw_kind: kinds::BLOCK,
            payload_len: 1 << 20,
            hash: Hash([2; 32]),
            start: 0,
            len,
        },
    };
    for (a, below, at) in [
        (4096, 4095, 4096),
        (512, 511, 512),
        (65_536, 65_535, 65_536),
    ] {
        let aligned = a - HEADER;
        assert_eq!(
            dedup_candidates::align(&run(aligned, below), a),
            None,
            "{a}"
        );
        let got = dedup_candidates::align(&run(aligned, at), a).unwrap();
        assert_eq!((got.target.start, got.target.len), (0, at), "{a}");
        // a misaligned physical start rounds up, the source advances by the same cut
        let got = dedup_candidates::align(&run(aligned + 10, 2 * at), a).unwrap();
        assert_eq!(got.target.physical_start() % a, 0, "{a}");
        assert_eq!(got.target.start, got.source.start, "{a}");
        assert_eq!(got.target.len as u64, a, "{a}");
    }

    // CHANGE: a compact maps the embedded copy of the same version only, at the injected
    // cluster sizes too
    for a in [4096u64, 512, 65_536] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s");
        let mut s = Store::create(&path).unwrap();
        let b = block("b-00001", &text(200_000, 21));
        let v1 = b.version_hash();
        s.commit_to_view("work", change(vec![Op::Create { block: b.clone() }]))
            .unwrap();
        let mut next = b.clone();
        next.body = text(200_000, 22);
        let v2 = next.version_hash();
        let upd = s
            .commit_to_view(
                "work",
                Change {
                    deps: vec![],
                    ..change(vec![Op::Update {
                        base: v1,
                        block: next,
                    }])
                },
            )
            .unwrap();
        // the same frame again is dropped whole
        let before = s.log_end();
        let again = s.put_blob(b"one blob").unwrap();
        let mid = s.log_end();
        assert_eq!(s.put_blob(b"one blob").unwrap(), again);
        assert_eq!(s.log_end(), mid, "a repeated frame is not written again");
        assert!(mid > before);
        let want = s.change(&upd).unwrap();
        pio::on_dedup_alignment(Some(Box::new(move || Some(Ok(Some(a))))));
        let r = s.compact(false);
        clear_hooks();
        let r = r.unwrap();
        assert!(r.deduplicated_bytes >= a, "{a}: {r:?}");
        drop(s);
        let frames = framed_at(&path);
        let b1 = find(&frames, kinds::BLOCK, v1).loc;
        let b2 = find(&frames, kinds::BLOCK, v2).loc;
        for m in map_of(&path).mappings() {
            assert_eq!(m.target.len as u64 % a, 0, "{a}");
            assert_eq!(m.target.physical_start() % a, 0, "{a}");
            // each CHANGE maps against its own version's BLOCK
            let source = m.source.place;
            assert!(source == b1 || source == b2, "{a}");
            assert_eq!(m.source.hash, if source == b1 { v1 } else { v2 });
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.change(&upd).unwrap(), want, "{a}");
    }
}

// ---------------------------------------------------------------------------------------------
// the threshold pass

/// The sealed log bytes of the closed store at `dir`: every segment file's length.
fn log_len(dir: &Path) -> u64 {
    std::fs::read_dir(dir.join("segments"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".log"))
        .map(|e| e.metadata().unwrap().len())
        .sum()
}

/// A closed store with no map row whose log is `below` bytes short of the threshold (within a
/// few KiB), written once per test binary; each test takes a copy of it.
fn below_threshold(below: u64) -> &'static Path {
    use std::sync::{Mutex, OnceLock};
    static BASES: OnceLock<Mutex<BTreeMap<u64, &'static Path>>> = OnceLock::new();
    let bases = BASES.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut b = bases.lock().unwrap();
    b.entry(below).or_insert_with(|| {
        let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        let path = dir.path().join("base");
        let mut s = Store::create(&path).unwrap();
        s.set_segment_max(1 << 30);
        let n = dedup::THRESHOLD_BYTES - below;
        s.put_blob(text(n as usize, 77).as_bytes()).unwrap();
        s.close().unwrap();
        Box::leak(path.into_boxed_path())
    })
}

fn copy_of(base: &Path, dir: &Path, name: &str) -> std::path::PathBuf {
    let to = dir.join(name);
    crate::carried_fixture::copy_store(base, &to);
    to
}

/// r5902: a write that leaves 67,108,863 unmapped bytes does no map step; one that leaves
/// 67,108,864 runs one pass after its own durable batch. A candidate-free pass writes one
/// Through, and the next write adds none. A failed append or sync of a threshold pass leaves
/// the write's reply successful; later writes continue and run no pass on that handle; the
/// next compact reports the exact note; a reopen permits a later scheduled pass.
#[test]
fn r5902_writes_add_no_sync_below_the_threshold() {
    let base = below_threshold(900_000);
    let dir = tempfile::tempdir().unwrap();
    // a probe write gives the bytes one blob write adds besides its own
    let probe = copy_of(base, dir.path(), "probe");
    let mut s = Store::open(&probe).unwrap();
    s.set_segment_max(1 << 30);
    s.put_blob(text(100_000, 1).as_bytes()).unwrap();
    s.close().unwrap();
    let overhead = log_len(&probe) - log_len(base) - 100_000;
    for (name, end, maps) in [
        ("below", dedup::THRESHOLD_BYTES - 1, false),
        ("at", dedup::THRESHOLD_BYTES, true),
    ] {
        let path = copy_of(&probe, dir.path(), name);
        let n = end - log_len(&path) - overhead;
        let mut s = Store::open(&path).unwrap();
        s.set_segment_max(1 << 30);
        let log = record_events();
        s.put_blob(text(n as usize, 2).as_bytes()).unwrap();
        clear_hooks();
        s.close().unwrap();
        assert_eq!(log_len(&path), end, "{name}: the write ends at {end}");
        let events = log.borrow().clone();
        if maps {
            // one pass: a scan, one Through row (no candidate), one sync
            assert_eq!(
                events,
                ["threshold", "scan", "append 0 true", "sync", "publish"],
                "{name}"
            );
            assert_eq!(map_of(&path).rows(), 1, "one Through");
            // the next write is below the threshold again: no step, no row
            let mut s = Store::open(&path).unwrap();
            let log = record_events();
            s.put_blob(b"one more").unwrap();
            assert!(log.borrow().is_empty(), "{:?}", log.borrow());
            let r = s.compact(false).unwrap();
            assert!(r.notes.is_empty(), "{:?}", r.notes);
            // a repeat at the same end adds no row
            log.borrow_mut().clear();
            s.compact(false).unwrap();
            clear_hooks();
            assert!(
                !log.borrow().iter().any(|e| e.starts_with("append")),
                "{:?}",
                log.borrow()
            );
            drop(s);
            assert_eq!(map_of(&path).rows(), 2, "compact's own Through only");
        } else {
            assert!(events.is_empty(), "{name}: {events:?}");
            assert_eq!(map_of(&path).rows(), 0);
        }
    }

    // a failed append or sync stops passes on the handle, never the write
    for fail in ["append", "sync"] {
        let path = copy_of(base, dir.path(), &format!("fail-{fail}"));
        let mut s = Store::open(&path).unwrap();
        let log = Rc::new(RefCell::new(vec![]));
        let l = log.clone();
        dedup::on_map_event(Some(Box::new(move |e: &MapEvent| {
            let name = match e {
                MapEvent::Append { .. } => "append",
                MapEvent::Sync => "sync",
                MapEvent::Threshold => "threshold",
                _ => "other",
            };
            l.borrow_mut().push(name);
            if name == fail {
                return Err(std::io::Error::other("injected"));
            }
            Ok(())
        })));
        let id = s
            .commit_to_view(
                "work",
                change(vec![Op::Create {
                    block: block("b-00001", &text(1_000_000, 5)),
                }]),
            )
            .expect("the write's reply stays successful");
        assert_eq!(log.borrow().first(), Some(&"threshold"), "{fail}");
        log.borrow_mut().clear();
        // later writes continue, past the threshold still, and run no pass on this handle
        let id2 = s
            .commit_to_view(
                "work",
                change(vec![Op::Create {
                    block: block("b-00002", &text(10_000, 6)),
                }]),
            )
            .unwrap();
        assert!(log.borrow().is_empty(), "{fail}: {:?}", log.borrow());
        clear_hooks();
        let r = s.compact(false).unwrap();
        assert!(
            r.notes
                .contains(&"span mapping stopped: map I/O failed; run rdy compact to retry".into()),
            "{fail}: {:?}",
            r.notes
        );
        assert!(s.change(&id).is_ok() && s.change(&id2).is_ok());
        s.close().unwrap();
        // a reopen permits a later scheduled pass
        let mut s = Store::open(&path).unwrap();
        let log = record_events();
        s.put_blob(text(dedup::THRESHOLD_BYTES as usize, 8).as_bytes())
            .unwrap();
        clear_hooks();
        assert!(
            log.borrow().iter().any(|e| e == "threshold"),
            "{fail}: {:?}",
            log.borrow()
        );
    }
}

// ---------------------------------------------------------------------------------------------
// kills at each map boundary

#[test]
#[ignore = "the kill pin runs this in a child process"]
fn r5902_kill_child() {
    let Ok(dir) = std::env::var("RDY_DEDUP_DIR") else {
        return;
    };
    let mode = std::env::var("RDY_DEDUP_MODE").unwrap();
    let kill = std::env::var("RDY_DEDUP_KILL").unwrap();
    let mut s = Store::open(Path::new(&dir)).unwrap();
    match rdy_store::segment::CrashPoint::parse(&kill) {
        Some(p) => s.crash_at(p),
        None => dedup::on_map_event(Some(Box::new(move |e: &MapEvent| {
            let name = match e {
                MapEvent::Append { .. } => "append",
                MapEvent::Sync => "sync",
                MapEvent::Publish => "publish",
                MapEvent::Punch(_) => "punch",
                MapEvent::Scan => "scan",
                MapEvent::Threshold => "threshold",
            };
            if name == kill {
                std::process::abort();
            }
            Ok(())
        }))),
    }
    if mode == "threshold" {
        s.commit_to_view(
            "work",
            change(vec![Op::Create {
                block: block("b-00009", &text(200_000, 41)),
            }]),
        )
        .unwrap();
    } else {
        s.compact(false).unwrap();
    }
    std::process::exit(3); // the kill point was never reached
}

/// Runs the child on `dir`; it must die at `kill`.
fn kill_at(dir: &Path, mode: &str, kill: &str) {
    let name = match module_path!().split_once("::") {
        Some((_, m)) => format!("{m}::r5902_kill_child"),
        None => "r5902_kill_child".into(),
    };
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            name.as_str(),
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env("RDY_DEDUP_DIR", dir)
        .env("RDY_DEDUP_MODE", mode)
        .env("RDY_DEDUP_KILL", kill)
        .output()
        .unwrap();
    assert_ne!(
        out.status.code(),
        Some(3),
        "{mode} {kill}: the child never reached its kill point"
    );
    assert!(
        !out.status.success(),
        "{mode} {kill}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The checks after a kill: a writable open recovers, every acknowledged change reads as it
/// did, and two compacts map and punch idempotently.
fn recovers(path: &Path, want: &[(Hash, Change)], what: &str) {
    let s = Store::open(path).unwrap_or_else(|e| panic!("{what}: {e:?}"));
    for (id, c) in want {
        assert_eq!(&s.change(id).unwrap(), c, "{what}");
    }
    s.compact(false).unwrap();
    let rows = s.log_bytes().unwrap().deduplicated_bytes;
    s.close().unwrap();
    let after = map_of(path).rows();
    let s = Store::open(path).unwrap();
    let r = s.compact(false).unwrap();
    assert_eq!(
        r.deduplicated_bytes, rows,
        "{what}: the repeat maps nothing new"
    );
    for (id, c) in want {
        assert_eq!(&s.change(id).unwrap(), c, "{what}");
    }
    s.check_objects().unwrap();
    drop(s);
    assert_eq!(map_of(path).rows(), after, "{what}: no row at the same end");
}

/// r5902: a kill before or after the ordinary write's marker sync, at the map append, at the
/// one map sync, at publication and at a punch, of a threshold pass and of an explicit compact
/// (which has no write marker), loses no acknowledged byte; the next open and compacts recover
/// and retry punching idempotently. A partial append is a torn tail the writable open
/// quarantines; an invalid interior row followed by a valid one refuses the open.
#[test]
fn r5902_kill_at_each_map_boundary_recovers() {
    let work = tempfile::tempdir().unwrap();
    // explicit compact
    let src = work.path().join("compact-src");
    let (id, _) = one_big_change(&src);
    let want = vec![(id, Store::open(&src).unwrap().change(&id).unwrap())];
    for kill in ["append", "sync", "publish", "punch"] {
        let path = copy_of(&src, work.path(), &format!("compact-{kill}"));
        kill_at(&path, "compact", kill);
        if kill == "sync" {
            // the rows reached the file, unsynced; a partial append leaves a torn row
            let torn = copy_of(&path, work.path(), "compact-partial");
            let p = torn.join(SPANMAP_FILE);
            let bytes = std::fs::read(&p).unwrap();
            assert!(bytes.len() > 12 + 5, "rows were written");
            std::fs::write(&p, &bytes[..bytes.len() - 5]).unwrap();
            recovers(&torn, &want, "compact partial append");
        }
        recovers(&path, &want, &format!("compact {kill}"));
    }

    // a threshold pass after an ordinary write
    let base = below_threshold(100_000);
    for kill in [
        "before_sync",
        "after_sync",
        "append",
        "sync",
        "publish",
        "punch",
    ] {
        let path = copy_of(base, work.path(), &format!("threshold-{kill}"));
        kill_at(&path, "threshold", kill);
        let s = Store::open(&path).unwrap();
        // the change is acknowledged once its marker is synced
        let written: Vec<(Hash, Change)> = s
            .included("work")
            .map(|v| v.to_vec())
            .unwrap_or_default()
            .into_iter()
            .map(|h| (h, s.change(&h).unwrap()))
            .collect();
        drop(s);
        if kill != "before_sync" {
            assert_eq!(written.len(), 1, "{kill}: the write was acknowledged");
        }
        recovers(&path, &written, &format!("threshold {kill}"));
    }
}

// ---------------------------------------------------------------------------------------------
// captured reads, damage and limits

/// r5902: a reader captured before a compact reads the exact original bytes before the map is
/// published (the bytes are still on disk) and after the punch (the map fills the hole), and a
/// reader racing the compact on another thread always reads them too.
#[test]
fn r5902_captured_read_cannot_observe_a_raw_hole() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let (id, _) = one_big_change(&path);
    let c = find(&framed_at(&path), kinds::CHANGE, id);
    let s = Store::open(&path).unwrap();
    let want = s.change(&id).unwrap();
    let captured = s.reader();
    let before_publish = Rc::new(RefCell::new(None));
    let b = before_publish.clone();
    let r = captured.clone();
    dedup::on_map_event(Some(Box::new(move |e: &MapEvent| {
        if *e == MapEvent::Sync {
            *b.borrow_mut() = Some(r.change(&id).unwrap());
        }
        Ok(())
    })));
    // a reader on another thread reads while the compact maps and punches
    let racer = s.reader();
    let start = std::sync::Arc::new(std::sync::Barrier::new(2));
    let go = start.clone();
    let w = want.clone();
    let t = std::thread::spawn(move || {
        go.wait();
        for _ in 0..50 {
            assert_eq!(racer.change(&id).unwrap(), w);
        }
    });
    start.wait();
    let report = s.compact(false);
    clear_hooks();
    report.unwrap();
    t.join().unwrap();
    assert_eq!(before_publish.borrow().as_ref(), Some(&want));
    assert_eq!(captured.change(&id).unwrap(), want, "after the punch");
    assert!(map_of(&path).mappings().count() > 0);
    // where the platform punched, the file holds zeros the map fills
    let raw = raw_payload(&path, &c);
    let m = *map_of(&path).mappings().next().unwrap();
    let range = m.target.start as usize..(m.target.start + m.target.len) as usize;
    if raw[range].iter().all(|&b| b == 0) {
        assert_eq!(captured.change(&id).unwrap(), want, "a hole reads whole");
    }

    // a removal whose erasure is still pending refuses the readers captured before it, and a
    // new reader too; the next open finishes it and nothing reads the text back
    use rdy_store::redaction_io::{FailAction, Failpoint, arm_failpoint};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let (secret, _, kept) = secret_and_kept(&mut s);
    assert!(s.compact(false).unwrap().deduplicated_bytes >= 2 * 4096);
    let want = s.change(&kept).unwrap();
    let captured = s.reader();
    assert_eq!(captured.change(&kept).unwrap(), want);
    captured.check_redaction_epoch().unwrap();
    arm_failpoint(Some((Failpoint::AfterApplySync, FailAction::Fail)));
    let failed = remove_live(&mut s, &secret);
    arm_failpoint(None);
    assert!(
        matches!(failed, Err(StoreError::RedactionPending)),
        "{failed:?}"
    );
    let old = captured.check_redaction_epoch();
    assert!(matches!(old, Err(StoreError::RedactionPending)), "{old:?}");
    let new = s.reader().check_redaction_epoch();
    assert!(matches!(new, Err(StoreError::RedactionPending)), "{new:?}");
    drop(s);
    let old = captured.check_redaction_epoch();
    assert!(matches!(old, Err(StoreError::RedactionPending)), "{old:?}");
    drop(captured);
    let s = Store::open(&path).unwrap();
    s.check_redaction_epoch().unwrap();
    assert_eq!(s.change(&kept).unwrap(), want);
    assert!(s.get_block(&secret.version_hash()).unwrap().is_redacted());
    s.check_objects().unwrap();
    drop(s);
    assert_eq!(holds_canary(&path), Vec::<String>::new());
}

/// The journal bytes of `records` after the file header, chained from the zero digest.
fn journal(records: &[dedup::SpanMapRecordV1]) -> Vec<u8> {
    let mut out = b"RDYSPANS".to_vec();
    out.extend_from_slice(&1u32.to_le_bytes());
    let mut prev = [0u8; 32];
    for r in records {
        let body = postcard::to_allocvec(r).unwrap();
        out.extend_from_slice(&row(&prev, &body));
        prev = digest(&prev, &body);
    }
    out
}

fn digest(prev: &[u8; 32], body: &[u8]) -> [u8; 32] {
    let mut bytes = b"rdy-span-map-1\0".to_vec();
    bytes.extend_from_slice(prev);
    bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
    bytes.extend_from_slice(body);
    Hash::of(&bytes).0
}

fn row(prev: &[u8; 32], body: &[u8]) -> Vec<u8> {
    let mut out = (body.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(body);
    out.extend_from_slice(&digest(prev, body));
    out
}

/// r5902: map damage and limits fail closed. Punched bytes with no map, and a row whose source
/// header is absent or wrong, fail the read-time checks; open reads no frame header to load the
/// map. A bad header, interior damage, a forward source, an overlap and an out-of-bounds range
/// fail the metadata checks; a 257-byte row and an allocation failure are DedupLimit; a full
/// map appends nothing past its capacity, notes it, keeps reads and writes working, and stops
/// threshold passes on its handle until a reopen.
#[test]
fn r5902_map_damage_and_limits_fail_closed() {
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    let (id, v) = one_big_change(&src);
    {
        let s = Store::open(&src).unwrap();
        s.compact(false).unwrap();
    }
    let good = map_of(&src);
    let maps: Vec<dedup::Mapping> = good.mappings().copied().collect();
    let through = good.through().unwrap();
    let rec = |m: &dedup::Mapping| dedup::SpanMapRecordV1::Map {
        target: m.target,
        source: m.source,
    };
    let c = find(&framed_at(&src), kinds::CHANGE, id).loc;
    let opened = |p: &Path| Store::open(p).and_then(|s| s.change(&id).map(|_| ()));
    // the mapped target ranges as a punch leaves them, whatever this filesystem did
    let zero_targets = |p: &Path| {
        for m in &maps {
            let seg = seg_path(&p.join("segments"), m.target.place.segment);
            let mut bytes = std::fs::read(&seg).unwrap();
            let a = m.target.physical_start() as usize;
            bytes[a..a + m.target.len as usize].fill(0);
            std::fs::write(&seg, bytes).unwrap();
        }
    };
    zero_targets(&src);
    assert!(opened(&src).is_ok(), "the map fills the holes");

    // the journal this test writes is the store's own: the same bytes
    let mine: Vec<dedup::SpanMapRecordV1> = maps
        .iter()
        .map(rec)
        .chain([dedup::SpanMapRecordV1::Through { end: through }])
        .collect();
    assert_eq!(
        journal(&mine),
        std::fs::read(src.join(SPANMAP_FILE)).unwrap()
    );

    // punched bytes without their map fail the existing hash check
    {
        let p = copy_of(&src, work.path(), "no-map");
        std::fs::remove_file(p.join(SPANMAP_FILE)).unwrap();
        assert!(
            opened(&p).is_err(),
            "a hole with no row is never read as data"
        );
    }
    // a wrong source header loads (open reads no frame header) and fails the read
    {
        let p = copy_of(&src, work.path(), "wrong-source");
        let mut bad = maps.clone();
        for m in &mut bad {
            m.source.hash = Hash([0xEE; 32]);
        }
        let rows: Vec<_> = bad
            .iter()
            .map(rec)
            .chain([dedup::SpanMapRecordV1::Through { end: through }])
            .collect();
        std::fs::write(p.join(SPANMAP_FILE), journal(&rows)).unwrap();
        assert!(
            SpanMap::load(&p, MapOpen::ReadOnly).is_ok(),
            "no header read"
        );
        assert!(opened(&p).is_err());
    }
    // metadata damage
    let refuses = |name: &str, bytes: Vec<u8>, want: fn(&StoreError) -> bool| {
        let p = copy_of(&src, work.path(), name);
        std::fs::write(p.join(SPANMAP_FILE), bytes).unwrap();
        let got = SpanMap::load(&p, MapOpen::ReadOnly).err();
        assert!(got.as_ref().is_some_and(want), "{name}: {got:?}");
        let got = Store::open(&p).err();
        assert!(got.as_ref().is_some_and(want), "{name}: {got:?}");
    };
    let corrupt = |e: &StoreError| matches!(e, StoreError::DedupCorrupt);
    let limit = |e: &StoreError| matches!(e, StoreError::DedupLimit);
    let mut header = journal(&mine);
    header[8] = 2;
    refuses("bad-header", header, corrupt);
    let mut digest_damage = journal(&mine);
    let first_digest = 12 + 4 + postcard::to_allocvec(&mine[0]).unwrap().len();
    digest_damage[first_digest] ^= 1;
    refuses("interior-digest", digest_damage, corrupt);
    let mut forward = maps[0];
    forward.source.place = rdy_store::segment::Location {
        segment: c.segment,
        offset: c.offset + 1_000_000,
    };
    refuses("forward-source", journal(&[rec(&forward)]), corrupt);
    refuses("overlap", journal(&[rec(&maps[0]), rec(&maps[0])]), corrupt);
    let mut over = maps[0];
    over.target.start = u32::MAX - 10;
    refuses("overflow", journal(&[rec(&over)]), corrupt);
    // a row of 257 bytes is over the bound; one of 256 bytes at the end is a torn tail
    let mut big = journal(&mine);
    big.extend_from_slice(&257u32.to_le_bytes());
    big.extend_from_slice(&[0u8; 257 + 32]);
    refuses("row-257", big, limit);
    let p = copy_of(&src, work.path(), "row-256");
    let mut torn = journal(&mine);
    torn.extend_from_slice(&256u32.to_le_bytes());
    torn.extend_from_slice(&[0u8; 256 + 32]);
    std::fs::write(p.join(SPANMAP_FILE), &torn).unwrap();
    let ro = Store::open_source_read_only(&p).unwrap();
    assert!(
        ro.open_report().unclean_shutdown,
        "ignored, not quarantined"
    );
    drop(ro);
    assert_eq!(
        std::fs::read(p.join(SPANMAP_FILE)).unwrap(),
        torn,
        "unchanged"
    );
    opened(&p).unwrap();
    assert_eq!(std::fs::read(p.join(SPANMAP_FILE)).unwrap(), journal(&mine));

    // an allocation failure of a pass is DedupLimit, and appends nothing
    let p = work.path().join("alloc");
    one_big_change(&p);
    let s = Store::open(&p).unwrap();
    dedup::fail_allocation_for_tests(true);
    let r = s.compact(false);
    dedup::fail_allocation_for_tests(false);
    assert!(
        matches!(&r, Err(e) if format!("{e:?}").contains("DedupLimit")),
        "{r:?}"
    );
    drop(s);
    assert_eq!(map_of(&p).rows(), 0);

    // at capacity N, a proposed N+1 appends nothing and notes the full map; reads, writes and
    // compact's retries continue
    let p = copy_of(&src, work.path(), "full");
    let rows = map_of(&p).rows();
    dedup::cap_rows_for_tests(Some(rows));
    let mut s = Store::open(&p).unwrap();
    let id2 = s
        .commit_to_view(
            "work",
            change(vec![Op::Create {
                block: block("b-00002", &text(40_000, 8)),
            }]),
        )
        .unwrap();
    let r = s.compact(false).unwrap();
    assert!(
        r.notes.iter().any(|n| n == dedup::FULL_NOTE),
        "{:?}",
        r.notes
    );
    assert_eq!(map_of(&p).rows(), rows, "nothing past the cap");
    assert!(s.change(&id).is_ok() && s.change(&id2).is_ok());
    assert!(
        s.log_bytes()
            .unwrap()
            .notes
            .iter()
            .any(|n| n == dedup::FULL_NOTE)
    );
    s.close().unwrap();
    dedup::cap_rows_for_tests(None);
    let _ = v;

    // a threshold pass that finds the map full stops threshold passes on its handle; a
    // reopen permits one scheduled capacity check
    let base = below_threshold(100_000);
    let p = copy_of(base, work.path(), "full-threshold");
    dedup::cap_rows_for_tests(Some(0));
    for open in 0..2 {
        let mut s = Store::open(&p).unwrap();
        let log = record_events();
        for i in 0..3 {
            let n = 100 + 10 * open + i;
            s.commit_to_view(
                "work",
                change(vec![Op::Create {
                    block: block(&format!("b-{n:05}"), &text(200_000, n as u64)),
                }]),
            )
            .unwrap();
        }
        dedup::on_map_event(None);
        let passes = log.borrow().iter().filter(|e| *e == "threshold").count();
        assert_eq!(passes, 1, "{:?}", log.borrow());
        assert!(!log.borrow().iter().any(|e| e.starts_with("append")));
        // explicit compact still runs, and notes the full map
        let r = s.compact(false).unwrap();
        assert!(r.notes.iter().any(|n| n == dedup::FULL_NOTE));
        s.close().unwrap();
    }
    dedup::cap_rows_for_tests(None);
    clear_hooks();
}

// ---------------------------------------------------------------------------------------------
// removal and platform punching

const CANARY: &str = "CANARYQX";

/// A body of `n` bytes with the canary every 1,000 bytes (text() writes no capital letter).
fn canary_body(n: usize) -> String {
    let mut out = String::with_capacity(n);
    let filler = text(n, 61);
    for (i, ch) in filler.chars().enumerate() {
        if i % 1000 == 0 && i + CANARY.len() <= n {
            out.push_str(CANARY);
        }
        out.push(ch);
        if out.len() >= n {
            break;
        }
    }
    out
}

/// Whether any file under `dir` holds the canary.
fn holds_canary(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
    let mut files = vec![];
    walk(dir, &mut files);
    files
        .into_iter()
        .filter(|p| {
            std::fs::read(p)
                .unwrap()
                .windows(CANARY.len())
                .any(|w| w == CANARY.as_bytes())
        })
        .map(|p| p.display().to_string())
        .collect()
}

/// r5902: an authorized removal of a block whose CHANGE copy is mapped to its BLOCK frame zeroes
/// the source and the mapped target, including a target whose punch was unsupported (its raw
/// bytes stayed): no file of the store holds the removed text, and reopen, a read-only open and
/// the checks pass with no mapped copy read back.
#[test]
fn r5902_removal_erases_all_mapped_copies() {
    use crate::redaction_fixture::{FixtureOptions, Proof, RequestSpec, authorized_batch_fixture};
    use rdy_model::redaction::RedactTarget;
    for unsupported in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s");
        let mut s = Store::create(&path).unwrap();
        let secret = block("b-00001", &canary_body(40_000));
        let gone = s
            .commit_to_view(
                "work",
                change(vec![Op::Create {
                    block: secret.clone(),
                }]),
            )
            .unwrap();
        let kept = s
            .commit_to_view(
                "work",
                change(vec![Op::Create {
                    block: block("b-00002", &text(40_000, 62)),
                }]),
            )
            .unwrap();
        // the removal fixture appends through a raw segment log, which reads no map, so the
        // punch keeps the bytes here: supported (as if punched later) or unsupported
        let answer = if unsupported {
            Punch::Unsupported
        } else {
            Punch::Supported
        };
        pio::on_punch(Some(Box::new(move |_, _| Some(Ok(answer)))));
        let r = s.compact(false);
        clear_hooks();
        assert!(r.unwrap().deduplicated_bytes >= 4096);
        let want = s.change(&kept).unwrap();
        s.close().unwrap();
        let map = map_of(&path);
        let frames = framed_at(&path);
        let target = find(&frames, kinds::CHANGE, gone).loc;
        assert!(
            map.mappings().any(|m| m.target.place == target),
            "the secret's CHANGE copy is mapped"
        );
        assert!(
            !holds_canary(&path).is_empty(),
            "the text is on disk before"
        );
        let spec = RequestSpec::new(
            "work",
            vec![RedactTarget::Version {
                id: secret.id.clone(),
                version: secret.version_hash(),
            }],
        );
        authorized_batch_fixture(
            &path,
            &[spec],
            &FixtureOptions::approved(Proof::local_human()),
        );
        let s = Store::open(&path).unwrap();
        assert_eq!(s.change(&kept).unwrap(), want, "{unsupported}");
        s.check_objects().unwrap();
        assert_eq!(
            s.log_bytes().unwrap().deduplicated_bytes,
            map_of(&path)
                .mappings()
                .filter(|m| m.target.place != target)
                .map(|m| u64::from(m.target.len))
                .sum::<u64>(),
            "a removed target counts no deduplicated byte"
        );
        s.close().unwrap();
        assert_eq!(holds_canary(&path), Vec::<String>::new(), "{unsupported}");
        let ro = Store::open_source_read_only(&path).unwrap();
        assert_eq!(ro.change(&kept).unwrap(), want);
        drop(ro);
        let s = Store::open(&path).unwrap();
        s.check_objects().unwrap();
        assert!(s.compact(false).is_ok());
        drop(s);
        assert_eq!(holds_canary(&path), Vec::<String>::new(), "{unsupported}");
    }
    removal_live_with_the_platform_punch();
    removal_erases_the_upgrade_backup();
    removal_carried_to_a_receiver();
}

fn change_in(view: &str, ops: Vec<Op>) -> Change {
    Change {
        view: view.into(),
        ..change(ops)
    }
}

fn writer() -> Provenance {
    change(vec![]).provenance
}

/// A local person's landing of `proposal`.
fn approve(proposal: Hash) -> rdy_store::LandRequest {
    rdy_store::LandRequest {
        proposal,
        accept: None,
        approver: Provenance {
            actor: "human:alice".into(),
            run: None,
            role: Role::Approver,
            at_ms: 50,
        },
        alt: None,
        relay: None,
        evidence: None,
        assessment: None,
    }
}

/// In view `src` of `s`: a change that creates the 40,000-byte canary block `secret`, then a
/// change that creates a 40,000-byte block that stays. Returns the secret, its change and the
/// kept change.
fn secret_and_kept(s: &mut Store) -> (Block, Hash, Hash) {
    let secret = block("b-00001", &canary_body(40_000));
    let gone = s
        .commit_to_view(
            "src",
            change_in(
                "src",
                vec![Op::Create {
                    block: secret.clone(),
                }],
            ),
        )
        .unwrap();
    let kept = s
        .commit_to_view(
            "src",
            change_in(
                "src",
                vec![Op::Create {
                    block: block("b-00002", &text(40_000, 62)),
                }],
            ),
        )
        .unwrap();
    (secret, gone, kept)
}

/// The live removal of `secret`'s version: the request through the store's checked writer in
/// view `work`, its proposal, and a person's landing, which seals the apply and erases before
/// it returns.
fn remove_live(s: &mut Store, secret: &Block) -> Result<(), StoreError> {
    use crate::redaction_fixture::{RequestSpec, build_request};
    use rdy_model::redaction::RedactTarget;
    let spec = RequestSpec::new(
        "work",
        vec![RedactTarget::Version {
            id: secret.id.clone(),
            version: secret.version_hash(),
        }],
    );
    let d = build_request(s, &spec);
    let p = s
        .prepare_redaction(&d.carrier, &d.request, &d.residues)
        .unwrap();
    s.commit_redaction(p).unwrap();
    let proposal = s.propose("work", "Remove content", writer()).unwrap();
    s.land_atomic(approve(proposal)).map(|_| ())
}

/// A full dump of `s` and its chain end (a follower's seed).
fn full_dump(s: &Store) -> (Vec<u8>, rdy_store::dump_chain::ChainEnd) {
    use rdy_store::dump::{DumpHeader, DumpWriter, RemovalTally};
    use rdy_store::dump_chain::{BatchMarker, ChainEnd, DumpChainHeader};
    let r = s.reader();
    let end = s.log_end();
    let repo = r.repo_id().unwrap().unwrap();
    let end_batch = r.batch_marker_at(end).unwrap().unwrap_or(BatchMarker::ZERO);
    let store = format::DumpStore::Features(r.store_features().unwrap());
    let header = DumpHeader {
        store: store.clone(),
        log_end: end,
        rdy: "test".into(),
        chain: Some(DumpChainHeader::full(repo, end_batch)),
    };
    let mut w = DumpWriter::new(Vec::new(), &header).unwrap();
    let mut tally = RemovalTally::default();
    let origin = rdy_store::segment::Location {
        segment: 0,
        offset: 0,
    };
    r.for_each_batch_after(origin, end, &mut |b| {
        tally.observe(&b)?;
        w.batch(&b)
    })
    .unwrap();
    let facts = tally.resolved(&r, end).unwrap();
    let (bytes, totals) = w.finish(facts).unwrap();
    (
        bytes,
        ChainEnd {
            repo_id: repo,
            store_format: store,
            end,
            end_batch,
            digest: totals.digest,
        },
    )
}

/// Loads the dump `bytes` into a new store at `to`.
fn load_dump(bytes: &[u8], to: &Path) {
    use rdy_store::dump::{DumpItem, DumpReader};
    let (mut reader, _, header) = DumpReader::open(bytes).unwrap();
    let mut ingest = rdy_store::ingest::Ingest::create(to).unwrap();
    let mut n = 0;
    while let Some(item) = reader.next().unwrap() {
        if let DumpItem::Batch(b) = item {
            n += 1;
            ingest.batch(n, &b).unwrap();
        }
    }
    ingest.upgrade(&header.store).unwrap();
    ingest.finish().unwrap();
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// The ordinary live removal on a store the platform's own punch call compacted: the source
/// BLOCK and its mapped CHANGE copy are both zeroed (a hole stays a hole); a reader captured
/// before it refuses; no file of the store, no dump after it, no store loaded from that dump
/// (before and after its own compact) and no follower that follows the removal batch holds the
/// text.
fn removal_live_with_the_platform_punch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let (secret, gone, kept) = secret_and_kept(&mut s);
    let r = s.compact(false).unwrap();
    assert!(r.deduplicated_bytes >= 2 * 4096, "{r:?}");
    let frames = crate::carried_fixture::framed(&s.reader(), s.log_end());
    let target = find(&frames, kinds::CHANGE, gone).loc;
    assert!(
        map_of(&path).mappings().any(|m| m.target.place == target),
        "the secret's CHANGE copy is mapped"
    );
    let unsupported = r.notes.iter().any(|n| n.starts_with(NOT_SUPPORTED));
    let c = find(&frames, kinds::CHANGE, gone);
    let holes = map_of(&path)
        .mappings()
        .filter(|m| m.target.place == target)
        .all(|m| {
            let raw = raw_payload(&path, &c);
            raw[m.target.start as usize..(m.target.start + m.target.len) as usize]
                .iter()
                .all(|&x| x == 0)
        });
    assert!(
        holes || unsupported,
        "the platform punched or said it could not"
    );
    eprintln!("live removal over the platform punch: holes {holes}, unsupported {unsupported}");
    let want = s.change(&kept).unwrap();
    let captured = s.reader();
    // a follower seeded now holds the text; the removal batch reaches it later
    let (seed, chain) = full_dump(&s);
    assert!(contains(&seed, CANARY.as_bytes()));
    let fdir = dir.path().join("follower");
    load_dump(&seed, &fdir);
    {
        let lock = rdy_store::StoreLock::take(&fdir).unwrap();
        rdy_store::follower::write_seed_mark(
            &fdir,
            &lock,
            &rdy_store::follower::FollowerMark {
                chain: chain.clone(),
                files: vec![],
            },
        )
        .unwrap();
    }

    remove_live(&mut s, &secret).unwrap();
    assert!(matches!(
        captured.check_redaction_epoch(),
        Err(StoreError::RedactedState)
    ));
    assert_eq!(holds_canary(&path), Vec::<String>::new(), "live");
    assert!(s.get_block(&secret.version_hash()).unwrap().is_redacted());
    assert_eq!(s.change(&kept).unwrap(), want);
    s.check_objects().unwrap();
    assert_eq!(
        s.log_bytes().unwrap().deduplicated_bytes,
        map_of(&path)
            .mappings()
            .filter(|m| m.target.place != target)
            .map(|m| u64::from(m.target.len))
            .sum::<u64>(),
        "a removed target counts no deduplicated byte"
    );

    // a dump after the removal, and a store loaded from it, before and after its own compact
    let (bytes, _) = full_dump(&s);
    assert!(!contains(&bytes, CANARY.as_bytes()), "the dump");
    let loaded = dir.path().join("loaded");
    load_dump(&bytes, &loaded);
    assert_eq!(holds_canary(&loaded), Vec::<String>::new(), "loaded");
    let l = Store::open(&loaded).unwrap();
    assert_eq!(l.change(&kept).unwrap(), want);
    l.check_objects().unwrap();
    assert!(l.compact(false).unwrap().deduplicated_bytes >= 4096);
    assert!(
        !map_of(&loaded)
            .mappings()
            .any(|m| m.target.place == target || m.source.place == target),
        "a removed frame is never mapped at a receiver"
    );
    drop(l);
    assert_eq!(holds_canary(&loaded), Vec::<String>::new(), "loaded");
    let l = Store::open(&loaded).unwrap();
    assert_eq!(l.change(&kept).unwrap(), want);
    l.check_objects().unwrap();
    drop(l);

    // the follower gets the removal batch (a follow stopped after its sync); its open erases
    let end = s.log_end();
    assert_eq!(end.segment, chain.end.segment, "one segment");
    let seg = seg_path(&path.join("segments"), end.segment);
    let tail =
        std::fs::read(&seg).unwrap()[chain.end.offset as usize..end.offset as usize].to_vec();
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(seg_path(&fdir.join("segments"), end.segment))
            .unwrap();
        f.write_all(&tail).unwrap();
        f.sync_all().unwrap();
    }
    let f = Store::open_follower(&fdir).unwrap();
    assert_eq!(f.change(&kept).unwrap(), want);
    f.check_objects().unwrap();
    drop(f);
    assert_eq!(holds_canary(&fdir), Vec::<String>::new(), "follower");
    assert!(!fdir.join(SPANMAP_FILE).exists(), "a follower never maps");

    // reopen, a read-only open, fsck and a repeat compact bring nothing back
    s.close().unwrap();
    let s = Store::open(&path).unwrap();
    assert_eq!(s.change(&kept).unwrap(), want);
    s.check_objects().unwrap();
    assert!(s.compact(false).is_ok());
    drop(s);
    let ro = Store::open_source_read_only(&path).unwrap();
    assert_eq!(ro.change(&kept).unwrap(), want);
    drop(ro);
    assert_eq!(holds_canary(&path), Vec::<String>::new(), "after reopen");
}

/// D-CRXNA: the first writer open of an old store backs it up before it maps its history; a
/// later removal of a mapped block erases that backup with the live copies.
fn removal_erases_the_upgrade_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let (secret, kept) = {
        let _legacy = format::legacy_writer_for_tests();
        let mut s = Store::create_legacy_for_tests(&path).unwrap();
        let (secret, _, kept) = secret_and_kept(&mut s);
        s.close().unwrap();
        (secret, kept)
    };
    let mut s = Store::open(&path).unwrap();
    assert!(s.has_feature_now(FEATURE_DEDUP_SPANS));
    assert!(map_of(&path).mappings().count() > 0, "the upgrade mapped");
    upgrade::publish_checked(&mut s).unwrap();
    assert!(!s.refresh_upgrade_pending().unwrap());
    let want = s.change(&kept).unwrap();
    let backups: Vec<String> = holds_canary(dir.path())
        .into_iter()
        .filter(|p| p.contains(upgrade::BACKUP_PREFIX))
        .collect();
    assert!(!backups.is_empty(), "the backup holds the text");
    remove_live(&mut s, &secret).unwrap();
    assert_eq!(s.change(&kept).unwrap(), want);
    s.check_objects().unwrap();
    drop(s);
    assert_eq!(
        holds_canary(dir.path()),
        Vec::<String>::new(),
        "no live, mapped or backup copy"
    );
}

/// A carried removal: a receiver loads a mapped source whose secret block and its CHANGE copy
/// a removal elsewhere dropped (`held: false`). The receiver holds zeros only, its compact maps
/// the kept pair and neither dropped frame, and nothing reads the text back.
fn removal_carried_to_a_receiver() {
    use crate::carried_fixture::{block_residue, change_residue, load, record, span};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let mut s = Store::create(&path).unwrap();
    let (secret, gone, kept) = secret_and_kept(&mut s);
    assert!(s.compact(false).unwrap().deduplicated_bytes >= 2 * 4096);
    let want = s.change(&kept).unwrap();
    let gone_change = s.change(&gone).unwrap();
    s.close().unwrap();
    let frames = framed_at(&path);
    let b = find(&frames, kinds::BLOCK, secret.version_hash());
    let c = find(&frames, kinds::CHANGE, gone);
    let residues = vec![block_residue(&secret), change_residue(&gone_change)];
    let mut spans = vec![
        span(&b, Some(&residues[0]), false),
        span(&c, Some(&residues[1]), false),
    ];
    spans.sort_by_key(|x| x.place);
    let rec = record(
        vec![rdy_model::redaction::RedactTarget::Version {
            id: secret.id.clone(),
            version: secret.version_hash(),
        }],
        spans,
    );
    let to = dir.path().join("to");
    load(&path, &to, &residues, &[rec]).unwrap();
    assert_eq!(holds_canary(&to), Vec::<String>::new(), "loaded");
    assert_eq!(map_of(&to).rows(), 0, "a load maps nothing");
    let r = Store::open(&to).unwrap();
    assert_eq!(r.change(&kept).unwrap(), want);
    r.check_objects().unwrap();
    assert!(r.compact(false).unwrap().deduplicated_bytes >= 4096);
    drop(r);
    let map = map_of(&to);
    assert!(map.mappings().count() > 0, "a local map");
    for m in map.mappings() {
        for place in [m.target.place, m.source.place] {
            assert!(
                place != b.loc && place != c.loc,
                "a dropped frame is mapped"
            );
        }
    }
    let r = Store::open(&to).unwrap();
    assert_eq!(r.change(&kept).unwrap(), want);
    assert!(r.get_block(&secret.version_hash()).unwrap().is_redacted());
    r.check_objects().unwrap();
    drop(r);
    assert_eq!(holds_canary(&to), Vec::<String>::new(), "after compact");
}

/// r5902: with the platform's own punch call, a compact keeps each segment's length and every
/// byte outside the mapped ranges, and the whole logical payload survives whether the platform
/// punched or not; an injected supported backend leaves a hole the map fills; unsupported and
/// failed calls keep the raw bytes and add their notes.
#[test]
fn r5902_native_punch_and_fallback() {
    // the platform call
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let (id, _) = one_big_change(&path);
    let c = find(&framed_at(&path), kinds::CHANGE, id);
    let seg = seg_path(&path.join("segments"), c.loc.segment);
    let raw_before = std::fs::read(&seg).unwrap();
    let s = Store::open(&path).unwrap();
    let want = s.change(&id).unwrap();
    let r = s.compact(false).unwrap();
    let unsupported = r.notes.iter().any(|n| n.starts_with(NOT_SUPPORTED));
    assert!(
        !r.notes.iter().any(|n| n.starts_with(FAILED)),
        "{:?}",
        r.notes
    );
    drop(s);
    let raw_after = std::fs::read(&seg).unwrap();
    assert_eq!(raw_after.len(), raw_before.len(), "the file length stays");
    let map = map_of(&path);
    let mut outside_before = raw_before.clone();
    let mut outside_after = raw_after.clone();
    let mut holes = 0;
    for m in map.mappings() {
        let a = m.target.physical_start() as usize;
        let b = a + m.target.len as usize;
        if raw_after[a..b].iter().all(|&x| x == 0) {
            holes += 1;
        } else {
            assert_eq!(raw_after[a..b], raw_before[a..b]);
        }
        outside_before[a..b].fill(0);
        outside_after[a..b].fill(0);
    }
    assert_eq!(
        outside_after, outside_before,
        "no byte outside a mapped range moves"
    );
    eprintln!(
        "native punch on this platform: {} ({holes} of {} mapped ranges read as holes)",
        if unsupported {
            "unsupported"
        } else {
            "supported"
        },
        map.mappings().count()
    );
    let s = Store::open(&path).unwrap();
    assert_eq!(s.change(&id).unwrap(), want);
    s.check_objects().unwrap();
    drop(s);

    // an injected supported backend: the test zeroes the range, as a punch does
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s");
    let (id, _) = one_big_change(&path);
    let seg = seg_path(&path.join("segments"), 0);
    let s = Store::open(&path).unwrap();
    let want = s.change(&id).unwrap();
    let mut file = std::fs::OpenOptions::new().write(true).open(&seg).unwrap();
    pio::on_punch(Some(Box::new(move |offset, len| {
        use std::io::{Seek, SeekFrom, Write};
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(&vec![0u8; len as usize]).unwrap();
        Some(Ok(Punch::Supported))
    })));
    let r = s.compact(false);
    clear_hooks();
    let r = r.unwrap();
    assert!(
        r.notes.iter().all(|n| !n.starts_with("hole punching")),
        "{:?}",
        r.notes
    );
    assert_eq!(s.change(&id).unwrap(), want, "the map fills the hole");
    drop(s);
    let s = Store::open(&path).unwrap();
    assert_eq!(s.change(&id).unwrap(), want);
    s.check_objects().unwrap();
}
