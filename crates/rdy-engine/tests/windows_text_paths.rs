//! wx11: the working-copy and export boundaries on Windows text and paths. A working file may
//! carry CR LF line syntax; a carriage return that is content stays content; an export writes LF
//! and keeps blob bytes; a case-only rename keeps the file; a failed export rename is `E_IO`.
#[allow(dead_code)]
mod common;

use rdy_engine::wc::files::{checkout, dirty_paths};
use rdy_engine::wc::ignore::Ignore;
use rdy_engine::wc::layout::read_tree;
use rdy_engine::wc::project::{Projection, project_state};
use rdy_engine::wc::state::{Place, WcState, stage_put};
use rdy_engine::wc::status::compute;
use rdy_engine::*;
use rdy_model::*;
use rdy_store::pio::{Boundary, DurableStep, on_durable};
use serde_json::json;
use std::path::Path;

fn agent() -> Session {
    Session {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
        kind: SessionKind::Human,
    }
}

fn founder() -> Session {
    Session {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
        kind: SessionKind::Human,
    }
}

fn pack() -> rdy_schema::Pack {
    rdy_schema::PackSet::with_builtin()
        .resolve("rdy-standard")
        .unwrap()
}

/// A project with folder `guides`, doc `intro` and one prose whose content holds a CR LF pair.
fn project(dir: &Path, folder: &str, body: &str) -> Rdy {
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let blob = r.store_mut().put_blob(BLOB).unwrap().to_string();
    r.write(&agent(), WriteRequest { view: "work".into(), ops: serde_json::from_value(json!([
        {"op": "create", "type": "folder", "id": "$f", "fields": {"name": folder, "title": "Guides"}},
        {"op": "create", "type": "doc", "id": "$d", "parent": "$f", "fields": {"name": "intro", "title": "Intro", "doc_type": "components"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Start"}},
        {"op": "create", "type": "prose", "parent": "$s", "body": body},
        {"op": "create", "type": "prose", "parent": "$s", "body": "Plain second paragraph."},
        {"op": "create", "type": "figure", "parent": "$s", "fields": {"blob": blob, "media_type": "application/octet-stream", "alt": "bytes"}}
    ])).unwrap(), justification: None, message: String::new() }).unwrap();
    let p = r
        .propose(&agent(), "work", "seed", Some(common::TEST_SUMMARY))
        .unwrap();
    r.land(&founder(), &p.id).unwrap();
    r
}

fn projection(r: &Rdy) -> Projection {
    let m = r.read().materialized(&Scope::Canon).unwrap();
    project_state(&m, &pack(), "wc", "canon:test", None).unwrap()
}

/// Bytes with CR LF pairs, a lone CR and bytes that are not UTF-8.
const BLOB: &[u8] = b"\r\n\x00\xff\r\nbinary\r";
const LITERAL: &str = "Line one\r\nLine two";

/// The files of `proj` rewritten with CR LF line ends, as `core.autocrlf=true` checks them out.
fn to_crlf(root: &Path, proj: &Projection) {
    for f in &proj.files {
        let at = root.join(&f.path);
        let text = std::fs::read_to_string(&at).unwrap();
        assert!(
            !text.contains('\r'),
            "a written file holds a raw CR: {at:?}"
        );
        std::fs::write(&at, text.replace('\n', "\r\n")).unwrap();
    }
}

fn checked_out(r: &Rdy, wc: &Path) -> (Place, WcState, Projection) {
    let proj = projection(r);
    let place = Place::main(wc);
    let mut st = WcState::default();
    let tree = read_tree(&place.root, &Ignore::empty(), &pack());
    checkout(&place, &mut st, &tree, &proj, false).unwrap();
    let st = WcState::load(&place).unwrap();
    (place, st, proj)
}

#[test]
fn wx11_crlf_working_copy_has_no_spurious_change() {
    let d = tempfile::tempdir().unwrap();
    let wc = tempfile::tempdir().unwrap();
    let r = project(d.path(), "guides", LITERAL);
    let (place, st, proj) = checked_out(&r, wc.path());
    to_crlf(wc.path(), &proj);

    // CR LF line syntax is no change: not dirty, not modified, nothing to commit
    let tree = read_tree(&place.root, &Ignore::empty(), &pack());
    assert!(tree.diags.is_empty(), "{:?}", tree.diags.len());
    assert_eq!(dirty_paths(&place, &st, &tree), Vec::<String>::new());
    let status = compute(&place, &st, &tree);
    assert!(status.unstaged.is_empty(), "{:?}", status.unstaged);
    assert!(status.staged.is_empty(), "{:?}", status.staged);

    // a CR that is content stays content: the body reads back with its CR LF pair
    let bodies: Vec<&str> = tree.blocks.iter().map(|b| b.block.body.as_str()).collect();
    assert!(bodies.contains(&LITERAL), "{bodies:?}");

    // a staged copy takes the LF form, and the CR LF file is not changed since it was staged
    let intro = "guides/intro.yaml";
    let crlf = std::fs::read(place.root.join(intro)).unwrap();
    stage_put(&place, intro, &crlf).unwrap();
    assert!(
        !std::fs::read(place.stage_file(intro))
            .unwrap()
            .contains(&b'\r')
    );
    let committed = std::iter::once(intro.to_string()).collect();
    assert!(rdy_engine::wc::files::changed_since_staged(&place, &st, &committed).is_empty());

    // the content change is still a change: the escape for the CR dropped, the file in CR LF
    let at = place.root.join(intro);
    let text = std::fs::read_to_string(&at).unwrap();
    assert!(text.contains("Line one\\r\\nLine two"), "{text}");
    std::fs::write(
        &at,
        text.replace("Line one\\r\\nLine two", "Line one\\nLine two"),
    )
    .unwrap();
    let tree = read_tree(&place.root, &Ignore::empty(), &pack());
    let status = compute(&place, &st, &tree);
    let changed: Vec<&str> = status.unstaged.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(changed, vec![intro], "{status:?}");
    assert_eq!(dirty_paths(&place, &st, &tree), vec![intro.to_string()]);
}

#[test]
fn wx11_exports_use_lf_without_changing_blob_bytes() {
    let d = tempfile::tempdir().unwrap();
    let r = project(d.path(), "guides", LITERAL);
    let out = tempfile::tempdir().unwrap();
    let out = out.path().join("export");
    r.read().export(&Scope::Canon, &out).unwrap();

    // every text file the export writes has LF line ends and no raw CR
    fn files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                files(&p, out);
            } else {
                out.push(p);
            }
        }
    }
    let mut all = vec![];
    files(&out, &mut all);
    let blobs = out.join("blobs");
    let mut text_files = 0;
    for f in all.iter().filter(|f| !f.starts_with(&blobs)) {
        let bytes = std::fs::read(f).unwrap();
        assert!(!bytes.contains(&b'\r'), "{f:?} holds a raw CR");
        text_files += 1;
    }
    assert!(text_files >= 4, "{all:?}");

    // the blob is the stored bytes, CR LF pairs and all
    let on_disk: Vec<Vec<u8>> = std::fs::read_dir(&blobs)
        .unwrap()
        .map(|e| std::fs::read(e.unwrap().path()).unwrap())
        .collect();
    assert_eq!(on_disk, vec![BLOB.to_vec()]);

    // content with a CR LF pair comes back unchanged from the export
    let original = r
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .snapshot()
        .blocks;
    let b_dir = tempfile::tempdir().unwrap();
    let mut b = Rdy::init(b_dir.path(), "rdy-standard").unwrap();
    b.import(&founder(), "import", &out).unwrap();
    let p = b
        .propose(&founder(), "import", "migrate", Some(common::TEST_SUMMARY))
        .unwrap();
    b.land(&founder(), &p.id).unwrap();
    let imported = b
        .read()
        .materialized(&Scope::Canon)
        .unwrap()
        .snapshot()
        .blocks;
    assert_eq!(imported, original);
}

