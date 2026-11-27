//! r5902 (D-1ER84): the first writer open of a project or holder that predates span maps runs
//! the optional `dedup-spans-1` step through the engine's open: one backed-up upgrade, its map
//! built through the source end before `FORMAT` is published, and no historical scan on any
//! later open. A dry-run plan names the step and creates no map.
use rdy_engine::*;
use rdy_model::*;
use rdy_store::dedup::{self, MapEvent, MapOpen, SPANMAP_FILE, SpanMap};
use rdy_store::format::FEATURE_DEDUP_SPANS;
use rdy_store::upgrade::{self, DEDUP_SPANS_STEP, SNAPSHOT_DELTA_STEP};
use serde_json::json;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

fn agent() -> Session {
    Session {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
        kind: rdy_engine::SessionKind::Human,
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

fn backups(parent: &Path) -> usize {
    std::fs::read_dir(parent)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with(upgrade::BACKUP_PREFIX))
        .count()
}

/// Records the scans and the first backup count seen at a scan, on this thread.
fn watch(parent: &Path) -> (Rc<RefCell<usize>>, Rc<RefCell<Option<usize>>>) {
    let scans = Rc::new(RefCell::new(0));
    let seen = Rc::new(RefCell::new(None));
    let (s, b, p) = (scans.clone(), seen.clone(), parent.to_path_buf());
    dedup::on_map_event(Some(Box::new(move |e: &MapEvent| {
        if *e == MapEvent::Scan {
            *s.borrow_mut() += 1;
            if b.borrow().is_none() {
                *b.borrow_mut() = Some(backups(&p));
            }
        }
        Ok(())
    })));
    (scans, seen)
}

/// r5902: a project written by the last rdy before span maps opens with this rdy's writer: the
/// backup is complete before the first scan, the map reaches the source end and maps the old
/// CHANGE/BLOCK pair, `FORMAT` lists the feature after the engine's checks, every old id reads
/// as before, and a second open scans nothing. A holder upgrades the same way, an empty one
/// to an empty checked map, and a dry-run plan creates no map.
#[test]
fn r5902_first_open_maps_once_after_backup() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("p");
    let (id, body) = {
        let _legacy = rdy_store::format::legacy_writer_for_tests();
        let mut r = Rdy::init(&root, "rdy-standard").unwrap();
        let body = text(40_000, 3);
        let w = r
            .write(
                &agent(),
                WriteRequest {
                    view: "work".into(),
                    ops: serde_json::from_value(json!([
                        {"op": "create", "type": "doc", "id": "$d",
                         "fields": {"title": "Spans", "doc_type": "page"}},
                        {"op": "create", "type": "prose", "id": "$p", "parent": "$d",
                         "body": body}
                    ]))
                    .unwrap(),
                    justification: None,
                    message: String::new(),
                },
            )
            .unwrap();
        let id = w
            .created
            .iter()
            .find(|(k, _)| *k == "$p")
            .map(|(_, v)| BlockId::parse(v).unwrap())
            .unwrap();
        r.close().unwrap();
        (id, body)
    };
    let store = root.join(".rdy/store");
    assert!(
        !store.join(SPANMAP_FILE).exists(),
        "an old store has no map"
    );

    // the dry run plans both optional steps and creates no map
    let plan = upgrade::inspect(&store).unwrap();
    assert_eq!(plan.steps, [SNAPSHOT_DELTA_STEP, DEDUP_SPANS_STEP]);
    assert!(!store.join(SPANMAP_FILE).exists());

    let parent = store.parent().unwrap().to_path_buf();
    let (scans, seen) = watch(&parent);
    let opened = Rdy::open(&root);
    dedup::on_map_event(None);
    let r = opened.unwrap();
    assert_eq!(
        *seen.borrow(),
        Some(1),
        "the backup precedes the first scan"
    );
    assert_eq!(*scans.borrow(), 1, "one historical scan");
    assert!(r.store().has_feature_now(FEATURE_DEDUP_SPANS));
    let map = SpanMap::load(&store, MapOpen::ReadOnly).unwrap();
    assert!(map.through().is_some(), "the map reaches the source end");
    assert!(map.mappings().count() > 0, "the old CHANGE copy is mapped");
    assert_eq!(
        r.read()
            .block(&Scope::View("work".into()), &id)
            .unwrap()
            .body,
        body
    );
    r.close().unwrap();
    let format = std::fs::read_to_string(store.join("FORMAT")).unwrap();
    assert!(format.contains(FEATURE_DEDUP_SPANS) || store.join("FEATURES").exists());
    assert!(upgrade::inspect(&store).unwrap().steps.is_empty());

    // a second open scans nothing and reads the same bytes
    let (scans, _) = watch(&parent);
    let r = Rdy::open(&root).unwrap();
    dedup::on_map_event(None);
    assert_eq!(*scans.borrow(), 0);
    assert_eq!(
        r.read()
            .block(&Scope::View("work".into()), &id)
            .unwrap()
            .body,
        body
    );
    r.fsck().unwrap();
    r.close().unwrap();
    assert_eq!(backups(&parent), 1, "one backup");

    // an empty old holder opens to an empty checked map with its Through row
    let holder = base.path().join("h");
    {
        let _legacy = rdy_store::format::legacy_writer_for_tests();
        Rdy::create_holder(&holder, RepoId([7; 16]))
            .unwrap()
            .close()
            .unwrap();
    }
    let hstore = holder.join(".rdy/store");
    assert!(!hstore.join(SPANMAP_FILE).exists());
    let h = Rdy::open_holder(&holder).unwrap();
    assert!(h.store().has_feature_now(FEATURE_DEDUP_SPANS));
    h.close().unwrap();
    let map = SpanMap::load(&hstore, MapOpen::ReadOnly).unwrap();
    assert_eq!(map.mappings().count(), 0);
    assert!(map.through().is_some(), "one Through row");
    let (scans, _) = watch(&holder.join(".rdy"));
    Rdy::open_holder(&holder).unwrap().close().unwrap();
    dedup::on_map_event(None);
    assert_eq!(*scans.borrow(), 0);
}
