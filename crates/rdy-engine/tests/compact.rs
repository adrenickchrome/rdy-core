//! mh4: compact through the engine (D-4YRQ4: "Compaction never moves a frame, never changes a byte
//! that rdy reads and never changes a hash."; "A dump of a compacted store equals a dump of the
//! same store before."). A dry run changes nothing, a real run seals nothing and publishes
//! nothing, and a report over its bound refuses before the first punch.
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Materialized, Rdy, RdyError, Scope, Session, SessionKind, WriteRequest};
use rdy_model::{Block, Hash, Provenance, Role};
use rdy_store::LandRequest;
use rdy_store::compact::{CensusSeed, on_census};
use rdy_store::inbox::{InboxCursor, InboxSub};
use rdy_store::pio::{self, Punch};
use rdy_store::segment::{RemovalStep, on_removal_step};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

fn write(r: &mut Rdy, ops: Value) -> rdy_engine::WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "mh4".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy, title: &str) {
    let p = r
        .propose(&session(Role::Writer), "work", title, Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
}

/// A landed project with a removed prose block, with no open batch.
fn removed_project(dir: &Path) -> Rdy {
    std::fs::create_dir_all(dir).unwrap();
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": "The launch code is ZEBRA-7731."},
            {"op": "create", "type": "prose", "id": "$k", "parent": "$d", "body": "This stays."}
        ]),
    );
    land(&mut r, "first");
    let s = session(Role::Writer);
    r.redact(
        &s,
        RedactInput {
            targets: vec![w.created["$p"].clone()],
            reason_class: "secret".into(),
            reason: "a leaked code".into(),
            dry_run: false,
        },
    )
    .unwrap();
    let p = r.propose(&s, "work", "Remove the code", None).unwrap();
    let version: Hash = p.id.parse().unwrap();
    r.seal_open().unwrap();
    r.store_mut().set_held(false).unwrap();
    r.store_mut()
        .land_atomic(LandRequest {
            proposal: version,
            accept: None,
            approver: Provenance {
                actor: "human:t".into(),
                run: None,
                role: Role::Approver,
                at_ms: 1_000_000,
            },
            alt: None,
            relay: None,
            evidence: None,
            assessment: None,
        })
        .unwrap();
    r.store_mut().set_held(true).unwrap();
    r.invalidate();
    r
}

fn blocks(m: &Materialized) -> Vec<Block> {
    m.iter().cloned().collect()
}

/// Every file and directory under `dir`: permissions, length, modified time and bytes. No access time.
fn tree(
    dir: &Path,
) -> BTreeMap<
    PathBuf,
    (
        std::fs::Permissions,
        u64,
        std::time::SystemTime,
        Option<Vec<u8>>,
    ),
> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let m = std::fs::symlink_metadata(&p).unwrap();
        let bytes = m.is_file().then(|| std::fs::read(&p).unwrap());
        if m.is_dir() {
            out.extend(tree(&p));
        }
        out.insert(p, (m.permissions(), m.len(), m.modified().unwrap(), bytes));
    }
    out
}

fn segments(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out: Vec<_> = std::fs::read_dir(root.join(".rdy/store/segments"))
        .unwrap()
        .map(|e| {
            let p = e.unwrap().path();
            let b = std::fs::read(&p).unwrap();
            (p, b)
        })
        .collect();
    out.sort();
    out
}

fn allocated(root: &Path) -> u64 {
    segments(root)
        .iter()
        .map(|(p, _)| pio::allocated_bytes(&std::fs::File::open(p).unwrap()).unwrap())
        .sum()
}

/// Removes the hooks when a test ends, also when it panics.
struct Hooks;
impl Drop for Hooks {
    fn drop(&mut self) {
        pio::on_punch(None);
        on_removal_step(None);
        on_census(None);
    }
}

