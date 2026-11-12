//! m4bc4: a Markdown import captures a local source directory once, with bounds, and imports it as
//! one candidate through the shared importer (D-XR09R, D-19BZD, D-FSWQE, D-2R9EY).
//!
//! The capture lists the source through directory handles: it never follows a link, reads only
//! the included Markdown files and the root `.rdyignore`, and reports every other regular file
//! without opening it. A second pass checks each entry's identity and size, so a source that
//! changes while it is read refuses. The source digest binds every included path, kind and
//! captured byte; an `expected_source` from a preview refuses before any append or view claim
//! when the source changed since. A preview validates the whole candidate and writes nothing.
//!
//! The link and junction fixtures live in rdy-qa (`io_markdown_capture.rs`).
use rdy_engine::engine::{Engine, Local};
use rdy_engine::import_api::{ImportOptions, run_import_with};
use rdy_engine::markdown_import::files::{self, between_passes_for_tests};
use rdy_engine::*;
use rdy_model::{Hash, Role};
use rdy_store::Store;
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const VIEW: &str = "md";
const CHANGED_AFTER: (&str, &str, &str) = (
    E_BAD_INPUT,
    "the Markdown source changed after preview",
    "preview the source again and review the new report",
);
const IGNORE_FIX: &str = "shorten .rdyignore; one pattern can cover many files (**/*.draft.yaml)";

