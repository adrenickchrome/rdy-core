#![cfg(unix)]
// Unix only: it makes symlinks. FW-XP1 Phase 4 ports it.
//! dl5: `rdy dump` writes the published log's sealed batches and the closed project files as one
//! stream (D-69AY3, D-D2NPN; F5 of D-FR0C4). Atomic at the target: a complete file at `out`, or
//! nothing.
use rdy_engine::{E_ALREADY_EXISTS, E_BAD_INPUT, E_IO, Engine, Local, Rdy, ReadState};
use rdy_qa::every_kind;
use rdy_qa::fixtures::{land, write, writer};
use rdy_qa::inbox::on;
use rdy_qa::packs::qa_project;
use rdy_store::dump::{DUMP_FORMAT, DumpBatch, DumpHeader, DumpItem, DumpReader};
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
    assert_eq!(format, DUMP_FORMAT);
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
        ".rdy/packs/my pack.yaml: rdy dump carries pack files named with A-Z a-z 0-9 . _ - only, and not starting with a dot"
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
        dumps.push((out, st));
    }
    writes.join().unwrap();

    let last = log_batches(&e.read());
    let mut lens = vec![];
    for (out, st) in &dumps {
        let (header, items) = read_back(out);
        let got = batches_of(&items);
        // the whole state that was dumped, no batch dropped at its end
        assert_eq!(got, log_batches(st), "{}", out.display());
        assert_eq!(header.log_end, st.seq(), "{}", out.display());
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
    drop(dumps);
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
    assert_eq!(summary.dump_format, 2);
    assert_eq!(summary.store_format, STORE_FORMAT);
}

/// The names in `dir` that hold `.partial-`.
fn partials(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".partial-"))
        .collect()
}

#[test]
fn dl5_a_dump_inside_the_project_rdy_directory_is_refused() {
    let (dir, r) = qa_project();
    let root = dir.path();
    for out in [
        root.join(".rdy/packs/backup.yaml"),
        root.join(".rdy/store/seg-000099.log"),
        root.join(".rdy/p.rdydump"),
        // the same directory by another spelling
        root.join(".rdy/packs/../store/p.rdydump"),
    ] {
        let e = r.read().dump(&out).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT, "{}", out.display());
        assert_eq!(
            e.message,
            format!("{} is inside the project .rdy directory", out.display())
        );
        assert_eq!(e.fix, "dump to a path outside .rdy");
        assert!(!out.exists(), "{}", out.display());
        let parent = out.parent().unwrap();
        assert_eq!(partials(parent), Vec::<String>::new(), "{}", out.display());
    }
    // the project still opens
    drop(r);
    Rdy::open(root).unwrap();
}

#[test]
fn dl5_a_dump_through_a_symlink_into_rdy_is_refused() {
    let (dir, r) = qa_project();
    let tmp = tempfile::tempdir().unwrap();
    let link = tmp.path().join("packs");
    std::os::unix::fs::symlink(dir.path().join(".rdy/packs"), &link).unwrap();
    let out = link.join("backup.yaml");
    let e = r.read().dump(&out).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(e.fix, "dump to a path outside .rdy");
    assert!(!dir.path().join(".rdy/packs/backup.yaml").exists());
}

#[test]
fn dl5_a_project_file_over_16_mib_is_refused_and_no_partial_file_is_left() {
    let (dir, r) = qa_project();
    std::fs::write(dir.path().join(".rdyignore"), vec![b'#'; (16 << 20) + 1]).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("p.rdydump");
    let e = r.read().dump(&out).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(e.message, ".rdyignore is larger than 16 MiB");
    assert_eq!(e.fix, "make the file 16 MiB or smaller");
    assert!(!out.exists());
    assert_eq!(partials(tmp.path()), Vec::<String>::new());

    // exactly 16 MiB is carried
    std::fs::write(dir.path().join(".rdyignore"), vec![b'#'; 16 << 20]).unwrap();
    r.read().dump(&out).unwrap();
}

#[test]
fn dl5_more_than_256_project_files_are_refused() {
    let (dir, r) = qa_project();
    let root = dir.path();
    std::fs::write(root.join(".rdyignore"), "# none\n").unwrap();
    // .rdy.yaml, .rdyignore, rdy-qa.yaml and 254 more: 257
    for i in 0..254 {
        std::fs::write(root.join(format!(".rdy/packs/p{i:03}.yaml")), "x: 1\n").unwrap();
    }
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("p.rdydump");
    let e = r.read().dump(&out).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(
        e.message,
        "the project has 257 files to dump, and rdy dump carries at most 256"
    );
    assert_eq!(e.fix, "keep at most 254 pack files");
    assert!(!out.exists());
    assert_eq!(partials(tmp.path()), Vec::<String>::new());

    // 256 are carried
    std::fs::remove_file(root.join(".rdy/packs/p000.yaml")).unwrap();
    assert_eq!(r.read().dump(&out).unwrap().files.len(), 256);
}

#[test]
fn dl5_a_hidden_pack_file_is_refused_with_the_dot_rule() {
    let (dir, r) = qa_project();
    std::fs::write(dir.path().join(".rdy/packs/.draft.yaml"), "name: x\n").unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let e = r.read().dump(&tmp.path().join("p.rdydump")).unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(
        e.message,
        ".rdy/packs/.draft.yaml: rdy dump carries pack files named with A-Z a-z 0-9 . _ - only, and not starting with a dot"
    );
    assert_eq!(e.fix, "rename the pack file");
}

#[test]
fn dl5_a_partial_file_of_another_dump_is_named_and_kept() {
    let (_dir, r) = qa_project();
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("p.rdydump");
    let partial = tmp
        .path()
        .join(format!("p.rdydump.partial-{}", std::process::id()));
    std::fs::write(&partial, "another dump\n").unwrap();
    let e = r.read().dump(&out).unwrap_err();
    assert_eq!(e.code, E_ALREADY_EXISTS);
    assert_eq!(
        e.message,
        format!(
            "{} exists: another dump may be writing it",
            partial.display()
        )
    );
    assert_eq!(e.fix, "remove it if no dump is running, then retry");
    assert_eq!(std::fs::read(&partial).unwrap(), b"another dump\n");
    assert!(!out.exists());
}

#[test]
fn dl5_io_errors_name_the_path() {
    let (dir, r) = qa_project();
    let tmp = tempfile::tempdir().unwrap();

    // out's parent is a file
    let file = tmp.path().join("notes.txt");
    std::fs::write(&file, "mine\n").unwrap();
    let out = file.join("p.rdydump");
    let e = r.read().dump(&out).unwrap_err();
    assert_eq!(e.code, E_IO);
    let partial = format!("{}.partial-{}", out.display(), std::process::id());
    assert!(e.message.starts_with(&format!("{partial}: ")), "{e:?}");
    assert_eq!(e.fix, "check file permissions and disk space");

    // the project file is gone
    let config = dir.path().join(".rdy.yaml");
    std::fs::remove_file(&config).unwrap();
    let e = r.read().dump(&tmp.path().join("p.rdydump")).unwrap_err();
    assert_eq!(e.code, E_IO);
    assert!(
        e.message.starts_with(&format!("{}: ", config.display())),
        "{e:?}"
    );
    assert_eq!(e.fix, "check file permissions and disk space");
    assert_eq!(partials(tmp.path()), Vec::<String>::new());
}
