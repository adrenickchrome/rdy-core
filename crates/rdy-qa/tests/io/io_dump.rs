//! dl5: `rdy dump` writes the published log's sealed batches and the closed project files as one
//! stream (D-69AY3, D-D2NPN; F5 of D-FR0C4). Atomic at the target: a complete file at `out`, or
//! nothing.
use rdy_engine::{E_ALREADY_EXISTS, E_BAD_INPUT, Engine, Local, Rdy, ReadState};
use rdy_qa::every_kind;
use rdy_qa::fixtures::{land, write, writer};
use rdy_qa::inbox::on;
use rdy_qa::packs::qa_project;
use rdy_store::dump::{DumpBatch, DumpHeader, DumpItem, DumpReader};
use rdy_store::format::STORE_FORMAT;
use rdy_store::kinds::{self, BatchCommit};
use rdy_store::segment::Location;
use serde_json::json;
use std::path::Path;
use std::sync::{Arc, mpsc};

const ORIGIN: Location = Location {
    segment: 0,
    offset: 0,
};

/// Reads the dump at `path` back to its end (every record verified): its header and items.
fn read_back(path: &Path) -> (DumpHeader, Vec<DumpItem>) {
    let f = std::io::BufReader::new(std::fs::File::open(path).unwrap());
    let (mut r, format, header) = DumpReader::open(f).unwrap();
    assert_eq!(format, 1);
    let mut items = vec![];
    while let Some(item) = r.next().unwrap() {
        items.push(item);
    }
    (header, items)
}

fn batches_of(items: &[DumpItem]) -> Vec<DumpBatch> {
    items
        .iter()
        .filter_map(|i| match i {
            DumpItem::Batch(b) => Some(b.clone()),
            DumpItem::File { .. } => None,
        })
        .collect()
}

fn files_of(items: &[DumpItem]) -> Vec<(String, Vec<u8>)> {
    items
        .iter()
        .filter_map(|i| match i {
            DumpItem::File { path, bytes } => Some((path.clone(), bytes.clone())),
            DumpItem::Batch(_) => None,
        })
        .collect()
}

/// Every sealed batch of `st`'s published log, through `for_each_batch` over `seq()`.
fn log_batches(st: &ReadState) -> Vec<DumpBatch> {
    let mut out = vec![];
    st.reader()
        .for_each_batch(st.seq(), |b| {
            out.push(b);
            Ok(())
        })
        .unwrap();
    out
}

#[test]
fn dl5_a_dump_holds_every_sealed_batch_of_the_published_state() {
    let dir = every_kind::project();
    let r = Rdy::open(dir.path()).unwrap();
    let st = r.read();
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("p.rdydump");
    let summary = st.dump(&out).unwrap();

    let want = log_batches(&st);
    let (header, items) = read_back(&out);
    let got = batches_of(&items);
    assert_eq!(got.len() as u64, summary.batches);
    assert_eq!(got.len(), want.len());
    let frames: usize = want.iter().map(|b| b.frames.len()).sum();
    assert_eq!(summary.frames, frames as u64);
    assert_eq!(
        got.iter().map(|b| b.frames.len()).sum::<usize>(),
        frames,
        "the dump holds every frame"
    );
    assert_eq!(got, want, "every batch, byte for byte");

    // the last batch ends at the header's log_end, which is seq()
    assert_eq!(header.log_end, st.seq());
    let mut last = None;
    st.reader()
        .for_each_frame_from(ORIGIN, st.seq(), |_, kind, _, payload, next| {
            last = Some((kind, payload.to_vec(), next));
            Ok(())
        })
        .unwrap();
    let (kind, payload, next) = last.expect("a log with frames");
    assert_eq!(kind, kinds::BATCH_COMMIT, "the log ends at a batch marker");
    assert_eq!(next, header.log_end);
    let marker = BatchCommit::decode(&payload).unwrap();
    assert_eq!(marker.start, got.last().unwrap().start);
    assert_eq!(marker.digest, got.last().unwrap().digest);

    assert_eq!(summary.bytes, std::fs::metadata(&out).unwrap().len());
    assert_eq!(summary.out, out.display().to_string());
}

#[test]
fn dl5_a_dump_carries_only_the_closed_project_files() {
    let dir = every_kind::project();
    let root = dir.path();
    // local state that never travels (F5): the project's own local.yaml, a run dir, a working
    // copy's and a worktree's state, and a file in the packs directory that is no pack
    assert!(root.join(".rdy/local.yaml").is_file());
    for d in [".rdy/run", ".rdy/wc", ".rdy/worktrees"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
        std::fs::write(root.join(d).join("x.yaml"), "local: true\n").unwrap();
    }
    std::fs::write(root.join(".rdy/packs/notes.txt"), "not a pack\n").unwrap();
    let r = Rdy::open(root).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("p.rdydump");
    let summary = r.read().dump(&out).unwrap();

    let carried = [".rdy.yaml", ".rdy/packs/rdy-qa.yaml", ".rdyignore"];
    assert_eq!(summary.files, carried);
    let (_, items) = read_back(&out);
    let files = files_of(&items);
    let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(paths, carried);
    for (path, bytes) in &files {
        assert_eq!(bytes, &std::fs::read(root.join(path)).unwrap(), "{path}");
        for local in ["local.yaml", "run", "wc", "worktrees", "notes.txt"] {
            assert!(
                !path.split('/').any(|seg| seg == local),
                "{path} is local state"
            );
        }
    }
}

