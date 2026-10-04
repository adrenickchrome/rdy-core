//! Export trees: symbolic links (sym1).
use rdy_engine::check_path;
use rdy_qa::fixtures::*;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::time::{Duration, Instant};

const DOC: &str = "- id: d-AAAAA\n  type: doc\n  order: a1\n  fields:\n    title: T\n    doc_type: glossary\n- id: s-AAAAA\n  type: section\n  parent: d-AAAAA\n  order: a1\n  fields:\n    title: Terms\n";
const FOLDER: &str = "id: fd-AAAAA\ntype: folder\norder: a1\n";

fn export_dir() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(
        t.path().join("rdy-export.yaml"),
        "format: 2\npack: rdy-standard\nscope: canon\n",
    )
    .unwrap();
    std::fs::create_dir_all(t.path().join("docs")).unwrap();
    t
}

fn pack() -> rdy_schema::Pack {
    rdy_schema::PackSet::with_builtin()
        .resolve("rdy-standard")
        .unwrap()
}

/// Import and rdy check both refuse `dir`, naming a symbolic link at line 1, quickly.
fn refuses_the_link(dir: &Path) {
    let (_d, mut r) = project();
    let t = Instant::now();
    let e = r.import(&approver(), "import", dir).unwrap_err();
    assert!(t.elapsed() < Duration::from_secs(5));
    assert_eq!(e.code, "E_FILE_FORMAT", "{e}");
    assert!(e.message.contains("symbolic link"), "{e}");
    let diags = check_path(&pack(), None, dir).unwrap();
    assert!(
        diags.iter().any(|d| d.code == "E_FILE_FORMAT"
            && d.line == 1
            && d.message.contains("symbolic link")),
        "{diags:?}"
    );
}

#[test]
fn sym1_a_symlinked_doc_file_is_refused_not_followed() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("x.yaml"), DOC).unwrap();
    let t = export_dir();
    symlink(outside.path().join("x.yaml"), t.path().join("docs/x.yaml")).unwrap();
    refuses_the_link(t.path());
}

#[test]
fn sym1_a_symlinked_folder_is_refused_not_followed() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(outside.path().join("area")).unwrap();
    std::fs::write(outside.path().join("area/_folder.yaml"), FOLDER).unwrap();
    std::fs::write(outside.path().join("area/x.yaml"), DOC).unwrap();
    let t = export_dir();
    symlink(outside.path().join("area"), t.path().join("docs/area")).unwrap();
    refuses_the_link(t.path());
}

#[test]
fn sym1_a_symlink_loop_is_refused_quickly() {
    let t = export_dir();
    symlink(t.path().join("docs"), t.path().join("docs/loop")).unwrap();
    refuses_the_link(t.path());
}

#[test]
fn sym1_a_symlinked_docs_root_is_refused() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("x.yaml"), DOC).unwrap();
    let t = export_dir();
    std::fs::remove_dir(t.path().join("docs")).unwrap();
    symlink(outside.path(), t.path().join("docs")).unwrap();
    refuses_the_link(t.path());
}

#[test]
fn sym1_a_symlinked_blobs_root_or_manifest_is_refused_by_import() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("big"), b"not a blob").unwrap();
    let t = export_dir();
    symlink(outside.path(), t.path().join("blobs")).unwrap();
    let (_d, mut r) = project();
    let e = r.import(&approver(), "import", t.path()).unwrap_err();
    assert_eq!(e.code, "E_FILE_FORMAT", "{e}");
    assert!(e.message.contains("symbolic link"), "{e}");

    let t = export_dir();
    let real = outside.path().join("m.yaml");
    std::fs::write(&real, "format: 2\npack: rdy-standard\nscope: canon\n").unwrap();
    std::fs::remove_file(t.path().join("rdy-export.yaml")).unwrap();
    symlink(&real, t.path().join("rdy-export.yaml")).unwrap();
    let e = r.import(&approver(), "import", t.path()).unwrap_err();
    assert_eq!(e.code, "E_FILE_FORMAT", "{e}");
    assert!(e.message.contains("symbolic link"), "{e}");
}
