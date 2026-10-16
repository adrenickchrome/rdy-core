#![cfg(unix)]
// Unix only: it sets file modes and makes symlinks. FW-XP1 Phase 4 ports it.
//! Rendering into an existing site (ren1).
use rdy_engine::*;
use rdy_qa::fixtures::*;
use serde_json::json;

fn one_doc() -> (tempfile::TempDir, Rdy, String) {
    let (d, mut r) = project();
    let doc = write(&mut r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Tx", "doc_type": "page"}},
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
    rdy_render::render_site(&r.read(), &Scope::Canon, &site).unwrap();
    assert!(site.join("tx.html").exists());
    assert!(site.join(rdy_render::SITE_MARKER).exists());
    write(
        &mut r,
        "work",
        json!([{"op": "update", "id": doc, "fields": {"name": "renamed"}}]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, &site).unwrap();
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
    let (_d, r, _doc) = one_doc();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "mine").unwrap();
    let e = rdy_render::render_site(&r.read(), &Scope::Canon, dir.path()).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
        "mine"
    );
    assert!(!dir.path().join(rdy_render::SITE_MARKER).exists());
    let file = dir.path().join("notes.txt");
    let e = rdy_render::render_site(&r.read(), &Scope::Canon, &file).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "mine");
}

#[test]
fn ren1_an_empty_directory_is_rendered_into() {
    let (_d, r, _doc) = one_doc();
    let dir = tempfile::tempdir().unwrap();
    rdy_render::render_site(&r.read(), &Scope::Canon, dir.path()).unwrap();
    assert!(dir.path().join("index.html").exists());
    assert!(dir.path().join(rdy_render::SITE_MARKER).exists());
}

/// A destructive swap never follows a link: the link and its target stay as they were.
#[cfg(unix)]
#[test]
fn ren1_a_symlinked_out_is_refused_untouched() {
    use std::os::unix::fs::MetadataExt;
    let (_d, r, _doc) = one_doc();
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
        let e = rdy_render::render_site(&r.read(), &Scope::Canon, &out).unwrap_err();
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

#[cfg(unix)]
const READ_ONLY_REASON: &str = "a read-only directory takes writes here (running as root?)";
/// A parent that cannot be written (a mount point's parent, `/var/www` over a user-owned site).
/// Restores the mode on drop so the temporary directory can be removed.
#[cfg(unix)]
struct ReadOnly(std::path::PathBuf);
#[cfg(unix)]
impl ReadOnly {
    /// `test` is the calling test's name, for the skip line when the mode has no effect.
    fn new(test: &str, dir: &std::path::Path) -> Option<ReadOnly> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let guard = ReadOnly(dir.to_path_buf());
        // root ignores the mode: nothing to test then
        let probe = dir.join(".probe");
        if std::fs::write(&probe, "").is_ok() {
            std::fs::remove_file(probe).unwrap();
            drop(guard);
            rdy_qa::skip::skip(test, rdy_qa::skip::Why::Prerequisite, READ_ONLY_REASON);
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
    let (_d, r, _doc) = one_doc();
    let parent = tempfile::tempdir().unwrap();
    let ro = parent.path().join("ro");
    let site = ro.join("site");
    std::fs::create_dir_all(&site).unwrap();
    let Some(_guard) = ReadOnly::new(
        "ren1_an_empty_out_under_an_unwritable_parent_is_rendered_into",
        &ro,
    ) else {
        return;
    };
    rdy_render::render_site(&r.read(), &Scope::Canon, &site).unwrap();
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
    rdy_render::render_site(&r.read(), &Scope::Canon, &site).unwrap();
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
    let Some(_guard) = ReadOnly::new(
        "ren1_a_rerender_under_an_unwritable_parent_replaces_the_site",
        &ro,
    ) else {
        return;
    };
    rdy_render::render_site(&r.read(), &Scope::Canon, &site).unwrap();
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
    let (_d, r, _doc) = one_doc();
    let parent = tempfile::tempdir().unwrap();
    let ro = parent.path().join("readonly-parent");
    std::fs::create_dir(&ro).unwrap();
    let Some(_guard) = ReadOnly::new("ren1_an_unwritable_new_out_names_the_path", &ro) else {
        return;
    };
    let site = ro.join("site");
    let e = rdy_render::render_site(&r.read(), &Scope::Canon, &site).unwrap_err();
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
    rdy_render::render_site(&r.read(), &Scope::Canon, &site).unwrap();
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
    let Some(_guard) = ReadOnly::new("ren1_a_refused_swap_falls_back_to_replacing_in_place", &old)
    else {
        return;
    };
    rdy_render::render_site(&r.read(), &Scope::Canon, &site).unwrap();
    assert!(site.join("renamed.html").exists());
    assert!(!site.join("tx.html").exists());
    assert!(site.join(rdy_render::SITE_MARKER).exists());
    assert!(
        !parent.path().join(".site.rdy-render-partial").exists(),
        "no staging left"
    );
}

/// `.rdy-site` as a link to a file is not the marker: `is_file()` would follow it and mark a
/// directory that rdy never rendered, and the render would delete what it holds.
#[cfg(unix)]
#[test]
fn ren1_a_symlinked_marker_does_not_mark_a_site() {
    let (_d, r, _doc) = one_doc();
    let parent = tempfile::tempdir().unwrap();
    let elsewhere = parent.path().join("elsewhere.txt");
    std::fs::write(&elsewhere, "a real file").unwrap();
    let dir = parent.path().join("notsite");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), "mine").unwrap();
    std::os::unix::fs::symlink(&elsewhere, dir.join(rdy_render::SITE_MARKER)).unwrap();
    let e = rdy_render::render_site(&r.read(), &Scope::Canon, &dir).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e}");
    assert_eq!(
        std::fs::read_to_string(dir.join("notes.txt")).unwrap(),
        "mine"
    );
    assert!(
        std::fs::symlink_metadata(dir.join(rdy_render::SITE_MARKER))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "a real file");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
}

