//! dl6: rdy load rebuilds a project from a dump, byte for byte, then builds the index and runs
//! fsck; a refused or failed load leaves the target as it was.
use rdy_engine::{Rdy, load::load};
use rdy_qa::every_kind;
use rdy_qa::reindex_check::{compare, derived};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Every path under `dir` with its bytes (None for a directory).
fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    if !dir.exists() {
        return out;
    }
    for e in walk(dir) {
        let rel = e.strip_prefix(dir).unwrap().to_path_buf();
        out.insert(
            rel,
            if e.is_dir() {
                None
            } else {
                Some(std::fs::read(&e).unwrap())
            },
        );
    }
    out
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut v = vec![];
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            v.extend(walk(&p));
        }
        v.push(p);
    }
    v
}

/// The every-kind project, a dump of it, and a scratch directory.
fn dumped() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let src = every_kind::project();
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join("p.rdydump");
    let r = Rdy::open(src.path()).unwrap();
    r.read().dump(&file).unwrap();
    r.close().unwrap();
    (src, scratch, file)
}

fn segs(p: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    tree(&p.join(".rdy/store/segments"))
}

#[test]
fn dl6_a_load_reproduces_the_log_byte_for_byte() {
    let (src, scratch, file) = dumped();
    let to = scratch.path().join("new");
    let s = load(&file, &to).unwrap();
    assert_eq!(segs(&to), segs(src.path()));
    assert_eq!(
        std::fs::read(to.join(".rdy/store/FORMAT")).unwrap(),
        std::fs::read(src.path().join(".rdy/store/FORMAT")).unwrap()
    );
    assert_eq!(
        s.files_written,
        [".rdy.yaml", ".rdy/packs/rdy-qa.yaml", ".rdyignore"]
    );
    assert!(
        !to.join(".rdy/local.yaml").exists(),
        "the local file never travels"
    );
    assert_eq!(
        s.next,
        "run rdy checkout to bind a working copy; a dump carries none"
    );
}

#[test]
fn dl6_a_load_keeps_every_ref_and_the_derived_state() {
    let (src, scratch, file) = dumped();
    let to = scratch.path().join("new");
    load(&file, &to).unwrap();
    let a = Rdy::open(src.path()).unwrap();
    let b = Rdy::open(&to).unwrap();
    assert_eq!(
        every_kind::facts(&b),
        every_kind::facts(&a),
        "refs, objects, landings, fsck"
    );
    let v = compare(&derived(&a).unwrap(), &derived(&b).unwrap());
    assert!(
        v.is_empty(),
        "tensions, conflicts, inboxes, moves, events: {v:#?}"
    );
    a.close().unwrap();
    b.close().unwrap();
}

#[test]
fn dl6_the_loaded_index_is_a_reindex() {
    let (_src, scratch, file) = dumped();
    let to = scratch.path().join("new");
    load(&file, &to).unwrap();
    let mut r = Rdy::open(&to).unwrap();
    let before = r.store().index_dump_for_tests().unwrap();
    r.reindex().unwrap();
    assert_eq!(r.store().index_dump_for_tests().unwrap(), before);
    r.close().unwrap();
}

#[test]
fn dl6_a_load_runs_fsck_and_reports_it() {
    let (_src, scratch, file) = dumped();
    let s = load(&file, &scratch.path().join("new")).unwrap();
    assert_eq!(s.fsck.unlogged_moves, 0);
    assert!(s.landings >= 3 && s.tags >= 1 && s.views >= 3, "{s:?}");
    assert_eq!(
        (s.dump_format, s.store_format),
        (1, rdy_store::format::STORE_FORMAT)
    );
}

