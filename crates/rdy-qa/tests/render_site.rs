//! Rendering into an existing site (ren1).
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::json;

fn one_doc() -> (tempfile::TempDir, Rdy, String) {
    let (d, mut r) = project();
    let doc = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "parent": "$d", "fields": {"title": "Terms"}}
    ]))
    .unwrap()
    .created["$d"]
    .clone();
    land(&mut r, "work").unwrap();
    (d, r, doc)
}

#[test]
fn ren1_rerendering_replaces_the_site_and_drops_stale_pages() {
    let (_d, mut r, doc) = one_doc();
    let parent = tempfile::tempdir().unwrap();
    let site = parent.path().join("site");
    rdy_render::render_site(&mut r, &Scope::Canon, &site).unwrap();
    assert!(site.join("tx.html").exists());
    assert!(site.join(rdy_render::SITE_MARKER).exists());
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": doc, "fields": {"name": "renamed"}}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, &site).unwrap();
    assert!(site.join("renamed.html").exists());
    assert!(!site.join("tx.html").exists(), "the stale page is gone");
    let left: Vec<_> = std::fs::read_dir(parent.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        left,
        vec![std::ffi::OsString::from("site")],
        "no staging or old copy left"
    );
}

/// Includes Review Focus 4: an existing file as --out is refused and untouched.
#[test]
fn ren1_a_non_empty_directory_that_is_not_a_site_is_refused_untouched() {
    let (_d, mut r, _doc) = one_doc();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "mine").unwrap();
    let e = rdy_render::render_site(&mut r, &Scope::Canon, dir.path()).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
        "mine"
    );
    assert!(!dir.path().join(rdy_render::SITE_MARKER).exists());
    let file = dir.path().join("notes.txt");
    let e = rdy_render::render_site(&mut r, &Scope::Canon, &file).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "mine");
}

#[test]
fn ren1_an_empty_directory_is_rendered_into() {
    let (_d, mut r, _doc) = one_doc();
    let dir = tempfile::tempdir().unwrap();
    rdy_render::render_site(&mut r, &Scope::Canon, dir.path()).unwrap();
    assert!(dir.path().join("index.html").exists());
    assert!(dir.path().join(rdy_render::SITE_MARKER).exists());
}

/// A destructive swap never follows a link: the link and its target stay as they were.
#[cfg(unix)]
#[test]
fn ren1_a_symlinked_out_is_refused_untouched() {
    let (_d, mut r, _doc) = one_doc();
    let parent = tempfile::tempdir().unwrap();
    let target = parent.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("keep.txt"), "mine").unwrap();
    let link = parent.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let e = rdy_render::render_site(&mut r, &Scope::Canon, &link).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_link(&link).unwrap(), target);
    assert_eq!(
        std::fs::read_to_string(target.join("keep.txt")).unwrap(),
        "mine"
    );
    assert_eq!(
        std::fs::read_dir(&target).unwrap().count(),
        1,
        "nothing was rendered into the target"
    );
    assert!(!target.join(rdy_render::SITE_MARKER).exists());
}