/// Takes the test backend of the durable steps off this thread when it drops.
struct NoDurableBackend;

impl Drop for NoDurableBackend {
    fn drop(&mut self) {
        on_durable(None);
    }
}

/// A failed final rename of an export is `E_IO` and names both paths. The target does not appear,
/// and the next export clears the staging directory and passes.
#[test]
fn wx11_failed_export_rename_is_io() {
    let d = tempfile::tempdir().unwrap();
    let r = project(d.path(), "guides", LITERAL);
    let parent = tempfile::tempdir().unwrap();
    let out = parent.path().join("export");
    let target = out.clone();
    let guard = NoDurableBackend;
    on_durable(Some(Box::new(move |step, at| match step {
        DurableStep::Rename { to, .. } if at == Boundary::Before && *to == target => {
            Err(std::io::Error::other("injected rename failure"))
        }
        _ => Ok(()),
    })));
    let e = r.read().export(&Scope::Canon, &out).unwrap_err();
    drop(guard);
    assert_eq!(e.code, "E_IO", "{e}");
    assert!(e.message.contains("injected rename failure"), "{e}");
    assert!(e.message.contains("could not rename"), "{}", e.message);
    assert!(
        e.message.contains(".export.rdy-export-partial"),
        "{}",
        e.message
    );
    assert!(std::fs::symlink_metadata(&out).is_err());
    let staging = parent.path().join(".export.rdy-export-partial");
    assert!(staging.is_dir());
    r.read().export(&Scope::Canon, &out).unwrap();
    assert!(out.join("docs").is_dir());
    assert!(std::fs::symlink_metadata(&staging).is_err());
}