#[test]
fn dl6_the_loaded_project_takes_writes() {
    // guard: the log continues after the loaded end
    let (_src, scratch, file) = dumped();
    let to = scratch.path().join("new");
    load(&file, &to).unwrap();
    let mut r = Rdy::open(&to).unwrap();
    rdy_qa::fixtures::write(
        &mut r,
        "after",
        serde_json::json!([
            {"op": "create", "type": "doc", "fields": {"title": "After the load", "doc_type": "page"}}
        ]),
    )
    .unwrap();
    rdy_qa::fixtures::land(&mut r, "after").unwrap();
    r.fsck().unwrap();
    r.close().unwrap();
}

#[test]
fn dl6_a_target_with_a_store_is_refused_and_untouched() {
    let (src, _scratch, file) = dumped();
    let before = tree(src.path());
    let e = load(&file, src.path()).unwrap_err();
    assert_eq!(e.code, "E_ALREADY_EXISTS", "{e}");
    assert_eq!(
        e.message,
        format!("{} already has a store", src.path().display())
    );
    assert_eq!(
        e.fix,
        "load into a new directory, or move .rdy/store aside first"
    );
    assert_eq!(tree(src.path()), before);
}

#[test]
fn dl6_a_project_file_with_other_bytes_is_refused_before_anything_is_written() {
    let (_src, scratch, file) = dumped();
    let to = scratch.path().join("new");
    std::fs::create_dir_all(to.join(".rdy/packs")).unwrap();
    std::fs::write(to.join(".rdy.yaml"), "pack: rdy-standard\n").unwrap();
    std::fs::write(to.join(".rdy/packs/other.yaml"), "name: other\n").unwrap();
    let before = tree(&to);
    let e = load(&file, &to).unwrap_err();
    assert_eq!(e.code, "E_ALREADY_EXISTS", "{e}");
    assert_eq!(
        e.message,
        format!(
            "{} has .rdy.yaml with other bytes than the dump",
            to.display()
        )
    );
    assert_eq!(
        e.fix,
        "move those files aside, or load into a new directory"
    );
    assert_eq!(tree(&to), before);
}

#[test]
fn dl6_a_project_file_with_the_same_bytes_is_kept() {
    let (src, scratch, file) = dumped();
    let to = scratch.path().join("new");
    std::fs::create_dir_all(&to).unwrap();
    std::fs::copy(src.path().join(".rdy.yaml"), to.join(".rdy.yaml")).unwrap();
    let s = load(&file, &to).unwrap();
    assert_eq!(s.files_kept, [".rdy.yaml"]);
    assert_eq!(s.files_written, [".rdy/packs/rdy-qa.yaml", ".rdyignore"]);
}

#[test]
fn dl6_a_load_in_progress_is_refused() {
    let (_src, scratch, file) = dumped();
    let to = scratch.path().join("new");
    std::fs::create_dir_all(to.join(".rdy/store.loading")).unwrap();
    let before = tree(&to);
    let e = load(&file, &to).unwrap_err();
    assert_eq!(e.code, "E_LOCKED", "{e}");
    assert_eq!(
        e.fix,
        "wait for the other load; if none runs, remove .rdy/store.loading"
    );
    assert_eq!(tree(&to), before);
}

#[test]
fn dl6_a_damaged_dump_leaves_the_target_as_it_was() {
    let (_src, scratch, file) = dumped();
    let good = std::fs::read(&file).unwrap();
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("a flipped byte late in the stream", {
            let mut b = good.clone();
            let i = b.len() - 200;
            b[i] ^= 1;
            b
        }),
        ("a cut before the trailer", good[..good.len() - 40].to_vec()),
        ("bytes after the trailer", {
            let mut b = good.clone();
            b.push(0);
            b
        }),
    ];
    for (what, bytes) in cases {
        let f = scratch.path().join("bad.rdydump");
        std::fs::write(&f, &bytes).unwrap();
        let to = scratch.path().join("new");
        let e = load(&f, &to).unwrap_err();
        assert_eq!(e.code, "E_DUMP", "{what}: {e}");
        assert!(
            e.message
                .starts_with(&format!("{} is damaged at byte ", f.display())),
            "{what}: {e}"
        );
        assert!(!to.exists(), "{what}: the load created {to:?} and left it");
    }
}

