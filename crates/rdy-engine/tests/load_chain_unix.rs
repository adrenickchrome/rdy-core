#![cfg(unix)]
// Unix only: it moves a directory that holds an open file, which Windows refuses.
//! hc7: a chain load's failed sync of its segments directory restores the target. The
//! directory moves under the ingest's open log, so its sync fails: the error is the target's,
//! and `.rdy/store.loading`, with all it holds, goes. The other placement faults are in
//! `load_chain.rs`.
use rdy_engine::load::{LoadSummary, load_chain};
use rdy_engine::*;
use rdy_model::Role;
use rdy_store::segment::on_segment_write;
use serde_json::json;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn writer() -> Session {
    Session::human("human:t".into(), Role::Writer).unwrap()
}

fn approver() -> Session {
    Session::human("human:t".into(), Role::Approver).unwrap()
}

/// Writes one doc with a prose block to view `work`, then lands it.
fn add_and_land(r: &mut Rdy, n: usize) {
    r.write(
        &writer(),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(json!([
                {"op": "create", "type": "doc", "id": "$d", "fields": {"name": format!("d{n}"), "title": format!("D{n}"), "doc_type": "page"}},
                {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": format!("block {n}")}
            ]))
            .unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    let p = r.propose(&writer(), "work", "Work", None).unwrap();
    r.land(&approver(), &p.id).unwrap();
}

/// Every path under `dir` with its bytes (None for a directory).
fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(dir).unwrap().to_path_buf();
            if p.is_dir() {
                stack.push(p);
                out.insert(rel, None);
            } else {
                out.insert(rel, Some(std::fs::read(&p).unwrap()));
            }
        }
    }
    out
}

/// Runs `step` once, at the first segment write of this thread (the ingest has started, after
/// the pre-pass). Dropping the guard removes it.
struct OnFirstWrite;

impl Drop for OnFirstWrite {
    fn drop(&mut self) {
        on_segment_write(None);
    }
}

fn at_first_write(step: impl FnOnce() + 'static) -> OnFirstWrite {
    let step = RefCell::new(Some(step));
    on_segment_write(Some(Box::new(move |_: &[u8]| {
        if let Some(f) = step.borrow_mut().take() {
            f();
        }
        Ok(())
    })));
    OnFirstWrite
}

#[test]
fn hc7_failed_segments_sync_restores_the_target() {
    let _clock = pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut r = Rdy::init(&d.join("p"), "rdy-standard").unwrap();
    add_and_land(&mut r, 0);
    let f0 = d.join("f0.rdydump");
    let s0 = r.read().dump(&f0).unwrap();
    add_and_land(&mut r, 1);
    let i1 = d.join("i1.rdydump");
    r.read().dump_after(&s0.chain_end(), &i1).unwrap();
    r.close().unwrap();
    let files = vec![f0, i1];

    // a target that holds a file of its own
    let to = d.join("sync");
    std::fs::create_dir_all(&to).unwrap();
    std::fs::write(to.join("keep.txt"), b"mine").unwrap();
    let before = tree(&to);
    let loading = to.join(".rdy/store.loading");
    let at = loading.clone();
    let g = at_first_write(move || {
        std::fs::rename(at.join("segments"), at.join("moved")).unwrap();
    });
    let e = match load_chain(&files, &to, None) {
        Ok(s) => panic!("the load passed: {s:?}"),
        Err(e) => e,
    };
    drop(g);
    assert_eq!(e.code, E_IO, "{e:?}");
    assert!(
        e.message.starts_with(&loading.display().to_string()),
        "{e:?}"
    );
    assert_eq!(tree(&to), before);
    // the same chain then loads into the same target
    let s: LoadSummary = load_chain(&files, &to, None).unwrap();
    assert_eq!(s.chain.len(), 2);
}
