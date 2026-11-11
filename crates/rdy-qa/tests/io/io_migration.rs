//! m4bc6 (D-19BZD, D-FSWQE, D-2R9EY, D-E4W0Y, D-Y9DNZ): a Markdown migration holds its contract
//! through the whole import path. The source digest a preview gives is the one the import
//! replies, a person can recompute it from the contract's spelling, and the source tree is never
//! changed. A refusal after earlier files were staged leaves every ref as it was. A process
//! killed before the seal leaves no import after reopen; one killed after it leaves all of it.
//! A dump of a project that holds `reconstructed` decisions and imports loads back with the same
//! basis, importer and derived state.
//!
//! The kill cases arm the store's crash point inside a child (this test binary run again with
//! `RDY_M4BC6_CHILD`), so they need no OS signal and run the same on every platform.
use rdy_engine::engine::{Engine, Local};
use rdy_engine::import_api::{ImportOptions, run_import_with};
use rdy_engine::load::load;
use rdy_engine::*;
use rdy_model::{BlockId, Hash, Role};
use rdy_qa::every_kind;
use rdy_qa::fixtures::{approver, as_agent};
use rdy_qa::intake::TEST_SUMMARY;
use rdy_qa::invariants::check;
use rdy_qa::reindex_check::{compare, derived};
use rdy_store::Store;
use rdy_store::segment::CrashPoint;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const VIEW: &str = "md";

fn session(actor: &str, role: Role) -> Session {
    Session {
        actor: actor.into(),
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

fn writer() -> Session {
    session("human:imp", Role::Writer)
}

/// A fresh project (closed) and an empty source directory beside it.
struct Fx {
    _dir: tempfile::TempDir,
    project: PathBuf,
    src: PathBuf,
}

fn fx() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project).unwrap();
    Rdy::init(&project, "rdy-standard")
        .unwrap()
        .close()
        .unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    Fx {
        _dir: dir,
        project,
        src,
    }
}

fn native(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, s| p.join(s))
}

/// Writes `bytes` at the logical path `rel` under `root`, making its directories.
fn put(root: &Path, rel: &str, bytes: impl AsRef<[u8]>) {
    let path = native(root, rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// The files, folders and Markdown a migration rehearsal starts from: two linked docs, a
/// picture, an empty folder, a file the `.rdyignore` leaves out, and the `.rdyignore`.
fn seed(src: &Path) {
    put(
        src,
        "index.md",
        "# Home\n\nStart with the [guide](guide/start.md#steps).\n",
    );
    put(
        src,
        "guide/start.md",
        "# Start\n\n## Steps\n\n```sh\nrdy init\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n",
    );
    put(src, "logo.svg", "<svg/>");
    put(src, "skip.draft.md", "# Draft\n\nNot for import.\n");
    std::fs::create_dir_all(native(src, "later")).unwrap();
    put(src, ".rdyignore", "*.draft.md\n");
}

/// What the project holds durably, read through a fresh open after reindex: the refs (views,
/// canon, features), the frame count and the store's FEATURES file. The log boundary is left
/// out: a killed process may leave an unacknowledged tail that the reopen quarantines.
#[derive(Debug, PartialEq)]
struct Durable {
    refs: Vec<(String, Vec<u8>)>,
    frames: usize,
    features: Vec<u8>,
}

fn durable(project: &Path) -> Durable {
    let store = project.join(".rdy").join("store");
    let mut s = Store::open(&store).unwrap();
    s.reindex().unwrap();
    Durable {
        refs: s.reader().refs_with_prefix("").unwrap(),
        frames: s.verify().unwrap(),
        features: std::fs::read(store.join("FEATURES")).unwrap_or_default(),
    }
}

fn engine(project: &Path) -> Local {
    Local::new(Rdy::open(project).unwrap())
}

fn md(dry_run: bool, expected: Option<&str>) -> ImportOptions {
    ImportOptions {
        md: true,
        dry_run,
        expected_source: expected.map(str::to_string),
    }
}

fn preview_in(e: &Local, view: &str, src: &Path) -> Result<Value, RdyError> {
    run_import_with(e, &writer(), view, src, &md(true, None))
}

fn import_in(e: &Local, view: &str, src: &Path, expected: Option<&str>) -> Result<Value, RdyError> {
    run_import_with(e, &writer(), view, src, &md(false, expected))
}

fn preview(e: &Local, src: &Path) -> Result<Value, RdyError> {
    preview_in(e, VIEW, src)
}

fn import(e: &Local, src: &Path, expected: Option<&str>) -> Result<Value, RdyError> {
    import_in(e, VIEW, src, expected)
}

fn keys(v: &Value) -> BTreeSet<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

/// Every directory and file under `root` with its logical path and bytes, in one digest: the
/// byte identity of a source tree.
fn tree_digest(root: &Path) -> String {
    fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, Option<Vec<u8>>)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if entry.file_type().unwrap().is_dir() {
                out.push((path.clone(), None));
                walk(&entry.path(), &path, out);
            } else {
                out.push((path, Some(std::fs::read(entry.path()).unwrap())));
            }
        }
    }
    let mut rows = vec![];
    walk(root, "", &mut rows);
    rows.sort();
    let mut buf = vec![];
    for (path, bytes) in rows {
        buf.extend((path.len() as u64).to_le_bytes());
        buf.extend(path.as_bytes());
        match bytes {
            Some(b) => {
                buf.push(1);
                buf.extend((b.len() as u64).to_le_bytes());
                buf.extend(b);
            }
            None => buf.push(0),
        }
    }
    Hash::of(&buf).to_string()
}