/// Counts every punch, with the given answer.
fn count_punches(answer: Punch) -> (Rc<Cell<u32>>, Hooks) {
    let n: Rc<Cell<u32>> = Rc::default();
    let c = Rc::clone(&n);
    pio::on_punch(Some(Box::new(move |_, _| {
        c.set(c.get() + 1);
        Some(Ok(answer))
    })));
    (n, Hooks)
}

#[test]
fn mh4_dry_run_has_no_side_effect() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("p");
    let r = removed_project(&root);
    let (punches, hooks) = count_punches(Punch::Supported);
    let steps: Rc<RefCell<Vec<RemovalStep>>> = Rc::default();
    let seen = Rc::clone(&steps);
    on_removal_step(Some(Box::new(move |s| {
        seen.borrow_mut().push(s);
        Ok(())
    })));
    let before = tree(&root);
    let state = r.read();
    let end = r.store().log_end();
    let epoch = r.store().redaction_epoch();
    let want = r.log_bytes().unwrap();

    let report = r.compact(true).unwrap();

    assert_eq!(tree(&root), before, "the project's files are as they were");
    assert_eq!(punches.get(), 0, "no punch");
    assert!(steps.borrow().is_empty(), "no zero, sync or removal step");
    assert!(Arc::ptr_eq(&r.read(), &state), "no publish");
    assert_eq!(r.store().log_end(), end, "no frame");
    assert_eq!(r.store().redaction_epoch(), epoch, "no epoch");
    assert_eq!(report.punched_bytes, 0);
    assert_eq!(report.on_disk_bytes, allocated(&root));
    assert_eq!(report.notes, Vec::<String>::new());
    assert_eq!(report.log_bytes, want.log_bytes);
    assert_eq!(report.removed_bytes, want.removed_bytes);
    assert_eq!(report.by_kind, want.by_kind);
    assert!(report.removed_bytes > 0);
    drop(hooks);
}

#[test]
fn mh4_compact_preserves_dump_positions_and_readers() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("p");
    let mut r = removed_project(&root);
    let sub = InboxSub {
        actor: "agent:sub".into(),
        run: Some("s1".into()),
        cursor: InboxCursor {
            at: r.store().log_end(),
            skip: 0,
        },
        areas: vec![],
        role: "reader".into(),
    };
    r.store_mut().put_inbox(&sub).unwrap();
    r.seal_open().unwrap();
    r.invalidate();

    let d1 = dir.path().join("before.rdydump");
    let first = r.read().dump(&d1).unwrap();
    let bytes = segments(&root);
    let captured = r.read();
    let canon = blocks(&captured.materialized(&Scope::Canon).unwrap());
    let end = r.store().log_end();
    let epoch = r.store().redaction_epoch();

    // the real file system's punch runs here
    let report = r.compact(false).unwrap();
    assert!(report.removed_bytes > 0);

    // no frame moved and no byte that rdy reads changed: headers, markers and positions included
    assert_eq!(segments(&root), bytes);
    assert_eq!(r.store().log_end(), end);
    assert_eq!(
        r.store().redaction_epoch(),
        epoch,
        "punching alone is no epoch"
    );
    assert!(Arc::ptr_eq(&r.read(), &captured), "no publish");
    assert_eq!(r.store().inbox("agent:sub", Some("s1")).unwrap(), Some(sub));
    // a captured read stays usable
    assert_eq!(
        blocks(&captured.materialized(&Scope::Canon).unwrap()),
        canon
    );
    r.fsck().unwrap();

    // a full dump is byte for byte the dump before
    let d2 = dir.path().join("after.rdydump");
    let second = r.read().dump(&d2).unwrap();
    assert_eq!(std::fs::read(&d1).unwrap(), std::fs::read(&d2).unwrap());
    assert_eq!(first.chain_end(), second.chain_end());

    // the earlier dump's end is still an increment's base, and the chain loads
    let nothing = dir.path().join("nothing.rdydump");
    assert!(
        r.read()
            .dump_after(&first.chain_end(), &nothing)
            .unwrap()
            .out
            .is_none()
    );
    write(
        &mut r,
        json!([{"op": "create", "type": "doc", "id": "$d", "fields": {"name": "next", "title": "Next", "doc_type": "page"}}]),
    );
    land(&mut r, "after compact");
    let inc = dir.path().join("inc.rdydump");
    r.read().dump_after(&first.chain_end(), &inc).unwrap();
    let want = blocks(&r.read().materialized(&Scope::Canon).unwrap());
    let to = dir.path().join("loaded");
    rdy_engine::load::load_chain(&[d1, inc], &to, None).unwrap();
    let loaded = Rdy::open(&to).unwrap();
    assert_eq!(
        blocks(&loaded.read().materialized(&Scope::Canon).unwrap()),
        want
    );
}

