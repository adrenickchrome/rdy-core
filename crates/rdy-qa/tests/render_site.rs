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
    use std::os::unix::fs::MetadataExt;
    let (_d, mut r, _doc) = one_doc();
    let parent = tempfile::tempdir().unwrap();
    let target = parent.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("keep.txt"), "mine").unwrap();
    // A marked target passes the marker gate, so only the link guard stands in the way.
    std::fs::write(target.join(rdy_render::SITE_MARKER), "old").unwrap();
    let inode = std::fs::metadata(&target).unwrap().ino();
    let link = parent.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    // Tab-completion appends a slash to a link to a directory, and `/.` names the same thing.
    for spelled in ["link", "link/", "link/."] {
        let out = parent.path().join(spelled);
        let e = rdy_render::render_site(&mut r, &Scope::Canon, &out).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{spelled}: {e}");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "{spelled}"
        );
        assert_eq!(std::fs::read_link(&link).unwrap(), target, "{spelled}");
        assert_eq!(
            std::fs::read_to_string(target.join("keep.txt")).unwrap(),
            "mine",
            "{spelled}"
        );
        assert_eq!(
            std::fs::read_dir(&target).unwrap().count(),
            2,
            "{spelled}: nothing was rendered into the target"
        );
        assert_eq!(
            std::fs::read_to_string(target.join(rdy_render::SITE_MARKER)).unwrap(),
            "old",
            "{spelled}"
        );
        assert_eq!(
            std::fs::metadata(&target).unwrap().ino(),
            inode,
            "{spelled}: the target was replaced"
        );
    }
    let names: Vec<_> = std::fs::read_dir(parent.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 2, "no staging left beside the link: {names:?}");
}

/// A parent that cannot be written (a mount point's parent, `/var/www` over a user-owned site).
/// Restores the mode on drop so the temporary directory can be removed.
#[cfg(unix)]
struct ReadOnly(std::path::PathBuf);
#[cfg(unix)]
impl ReadOnly {
    fn new(dir: &std::path::Path) -> Option<ReadOnly> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let guard = ReadOnly(dir.to_path_buf());
        // root ignores the mode: nothing to test then
        let probe = dir.join(".probe");
        if std::fs::write(&probe, "").is_ok() {
            std::fs::remove_file(probe).unwrap();
            return None;
        }
        Some(guard)
    }
}
#[cfg(unix)]
impl Drop for ReadOnly {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

#[cfg(unix)]
#[test]
fn ren1_an_empty_out_under_an_unwritable_parent_is_rendered_into() {
    let (_d, mut r, _doc) = one_doc();
    let parent = tempfile::tempdir().unwrap();
    let ro = parent.path().join("ro");
    let site = ro.join("site");
    std::fs::create_dir_all(&site).unwrap();
    let Some(_guard) = ReadOnly::new(&ro) else {
        return;
    };
    rdy_render::render_site(&mut r, &Scope::Canon, &site).unwrap();
    assert!(site.join("index.html").exists());
    assert!(site.join(rdy_render::SITE_MARKER).exists());
    let left: Vec<_> = std::fs::read_dir(&site)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        !left.iter().any(|n| n.contains("rdy-render")),
        "no staging left: {left:?}"
    );
}

#[cfg(unix)]
#[test]
fn ren1_a_rerender_under_an_unwritable_parent_replaces_the_site() {
    let (_d, mut r, doc) = one_doc();
    let parent = tempfile::tempdir().unwrap();
    let ro = parent.path().join("ro");
    let site = ro.join("site");
    rdy_render::render_site(&mut r, &Scope::Canon, &site).unwrap();
    assert!(site.join("tx.html").exists());
    std::fs::create_dir(site.join("stale-dir")).unwrap();
    std::fs::write(site.join("stale-dir").join("x"), "x").unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": doc, "fields": {"name": "renamed"}}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let Some(_guard) = ReadOnly::new(&ro) else {
        return;
    };
    rdy_render::render_site(&mut r, &Scope::Canon, &site).unwrap();
    assert!(site.join("renamed.html").exists());
    assert!(!site.join("tx.html").exists(), "the stale page is gone");
    assert!(
        !site.join("stale-dir").exists(),
        "the stale directory is gone"
    );
    assert!(site.join(rdy_render::SITE_MARKER).exists());
    let left: Vec<_> = std::fs::read_dir(&site)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        !left.iter().any(|n| n.contains("rdy-render")),
        "no staging left: {left:?}"
    );
}

/// A failed unwritable swap leaves a refusal that names the path.
#[cfg(unix)]
#[test]
fn ren1_an_unwritable_new_out_names_the_path() {
    let (_d, mut r, _doc) = one_doc();
    let parent = tempfile::tempdir().unwrap();
    let ro = parent.path().join("readonly-parent");
    std::fs::create_dir(&ro).unwrap();
    let Some(_guard) = ReadOnly::new(&ro) else {
        return;
    };
    let site = ro.join("site");
    let e = rdy_render::render_site(&mut r, &Scope::Canon, &site).unwrap_err();
    assert_eq!(e.code, "E_IO", "{e}");
    assert!(
        e.to_string().contains("readonly-parent"),
        "names the path: {e}"
    );
    assert!(!site.exists());
}

/// The rename of `out` is refused (here: the old copy from a crashed render can't be cleared, as
/// a busy mount can't be moved). The staged site is moved in place, and no staging is left.
#[cfg(unix)]
#[test]
fn ren1_a_refused_swap_falls_back_to_replacing_in_place() {
    let (_d, mut r, doc) = one_doc();
    let parent = tempfile::tempdir().unwrap();
    let site = parent.path().join("site");
    rdy_render::render_site(&mut r, &Scope::Canon, &site).unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": doc, "fields": {"name": "renamed"}}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let old = parent.path().join(".site.rdy-render-old");
    std::fs::create_dir(&old).unwrap();
    std::fs::write(old.join("x"), "x").unwrap();
    let Some(_guard) = ReadOnly::new(&old) else {
        return;
    };
    rdy_render::render_site(&mut r, &Scope::Canon, &site).unwrap();
    assert!(site.join("renamed.html").exists());
    assert!(!site.join("tx.html").exists());
    assert!(site.join(rdy_render::SITE_MARKER).exists());
    assert!(
        !parent.path().join(".site.rdy-render-partial").exists(),
        "no staging left"
    );
}