/// The digest input as the contract spells it: the tag, then each (kind, path, content) entry,
/// sorted by path bytes, each length a u64 LE.
fn spelled(entries: &[(u8, &str, &[u8])]) -> String {
    let mut sorted = entries.to_vec();

    sorted.sort_by(|a, b| a.1.as_bytes().cmp(b.1.as_bytes()));
    let mut buf = b"rdy-md-import-1\0".to_vec();
    for (kind, path, content) in sorted {
        buf.push(kind);
        buf.extend((path.len() as u64).to_le_bytes());
        buf.extend(path.as_bytes());
        buf.extend((content.len() as u64).to_le_bytes());
        buf.extend(content);
    }
    Hash::of(&buf).to_string()
}

fn is_digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[test]
fn m4bc6_source_digest_and_baseline_survive_the_full_import_path() {
    let f = fx();
    seed(&f.src);
    let source_before = tree_digest(&f.src);
    let was = durable(&f.project);

    // the preview: a digest, the files, the reported file; nothing written, no view, no claim
    let e = engine(&f.project);
    let pv = preview(&e, &f.src).unwrap();
    assert_eq!(
        keys(&pv),
        ["blocks", "dry_run", "files", "source_digest", "unmapped"]
            .map(String::from)
            .into_iter()
            .collect()
    );
    assert_eq!(pv["dry_run"], json!(true));
    let digest = pv["source_digest"].as_str().unwrap().to_string();
    assert!(is_digest(&digest), "{digest}");
    let paths: Vec<&str> = pv["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["guide/start.md", "index.md"], "the ignored draft");
    let keys_of_docs: Vec<&str> = pv["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["doc_key"].as_str().unwrap())
        .collect();
    assert_eq!(keys_of_docs, ["guide.start", "doc-index"]);
    assert_eq!(
        pv["unmapped"],
        json!([{"path": "logo.svg", "line": 0, "kind": "non_markdown_file"}])
    );
    assert!(
        e.read().view_head(VIEW).is_none(),
        "a preview claims no view"
    );
    assert_eq!(
        tree_digest(&f.src),
        source_before,
        "the preview left the source alone"
    );

    // a person can recompute the digest from the contract: the tag, then every included entry
    // (a folder, a Markdown file, a reported file, the `.rdyignore`) by path bytes
    let index = std::fs::read(f.src.join("index.md")).unwrap();
    let start = std::fs::read(native(&f.src, "guide/start.md")).unwrap();
    let want = spelled(&[
        (0, "index.md", &index),
        (0, "guide/start.md", &start),
        (1, "logo.svg", b""),
        (2, ".rdyignore", b"*.draft.md\n"),
        (3, "guide", b""),
        (3, "later", b""),
    ]);
    assert_eq!(
        digest, want,
        "the contract's spelling gives the preview's digest"
    );
    e.close().unwrap();
    assert_eq!(durable(&f.project), was, "a preview writes nothing");

    // the import with the preview's digest: the same digest, files and reported rows
    let e = engine(&f.project);
    let v = import(&e, &f.src, Some(&digest)).unwrap();
    assert_eq!(v["source_digest"], json!(digest));
    assert_eq!(v["files"], pv["files"]);
    assert_eq!(v["unmapped"], pv["unmapped"]);
    assert_eq!(
        v["created"].as_object().map(|o| o.len()),
        pv["blocks"].as_u64().map(|n| n as usize),
        "the preview counted the blocks the import created"
    );
    assert!(v.get("coverage_error").is_none(), "{v}");
    assert_eq!(
        tree_digest(&f.src),
        source_before,
        "the import left the source alone"
    );

    // the baseline: the reply's coverage and onboarding are the view's current reading, and
    // the next phase agrees with it. Only the present state is checked, never progress.
    let scope = Scope::View(VIEW.into());
    let st = e.read();
    let report = st.coverage_report(&scope, None).unwrap();
    assert_eq!(v["coverage"]["score"], json!(report.score), "{v}");
    assert_eq!(v["coverage"]["dimensions"], json!(report.dimensions), "{v}");
    let onboarding = serde_json::to_value(st.onboarding(&scope).unwrap()).unwrap();
    assert_eq!(v["onboarding"], onboarding, "{v}");
    assert_eq!(
        v["onboarding"]["done"],
        json!(v["onboarding"]["next"].is_null()),
        "done and the next phase agree: {}",
        v["onboarding"]
    );
    assert_eq!(v["onboarding"]["phases"].as_array().map(Vec::len), Some(4));
    let owner = st.view_head(VIEW).and_then(|h| h.owner.clone());
    assert_eq!(owner.map(|o| o.actor).as_deref(), Some("human:imp"));
    drop(st);
    e.close().unwrap();

    // the digest survives the import: the same source previews to the same digest (into a view
    // of its own: the imported names would clash in this one)
    let e = engine(&f.project);
    let again = preview_in(&e, "again", &f.src).unwrap();
    assert_eq!(again["source_digest"], json!(digest));
    e.close().unwrap();
    assert_eq!(tree_digest(&f.src), source_before);
    // and a source that changed since the preview no longer matches it
    put(&f.src, "index.md", "# Home\n\nChanged.\n");
    let was = durable(&f.project);
    let e = engine(&f.project);
    let err = import_in(&e, "again", &f.src, Some(&digest)).unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str(), err.fix.as_str()),
        (
            E_BAD_INPUT,
            "the Markdown source changed after preview",
            "preview the source again and review the new report"
        )
    );
    e.close().unwrap();
    assert_eq!(durable(&f.project), was);
}