#[cfg(unix)]
mod in_cwd {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::process::Command;

    fn render(
        project: &std::path::Path,
        cwd: &std::path::Path,
        out: &str,
    ) -> (bool, String, String) {
        let (_cfg, env) = human_env("human:qa");
        let o = Command::new(rdy_bin())
            .args([
                "--no-daemon",
                "--project",
                project.to_str().unwrap(),
                "render",
                "--out",
                out,
            ])
            .current_dir(cwd)
            .env("RDY_ACTOR", "human:qa")
            .envs(env)
            .output()
            .unwrap();
        (
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).into(),
            String::from_utf8_lossy(&o.stderr).into(),
        )
    }

    fn names(dir: &std::path::Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        v.sort();
        v
    }

    /// Rendering into the shell's own directory, spelled `.` or as its absolute path, replaces
    /// what it holds and leaves the directory itself (inode, mode) alone, so the shell is not
    /// left standing in a deleted directory.
    #[test]
    fn ren1_out_dot_and_out_equal_to_the_cwd_keep_the_directory() {
        let (proj, r, _doc) = one_doc();
        drop(r);
        let parent = tempfile::tempdir().unwrap();
        let site = parent.path().join("site");
        std::fs::create_dir(&site).unwrap();
        std::fs::set_permissions(&site, std::fs::Permissions::from_mode(0o750)).unwrap();
        let before = std::fs::metadata(&site).unwrap();
        let abs = std::fs::canonicalize(&site).unwrap();
        for (round, spelled) in [".", abs.to_str().unwrap(), "."].into_iter().enumerate() {
            if round > 0 {
                // a page and a directory that the next render must drop
                std::fs::write(site.join("stale.html"), "x").unwrap();
                std::fs::create_dir(site.join("stale-dir")).unwrap();
            }
            let (ok, out, err) = render(proj.path(), &site, spelled);
            assert!(ok, "round {round} --out {spelled}: {out} {err}");
            let now = std::fs::metadata(&site).unwrap();
            assert_eq!(
                now.ino(),
                before.ino(),
                "round {round}: the directory was replaced"
            );
            assert_eq!(now.mode(), before.mode(), "round {round}: mode");
            assert_eq!(now.uid(), before.uid(), "round {round}: owner");
            let n = names(&site);
            assert!(n.contains(&"index.html".to_string()), "{n:?}");
            assert!(n.contains(&rdy_render::SITE_MARKER.to_string()), "{n:?}");
            assert!(
                !n.iter()
                    .any(|f| f.contains("rdy-render") || f.starts_with("stale")),
                "round {round}: {n:?}"
            );
            assert_eq!(
                names(parent.path()),
                vec!["site".to_string()],
                "round {round}: nothing left beside it"
            );
        }
    }

    /// `..` and `x/..` name a directory only once resolved, and are then checked like any other.
    #[test]
    fn ren1_out_dotdot_is_resolved_before_it_is_checked() {
        let (proj, r, _doc) = one_doc();
        drop(r);
        let parent = tempfile::tempdir().unwrap();
        // from `sub`, `..` is `site`: not empty, not a site, so refused and untouched
        let site = parent.path().join("site");
        let sub = site.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let (ok, _out, err) = render(proj.path(), &sub, "..");
        assert!(!ok);
        assert!(err.contains("not empty and is not a site"), "{err}");
        assert_eq!(names(&site), vec!["sub".to_string()]);
        // `fresh/x/..` is `fresh`
        let fresh = parent.path().join("fresh");
        std::fs::create_dir_all(fresh.join("x")).unwrap();
        let (ok, _out, err) = render(proj.path(), parent.path(), "fresh/x/..");
        assert!(!ok);
        assert!(err.contains("not empty and is not a site"), "{err}");
        assert_eq!(names(&fresh), vec!["x".to_string()]);
        // a path that does not resolve is refused, not rendered somewhere else
        let (ok, _out, err) = render(proj.path(), parent.path(), "gone/x/..");
        assert!(!ok);
        assert!(err.contains("does not name a directory"), "{err}");
        assert!(!parent.path().join("gone").exists());
        // once empty, it is rendered into
        std::fs::remove_dir(fresh.join("x")).unwrap();
        let (ok, out, err) = render(proj.path(), &fresh, ".");
        assert!(ok, "{out} {err}");
        assert!(fresh.join("index.html").exists());
    }
}