#[test]
fn dl5_a_dump_never_overwrites_a_file() {
    let (_dir, r) = qa_project();
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("p.rdydump");
    r.read().dump(&out).unwrap();
    let first = std::fs::read(&out).unwrap();

    let e = r.read().dump(&out).unwrap_err();
    assert_eq!(e.code, E_ALREADY_EXISTS);
    assert_eq!(e.message, format!("{} exists", out.display()));
    assert_eq!(
        e.fix,
        "pick a new file name; rdy dump never overwrites a file"
    );
    assert_eq!(
        std::fs::read(&out).unwrap(),
        first,
        "the first dump is unchanged"
    );

    // a file that is no dump is never overwritten either
    let other = tmp.path().join("notes.txt");
    std::fs::write(&other, "mine\n").unwrap();
    let e = r.read().dump(&other).unwrap_err();
    assert_eq!(e.code, E_ALREADY_EXISTS);
    assert_eq!(std::fs::read(&other).unwrap(), b"mine\n");

    let names: Vec<String> = std::fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|n| !n.contains(".partial-")),
        "no partial file is left: {names:?}"
    );
}

#[test]
fn dl5_a_dump_into_a_missing_directory_is_refused() {
    let (_dir, r) = qa_project();
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("nope");
    let e = r.read().dump(&missing.join("p.rdydump")).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(e.message, format!("{} does not exist", missing.display()));
    assert_eq!(e.fix, "create the directory first, or pick another path");
    assert!(!missing.exists(), "the dump creates no directory");
}

#[test]
fn dl5_a_pack_file_named_outside_the_set_is_refused() {
    let (dir, r) = qa_project();
    std::fs::write(dir.path().join(".rdy/packs/my pack.yaml"), "name: x\n").unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("p.rdydump");
    let e = r.read().dump(&out).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(
        e.message,
        ".rdy/packs/my pack.yaml: rdy dump carries pack files named with A-Z a-z 0-9 . _ - only"
    );
    assert_eq!(e.fix, "rename the pack file");
    assert_eq!(
        std::fs::read_dir(tmp.path()).unwrap().count(),
        0,
        "nothing is written"
    );
}

#[test]
fn dl5_two_dumps_of_one_state_are_the_same_bytes() {
    let dir = every_kind::project();
    let r = Rdy::open(dir.path()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, c) = (
        tmp.path().join("a.rdydump"),
        tmp.path().join("b.rdydump"),
        tmp.path().join("c.rdydump"),
    );
    let st = r.read();
    st.dump(&a).unwrap();
    st.dump(&b).unwrap();
    // a later read of the idle engine is the same state
    r.read().dump(&c).unwrap();
    let a = std::fs::read(a).unwrap();
    assert_eq!(a, std::fs::read(b).unwrap());
    assert_eq!(a, std::fs::read(c).unwrap());
}

#[test]
fn dl5_a_dump_under_concurrent_writes_is_a_whole_prefix() {
    let (dir, r) = qa_project();
    let e = Arc::new(Local::new(r));
    let doc = on(&e, |r| {
        r.new_doc(&writer(), "work", "busy", Some("overview"), Some("Busy"))
            .unwrap();
        let doc = r
            .read()
            .resolve_path(&rdy_engine::Scope::View("work".into()), "busy")
            .unwrap();
        land(r, "work").unwrap();
        doc.to_string()
    });
    // the writer stops at three points until the main thread has taken its state, then writes
    // on while that state is dumped: each dump is of a state strictly before the last write
    let (at_tx, at_rx) = mpsc::channel::<()>();
    let (taken_tx, taken_rx) = mpsc::channel::<()>();
    let w = Arc::clone(&e);
    let writes = std::thread::spawn(move || {
        for i in 0..50 {
            let doc = doc.clone();
            on(&w, move |r| {
                write(
                    r,
                    "work",
                    json!([{"op": "create", "type": "claim", "parent": doc,
                            "body": format!("Claim {i} holds.")}]),
                )
                .unwrap();
            });
            if [10, 25, 40].contains(&i) {
                at_tx.send(()).unwrap();
                taken_rx.recv().unwrap();
            }
        }
    });
    let tmp = tempfile::tempdir().unwrap();
    let mut dumps = vec![];
    for n in 0..3 {
        at_rx.recv().unwrap();
        let st = e.read();
        taken_tx.send(()).unwrap();
        let out = tmp.path().join(format!("{n}.rdydump"));
        st.dump(&out).unwrap();
        dumps.push(out);
    }
    writes.join().unwrap();

    let last = log_batches(&e.read());
    let mut lens = vec![];
    for out in &dumps {
        let (header, items) = read_back(out);
        let got = batches_of(&items);
        assert!(header.log_end < e.read().seq(), "{}", out.display());
        assert!(got.len() < last.len(), "{}", out.display());
        for (g, l) in got.iter().zip(&last) {
            assert_eq!(
                (g.start, g.digest),
                (l.start, l.digest),
                "{}",
                out.display()
            );
        }
        lens.push(got.len());
    }
    assert!(lens.is_sorted() && lens[0] < lens[2], "{lens:?}");
    let e = Arc::into_inner(e).expect("the writer thread is done");
    e.close().unwrap();
    drop(dir);
}

#[test]
fn dl5_the_header_names_the_rdy_that_wrote_it() {
    let (_dir, r) = qa_project();
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("p.rdydump");
    let summary = r.read().dump(&out).unwrap();
    let (header, _) = read_back(&out);
    assert_eq!(header.rdy, env!("CARGO_PKG_VERSION"));
    assert_eq!(header.store_format, STORE_FORMAT);
    assert_eq!(summary.dump_format, 1);
    assert_eq!(summary.store_format, STORE_FORMAT);
}
