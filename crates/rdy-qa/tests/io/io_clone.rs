//! ec5: rdy clone (plan Export and clone; D-9QV85, D-D2NPN, D-DAGNB, D-C8C70, RULING F3, F4): a
//! clone is a dump and a load through one stream, then one origin batch. Its log is the source's
//! log byte for byte, then that batch; it keeps the repo id (a fork takes a new one) and records
//! where it came from. A refused or failed clone leaves the target as it was.
use rdy_engine::Rdy;
use rdy_engine::clone::{CloneSource, CloneSummary, SourceKind, clone, source_kind};
use rdy_qa::every_kind;
use rdy_qa::reindex_check::{compare, derived, reindex_reproduces};
use rdy_store::kinds::{self, RefUpdate};
use rdy_store::meta::{Origin, REF_ORIGIN, REF_REPO_ID};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const NEXT: &str = "run rdy checkout to bind a working copy; a clone carries none";

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

/// The project's log: its segment files' bytes, in segment order.
fn log_bytes(project: &Path) -> Vec<u8> {
    let dir = project.join(".rdy/store/segments");
    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("seg-") && n.ends_with(".log"))
        })
        .collect();
    names.sort();
    names
        .iter()
        .flat_map(|p| std::fs::read(p).unwrap())
        .collect()
}

/// Clones the open project `src` into `to`.
fn clone_of(src: &Rdy, to: &Path, fork: bool) -> CloneSummary {
    let st = src.read();
    clone(CloneSource::State(&st), to, fork).unwrap()
}

