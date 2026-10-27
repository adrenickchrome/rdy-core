//! The project ops: check, reindex and fsck, and the quarantine files an open leaves behind.
use super::*;
use rdy_engine::engine::open_report_json;
use rdy_engine::{E_PERMISSION, RdyError, Scope, check_path, check_wc, resolve_pack};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FsckArgs {
    #[serde(default)]
    clean_quarantine: bool,
    /// On a passing check, list the signature state of every canon landing and its tags.
    #[serde(default)]
    verbose: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckArgs {
    path: String,
    pack: Option<String>,
    /// Lint the working copy that holds `path` against `view`, not a path against canon.
    wc: Option<bool>,
    view: Option<String>,
}

/// The quarantine files an open left behind: torn segment tails (`segments/<name>`) and damaged
/// indexes, sorted (the engine's listing, shared with the redaction report).
pub use rdy_engine::quarantine::names as quarantine;

/// What `clean_quarantine` did: the names it removed and the ones it left, with why.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cleaned {
    pub removed: Vec<String>,
    pub skipped: Vec<(String, String)>,
}

/// Removes the quarantine files `quarantine(root)` names, in its order; a damaged index is
/// overwritten with zeros and synced before it goes (D-6H0ZX), and one with a second link is not
/// rdy's own copy, so it is skipped untouched. Only regular files are removed: a symlink or a
/// directory carrying a quarantine name is skipped untouched (never followed, never recursed),
/// and an error leaves the file in `skipped` with the error text.
/// Then each directory a file was removed from is fsynced, so a removal reported here survives a
/// power loss; a directory whose fsync fails goes in `skipped` under its store-relative name
/// (`.` for the store itself).
pub fn clean_quarantine(root: &Path) -> Cleaned {
    let store = root.join(".rdy/store");
    let mut c = Cleaned::default();
    let mut dirs = std::collections::BTreeSet::new();
    for name in quarantine(root) {
        let path = store.join(&name);
        let result = std::fs::symlink_metadata(&path)
            .map_err(|e| e.to_string())
            .and_then(|m| {
                if !m.file_type().is_file() {
                    Err("not a regular file".to_string())
                } else if name.starts_with("index.redb.damaged-") {
                    // a damaged index may hold drawn text a removal erased (D-6H0ZX)
                    rdy_store::fsutil::zero_then_remove(&path).map_err(|e| e.to_string())
                } else {
                    std::fs::remove_file(&path).map_err(|e| e.to_string())
                }
            });
        match result {
            Ok(()) => {
                let dir = Path::new(&name)
                    .parent()
                    .unwrap_or(Path::new(""))
                    .to_path_buf();
                dirs.insert(dir);
                c.removed.push(name);
            }
            Err(reason) => c.skipped.push((name, reason)),
        }
    }
    for dir in dirs {
        if let Err(e) = rdy_engine::sync_dir(&store.join(&dir)) {
            let shown = if dir.as_os_str().is_empty() {
                ".".to_string()
            } else {
                dir.display().to_string()
            };
            c.skipped.push((
                shown,
                format!("removed, but the directory fsync failed: {e}"),
            ));
        }
    }
    c
}

pub(super) fn check(cx: &OpContext, a: CheckArgs) -> Result<Value, RdyError> {
    let st = cx.engine.read();
    let owned;
    let pack = match a.pack.as_deref() {
        Some(name) => {
            owned = resolve_pack(&cx.engine.root(), name)?;
            &owned
        }
        None => st.pack(),
    };
    let path = resolve_path(cx.cwd, &a.path);
    if a.wc == Some(true) {
        // the working copy that holds the path (a worktree has its own root), against the view
        let view = view_of(cx.session, a.view);
        let scope = if st.view_head(&view).is_some() {
            Scope::View(view)
        } else {
            Scope::Canon
        };
        let root = rdy_engine::wc::state::Place::locate(&path)
            .map_or_else(|| cx.engine.root(), |p| p.root);
        let mat = st.materialized(&scope)?;
        return Ok(to_json(&check_wc(pack, &mat, &root, Some(&path))?));
    }
    let canon = st.materialized(&Scope::Canon)?;
    Ok(to_json(&check_path(pack, Some(&*canon), &path)?))
}

pub(super) fn reindex(cx: &OpContext) -> Result<Value, RdyError> {
    cx.engine.write(Box::new(|r| {
        r.reindex()?;
        Ok(json!({"reindexed": true}))
    }))
}

/// `fsck`; with `clean_quarantine`, a passing check then removes the quarantine files (the
/// human's only: an agent is refused before the check runs). A failed check removes nothing.
/// With `verbose`, a passing check adds `signature_states` (no trust file is read), read in the
/// same write as the check: no other write can land between them, so the states are of exactly
/// the records the check counted.
pub(super) fn fsck(cx: &OpContext, a: FsckArgs) -> Result<Value, RdyError> {
    if a.clean_quarantine && !cx.session.is_human() {
        return Err(RdyError::new(
            E_PERMISSION,
            "only the human removes quarantine files",
            "run rdy fsck --clean-quarantine as the human (unset RDY_ACTOR)",
        ));
    }
    let verbose = a.verbose;
    let mut v = cx.engine.write(Box::new(move |r| {
        let mut v = to_json(&r.fsck()?);
        if verbose {
            v["signature_states"] = rdy_engine::signing_read::signature_states(&r.read())?;
        }
        Ok(v)
    }))?;
    if a.clean_quarantine {
        let c = clean_quarantine(&cx.engine.root());
        v["removed"] = json!(c.removed);
        v["skipped"] = json!(
            c.skipped
                .iter()
                .map(|(n, r)| json!({"name": n, "reason": r}))
                .collect::<Vec<_>>()
        );
    }
    v["open_report"] = open_report_json(&cx.engine.open_report());
    v["quarantine"] = json!(quarantine(&cx.engine.root()));
    Ok(v)
}