#[test]
fn dl6_a_dump_that_fails_fsck_leaves_the_target_as_it_was() {
    let src = every_kind::project();
    {
        let mut r = Rdy::open(src.path()).unwrap();
        // a view ref that names no view op: fsck refuses it
        r.store_mut()
            .set_refs_unchecked_for_tests(vec![("view/forged".into(), vec![0u8; 32])])
            .unwrap();
        r.close().unwrap();
    }
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join("p.rdydump");
    let r = Rdy::open(src.path()).unwrap();
    r.read().dump(&file).unwrap();
    r.close().unwrap();
    let to = scratch.path().join("new");
    std::fs::create_dir_all(&to).unwrap();
    std::fs::write(to.join("README.md"), "kept").unwrap();
    let before = tree(&to);
    let e = load(&file, &to).unwrap_err();
    assert_eq!(e.code, "E_DUMP", "{e}");
    assert!(
        e.message
            .starts_with("the dump loads, but the project fails fsck: "),
        "{e}"
    );
    assert_eq!(
        tree(&to),
        before,
        "the store, the written files and .rdy are gone again"
    );
}

#[test]
fn dl6_a_dump_of_another_format_is_refused_before_anything_is_written() {
    let (_src, scratch, file) = dumped();
    let mut b = std::fs::read(&file).unwrap();
    b[8] = 2; // dump format 2
    let f = scratch.path().join("v2.rdydump");
    std::fs::write(&f, &b).unwrap();
    let to = scratch.path().join("new");
    let e = load(&f, &to).unwrap_err();
    assert_eq!(e.code, "E_DUMP");
    assert_eq!(
        e.message,
        format!("{}: dump format 2 is not one this rdy reads", f.display())
    );
    assert_eq!(e.fix, "load it with the rdy that wrote it, or a later one");
    assert!(!to.exists());
}

/// Rewrites the dump `file` to `out`, keeping its project files, after `edit` changes its header
/// and batches. The new trailer is consistent with what `out` holds.
fn rewrite(
    file: &Path,
    out: &Path,
    edit: impl FnOnce(&mut rdy_store::dump::DumpHeader, &mut Vec<rdy_store::dump::DumpBatch>),
) {
    use rdy_store::dump::{DumpItem, DumpReader, DumpWriter};
    let f = std::io::BufReader::new(std::fs::File::open(file).unwrap());
    let (mut r, _, mut header) = DumpReader::open(f).unwrap();
    let mut files = vec![];
    let mut batches = vec![];
    while let Some(item) = r.next().unwrap() {
        match item {
            DumpItem::File { path, bytes } => files.push((path, bytes)),
            DumpItem::Batch(b) => batches.push(b),
        }
    }
    edit(&mut header, &mut batches);
    let mut w = DumpWriter::new(std::fs::File::create(out).unwrap(), &header).unwrap();
    for (path, bytes) in &files {
        w.file(path, bytes).unwrap();
    }
    for b in &batches {
        w.batch(b).unwrap();
    }
    w.finish().unwrap();
}

#[test]
fn dl6_a_dump_with_missing_batches_is_refused() {
    let (_src, scratch, file) = dumped();
    let short = scratch.path().join("short.rdydump");
    let mut log_end = None;
    rewrite(&file, &short, |h, b| {
        assert!(b.len() >= 2, "the every-kind log holds batches");
        b.truncate(b.len() / 2);
        log_end = Some(h.log_end);
    });
    let log_end = log_end.unwrap();
    let to = scratch.path().join("new");
    let e = load(&short, &to).unwrap_err();
    assert_eq!(e.code, "E_DUMP", "{e}");
    assert!(
        e.message.starts_with(&format!(
            "the dump is incomplete: its header says the log ends at segment {} offset {}, but its batches end at segment ",
            log_end.segment, log_end.offset
        )),
        "{e}"
    );
    assert_eq!(e.fix, "make a new dump from the source with rdy dump");
    assert!(!to.exists(), "the load created {to:?} and left it");
}

