//! Checkout, dirty paths and the write-back: the working copy's files against its base.

use crate::error::*;
use crate::now_ms;
use crate::wc::layout::{FileKind, WcTree};
use crate::wc::project::Projection;
use crate::wc::state::{Base, BaseBlock, Bound, Place, Stage, WcState, stage_paths};
use rdy_model::BlockId;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// What a checkout did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckoutSummary {
    /// The paths written, in the projection's order.
    pub written: Vec<String>,
    /// The tracked paths the projection lacks, deleted.
    pub removed: Vec<String>,
    pub docs: usize,
    pub folders: usize,
    pub blobs: usize,
}

/// The base a projection checks out: every written block's version, file and hidden links, and
/// every file's head.
pub fn base_from(proj: &Projection) -> Base {
    Base {
        blocks: proj
            .blocks
            .iter()
            .filter_map(|(id, b)| {
                Some((
                    BlockId::parse(id).ok()?,
                    BaseBlock {
                        version: b.version.clone(),
                        file: b.file.clone(),
                        hidden: b.hidden.clone(),
                    },
                ))
            })
            .collect(),
        files: proj
            .files
            .iter()
            .filter_map(|f| Some((f.path.clone(), BlockId::parse(&f.id).ok()?)))
            .collect(),
    }
}

/// The bytes of the regular file at `path`; `None` when it is missing, is not a regular file
/// (a link is never followed) or cannot be read.
fn regular_bytes(path: &Path) -> Option<Vec<u8>> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    fs::read(path).ok()
}

/// The paths with edits a checkout would discard, sorted: a tracked file missing or whose bytes
/// differ from its `base/files` copy, a member file not in `base.files`, every staged path.
pub fn dirty_paths(place: &Place, st: &WcState, tree: &WcTree) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for rel in st.base.files.keys() {
        let disk = regular_bytes(&place.root.join(rel));
        let base = regular_bytes(&place.base_file(rel));
        if disk.is_none() || disk != base {
            out.insert(rel.clone());
        }
    }
    out.extend(
        tree.files
            .keys()
            .filter(|rel| !st.base.files.contains_key(*rel))
            .cloned(),
    );
    out.extend(stage_paths(place));
    out.extend(st.stage.deleted.iter().cloned());
    out.into_iter().collect()
}

fn io_err(path: &Path, what: &str, e: impl std::fmt::Display) -> RdyError {
    RdyError::new(
        E_IO,
        format!("cannot {what} {}: {e}", path.display()),
        "check the file and its permissions, then run rdy checkout again",
    )
}

/// `rel` as a path under the root: only plain names, never `..`, `/` or a prefix.
fn inside(rel: &str) -> Result<&Path, RdyError> {
    let p = Path::new(rel);
    if rel.is_empty() || !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("{rel:?} is not a path inside the working copy"),
            "rename the doc or folder so its path is a plain relative path",
        ));
    }
    Ok(p)
}

/// The directories between the root and `rel`'s file, nearest the root first.
fn ancestors(root: &Path, rel: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut cur = root.to_path_buf();
    let comps: Vec<_> = rel.components().collect();
    for c in &comps[..comps.len().saturating_sub(1)] {
        cur.push(c);
        out.push(cur.clone());
    }
    out
}

/// Whether writing `rel` would overwrite something checkout does not track: the path exists and
/// is not tracked, or one of its directories exists as something other than a directory.
fn occupied(root: &Path, rel: &str, tracked: &BTreeMap<String, BlockId>) -> Result<bool, RdyError> {
    let p = inside(rel)?;
    if fs::symlink_metadata(root.join(p)).is_ok() && !tracked.contains_key(rel) {
        return Ok(true);
    }
    Ok(ancestors(root, p)
        .iter()
        .any(|d| fs::symlink_metadata(d).is_ok_and(|m| !m.file_type().is_dir())))
}

fn remove_file(path: &Path) -> Result<(), RdyError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(path, "remove", e)),
    }
}