/// The every-kind project, open, and a scratch directory.
fn source() -> (tempfile::TempDir, Rdy, tempfile::TempDir) {
    let src = every_kind::project();
    let r = Rdy::open(src.path()).unwrap();
    (src, r, tempfile::tempdir().unwrap())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The origin `r`'s store records.
fn origin_of(r: &Rdy) -> Option<Origin> {
    r.store().reader().origin().unwrap()
}

fn repo_id_of(r: &Rdy) -> rdy_model::RepoId {
    r.store().reader().repo_id().unwrap().unwrap()
}

/// The origin a clone of `src` must record (O8): its repo id, its newest version and canon's
/// head.
fn expected_origin(src: &Rdy, fork: bool) -> Origin {
    let st = src.read();
    Origin {
        fork,
        repo_id: repo_id_of(src),
        tag: st.versions().unwrap().last().map(|v| v.name.clone()),
        landing: st.canon_head(),
    }
}

/// The ref updates of each batch of `r`'s log that starts at or after `from`, in log order.
fn batches_from(r: &Rdy, from: rdy_store::segment::Location) -> Vec<Vec<(u8, Option<String>)>> {
    let mut out = vec![];
    let store = r.store();
    store
        .reader()
        .for_each_batch(store.log_end(), |b| {
            if b.start >= from {
                out.push(
                    b.frames
                        .iter()
                        .map(|f| {
                            let name = (f.kind() == kinds::REF_UPDATE).then(|| {
                                postcard::from_bytes::<RefUpdate>(f.present_payload().unwrap())
                                    .unwrap()
                                    .name
                            });
                            (f.kind(), name)
                        })
                        .collect(),
                );
            }
            Ok(())
        })
        .unwrap();
    out
}

#[test]
fn ec5_a_clone_keeps_every_hash_ref_and_landing_of_the_every_kind_project() {
    let (src, a, scratch) = source();
    let to = scratch.path().join("clone");
    let s = clone_of(&a, &to, false);
    let b = Rdy::open(&to).unwrap();
    let mut fa = every_kind::facts(&a);
    let mut fb = every_kind::facts(&b);
    // the clone holds meta/origin, and it names the source
    let origin = fb["refs"][REF_ORIGIN].as_str().unwrap().to_string();
    assert_eq!(
        origin,
        hex(&rdy_store::meta::encode_origin(&expected_origin(&a, false)))
    );
    for f in [&mut fa, &mut fb] {
        f["refs"].as_object_mut().unwrap().remove(REF_ORIGIN);
    }
    let ru = kinds::REF_UPDATE.to_string();
    let n = fa["objects"][&ru].as_u64().unwrap();
    fa["objects"][&ru] = (n + 1).into();
    let frames = fa["fsck"]["frames"].as_u64().unwrap();
    fa["fsck"]["frames"] = (frames + 1).into();
    assert_eq!(fb, fa, "refs, objects, landings, fsck");
    let (la, lb) = (log_bytes(src.path()), log_bytes(&to));
    assert!(
        lb.len() > la.len() && lb[..la.len()] == la[..],
        "the source log is a byte prefix of the clone's"
    );
    assert_eq!(s.load.fsck.frames as u64, frames + 1);
    b.close().unwrap();
    a.close().unwrap();
}

#[test]
fn ec5_a_clone_keeps_the_repo_id_and_records_its_origin() {
    let (_src, a, scratch) = source();
    let to = scratch.path().join("clone");
    let s = clone_of(&a, &to, false);
    let want = expected_origin(&a, false);
    assert!(want.tag.is_some() && want.landing.is_some(), "{want:?}");
    let src_id = repo_id_of(&a).to_string();
    assert_eq!(s.repo_id, src_id);
    assert!(!s.fork);
    assert!(s.forked_from.is_none());
    let o = s.origin.clone().expect("a clone names its origin");
    assert_eq!(o.repo_id, src_id);
    assert_eq!(o.tag, want.tag);
    assert_eq!(o.landing, want.landing.map(|h| h.to_string()));
    assert_eq!(s.load.next, NEXT);
    let j = serde_json::to_value(&s).unwrap();
    assert!(j.get("forked_from").is_none(), "{j}");
    assert_eq!(j["origin"]["repo_id"], src_id.as_str());
    assert_eq!(j["next"], NEXT, "the load's fields are flattened");
    let b = Rdy::open(&to).unwrap();
    assert_eq!(repo_id_of(&b), repo_id_of(&a));
    assert_eq!(origin_of(&b), Some(want));
    b.close().unwrap();
    a.close().unwrap();
}

#[test]
fn ec5_a_fork_gets_a_new_repo_id_and_records_forked_from() {
    let (_src, a, scratch) = source();
    let to = scratch.path().join("fork");
    let s = clone_of(&a, &to, true);
    let src_id = repo_id_of(&a);
    assert!(s.fork);
    assert!(s.origin.is_none());
    assert_ne!(s.repo_id, src_id.to_string());
    let f = s
        .forked_from
        .clone()
        .expect("a fork names where it came from");
    assert_eq!(f.repo_id, src_id.to_string());
    let j = serde_json::to_value(&s).unwrap();
    assert!(j.get("origin").is_none(), "{j}");
    assert_eq!(j["fork"], true);
    let b = Rdy::open(&to).unwrap();
    assert_eq!(repo_id_of(&b).to_string(), s.repo_id);
    assert_eq!(origin_of(&b), Some(expected_origin(&a, true)));
    let ru = kinds::REF_UPDATE.to_string();
    let (fa, fb) = (every_kind::facts(&a), every_kind::facts(&b));
    assert_eq!(
        fb["objects"][&ru].as_u64().unwrap(),
        fa["objects"][&ru].as_u64().unwrap() + 2,
        "a new meta/repo_id and meta/origin"
    );
    b.close().unwrap();
    a.close().unwrap();
}

#[test]
fn ec5_a_clone_carries_dismissals_and_mention_reviews() {
    let (_src, a, scratch) = source();
    let to = scratch.path().join("clone");
    clone_of(&a, &to, false);
    let b = Rdy::open(&to).unwrap();
    for family in ["dismiss/", "mention/"] {
        let ra = a.store().reader().refs_with_prefix(family).unwrap();
        let rb = b.store().reader().refs_with_prefix(family).unwrap();
        assert!(!ra.is_empty(), "the every-kind project holds {family}");
        assert_eq!(rb, ra, "{family}");
    }
    let v = compare(&derived(&a).unwrap(), &derived(&b).unwrap());
    assert!(v.is_empty(), "the reviews read the same: {v:#?}");
    b.close().unwrap();
    a.close().unwrap();
}

#[test]
fn ec5_a_clone_from_a_dump_file_equals_a_clone_from_the_project() {
    let (_src, a, scratch) = source();
    let file = scratch.path().join("p.rdydump");
    a.read().dump(&file).unwrap();
    let from_state = scratch.path().join("from-state");
    let from_file = scratch.path().join("from-file");
    let s1 = clone_of(&a, &from_state, false);
    let s2 = clone(CloneSource::File(&file), &from_file, false).unwrap();
    assert_eq!(log_bytes(&from_file), log_bytes(&from_state));
    assert_eq!(
        serde_json::to_value(&s2.origin).unwrap(),
        serde_json::to_value(&s1.origin).unwrap()
    );
    assert_eq!(s2.repo_id, s1.repo_id);
    let b1 = Rdy::open(&from_state).unwrap();
    let b2 = Rdy::open(&from_file).unwrap();
    assert_eq!(origin_of(&b2), origin_of(&b1));
    b1.close().unwrap();
    b2.close().unwrap();
    a.close().unwrap();
}

#[test]
fn ec5_a_clone_reindexes_to_the_same_derived_state() {
    let (_src, a, scratch) = source();
    let to = scratch.path().join("clone");
    clone_of(&a, &to, false);
    a.close().unwrap();
    if let Err(v) = reindex_reproduces(&to) {
        panic!("{v:#?}");
    }
}

#[test]
fn ec5_a_clone_of_a_clone_records_its_nearest_source() {
    let (_src, a, scratch) = source();
    let fork = scratch.path().join("fork");
    clone_of(&a, &fork, true);
    a.close().unwrap();
    let f = Rdy::open(&fork).unwrap();
    let to = scratch.path().join("clone");
    let s = clone_of(&f, &to, false);
    let fork_id = repo_id_of(&f);
    assert_eq!(s.origin.as_ref().unwrap().repo_id, fork_id.to_string());
    let b = Rdy::open(&to).unwrap();
    assert_eq!(
        origin_of(&b),
        Some(expected_origin(&f, false)),
        "the fork, not the project it was forked from"
    );
    assert_eq!(repo_id_of(&b), fork_id);
    let (lf, lb) = (log_bytes(&fork), log_bytes(&to));
    assert!(lb.len() > lf.len() && lb[..lf.len()] == lf[..]);
    b.close().unwrap();
    f.close().unwrap();
}

#[test]
fn ec5_a_clone_of_a_dump_older_than_store_format_12_is_refused() {
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/dump/v1-s11.rdydump");
    let scratch = tempfile::tempdir().unwrap();
    let to = scratch.path().join("clone");
    std::fs::create_dir_all(&to).unwrap();
    std::fs::write(to.join("README.md"), "kept").unwrap();
    let before = tree(&to);
    let e = clone(CloneSource::File(&file), &to, false).unwrap_err();
    assert_eq!(e.code, "E_DUMP", "{e}");
    assert_eq!(
        e.message,
        format!(
            "{} is a dump of store format 11, which has no repo id, so rdy clone cannot record its origin",
            file.display()
        )
    );
    assert_eq!(
        e.fix,
        format!(
            "load it with rdy load {} <dir>, then clone that project",
            file.display()
        )
    );
    assert_eq!(tree(&to), before, "the target is untouched");
    let e = clone(CloneSource::File(&file), &scratch.path().join("new"), true).unwrap_err();
    assert_eq!(e.code, "E_DUMP", "a fork too: {e}");
    assert!(!scratch.path().join("new").exists());
}

#[test]
fn ec5_a_clone_into_a_directory_with_a_store_is_refused_and_untouched() {
    let (_src, a, scratch) = source();
    let file = scratch.path().join("p.rdydump");
    a.read().dump(&file).unwrap();
    let to = scratch.path().join("clone");
    clone_of(&a, &to, false);
    let before = tree(&to);
    for fork in [false, true] {
        let st = a.read();
        for (what, src) in [
            ("project", CloneSource::State(&st)),
            ("file", CloneSource::File(&file)),
        ] {
            let e = clone(src, &to, fork).unwrap_err();
            assert_eq!(e.code, "E_ALREADY_EXISTS", "{what} {fork}: {e}");
            assert_eq!(
                e.message,
                format!("{} already has a store", to.display()),
                "{what} {fork}"
            );
            assert_eq!(tree(&to), before, "{what} {fork}: untouched");
        }
    }
    a.close().unwrap();
}

#[test]
fn ec5_a_failed_clone_leaves_the_target_as_it_was() {
    let (_src, a, scratch) = source();
    let file = scratch.path().join("p.rdydump");
    a.read().dump(&file).unwrap();
    // a target .rdy.yaml with other bytes
    let to = scratch.path().join("other");
    std::fs::create_dir_all(&to).unwrap();
    std::fs::write(to.join(".rdy.yaml"), "pack: rdy-standard\n").unwrap();
    let before = tree(&to);
    for fork in [false, true] {
        let st = a.read();
        for src in [CloneSource::State(&st), CloneSource::File(&file)] {
            let e = clone(src, &to, fork).unwrap_err();
            assert_eq!(e.code, "E_ALREADY_EXISTS", "{e}");
            assert_eq!(
                e.message,
                format!(
                    "{} has .rdy.yaml with other bytes than the dump",
                    to.display()
                )
            );
            assert_eq!(tree(&to), before);
        }
    }
    // a truncated dump
    let good = std::fs::read(&file).unwrap();
    let cut = scratch.path().join("cut.rdydump");
    std::fs::write(&cut, &good[..good.len() - 40]).unwrap();
    let to = scratch.path().join("cut");
    std::fs::create_dir_all(&to).unwrap();
    std::fs::write(to.join("README.md"), "kept").unwrap();
    let before = tree(&to);
    for fork in [false, true] {
        let e = clone(CloneSource::File(&cut), &to, fork).unwrap_err();
        assert_eq!(e.code, "E_DUMP", "{e}");
        assert!(
            e.message
                .starts_with(&format!("{} is damaged at byte ", cut.display())),
            "{e}"
        );
        assert_eq!(tree(&to), before, "the store, the files and .rdy are gone");
    }
    a.close().unwrap();
}

#[test]
fn ec5_a_clone_never_carries_the_local_file() {
    let (src, a, scratch) = source();
    assert!(src.path().join(".rdy/local.yaml").is_file());
    let to = scratch.path().join("clone");
    let s = clone_of(&a, &to, false);
    assert!(!to.join(".rdy/local.yaml").exists());
    assert!(
        !s.load.files_written.iter().any(|f| f.contains("local")),
        "{:?}",
        s.load.files_written
    );
    a.close().unwrap();
}

#[test]
fn ec5_the_origin_batch_holds_only_its_refs() {
    // guard (O23): past the source's end, the clone's log is exactly the origin batch
    let (_src, a, scratch) = source();
    let end = a.store().log_end();
    for (fork, names) in [
        (false, vec![REF_ORIGIN]),
        (true, vec![REF_REPO_ID, REF_ORIGIN]),
    ] {
        let to = scratch.path().join(format!("clone-{fork}"));
        clone_of(&a, &to, fork);
        let b = Rdy::open(&to).unwrap();
        let tail = batches_from(&b, end);
        let want: Vec<(u8, Option<String>)> = names
            .iter()
            .map(|n| (kinds::REF_UPDATE, Some(n.to_string())))
            .collect();
        assert_eq!(tail, vec![want], "fork {fork}: one batch, no TRANSITION");
        assert!(
            b.store().open_batch().is_none(),
            "fork {fork}: no batch left open"
        );
        b.close().unwrap();
    }
    a.close().unwrap();
}

#[test]
fn ec5_a_url_and_a_plain_directory_are_refused_with_their_texts() {
    let scratch = tempfile::tempdir().unwrap();
    for url in ["https://hub.example/acme/design", "file:///tmp/x"] {
        let e = source_kind(Path::new(url)).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{url}");
        assert_eq!(
            e.message,
            format!("{url} is a URL: cloning from a hub comes with the sync client")
        );
        assert_eq!(
            e.fix,
            "run rdy dump --out <file> in that project, then clone the file"
        );
    }
    let plain = scratch.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let config_only = scratch.path().join("config-only");
    std::fs::create_dir_all(&config_only).unwrap();
    std::fs::write(config_only.join(".rdy.yaml"), "pack: rdy-standard\n").unwrap();
    for p in [plain, config_only, scratch.path().join("missing")] {
        let e = source_kind(&p).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{p:?}");
        assert_eq!(
            e.message,
            format!("{} is neither a dump file nor an rdy project", p.display())
        );
        assert_eq!(
            e.fix,
            "pass a dump file, or a directory with .rdy.yaml and .rdy/store"
        );
    }
    // the two cases the list accepts
    let (src, a, scratch2) = source();
    let file = scratch2.path().join("p.rdydump");
    a.read().dump(&file).unwrap();
    assert!(matches!(source_kind(&file), Ok(SourceKind::File)));
    assert!(matches!(source_kind(src.path()), Ok(SourceKind::Project)));
    a.close().unwrap();
}

/// A small project whose store holds the forged `meta/origin` value `origin`, open, with a dump
/// of it and a scratch directory (fix round 1: a source that `fsck` and `rdy load` refuse).
fn forged_source(origin: Vec<u8>) -> (tempfile::TempDir, Rdy, PathBuf) {
    forged_source_with(vec![(REF_ORIGIN.into(), origin)])
}

/// [`forged_source`] with the forged refs `refs`.
fn forged_source_with(refs: Vec<(String, Vec<u8>)>) -> (tempfile::TempDir, Rdy, PathBuf) {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().join("src");
    std::fs::create_dir_all(&dir).unwrap();
    let mut r = Rdy::init(&dir, "rdy-standard").unwrap();
    r.store_mut().set_refs_unchecked_for_tests(refs).unwrap();
    r.close().unwrap();
    let r = Rdy::open(&dir).unwrap();
    let file = scratch.path().join("src.rdydump");
    r.read().dump(&file).unwrap();
    (scratch, r, file)
}

/// Ruling task-5: a clone refuses every source that `rdy load` of the same dump refuses, with
/// load's error, and leaves the target as it was; for a clone and a fork, from the project and
/// from the file.
fn refuses_as_load_does(scratch: &Path, r: &Rdy, file: &Path) {
    let loaded = rdy_engine::load::load(file, &scratch.join("loaded")).unwrap_err();
    assert_eq!(loaded.code, "E_DUMP", "{loaded}");
    assert!(
        loaded
            .message
            .starts_with("the dump loads, but the project fails fsck: "),
        "{loaded}"
    );
    for fork in [false, true] {
        let st = r.read();
        for (what, src) in [
            ("project", CloneSource::State(&st)),
            ("file", CloneSource::File(file)),
        ] {
            let to = scratch.join(format!("clone-{what}-{fork}"));
            std::fs::create_dir_all(&to).unwrap();
            std::fs::write(to.join("README.md"), "kept").unwrap();
            let before = tree(&to);
            let e = clone(src, &to, fork).unwrap_err();
            assert_eq!(e.code, loaded.code, "{what} {fork}: {e}");
            assert_eq!(e.message, loaded.message, "{what} {fork}");
            assert_eq!(e.fix, loaded.fix, "{what} {fork}");
            assert!(!to.join(".rdy").exists(), "{what} {fork}: no .rdy");
            assert_eq!(tree(&to), before, "{what} {fork}: untouched");
        }
    }
}

#[test]
fn ec5_a_clone_refuses_a_source_whose_origin_does_not_decode() {
    let (scratch, r, file) = forged_source(vec![9]);
    refuses_as_load_does(scratch.path(), &r, &file);
    r.close().unwrap();
}

#[test]
fn ec5_a_clone_refuses_a_source_whose_origin_names_another_repo() {
    // a decodable clone origin that names a repo id other than the source's
    let other = rdy_model::RepoId::new();
    let o = Origin {
        fork: false,
        repo_id: other,
        tag: None,
        landing: None,
    };
    let (scratch, r, file) = forged_source(rdy_store::meta::encode_origin(&o));
    assert_ne!(repo_id_of(&r), other);
    refuses_as_load_does(scratch.path(), &r, &file);
    r.close().unwrap();
}

#[test]
fn ec5_a_clone_of_a_source_with_two_faults_gives_the_fault_load_gives() {
    // fix round 1: a source whose origin does not decode and whose view ref is not a view head;
    // fsck finds the view first, so load names the view, and the clone names it too
    let (scratch, r, file) = forged_source_with(vec![
        (REF_ORIGIN.into(), vec![9]),
        ("view/broken".into(), vec![9]),
    ]);
    refuses_as_load_does(scratch.path(), &r, &file);
    r.close().unwrap();
}

#[test]
fn ec5_a_dump_replaced_after_its_header_check_is_never_cloned_from_the_new_bytes() {
    // fix round 1: the O10 check reads the header of the stream the load consumes. While the
    // source path flips between a format-12 dump and a format-11 one by atomic renames, every
    // clone either refuses with O10 or clones the format-12 dump; none ingests format 11.
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().join("good");
    std::fs::create_dir_all(&dir).unwrap();
    let g = Rdy::init(&dir, "rdy-standard").unwrap();
    let good_id = repo_id_of(&g);
    let good = scratch.path().join("good.rdydump");
    g.read().dump(&good).unwrap();
    g.close().unwrap();
    let good = std::fs::read(&good).unwrap();
    let old = std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/dump/v1-s11.rdydump"),
    )
    .unwrap();
    let src = scratch.path().join("src.rdydump");
    std::fs::write(&src, &good).unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let swapper = {
        let (stop, src, stage) = (stop.clone(), src.clone(), scratch.path().join("stage"));
        std::thread::spawn(move || {
            let mut flip = false;
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                flip = !flip;
                std::fs::write(&stage, if flip { &old } else { &good }).unwrap();
                // a rename onto an open file may fail on some platforms: try again
                let _ = std::fs::rename(&stage, &src);
            }
        })
    };
    let o10 = format!(
        "{} is a dump of store format 11, which has no repo id, so rdy clone cannot record its origin",
        src.display()
    );
    let mut outcome = Ok(());
    for i in 0..300 {
        let to = scratch.path().join(format!("t{i}"));
        match clone(CloneSource::File(&src), &to, false) {
            Ok(s) if s.load.store_format >= 12 && s.repo_id == good_id.to_string() => {}
            Ok(s) => {
                outcome = Err(format!(
                    "attempt {i}: cloned store format {} with repo id {}",
                    s.load.store_format, s.repo_id
                ));
                break;
            }
            Err(e) if e.code == "E_DUMP" && e.message == o10 => {
                assert!(!to.exists(), "attempt {i}: the target is untouched");
            }
            Err(e) => {
                outcome = Err(format!("attempt {i}: {e}"));
                break;
            }
        }
    }
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    swapper.join().unwrap();
    outcome.unwrap();
}