/// A case-only rename of a folder, checked out again, round-trips: the files take the new
/// spelling, nothing is left under the old one and the content is kept. On a case-insensitive
/// volume the old and the new spelling are one entry, so a copy that is not renamed through a
/// temporary name would keep the old spelling or lose the file. This test changes only the
/// folder's spelling and has no failure during the temporary-name sequence.
#[test]
fn wx11_case_only_rename_round_trips() {
    let d = tempfile::tempdir().unwrap();
    let wc = tempfile::tempdir().unwrap();
    let r = project(d.path(), "guides", LITERAL);
    let (place, _st, first) = checked_out(&r, wc.path());
    let before: Vec<(String, String)> = first
        .files
        .iter()
        .map(|f| (f.path.clone(), f.text.clone()))
        .collect();

    // the same blocks under a folder spelled `Guides`
    let d2 = tempfile::tempdir().unwrap();
    let mut proj = first.clone();
    for f in &mut proj.files {
        f.path = f.path.replacen("guides/", "Guides/", 1);
    }
    drop(d2);
    let mut st = WcState::load(&place).unwrap();
    let tree = read_tree(&place.root, &Ignore::empty(), &pack());
    checkout(&place, &mut st, &tree, &proj, false).unwrap();

    let names: Vec<String> = std::fs::read_dir(&place.root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.eq_ignore_ascii_case("guides"))
        .collect();
    assert_eq!(names, vec!["Guides".to_string()]);
    let inner: Vec<String> = std::fs::read_dir(place.root.join("Guides"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(inner.contains(&"intro.yaml".to_string()), "{inner:?}");
    for (path, text) in &before {
        let moved = path.replacen("guides/", "Guides/", 1);
        assert_eq!(
            &std::fs::read_to_string(place.root.join(&moved)).unwrap(),
            text
        );
    }
    // the base copies follow the new spelling and the working copy is clean
    let st = WcState::load(&place).unwrap();
    assert!(
        st.base.files.contains_key("Guides/intro.yaml"),
        "{:?}",
        st.base.files
    );
    assert!(!st.base.files.contains_key("guides/intro.yaml"));
    let tree = read_tree(&place.root, &Ignore::empty(), &pack());
    assert_eq!(dirty_paths(&place, &st, &tree), Vec::<String>::new());

    // and back
    let mut st = WcState::load(&place).unwrap();
    checkout(&place, &mut st, &tree, &first, false).unwrap();
    let names: Vec<String> = std::fs::read_dir(&place.root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.eq_ignore_ascii_case("guides"))
        .collect();
    assert_eq!(names, vec!["guides".to_string()]);
}

/// A path with a backslash or a drive letter is refused with one diagnostic on every host.
#[test]
fn wx11_backslash_and_drive_paths_are_refused_on_every_host() {
    let d = tempfile::tempdir().unwrap();
    let wc = tempfile::tempdir().unwrap();
    let r = project(d.path(), "guides", LITERAL);
    let (place, _st, mut proj) = checked_out(&r, wc.path());
    for bad in [
        "guides\\intro.yaml",
        "C:/intro.yaml",
        "C:intro.yaml",
        "a//b.yaml",
        "../x.yaml",
    ] {
        proj.files[0].path = bad.to_string();
        let mut st = WcState::default();
        let tree = read_tree(&place.root, &Ignore::empty(), &pack());
        let e = checkout(&place, &mut st, &tree, &proj, true).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{bad}: {e}");
        assert!(
            e.message.contains("is not a path inside the working copy"),
            "{bad}: {e}"
        );
    }
}

#[path = "../../rdy-daemon/build_support.rs"]
mod build_support;

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=rdy",
            "-c",
            "user.email=rdy@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            &format!(
                "core.hooksPath={}",
                rdy_engine::code_index::empty_hooks_dir().display()
            ),
        ])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs")
}

/// A checkout with `core.autocrlf=true` of the source files that build_support hashes gives the
/// same source build id as the files committed: the text kinds are pinned to LF and the released
/// binary fixtures are not converted.
#[test]
fn wx11_autocrlf_checkout_keeps_the_source_build_id() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let listed = git(
        &root,
        &["ls-files", "-z", "--", "Cargo.toml", "Cargo.lock", "crates"],
    );
    if !listed.status.success() || !root.join(".gitattributes").is_file() {
        return; // not a git checkout (a packaged source tree)
    }
    let copy = tempfile::tempdir().unwrap();
    let copy = copy.path();
    for rel in listed.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let rel = std::str::from_utf8(rel).unwrap();
        let from = root.join(rel);
        if !from.is_file() {
            continue;
        }
        let to = copy.join(rel);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(&from, &to).unwrap();
    }
    std::fs::copy(root.join(".gitattributes"), copy.join(".gitattributes")).unwrap();
    let want = build_support::source_build_id(copy).unwrap();

    assert!(git(copy, &["init", "-q"]).status.success());
    assert!(
        git(copy, &["-c", "core.autocrlf=false", "add", "-A"])
            .status
            .success()
    );
    let c = git(
        copy,
        &["-c", "core.autocrlf=false", "commit", "-q", "-m", "x"],
    );
    assert!(c.status.success(), "{}", String::from_utf8_lossy(&c.stderr));
    std::fs::remove_dir_all(copy.join("crates")).unwrap();
    std::fs::remove_file(copy.join("Cargo.toml")).unwrap();
    std::fs::remove_file(copy.join("Cargo.lock")).unwrap();
    let out = git(
        copy,
        &[
            "-c",
            "core.autocrlf=true",
            "checkout",
            "-f",
            "HEAD",
            "--",
            ".",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(build_support::source_build_id(copy).unwrap(), want);
}