/// A destination view that already holds a doc named `zz-last`, so the last source file's name
/// clashes only when the batch is written.
fn clash_with_last(project: &Path) {
    let mut r = Rdy::open(project).unwrap();
    r.new_doc(&writer(), VIEW, "zz-last", Some("page"), Some("Mine"))
        .unwrap();
    r.close().unwrap();
}

#[test]
fn m4bc6_invalid_last_file_leaves_every_project_ref_unchanged() {
    // the last file (by path bytes) is the one that fails; every earlier file is good, in
    // several folders, so the candidate has staged most of its blocks when the failure comes
    type Prepare = fn(&Path);
    let last_is_good = "# Last\n\nFine.\n";
    let cases: [(&str, &str, Prepare); 2] = [
        (
            "an unknown doc_type",
            "---\ndoc_type: nonesuch\n---\n# Last\n\nBody.\n",
            |_| {},
        ),
        (
            "a name the view already holds",
            last_is_good,
            clash_with_last,
        ),
    ];
    for (what, last, prepare) in cases {
        let f = fx();
        for (rel, body) in [
            ("a.md", "# A\n\nOne.\n"),
            ("b/c.md", "# C\n\nTwo, see [a](../a.md).\n"),
            ("b/d/e.md", "# E\n\nThree.\n\n| x |\n|---|\n| 1 |\n"),
            ("m.md", "# M\n\n- one\n- two\n"),
        ] {
            put(&f.src, rel, body);
        }
        put(&f.src, "zz-last.md", last);
        prepare(&f.project);
        let source_before = tree_digest(&f.src);
        let was = durable(&f.project);
        let head_before = {
            let e = engine(&f.project);
            let h = e.read().view_head(VIEW).cloned();
            e.close().unwrap();
            h
        };

        let e = engine(&f.project);
        let err = import(&e, &f.src, None).unwrap_err();
        match what {
            "an unknown doc_type" => {
                assert_eq!(err.code, "E_DOC_TYPE", "{what}: {err:?}");
                assert!(
                    err.message.starts_with("zz-last.md:")
                        && err.message.ends_with("unknown doc_type \"nonesuch\""),
                    "{what}: the refusal names the source path: {err:?}"
                );
            }
            _ => assert_eq!(err.code, E_PATH_CLASH, "{what}: {err:?}"),
        }
        let batch_open = e.inspect(|r| Ok(r.store().open_batch().is_some())).unwrap();
        assert!(!batch_open, "{what}: no batch is left open");
        assert_eq!(
            e.read().view_head(VIEW).cloned(),
            head_before,
            "{what}: the view is as it was"
        );
        e.close().unwrap();
        assert_eq!(
            durable(&f.project),
            was,
            "{what}: every ref, the frame count and the features are as before"
        );
        assert_eq!(tree_digest(&f.src), source_before, "{what}: the source");

        // a preview refuses the same way and also writes nothing
        let e = engine(&f.project);
        let pv = preview(&e, &f.src).unwrap_err();
        if what == "an unknown doc_type" {
            assert_eq!(pv.code, err.code);
            assert_eq!(pv.message, err.message);
        }
        e.close().unwrap();
        assert_eq!(durable(&f.project), was, "{what}: the preview");

        // the project is whole and takes the corrected source
        let r = Rdy::open(&f.project).unwrap();
        check(&r, &Scope::Canon).unwrap();
        r.close().unwrap();
        if what == "an unknown doc_type" {
            put(&f.src, "zz-last.md", last_is_good);
            let e = engine(&f.project);
            import(&e, &f.src, None).unwrap();
            e.close().unwrap();
            assert_ne!(
                durable(&f.project),
                was,
                "{what}: the corrected source lands"
            );
        }
    }
}