fn remove_tree(path: &Path) -> Result<(), RdyError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(path, "remove", e)),
    }
}

/// Writes `text` at `rel` under `root`. What stands in the way and is not a directory (a file or
/// a link, checked by the caller or discarded with `--force`) is removed first, so a link is
/// replaced, never written through.
fn write_at(root: &Path, rel: &str, text: &str) -> Result<(), RdyError> {
    let p = inside(rel)?;
    for d in ancestors(root, p) {
        match fs::symlink_metadata(&d) {
            Ok(m) if m.file_type().is_dir() => {}
            Ok(_) => {
                remove_file(&d)?;
                fs::create_dir(&d).map_err(|e| io_err(&d, "create", e))?;
            }
            Err(_) => fs::create_dir(&d).map_err(|e| io_err(&d, "create", e))?,
        }
    }
    let path = root.join(p);
    if fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_dir()) {
        remove_file(&path)?;
    }
    fs::write(&path, text).map_err(|e| io_err(&path, "write", e))
}

/// Checks out `proj` into the working copy (wcco1, exact order): refuses with `E_WC_OCCUPIED`
/// before writing anything when a projected path holds a file checkout does not track (unless
/// `force`); deletes the tracked files the projection lacks and the folder directories that
/// leaves empty; writes every projected file; replaces `base` and its file copies; clears the
/// stage, the conflicts and the briefings; binds to the projection's view and head; saves.
/// `_tree` is the working tree as read before the checkout; the occupied check reads the disk
/// itself, so ignored files count too.
pub fn checkout(
    place: &Place,
    st: &mut WcState,
    _tree: &WcTree,
    proj: &Projection,
    force: bool,
) -> Result<CheckoutSummary, RdyError> {
    let root = &place.root;
    let mut taken = vec![];
    for f in &proj.files {
        if occupied(root, &f.path, &st.base.files)? {
            taken.push(f.path.clone());
        }
    }
    if !taken.is_empty() && !force {
        return Err(RdyError::new(
            E_WC_OCCUPIED,
            format!(
                "checkout would overwrite {} untracked or ignored file(s): {}",
                taken.len(),
                taken
                    .iter()
                    .take(10)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "move them, or pass --force",
        ));
    }
    let projected: BTreeSet<&str> = proj.files.iter().map(|f| f.path.as_str()).collect();
    let mut summary = CheckoutSummary {
        blobs: proj.blobs.len(),
        ..CheckoutSummary::default()
    };
    // the tracked files the projection lacks, then the folder directories left empty
    let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
    for rel in st.base.files.keys() {
        if projected.contains(rel.as_str()) {
            continue;
        }
        let Ok(p) = inside(rel) else { continue };
        let path = root.join(p);
        if fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_dir()) {
            remove_file(&path)?;
        }
        dirs.extend(ancestors(root, p));
        summary.removed.push(rel.clone());
    }
    let mut deepest: Vec<PathBuf> = dirs.into_iter().collect();
    deepest.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for d in deepest {
        let _ = fs::remove_dir(&d); // fails, and stays, while it holds anything
    }
    for f in &proj.files {
        write_at(root, &f.path, &f.text)?;
        summary.written.push(f.path.clone());
        match f.kind {
            FileKind::Doc => summary.docs += 1,
            FileKind::Folder => summary.folders += 1,
        }
    }
    let base_dir = place.state.join("base/files");
    remove_tree(&base_dir)?;
    fs::create_dir_all(&base_dir).map_err(|e| io_err(&base_dir, "create", e))?;
    for f in &proj.files {
        write_at(&base_dir, &f.path, &f.text)?;
    }
    st.base = base_from(proj);
    remove_tree(&place.state.join("stage/files"))?;
    st.stage = Stage::default();
    remove_tree(&place.state.join("conflicts"))?;
    st.conflicts.clear();
    st.briefs.clear();
    st.bound = Some(Bound {
        view: proj.view.clone(),
        head: proj.head.clone(),
        at_ms: now_ms(),
    });
    st.save(place)?;
    Ok(summary)
}
