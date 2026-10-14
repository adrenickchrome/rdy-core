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
