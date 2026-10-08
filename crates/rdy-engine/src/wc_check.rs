//! `rdy check` on a working copy: the tree lint against a view, and paths under the checkout.

use crate::{Diagnostic, E_BAD_INPUT, Materialized, RdyError, W_WC_UNTRACKED, wc};
use rdy_schema::Pack;
use std::path::{Path, PathBuf};

/// Lints the working tree under `root` against `view` (the bound view's materialization): the
/// tree read with the project's `.rdyignore`, then `lint_tree` with references resolved in the
/// view, then one `W_WC_UNTRACKED` warning for each file the scan does not read as a doc file.
/// With `path` (absolute, or relative to `root`), only the diagnostics in files under it.
pub fn check_wc(
    pack: &Pack,
    view: &Materialized,
    root: &Path,
    path: Option<&Path>,
) -> Result<Vec<Diagnostic>, RdyError> {
    use wc::layout::read_tree;
    let ignore = wc::ignore::Ignore::load(root)?;
    let mut tree = read_tree(root, &ignore, pack);
    let mut diags = wc::lint::lint_tree(&mut tree, pack, Some(view), Some(root));
    for u in &tree.untracked {
        diags.push(Diagnostic {
            file: u.path.clone(),
            line: 1,
            column: 1,
            code: W_WC_UNTRACKED.into(),
            message: format!("{}: {}", u.path, u.reason),
            fix: format!("rdy check {}", u.path),
            severity: rdy_schema::Severity::Warning,
        });
    }
    if let Some(under) = path.map(|p| relative_to(root, p)).transpose()?.flatten() {
        let prefix = format!("{under}/");
        diags.retain(|d| d.file == under || d.file.starts_with(&prefix));
    }
    diags.sort_by(|a, b| (&a.file, a.line, a.column).cmp(&(&b.file, b.line, b.column)));
    Ok(diags)
}

/// `path` with `.` and `..` resolved by the words alone. `None` for a relative path whose `..`
/// climbs above its start. On an absolute path, `..` at the filesystem root stays there.
pub fn lexical_path(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                let climbs = matches!(out.components().next_back(), Some(Component::Normal(_)));
                if climbs {
                    out.pop();
                } else if !out.has_root() {
                    return None;
                }
            }
            c => out.push(c.as_os_str()),
        }
    }
    Some(out)
}

/// `path` as a `/`-separated path under `root`; `Ok(None)` for the root itself. `..` is resolved
/// first (`Path::starts_with` compares components, so `/root/../x` would start with `/root`); a
/// path that is not under `root` is `E_BAD_INPUT`, never a path read as if it were.
fn relative_to(root: &Path, path: &Path) -> Result<Option<String>, RdyError> {
    use std::path::Component;
    let outside = || {
        RdyError::new(
            E_BAD_INPUT,
            format!("{} is not under {}", path.display(), root.display()),
            "pass a path inside the working tree",
        )
    };
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let rel = if path.is_absolute() {
        let flat = lexical_path(path).ok_or_else(outside)?;
        let (root_flat, root_canon) = (
            lexical_path(root).unwrap_or_else(|| root.to_path_buf()),
            canon(root),
        );
        let real = canon(&flat);
        flat.strip_prefix(&root_flat)
            .or_else(|_| flat.strip_prefix(&root_canon))
            .or_else(|_| real.strip_prefix(&root_canon))
            .map(Path::to_path_buf)
            .map_err(|_| outside())?
    } else {
        lexical_path(path).ok_or_else(outside)?
    };
    let parts: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    Ok((!parts.is_empty()).then(|| parts.join("/")))
}

#[cfg(test)]
mod relative_to_tests {
    use super::{E_BAD_INPUT, lexical_path, relative_to};
    use std::path::Path;

    /// Fix round 1 (wcck1): `..` is resolved, and a path that leaves the root is an error, not
    /// a path with its `..` dropped.
    #[test]
    fn wcck1_relative_to_resolves_dotdot_and_refuses_a_path_outside() {
        let root = Path::new("/nonexistent-root/tree");
        let rel = |p: &str| relative_to(root, Path::new(p));
        assert_eq!(rel("/nonexistent-root/tree").unwrap(), None);
        assert_eq!(
            rel("/nonexistent-root/tree/a/../b/c.yaml")
                .unwrap()
                .as_deref(),
            Some("b/c.yaml")
        );
        assert_eq!(rel("a/./b.yaml").unwrap().as_deref(), Some("a/b.yaml"));
        for outside in [
            "/nonexistent-root/tree/../elsewhere/x.yaml",
            "/nonexistent-root/elsewhere",
            "../elsewhere",
            "a/../../elsewhere",
        ] {
            let e = rel(outside).unwrap_err();
            assert_eq!(e.code, E_BAD_INPUT, "{outside}");
        }
        assert_eq!(
            lexical_path(Path::new("/a/../..")).as_deref(),
            Some(Path::new("/"))
        );
        assert_eq!(lexical_path(Path::new("../a")), None);
    }
}