fn writer() -> Session {
    Session {
        actor: "human:imp".into(),
        run: None,
        role: Role::Writer,
        kind: SessionKind::Human,
    }
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

/// Writes `bytes` at the logical path `rel` under `root`, making its directories.
fn put(root: &Path, rel: &str, bytes: impl AsRef<[u8]>) {
    let path = native(root, rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn native(root: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(root.to_path_buf(), |p, s| p.join(s))
}

fn mkdir(root: &Path, rel: &str) {
    std::fs::create_dir_all(native(root, rel)).unwrap();
}

/// What the project holds durably, read through a fresh open after reindex: the log boundary,
/// every ref (views, canon, features), the frame count and the store's FEATURES file.
#[derive(Debug, PartialEq)]
struct Durable {
    end: Location,
    refs: Vec<(String, Vec<u8>)>,
    frames: usize,
    features: Vec<u8>,
}

fn durable(project: &Path) -> Durable {
    let store = project.join(".rdy").join("store");
    let mut s = Store::open(&store).unwrap();
    s.reindex().unwrap();
    Durable {
        end: s.log_end(),
        refs: s.reader().refs_with_prefix("").unwrap(),
        frames: s.verify().unwrap(),
        features: std::fs::read(store.join("FEATURES")).unwrap_or_default(),
    }
}

fn engine(f: &Fx) -> Local {
    Local::new(Rdy::open(&f.project).unwrap())
}

fn md(dry_run: bool, expected: Option<&str>) -> ImportOptions {
    ImportOptions {
        md: true,
        dry_run,
        expected_source: expected.map(str::to_string),
    }
}

fn preview(e: &Local, src: &Path) -> Result<Value, RdyError> {
    run_import_with(e, &writer(), VIEW, src, &md(true, None))
}

fn import(e: &Local, src: &Path, expected: Option<&str>) -> Result<Value, RdyError> {
    run_import_with(e, &writer(), VIEW, src, &md(false, expected))
}

fn triple(e: &RdyError) -> (&str, &str, &str) {
    (e.code, e.message.as_str(), e.fix.as_str())
}

/// `run` refuses with `want` and the project stays as it was, with no view `md`.
fn refuses_unchanged(
    f: &Fx,
    what: &str,
    want: (&str, &str, &str),
    run: impl FnOnce(&Local) -> Result<Value, RdyError>,
) {
    let was = durable(&f.project);
    let e = engine(f);
    let err = run(&e).unwrap_err();
    assert_eq!(triple(&err), want, "{what}: {err:?}");
    assert!(e.read().view_head(VIEW).is_none(), "{what}: no view claim");
    let open = e.inspect(|r| Ok(r.store().open_batch().is_some())).unwrap();
    assert!(!open, "{what}: no open batch");
    e.close().unwrap();
    assert_eq!(durable(&f.project), was, "{what}: the project is as before");
}

fn digest(src: &Path) -> String {
    files::capture(src).unwrap().source_digest
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

fn keys(v: &Value) -> BTreeSet<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

#[test]
fn m4bc4_preview_has_no_store_view_or_receipt_effect() {
    let f = fx();
    put(
        &f.src,
        "index.md",
        "# Home\n\nSee [the start](guide/start.md).\n",
    );
    put(&f.src, "guide/start.md", "# Start\n\nText.\n");
    put(&f.src, "img.png", [0x89, b'P', b'N', b'G']);
    let was = durable(&f.project);
    let e = engine(&f);
    let briefs = e.briefs().len();
    let v = preview(&e, &f.src).unwrap();
    let want: BTreeSet<String> = ["blocks", "dry_run", "files", "source_digest", "unmapped"]
        .map(String::from)
        .into();
    assert_eq!(keys(&v), want, "no receipt or change id: {v}");
    assert_eq!(v["dry_run"], true);
    let d = v["source_digest"].as_str().unwrap();
    assert_eq!(d, digest(&f.src));
    assert!(
        d.len() == 64
            && d.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    // one folder, and two docs of a section and a prose each
    assert_eq!(v["blocks"], 7, "{v}");
    let files = v["files"].as_array().unwrap();
    let paths: Vec<&str> = files.iter().map(|r| r["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["guide/start.md", "index.md"]);
    for r in files {
        assert_eq!(
            keys(r),
            ["blocks", "doc_key", "path"].map(String::from).into()
        );
        assert_eq!(r["blocks"], 3, "the doc and its two blocks: {r}");
        assert!(!r["doc_key"].as_str().unwrap().is_empty(), "{r}");
    }
    assert_eq!(
        v["unmapped"],
        json!([{"path": "img.png", "line": 0, "kind": "non_markdown_file"}])
    );
    assert_eq!(e.briefs().len(), briefs, "no briefing registered");
    assert!(
        e.read().view_head(VIEW).is_none(),
        "no view created or claimed"
    );
    let open = e.inspect(|r| Ok(r.store().open_batch().is_some())).unwrap();
    assert!(!open);
    e.close().unwrap();
    assert_eq!(durable(&f.project), was, "the preview wrote nothing");

    // the import of the same source reports the same files, digest and rows, with receipts
    let e = engine(&f);
    let got = import(&e, &f.src, Some(d)).unwrap();
    assert_eq!(got["files"], v["files"]);
    assert_eq!(got["source_digest"], v["source_digest"]);
    assert_eq!(got["unmapped"], v["unmapped"]);
    assert!(got.get("dry_run").is_none(), "{got}");
    assert!(got.get("briefing").is_some(), "{got}");
    assert_eq!(
        e.briefs().len(),
        briefs + 1,
        "the import registers its receipt"
    );
    e.close().unwrap();
}

#[test]
fn m4bc4_digest_binds_paths_bytes_and_ignore_rules() {
    let f = fx();
    let src = &f.src;
    put(src, "a.md", "# A\n");
    put(src, "sub/b.markdown", "b text\n");
    mkdir(src, "empty");
    put(src, "notes.txt", "not read");
    put(src, ".rdyignore", "*.tmp\n");
    put(src, "x.tmp", "ignored");
    // .rdy and .git are always left out
    put(src, ".git/HEAD", "ref: x\n");
    put(src, ".rdy/x", "y");
    let c = files::capture(src).unwrap();
    assert_eq!(c.dirs, ["empty", "sub"]);
    assert_eq!(c.reported, ["notes.txt"]);
    let paths: Vec<&str> = c.files.iter().map(|m| m.path.as_str()).collect();
    assert_eq!(paths, ["a.md", "sub/b.markdown"]);
    assert_eq!(c.ignore, b"*.tmp\n");
    let d0 = spelled(&[
        (2, ".rdyignore", b"*.tmp\n"),
        (0, "a.md", b"# A\n"),
        (3, "empty", b""),
        (1, "notes.txt", b""),
        (3, "sub", b""),
        (0, "sub/b.markdown", b"b text\n"),
    ]);
    assert_eq!(c.source_digest, d0, "the digest is the contract's input");

    // the native root is not part of it
    let other = tempfile::tempdir().unwrap();
    for (rel, bytes) in [
        ("a.md", &b"# A\n"[..]),
        ("sub/b.markdown", b"b text\n"),
        ("notes.txt", b"other bytes, never read"),
        (".rdyignore", b"*.tmp\n"),
    ] {
        put(other.path(), rel, bytes);
    }
    mkdir(other.path(), "empty");
    assert_eq!(
        digest(other.path()),
        d0,
        "same paths and bytes, another root"
    );

    // an ignored file, or one under .git, does not count
    put(src, "y.tmp", "ignored too");
    put(src, ".git/ORIG_HEAD", "z");
    assert_eq!(digest(src), d0);

    let changed = |what: &str, src: &Path| {
        let d = digest(src);
        assert_ne!(d, d0, "{what} changes the digest");
        d
    };
    // one Markdown byte
    put(src, "a.md", "# B\n");
    changed("a Markdown byte", src);
    put(src, "a.md", "# A\n");
    assert_eq!(digest(src), d0);
    // a reported file's path
    std::fs::rename(src.join("notes.txt"), src.join("notes2.txt")).unwrap();
    changed("a renamed reported file", src);
    std::fs::rename(src.join("notes2.txt"), src.join("notes.txt")).unwrap();
    assert_eq!(digest(src), d0);
    // the ignore file's exact bytes, a comment too
    put(src, ".rdyignore", "*.tmp\n# a comment\n");
    changed("an ignore comment", src);
    // an ignore rule that leaves a file out
    put(src, ".rdyignore", "*.tmp\nnotes.txt\n");
    let without_notes = changed("an ignore rule", src);
    assert_eq!(
        without_notes,
        spelled(&[
            (2, ".rdyignore", b"*.tmp\nnotes.txt\n"),
            (0, "a.md", b"# A\n"),
            (3, "empty", b""),
            (3, "sub", b""),
            (0, "sub/b.markdown", b"b text\n"),
        ])
    );
    // no ignore file: an empty .rdyignore entry, and the .tmp files are included again
    std::fs::remove_file(src.join(".rdyignore")).unwrap();
    std::fs::remove_file(src.join("x.tmp")).unwrap();
    std::fs::remove_file(src.join("y.tmp")).unwrap();
    assert_eq!(
        digest(src),
        spelled(&[
            (2, ".rdyignore", b""),
            (0, "a.md", b"# A\n"),
            (3, "empty", b""),
            (1, "notes.txt", b""),
            (3, "sub", b""),
            (0, "sub/b.markdown", b"b text\n"),
        ])
    );
}

#[test]
fn m4bc4_empty_folder_add_remove_or_rename_after_preview_refuses() {
    let f = fx();
    put(&f.src, "a.md", "# A\n\nText.\n");
    mkdir(&f.src, "keep");
    let e = engine(&f);
    let d0 = preview(&e, &f.src).unwrap()["source_digest"]
        .as_str()
        .unwrap()
        .to_string();
    e.close().unwrap();

    let stale = |f: &Fx, what: &str, d0: &str| {
        let now = digest(&f.src);
        assert_ne!(now, d0, "{what} changes the digest");
        let src = f.src.clone();
        let d = d0.to_string();
        refuses_unchanged(f, what, CHANGED_AFTER, move |e| import(e, &src, Some(&d)));
        now
    };
    // add an empty folder
    mkdir(&f.src, "new");
    stale(&f, "an added empty folder", &d0);
    std::fs::remove_dir(f.src.join("new")).unwrap();
    assert_eq!(digest(&f.src), d0);
    // remove one
    std::fs::remove_dir(f.src.join("keep")).unwrap();
    stale(&f, "a removed empty folder", &d0);
    // rename one
    mkdir(&f.src, "kept");
    let now = stale(&f, "a renamed empty folder", &d0);

    // the current digest imports, and the empty folder becomes a folder
    let e = engine(&f);
    import(&e, &f.src, Some(&now)).unwrap();
    let tree = e
        .read()
        .tree(&Scope::View(VIEW.into()), None, None)
        .unwrap();
    let names: Vec<(&str, &str)> = tree
        .iter()
        .map(|n| (n.name.as_str(), n.block_type.as_str()))
        .collect();
    assert_eq!(names, [("a", "doc"), ("kept", "folder")]);
    e.close().unwrap();
}

/// A `.rdyignore` of exactly `bytes` bytes with `patterns` patterns, padded with one comment.
fn ignore_of(bytes: usize, patterns: usize) -> String {
    let mut text: String = (0..patterns).map(|i| format!("p{i:04}.tmp\n")).collect();
    let pad = bytes - text.len() - 2;
    text.push('#');
    text.push_str(&"x".repeat(pad));
    text.push('\n');
    assert_eq!(text.len(), bytes);
    text
}

#[test]
fn m4bc4_ignore_accepts_65536_bytes_and_refuses_65537_with_e_config() {
    let f = fx();
    put(&f.src, "a.md", "# A\n");
    put(&f.src, "p0001.tmp", "ignored");
    put(&f.src, ".rdyignore", ignore_of(65_536, 10));
    let e = engine(&f);
    let v = preview(&e, &f.src).unwrap();
    assert_eq!(
        v["unmapped"],
        json!([]),
        "the ignored file is not reported: {v}"
    );
    e.close().unwrap();

    put(&f.src, ".rdyignore", ignore_of(65_537, 10));
    let over = (E_CONFIG, ".rdyignore is over 65536 bytes", IGNORE_FIX);
    let src = f.src.clone();
    refuses_unchanged(&f, "preview over the byte limit", over, |e| {
        preview(e, &src)
    });
    refuses_unchanged(&f, "import over the byte limit", over, |e| {
        import(e, &src, None)
    });

    put(&f.src, ".rdyignore", ignore_of(65_536, 10));
    let e = engine(&f);
    let v = import(&e, &f.src, None).unwrap();
    assert_eq!(v["unmapped"], json!([]), "{v}");
    e.close().unwrap();
}

#[test]
fn m4bc4_ignore_accepts_1024_patterns_and_refuses_1025_with_e_config() {
    // comments and empty lines do not count
    let with = |n: usize| {
        let mut text = String::from("# patterns\n\n");
        for i in 0..n {
            text.push_str(&format!("p{i:04}.tmp\n"));
            if i % 100 == 0 {
                text.push_str("# more\n\n");
            }
        }
        assert!(text.len() < 65_536);
        text
    };
    let f = fx();
    put(&f.src, "a.md", "# A\n");
    put(&f.src, "p1023.tmp", "ignored");
    put(&f.src, ".rdyignore", with(1024));
    let e = engine(&f);
    let v = preview(&e, &f.src).unwrap();
    assert_eq!(v["unmapped"], json!([]), "{v}");
    e.close().unwrap();

    put(&f.src, ".rdyignore", with(1025));
    let over = (E_CONFIG, ".rdyignore is over 1024 patterns", IGNORE_FIX);
    let src = f.src.clone();
    refuses_unchanged(&f, "preview over the pattern limit", over, |e| {
        preview(e, &src)
    });
    refuses_unchanged(&f, "import over the pattern limit", over, |e| {
        import(e, &src, None)
    });

    put(&f.src, ".rdyignore", with(1024));
    let e = engine(&f);
    import(&e, &f.src, None).unwrap();
    e.close().unwrap();
}

#[test]
fn m4bc4_changed_or_escaping_sources_refuse_without_a_write() {
    let changed = |path: &str| {
        (
            E_BAD_INPUT,
            format!("Markdown source changed while reading {path}"),
            "copy the source to a private directory and retry",
        )
    };
    type Change = fn(&Path);
    let cases: [(&str, &str, Change); 6] = [
        ("a Markdown file grows", "a.md", |s| {
            put(s, "a.md", "# A\n\nText.\nMore text.\n")
        }),
        (
            "a Markdown file is replaced with one of the same size",
            "b.md",
            |s| {
                put(s, "b.tmp-new", "# C\n");
                std::fs::rename(s.join("b.tmp-new"), s.join("b.md")).unwrap();
            },
        ),
        ("a file is added", "c.md", |s| put(s, "c.md", "# C\n")),
        ("an empty folder is removed", "d", |s| {
            std::fs::remove_dir(s.join("d")).unwrap()
        }),
        ("a reported file is removed", "n.txt", |s| {
            std::fs::remove_file(s.join("n.txt")).unwrap()
        }),
        ("the ignore file changes", ".rdyignore", |s| {
            put(s, ".rdyignore", "*.tmp\n# more\n")
        }),
    ];
    for (what, path, change) in cases {
        for dry_run in [true, false] {
            let f = fx();
            put(&f.src, "a.md", "# A\n\nText.\n");
            put(&f.src, "b.md", "# B\n");
            put(&f.src, "n.txt", "x");
            put(&f.src, ".rdyignore", "*.tmp\n");
            mkdir(&f.src, "d");
            let src = f.src.clone();
            let (code, message, fix) = changed(path);
            refuses_unchanged(&f, what, (code, &message, fix), move |e| {
                let at = src.clone();
                between_passes_for_tests(Box::new(move || change(&at)));
                run_import_with(e, &writer(), VIEW, &src, &md(dry_run, None))
            });
        }
    }

    // a native name that is not one logical segment (a `\` would climb out of the source on
    // Windows, where no such name can exist) refuses as an unsafe source path
    if !cfg!(windows) {
        for name in ["..\\up.md", "..\\up"] {
            let f = fx();
            put(&f.src, "a.md", "# A\n");
            if name.ends_with(".md") {
                std::fs::write(f.src.join(name), "# Up\n").unwrap();
            } else {
                std::fs::create_dir(f.src.join(name)).unwrap();
            }
            let message = format!(
                "Markdown source path is not a contained regular file or directory: {name}"
            );
            let src = f.src.clone();
            refuses_unchanged(
                &f,
                name,
                (
                    E_BAD_INPUT,
                    &message,
                    "copy regular files into a private source directory",
                ),
                move |e| import(e, &src, None),
            );
        }
    }
    // a Markdown option without md refuses before anything is read
    let f = fx();
    put(&f.src, "a.md", "# A\n");
    let src = f.src.clone();
    refuses_unchanged(
        &f,
        "dry_run without md",
        (
            E_BAD_INPUT,
            "dry_run and expected_source require md: true",
            "use rdy_import with md: true, or rdy import --md",
        ),
        move |e| {
            let options = ImportOptions {
                md: false,
                dry_run: true,
                expected_source: None,
            };
            run_import_with(e, &writer(), VIEW, &src, &options)
        },
    );
    // a malformed expected digest refuses before the source is read
    let f = fx();
    put(&f.src, "a.md", "# A\n");
    let src = f.src.clone();
    let upper = "A".repeat(64);
    refuses_unchanged(
        &f,
        "a malformed digest",
        (
            E_BAD_INPUT,
            "expected_source must be 64 lowercase hexadecimal characters",
            "use source_digest from a Markdown import preview",
        ),
        move |e| import(e, &src, Some(&upper)),
    );
}

#[test]
fn m4bc4_native_paths_case_collisions_and_crlf_are_portable() {
    let f = fx();
    put(
        &f.src,
        "Getting Started/Ünïcode notes.md",
        "# Café\r\n\r\nLine one\r\nline two\r\n",
    );
    put(
        &f.src,
        "Guide.MD",
        "# Guide\n\nSee [notes](Getting%20Started/%C3%9Cn%C3%AFcode%20notes.md).\n",
    );
    put(&f.src, "Extra.Markdown", "Plain text.\n");
    put(&f.src, "docs/space name.md", "# Spaced\n");
    let e = engine(&f);
    let v = import(&e, &f.src, None).unwrap();
    let paths: Vec<&str> = v["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        [
            "Extra.Markdown",
            "Getting Started/Ünïcode notes.md",
            "Guide.MD",
            "docs/space name.md"
        ],
        "logical paths, `/` between segments, sorted by UTF-8 bytes"
    );
    assert_eq!(v["unmapped"], json!([]), "the link resolved: {v}");
    let st = e.read();
    let scope = Scope::View(VIEW.into());
    let m = st.materialized(&scope).unwrap();
    let doc = m
        .iter()
        .find(|b| b.block_type == "doc" && b.field_str("title") == Some("Café"))
        .expect("the CRLF doc took its heading as title");
    let prose: Vec<&str> = m
        .iter()
        .filter(|b| b.block_type == "prose" && m.get(b.parent.as_ref().unwrap()).is_some())
        .map(|b| b.body.as_str())
        .collect();
    assert!(prose.contains(&"Line one\nline two"), "{prose:?}");
    assert!(prose.iter().all(|p| !p.contains('\r')), "{prose:?}");
    assert!(
        m.iter()
            .any(|b| b.body.contains(&format!("{{{{{}", doc.id))),
        "the link is a ref"
    );
    drop(st);
    e.close().unwrap();

    // two paths that differ only in case collide; a file system that cannot hold both skips
    for (first, second, make) in [("Same.md", "same.md", true), ("Docs", "docs", false)] {
        let f = fx();
        put(&f.src, "a.md", "# A\n");
        for name in [first, second] {
            if make {
                std::fs::write(f.src.join(name), "# X\n").unwrap();
            } else {
                std::fs::create_dir(f.src.join(name)).unwrap_or(());
            }
        }
        let listed = std::fs::read_dir(&f.src).unwrap().count();
        if listed < 3 {
            eprintln!("{first}/{second}: this file system folds case; the collision cannot exist");
            continue;
        }
        let message = format!("Markdown source paths collide: {first}, {second}");
        let src = f.src.clone();
        refuses_unchanged(
            &f,
            first,
            (
                E_BAD_INPUT,
                &message,
                "rename one source path and update its links",
            ),
            move |e| import(e, &src, None),
        );
    }
}

#[test]
fn m4bc4_markdown_import_is_one_batch_with_coverage_and_onboarding() {
    let f = fx();
    put(
        &f.src,
        "index.md",
        "# Home\n\nIntro [start](guide/start.md#steps).\n",
    );
    put(
        &f.src,
        "guide/start.md",
        "# Start\n\n## Steps\n\n```sh\nrdy init\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n",
    );
    put(&f.src, "logo.svg", "<svg/>");
    let was = durable(&f.project);
    let e = engine(&f);
    let v = import(&e, &f.src, None).unwrap();
    assert_eq!(
        v["changes"].as_array().map(Vec::len),
        Some(1),
        "one change: {v}"
    );
    assert!(v["source_digest"].is_string(), "{v}");
    assert_eq!(v["files"].as_array().map(Vec::len), Some(2), "{v}");
    assert_eq!(
        v["unmapped"],
        json!([{"path": "logo.svg", "line": 0, "kind": "non_markdown_file"}])
    );
    assert!(v.get("coverage").is_some(), "{v}");
    assert!(v.get("onboarding").is_some(), "{v}");
    assert!(v.get("coverage_error").is_none(), "{v}");
    // folder guide; doc index with section and prose; doc start with two sections, code, table
    assert_eq!(v["created"].as_object().map(|o| o.len()), Some(9), "{v}");
    let (batch, frames) = e
        .inspect(|r| Ok((r.store().last_batch().len(), r.store().verify().unwrap())))
        .unwrap();
    assert_eq!(
        batch,
        frames - was.frames,
        "every new frame is in the one sealed batch"
    );
    let owner = e.read().view_head(VIEW).and_then(|h| h.owner.clone());
    assert_eq!(owner.map(|o| o.actor).as_deref(), Some("human:imp"));
    e.close().unwrap();
    let after = durable(&f.project);
    assert_eq!(
        after.features, was.features,
        "FEATURES unchanged: no foreign_copy"
    );
    assert_ne!(after.end, was.end);
}

#[test]
fn m4bc4_repeated_import_never_overwrites_existing_content() {
    let f = fx();
    put(&f.src, "notes.md", "# Notes\n\nFrom Markdown.\n");
    let e = engine(&f);
    import(&e, &f.src, None).unwrap();
    e.close().unwrap();
    // the same source again: the existing name refuses; nothing is replaced
    let src = f.src.clone();
    let was = durable(&f.project);
    let e = engine(&f);
    let err = import(&e, &src, None).unwrap_err();
    assert_eq!(err.code, E_PATH_CLASH, "{err:?}");
    e.close().unwrap();
    assert_eq!(durable(&f.project), was);

    // a populated destination: a doc written before the import keeps its content
    let f = fx();
    let mut r = Rdy::open(&f.project).unwrap();
    r.new_doc(&writer(), VIEW, "notes", Some("page"), Some("Mine"))
        .unwrap();
    let id = r
        .read()
        .resolve_path(&Scope::View(VIEW.into()), "notes")
        .unwrap();
    let mine = r.read().block(&Scope::View(VIEW.into()), &id).unwrap();
    r.close().unwrap();
    put(&f.src, "notes.md", "# Notes\n\nFrom Markdown.\n");
    let was = durable(&f.project);
    let e = engine(&f);
    let err = import(&e, &f.src, None).unwrap_err();
    assert_eq!(err.code, E_PATH_CLASH, "{err:?}");
    e.close().unwrap();
    assert_eq!(durable(&f.project), was);
    // another name imports beside it
    std::fs::rename(f.src.join("notes.md"), f.src.join("other.md")).unwrap();
    let e = engine(&f);
    import(&e, &f.src, None).unwrap();
    let st = e.read();
    let scope = Scope::View(VIEW.into());
    assert_eq!(
        st.block(&scope, &id).unwrap(),
        mine,
        "the existing doc is unchanged"
    );
    assert!(st.resolve_path(&scope, "other").is_ok());
    drop(st);
    e.close().unwrap();
}

#[test]
fn m4bc4_a_non_markdown_file_row_has_line_zero() {
    let f = fx();
    put(&f.src, "a.md", "# A\n\n<b>html</b>\n");
    put(&f.src, "pics/one.png", [1, 2, 3]);
    put(&f.src, "z.txt", "text");
    let e = engine(&f);
    let v = preview(&e, &f.src).unwrap();
    assert_eq!(
        v["unmapped"],
        json!([
            {"path": "a.md", "line": 3, "kind": "html"},
            {"path": "pics/one.png", "line": 0, "kind": "non_markdown_file"},
            {"path": "z.txt", "line": 0, "kind": "non_markdown_file"},
        ])
    );
    e.close().unwrap();
}