/// Final review C1: a clone or fork into the source's own `.rdy`, reached through a symlinked
/// `.rdy` whose resolved directory has another name, is refused before anything is written, from
/// the open project and from a dump file alike. The source and the target keep their bytes, and
/// the source still opens and passes `fsck`.
#[test]
fn ec5_a_clone_into_a_symlinked_source_dot_rdy_is_refused_and_both_are_untouched() {
    let src = every_kind::project();
    let scratch = tempfile::tempdir().unwrap();
    // the source's .rdy lives elsewhere, under another name, behind a symlink
    let physical = scratch.path().join("physical-store");
    std::fs::rename(src.path().join(".rdy"), &physical).unwrap();
    rdy_qa::platform::symlink(&physical, &src.path().join(".rdy")).unwrap();
    // a second link to the source's .rdy, not named .rdy
    let link = scratch.path().join("link");
    rdy_qa::platform::symlink(&src.path().join(".rdy"), &link).unwrap();
    let a = Rdy::open(src.path()).unwrap();
    let file = scratch.path().join("p.rdydump");
    a.read().dump(&file).unwrap();
    let dot = src.path().join(".rdy");
    let seg = std::fs::read_dir(dot.join("store/segments"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "log"))
        .unwrap();
    let targets = [
        dot.join("store/segments/seg-999999.log"),
        seg.clone(),
        dot.join("store/new"),
        dot.join("packs"),
        dot.join("packs/new"),
        dot.join("new/deeper"),
        dot.clone(),
        physical.join("packs/new"),
        physical.join("store/new"),
        link.join("packs/new"),
    ];
    let source_before = tree(&physical);
    let log_before = log_bytes(src.path());
    // a target's bytes: a file's own, or its tree
    let snap = |to: &Path| {
        (
            std::fs::symlink_metadata(to).is_ok(),
            std::fs::read(to).ok(),
            to.is_dir().then(|| tree(to)),
        )
    };
    for to in &targets {
        let target_before = snap(to);
        for fork in [false, true] {
            let st = a.read();
            for (what, from) in [
                ("project", CloneSource::State(&st)),
                ("file", CloneSource::File(&file)),
            ] {
                let e = clone(from, to, fork).unwrap_err();
                assert_eq!(e.code, "E_BAD_INPUT", "{what} {fork} {to:?}: {e}");
                assert_eq!(
                    e.message,
                    format!("{} is inside a project's .rdy directory", to.display()),
                    "{what} {fork}"
                );
                assert_eq!(snap(to), target_before, "{what} {fork} {to:?}: the target");
                assert_eq!(
                    tree(&physical),
                    source_before,
                    "{what} {fork} {to:?}: the source"
                );
            }
        }
        let e = rdy_engine::load::load(&file, to).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "load {to:?}: {e}");
        assert_eq!(tree(&physical), source_before, "load {to:?}: the source");
    }
    assert_eq!(log_bytes(src.path()), log_before);
    a.close().unwrap();
    // the source still opens and is clean
    let again = Rdy::open(src.path()).unwrap();
    again.fsck().expect("the source passes fsck");
    again.close().unwrap();
    // a sibling of the source still clones
    let ok = scratch.path().join("sibling");
    let st = Rdy::open(src.path()).unwrap();
    clone_of(&st, &ok, true);
    st.close().unwrap();
}