#[test]
fn dl6_a_dump_with_no_batches_is_refused() {
    // every store holds its format batch from its first open, so a dump of no batches is never
    // a whole project, even when its header says the log ends at 0/0
    let (_src, scratch, file) = dumped();
    let empty = scratch.path().join("empty.rdydump");
    rewrite(&file, &empty, |h, b| {
        b.clear();
        h.log_end = rdy_store::segment::Location {
            segment: 0,
            offset: 0,
        };
    });
    let to = scratch.path().join("new");
    let e = load(&empty, &to).unwrap_err();
    assert_eq!(e.code, "E_DUMP", "{e}");
    assert_eq!(e.message, "the dump is incomplete: it holds no batches");
    assert_eq!(e.fix, "make a new dump from the source with rdy dump");
    assert!(!to.exists(), "the load created {to:?} and left it");
}

#[test]
fn dl6_a_missing_dump_is_refused_and_the_target_untouched() {
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join("none.rdydump");
    let to = scratch.path().join("new");
    std::fs::create_dir_all(&to).unwrap();
    std::fs::write(to.join("README.md"), "kept").unwrap();
    let before = tree(&to);
    let e = load(&file, &to).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert_eq!(e.message, format!("{} does not exist", file.display()));
    assert_eq!(e.fix, "check the path of the dump");
    assert_eq!(tree(&to), before);
}

/// Loads `file` into `to` while another thread opens the project as soon as `.rdy/store`
/// appears, and holds it until the load returns. Returns the load's result and what the racer's
/// opens returned: an error code, or `ok`.
fn race(
    file: &Path,
    to: &Path,
) -> (
    Result<rdy_engine::LoadSummary, rdy_engine::RdyError>,
    std::collections::BTreeSet<String>,
) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let done = Arc::new(AtomicBool::new(false));
    let racer = {
        let done = Arc::clone(&done);
        let to = to.to_path_buf();
        std::thread::spawn(move || {
            let mut seen = std::collections::BTreeSet::new();
            while !done.load(Ordering::SeqCst) {
                if !to.join(".rdy/store").exists() {
                    std::hint::spin_loop();
                    continue;
                }
                match Rdy::open(&to) {
                    Ok(r) => {
                        seen.insert("ok".to_string());
                        while !done.load(Ordering::SeqCst) {
                            std::thread::yield_now();
                        }
                        let _ = r.close();
                        break;
                    }
                    Err(e) => {
                        seen.insert(e.code.to_string());
                    }
                }
            }
            seen
        })
    };
    let out = load(file, to);
    done.store(true, Ordering::SeqCst);
    (out, racer.join().unwrap())
}

#[test]
fn dl6_a_racing_open_does_not_break_a_load() {
    let (src, scratch, file) = dumped();
    let mut raced = false;
    for round in 0..4 {
        let to = scratch.path().join(format!("new{round}"));
        let (out, seen) = race(&file, &to);
        raced |= !seen.is_empty();
        let s = out.unwrap_or_else(|e| panic!("round {round}: {e}; the racer saw {seen:?}"));
        assert!(
            seen.iter().all(|c| c == "E_LOCKED" || c == "ok"),
            "round {round}: {seen:?}"
        );
        assert!(s.landings >= 3, "round {round}: {s:?}");
        // nothing was deleted: the store opens and holds the source log
        let r = Rdy::open(&to).unwrap();
        r.fsck().unwrap();
        r.close().unwrap();
        for (path, bytes) in segs(src.path()) {
            let have = std::fs::read(to.join(".rdy/store/segments").join(&path)).ok();
            let want = bytes.unwrap_or_default();
            assert!(
                have.is_some_and(|h| h.starts_with(&want)),
                "round {round}: segment {path:?} lost bytes"
            );
        }
    }
    assert!(raced, "the racer never tried an open while the load ran");
}