/// The frames of the project's segments, markers included, by a walk of their headers.
fn frame_count(root: &Path) -> u64 {
    let mut n = 0;
    for (_, bytes) in segments(root) {
        let mut at = 0usize;
        while at < bytes.len() {
            let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            at += 37 + len;
            n += 1;
        }
    }
    n
}

fn triple(e: RdyError) -> (String, String, String) {
    (e.code.to_string(), e.message, e.fix)
}

#[test]
fn mh4_reports_refuse_bounds_before_punching() {
    const BUG: &str = "report this as a bug in rdy";
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("p");
    let r = removed_project(&root);
    let segs = segments(&root).len() as u64;
    let frames = frame_count(&root);
    let spans = r.store().verify_report().unwrap().redaction.unwrap().frames;
    let bytes = r.log_bytes().unwrap().log_bytes;
    assert!(spans > 0);

    let cases: [(&str, CensusSeed, String); 4] = [
        (
            "segments",
            CensusSeed {
                segments: 65_536 - segs,
                ..CensusSeed::default()
            },
            "the log byte report exceeds 65536 segments".to_string(),
        ),
        (
            "frames",
            CensusSeed {
                frames: 100_000_000 - frames,
                ..CensusSeed::default()
            },
            "the log byte report exceeds 100000000 frames".to_string(),
        ),
        (
            "removed spans",
            CensusSeed {
                spans: 1_000_000 - spans,
                ..CensusSeed::default()
            },
            "the log byte report exceeds 1000000 removed spans".to_string(),
        ),
        (
            "bytes",
            CensusSeed {
                bytes: u64::MAX - bytes,
                ..CensusSeed::default()
            },
            "the log byte count exceeds 18446744073709551615 bytes".to_string(),
        ),
    ];
    for (name, at_limit, message) in cases {
        // at the limit: the report and the punches go ahead
        let (punches, hooks) = count_punches(Punch::Supported);
        on_census(Some(at_limit));
        r.log_bytes().unwrap_or_else(|e| panic!("{name}: {e}"));
        r.compact(true).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(punches.get(), 0, "{name}");
        r.compact(false).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(u64::from(punches.get()), spans, "{name}");
        drop(hooks);

        // one past it: the exact refusal, for every call, with no punch
        let (punches, hooks) = count_punches(Punch::Supported);
        let past = CensusSeed {
            segments: at_limit.segments + u64::from(name == "segments"),
            frames: at_limit.frames + u64::from(name == "frames"),
            spans: at_limit.spans + u64::from(name == "removed spans"),
            bytes: at_limit.bytes.saturating_add(u64::from(name == "bytes")),
        };
        on_census(Some(past));
        let want = ("E_LIMIT".to_string(), message.clone(), BUG.to_string());
        assert_eq!(triple(r.log_bytes().unwrap_err()), want, "{name}");
        assert_eq!(triple(r.compact(true).unwrap_err()), want, "{name}");
        assert_eq!(triple(r.compact(false).unwrap_err()), want, "{name}");
        assert_eq!(punches.get(), 0, "{name}");
        drop(hooks);
    }
}