/// The child of `m4bc6_kill_before_seal_keeps_old_state_and_after_seal_keeps_all`: the import
/// with a crash point armed on the store, so the process aborts when the seal reaches it.
fn crash_child(project: &Path, src: &Path, point: CrashPoint) -> ! {
    let mut r = Rdy::open(project).unwrap();
    r.store_mut().crash_at(point);
    let e = Local::new(r);
    let _ = import(&e, src, None);
    unreachable!("the crash point aborts the process");
}

fn run_child(project: &Path, src: &Path, point: CrashPoint) -> std::process::ExitStatus {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &rdy_qa::test_name!("m4bc6_kill_before_seal_keeps_old_state_and_after_seal_keeps_all"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("RDY_M4BC6_CHILD", project)
        .env("RDY_M4BC6_SRC", src)
        .env("RDY_M4BC6_POINT", point.name())
        .status()
        .unwrap()
}

/// The doc paths a good import of `seed` creates.
const DOCS: [&str; 2] = ["doc-index", "guide.start"];

#[test]
fn m4bc6_kill_before_seal_keeps_old_state_and_after_seal_keeps_all() {
    if let Ok(project) = std::env::var("RDY_M4BC6_CHILD") {
        let src = std::env::var("RDY_M4BC6_SRC").unwrap();
        let point = CrashPoint::parse(&std::env::var("RDY_M4BC6_POINT").unwrap()).unwrap();
        crash_child(Path::new(&project), Path::new(&src), point);
    }
    // the same import, uninterrupted, in a twin project: what "all of it" means
    let twin = fx();
    seed(&twin.src);
    let (twin_frames, twin_blocks) = {
        let e = engine(&twin.project);
        let v = import(&e, &twin.src, None).unwrap();
        e.close().unwrap();
        (
            durable(&twin.project).frames,
            v["created"].as_object().unwrap().len(),
        )
    };
    assert!(twin_blocks > 0);

    for (point, lands) in [
        (CrashPoint::BeforeMarker, false),
        (CrashPoint::BeforeSync, true),
        (CrashPoint::AfterSync, true),
        (CrashPoint::AfterIndex, true),
    ] {
        let f = fx();
        seed(&f.src);
        let source_before = tree_digest(&f.src);
        let was = durable(&f.project);
        let status = run_child(&f.project, &f.src, point);
        // an abort, not a failed run: a panic in the child exits with 101 and would pass for
        // "nothing landed"
        assert!(
            !status.success() && status.code() != Some(101),
            "{point:?}: the child aborted at its crash point: {status:?}"
        );

        // reopen: the store is consistent, and the import is all there or not there at all
        let now = durable(&f.project);
        let r = Rdy::open(&f.project).unwrap();
        r.fsck().unwrap();
        check(&r, &Scope::Canon).unwrap();
        let scope = Scope::View(VIEW.into());
        if lands {
            assert!(r.read().view_head(VIEW).is_some(), "{point:?}: the view");
            assert_eq!(
                now.frames, twin_frames,
                "{point:?}: every frame of the import"
            );
            for doc in DOCS {
                assert!(
                    r.read().resolve_id(&scope, doc).is_ok(),
                    "{point:?}: doc {doc}"
                );
            }
            let owner = r.read().view_head(VIEW).and_then(|h| h.owner.clone());
            assert_eq!(
                owner.map(|o| o.actor).as_deref(),
                Some("human:imp"),
                "{point:?}"
            );
            check(&r, &scope).unwrap();
            r.close().unwrap();
            // the import is there once: the same source refuses to land a second time
            let e = engine(&f.project);
            let err = import(&e, &f.src, None).unwrap_err();
            assert_eq!(err.code, E_PATH_CLASH, "{point:?}: {err:?}");
            e.close().unwrap();
            assert_eq!(
                durable(&f.project),
                now,
                "{point:?}: the second import wrote nothing"
            );
        } else {
            assert_eq!(now, was, "{point:?}: the old state, ref for ref");
            assert!(
                r.read().view_head(VIEW).is_none(),
                "{point:?}: no view claim"
            );
            r.close().unwrap();
            // the old state takes the import whole
            let e = engine(&f.project);
            let v = import(&e, &f.src, None).unwrap();
            e.close().unwrap();
            assert_eq!(v["created"].as_object().map(|o| o.len()), Some(twin_blocks));
            assert_eq!(durable(&f.project).frames, twin_frames, "{point:?}");
        }
        assert_eq!(tree_digest(&f.src), source_before, "{point:?}: the source");
    }
}

/// Lands `view` as the approver, after `owner` (the view's owner) answered its open items and
/// proposed it with a summary.
fn land_as(r: &mut Rdy, owner: &Session, view: &str) {
    rdy_qa::intake::answer_open_items(r, owner, view);
    let p = r.propose(owner, view, view, Some(TEST_SUMMARY)).unwrap();
    rdy_qa::intake::confirm_before_landing(r, &approver(), &p);
    r.land(&approver(), &p.id).unwrap();
}

fn basis_of(r: &Rdy, scope: &Scope, id: &str) -> Option<String> {
    r.read()
        .block(scope, &BlockId::parse(id).unwrap())
        .unwrap()
        .field_str("authority_basis")
        .map(String::from)
}

#[test]
fn m4bc6_dump_load_reindex_keeps_reconstructed_and_import_provenance() {
    let tmp = tempfile::tempdir().unwrap();

    // a project that holds a `reconstructed` decision, as an agent wrote it and a person
    // approved it
    let origin = tmp.path().join("origin");
    std::fs::create_dir_all(&origin).unwrap();
    let designer = as_agent("agent:designer", "run-1");
    let mut a = Rdy::init(&origin, "rdy-standard").unwrap();
    let w = a
        .write(
            &designer,
            WriteRequest {
                view: "run-1".into(),
                ops: parse_ops(
                    serde_json::from_value(json!([
                        {"op": "create", "type": "doc", "id": "$d",
                         "fields": {"title": "Decisions", "doc_type": "decisions-log"}},
                        {"op": "create", "type": "section", "id": "$s", "parent": "$d",
                         "fields": {"title": "Recovered"}},
                        {"op": "create", "type": "decision", "id": "$x", "parent": "$s",
                         "fields": {"statement": "Cache is write-through",
                                    "status": "proposed", "authority_basis": "reconstructed"}}
                    ]))
                    .unwrap(),
                )
                .unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
    let decision = w.created["$x"].clone();
    land_as(&mut a, &designer, "run-1");
    assert_eq!(
        basis_of(&a, &Scope::Canon, &decision).as_deref(),
        Some("reconstructed"),
        "approval keeps the basis"
    );
    // the newest author of the decision at the source: the approval that accepted it
    let newest = {
        let id = BlockId::parse(&decision).unwrap();
        let h = a.read().history(&Scope::Canon, &id, 10).unwrap();
        let n = &h.entries[0].author;
        (n.actor.clone(), n.run.clone())
    };
    let section = w.created["$s"].clone();
    let export = tmp.path().join("export");
    a.read().export(&Scope::Canon, &export).unwrap();
    a.close().unwrap();

    // the migration target: a format-3 import of that export (author groups, importer), and a
    // Markdown import; a person approves both
    let target = tmp.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    Rdy::init(&target, "rdy-standard").unwrap().close().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    seed(&src);
    let importer = writer();
    let mut b = Rdy::open(&target).unwrap();
    b.import(&importer, "import", &export).unwrap();
    b.close().unwrap();
    let e = engine(&target);
    let v = import(&e, &src, None).unwrap();
    e.close().unwrap();
    assert!(v["created"].as_object().is_some_and(|o| !o.is_empty()));
    let mut b = Rdy::open(&target).unwrap();
    land_as(&mut b, &importer, "import");
    land_as(&mut b, &importer, VIEW);
    assert_eq!(
        basis_of(&b, &Scope::Canon, &decision).as_deref(),
        Some("reconstructed"),
        "the imported decision keeps its basis after approval"
    );

    // dump, load into a new place, reindex: the same refs, objects, landings and derived state
    let file = tmp.path().join("target.rdydump");
    b.read().dump(&file).unwrap();
    let loaded = tmp.path().join("loaded");
    load(&file, &loaded).unwrap();
    let c = Rdy::open(&loaded).unwrap();
    assert_eq!(
        every_kind::facts(&c),
        every_kind::facts(&b),
        "refs, objects, landings, fsck"
    );
    let before = derived(&c).unwrap();
    c.close().unwrap();
    let mut c = Rdy::open(&loaded).unwrap();
    c.store_mut().reindex().unwrap();
    c.fsck().unwrap();
    let after = derived(&c).unwrap();
    let diff = compare(&before, &after);
    assert!(
        diff.is_empty(),
        "a reindex reproduces the derived state: {diff:?}"
    );
    assert_eq!(
        derived(&b).unwrap(),
        after,
        "and it is the dumped project's"
    );

    // the loaded project: the basis, and the importer of each imported change
    let id = BlockId::parse(&decision).unwrap();
    assert_eq!(
        basis_of(&c, &Scope::Canon, &decision).as_deref(),
        Some("reconstructed")
    );
    // each imported change names its importer and keeps the source's author beside it: the
    // section as the agent wrote it, the decision as its newest author (the approval) left it
    let created_of = |id: &BlockId| {
        let h = c.read().history(&Scope::Canon, id, 10).unwrap();
        h.entries.last().cloned().expect("a creating change")
    };
    let created = created_of(&id);
    assert_eq!(
        created.imported_by.as_ref().map(|i| i.actor.as_str()),
        Some("human:imp")
    );
    assert_eq!(
        (created.author.actor.clone(), created.author.run.clone()),
        newest,
        "the decision keeps its source author beside the importer"
    );
    let created = created_of(&BlockId::parse(&section).unwrap());
    assert_eq!(
        created.imported_by.as_ref().map(|i| i.actor.as_str()),
        Some("human:imp")
    );
    assert_eq!(
        (created.author.actor.as_str(), created.author.run.as_deref()),
        ("agent:designer", Some("run-1")),
        "the section keeps the agent that wrote it"
    );
    let doc = c.read().resolve_id(&Scope::Canon, "doc-index").unwrap();
    let created = created_of(&doc);
    assert_eq!(
        created.imported_by.as_ref().map(|i| i.actor.as_str()),
        Some("human:imp")
    );
    assert_eq!(
        created.author.actor, "human:imp",
        "a Markdown block names the importing session"
    );
    check(&c, &Scope::Canon).unwrap();
    c.close().unwrap();
    b.close().unwrap();
}