/// A dump of the every-kind project with a view ref that names no view op: fsck refuses it.
fn fsck_failing_dump(scratch: &Path) -> PathBuf {
    let src = every_kind::project();
    {
        let mut r = Rdy::open(src.path()).unwrap();
        r.store_mut()
            .set_refs_unchecked_for_tests(vec![("view/forged".into(), vec![0u8; 32])])
            .unwrap();
        r.close().unwrap();
    }
    let file = scratch.join("forged.rdydump");
    let r = Rdy::open(src.path()).unwrap();
    r.read().dump(&file).unwrap();
    r.close().unwrap();
    file
}

#[test]
fn dl6_a_failed_check_undoes_while_locked() {
    let scratch = tempfile::tempdir().unwrap();
    let file = fsck_failing_dump(scratch.path());
    let mut raced = false;
    for round in 0..4 {
        let to = scratch.path().join(format!("new{round}"));
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(to.join("README.md"), "kept").unwrap();
        let before = tree(&to);
        let (out, seen) = race(&file, &to);
        raced |= !seen.is_empty();
        let e = out.unwrap_err();
        assert_eq!(e.code, "E_DUMP", "round {round}: {e}");
        assert!(
            e.message
                .starts_with("the dump loads, but the project fails fsck: ")
                && !e.message.contains("held by another"),
            "round {round}: {e}; the racer saw {seen:?}"
        );
        assert!(
            !seen.contains("ok"),
            "round {round}: the racer opened the store the load then removed: {seen:?}"
        );
        assert_eq!(tree(&to), before, "round {round}");
    }
    assert!(raced, "the racer never tried an open while the load ran");
}

/// `tree(dir)` without the paths under `skip` (relative to `dir`): a live daemon's run files and
/// a store it still serves change by themselves.
fn tree_but(dir: &Path, skip: &[&str]) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    for e in walk(dir) {
        let rel = e.strip_prefix(dir).unwrap().to_path_buf();
        if skip.iter().any(|s| rel.starts_with(s)) {
            continue;
        }
        out.insert(
            rel,
            if e.is_dir() {
                None
            } else {
                Some(std::fs::read(&e).unwrap())
            },
        );
    }
    out
}

#[test]
fn dl6_a_live_daemon_on_the_target_refuses_the_load() {
    // the rdy binary sets this probe in main; a test process sets it itself
    rdy_engine::load::set_daemon_probe(rdy_daemon::live_daemon);
    let dir = rdy_qa::daemon::short_tempdir();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join("p.rdydump");
    let r = Rdy::open(dir.path()).unwrap();
    r.read().dump(&file).unwrap();
    r.close().unwrap();
    let mut host = rdy_qa::daemon::Hosted::start_in(dir);
    let p = host.dir.path().to_path_buf();
    let store = p.join(".rdy/store");
    let aside = p.join("store-aside");
    // the in-place move with the daemon still up: the daemon serves the store set aside
    std::fs::rename(&store, &aside).unwrap();
    let skip = [".rdy/run", "store-aside"];
    let before = tree_but(&p, &skip);
    let e = load(&file, &p).unwrap_err();
    assert_eq!(e.code, "E_LOCKED", "{e}");
    assert_eq!(
        e.message,
        format!(
            "a daemon still runs for {} (pid {})",
            p.display(),
            std::process::id()
        )
    );
    assert_eq!(
        e.fix,
        format!("run rdy serve --stop in {}, then load again", p.display())
    );
    assert_eq!(tree_but(&p, &skip), before, "the target is untouched");
    // the order the fix of E_STORE_FORMAT gives: stop the daemon, then move the store aside
    std::fs::rename(&aside, &store).unwrap();
    host.stop();
    std::fs::rename(&store, &aside).unwrap();
    let s = load(&file, &p).unwrap();
    assert_eq!(s.files_kept, [".rdy.yaml"]);
}