/// The old copy of a swapped site that cannot be removed is a hidden full copy beside `out`: the
/// render succeeds, and says where it is left.
#[cfg(unix)]
#[test]
fn ren1_an_old_copy_that_cannot_be_removed_is_a_warning() {
    use std::os::unix::fs::PermissionsExt;
    let (_d, r, _doc) = one_doc();
    let parent = tempfile::tempdir().unwrap();
    let site = parent.path().join("site");
    let first = rdy_render::render_site(&r.read(), &Scope::Canon, &site).unwrap();
    assert!(first.warnings.is_empty(), "{:?}", first.warnings);
    // a read-only directory holding a file can't be emptied, so the old site can't be removed
    let locked = site.join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::write(locked.join("f"), "x").unwrap();
    let Some(guard) = ReadOnly::new(
        "ren1_an_old_copy_that_cannot_be_removed_is_a_warning",
        &locked,
    ) else {
        return;
    };
    let second = rdy_render::render_site(&r.read(), &Scope::Canon, &site).unwrap();
    let old = parent.path().join(".site.rdy-render-old");
    std::mem::forget(guard); // the directory has moved; restore its mode by its new path below
    let left = old.join("locked");
    let kept = left.join("f").exists();
    std::fs::set_permissions(&left, std::fs::Permissions::from_mode(0o755)).ok();
    assert!(kept, "the old copy is still there");
    assert_eq!(second.warnings.len(), 1, "{:?}", second.warnings);
    assert!(
        second.warnings[0].contains(".site.rdy-render-old"),
        "names the left-over copy: {:?}",
        second.warnings
    );
    assert!(site.join("index.html").exists());
    assert!(!site.join("locked").exists(), "the new site is in place");
    let json = serde_json::to_value(&second).unwrap();
    assert!(json["warnings"].is_array(), "{json}");
    let json = serde_json::to_value(&first).unwrap();
    assert!(
        json.get("warnings").is_none(),
        "empty warnings are skipped: {json}"
    );
}