#[test]
fn dl6_the_daemon_probe_answers_in_and_out_of_a_runtime() {
    // a pin: `rdy load` asks the probe with no runtime, and `rdy_load` asks it from a blocking
    // thread of the server's runtime; a caller inside a runtime must not make it panic either
    let dir = rdy_qa::daemon::short_tempdir();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    let mut host = rdy_qa::daemon::Hosted::start_in(dir);
    let p = host.dir.path().to_path_buf();
    let me = Some(std::process::id());
    assert_eq!(rdy_daemon::live_daemon(&p), me, "no runtime");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let q = p.clone();
    let blocking = rt.block_on(async move {
        tokio::task::spawn_blocking(move || rdy_daemon::live_daemon(&q))
            .await
            .unwrap()
    });
    assert_eq!(blocking, me, "a blocking thread of a runtime");
    let q = p.clone();
    let inside = rt.block_on(async move { rdy_daemon::live_daemon(&q) });
    assert_eq!(inside, me, "inside an async task");
    host.stop();
    assert_eq!(rdy_daemon::live_daemon(&p), None, "after the stop");
}

#[test]
fn dl6_a_bad_target_only_file_is_named() {
    let (_src, scratch, file) = dumped();
    let cases = [
        (".rdy/local.yaml", "actor: [\n"),
        (".rdy/packs/zz-other.yaml", "name: [\n"),
    ];
    for (i, (rel, bytes)) in cases.into_iter().enumerate() {
        let to = scratch.path().join(format!("new{i}"));
        let at = to.join(rel);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(&at, bytes).unwrap();
        let before = tree(&to);
        let e = load(&file, &to).unwrap_err();
        assert_eq!(e.code, "E_DUMP", "{rel}: {e}");
        let head = format!(
            "the dump loaded, but {}, which the dump does not carry, stops the project from opening: ",
            at.display()
        );
        assert!(e.message.starts_with(&head), "{rel}: {e}");
        assert_eq!(
            e.fix,
            format!("fix or move {}, then load again", at.display())
        );
        assert_eq!(tree(&to), before, "{rel}: the target is as it was");
    }
}

#[test]
fn dl6_a_file_that_is_not_a_dump_is_refused() {
    // pins the fix the load picks by the reader's refusal kind (DumpRefusal::NotADump)
    let scratch = tempfile::tempdir().unwrap();
    let f = scratch.path().join("notes.txt");
    std::fs::write(&f, "not a dump\n").unwrap();
    let to = scratch.path().join("new");
    let e = load(&f, &to).unwrap_err();
    assert_eq!(e.code, "E_DUMP", "{e}");
    assert_eq!(
        e.message,
        format!("{}: it does not start with RDYDUMP", f.display())
    );
    assert_eq!(e.fix, "pass a file that rdy dump wrote");
    assert!(!to.exists());
}

#[test]
fn dl6_a_load_into_a_project_dot_rdy_is_refused() {
    let (src, scratch, file) = dumped();
    let link = scratch.path().join("link");
    std::os::unix::fs::symlink(src.path().join(".rdy"), &link).unwrap();
    let before = tree(src.path());
    for to in [
        src.path().join(".rdy"),
        src.path().join(".rdy/packs"),
        src.path().join(".rdy/new/deeper"),
        src.path().join(".rdy/store/../packs"),
        link.join("packs"),
        link.clone(),
    ] {
        let e = load(&file, &to).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{to:?}: {e}");
        assert_eq!(
            e.message,
            format!("{} is inside a project's .rdy directory", to.display())
        );
        assert_eq!(
            e.fix,
            "load into a new directory outside any .rdy, for example a sibling of the project"
        );
        assert_eq!(tree(src.path()), before, "{to:?}");
    }
}
