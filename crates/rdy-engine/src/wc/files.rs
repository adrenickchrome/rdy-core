//! Checkout, dirty paths and the write-back: the working copy's files against its base.

use crate::error::*;
use crate::now_ms;
use crate::tree_io::Diagnostic;
use crate::wc::layout::{BLOBS_DIR, FileKind, WcTree};
use crate::wc::project::Projection;
use crate::wc::state::{
    Base, BaseBlock, Bound, CheckoutMark, ConflictEntry, ConflictKind, Place, Stage, UPDATE_FILE,
    WcState, new_nonce, stage_paths,
};
use rdy_model::BlockId;
use rdy_schema::Severity;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
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

/// The paths with edits a checkout would discard, sorted: a tracked file missing, reached
/// through a directory that is a link or a file (never read through it), or whose bytes differ
/// from its `base/files` copy; a member file not in `base.files`; every staged path. While a
/// checkout is interrupted, every tracked and every member file.
pub fn dirty_paths(place: &Place, st: &WcState, tree: &WcTree) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for rel in st.base.files.keys() {
        let disk = match inside(rel) {
            Ok(p) if !behind_link(&place.root, p) => regular_bytes(&place.root.join(p)),
            _ => None,
        };
        let base = regular_bytes(&place.base_file(rel));
        if disk.is_none() || disk != base || st.checkout.is_some() {
            out.insert(rel.clone());
        }
    }
    out.extend(
        tree.files
            .keys()
            .filter(|rel| st.checkout.is_some() || !st.base.files.contains_key(*rel))
            .cloned(),
    );
    out.extend(stage_paths(place));
    out.extend(st.stage.deleted.iter().cloned());
    out.into_iter().collect()
}

/// Deletes a worktree's tracked files (every path in `base.files`, never through a link) and the
/// directories that leaves empty, then the `_blobs` directory if it is empty. Returns the paths
/// removed. Untracked files stay.
pub fn remove_tracked(place: &Place, st: &WcState) -> Result<Vec<String>, RdyError> {
    let root = &place.root;
    let mut removed = vec![];
    let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
    for rel in st.base.files.keys() {
        let Ok(p) = inside(rel) else { continue };
        if behind_link(root, p) {
            continue;
        }
        let path = root.join(p);
        if fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_dir()) {
            remove_file(&path)?;
            removed.push(rel.clone());
        }
        dirs.extend(ancestors(root, p));
    }
    let mut deepest: Vec<PathBuf> = dirs.into_iter().collect();
    deepest.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for d in deepest {
        let _ = fs::remove_dir(&d); // fails, and stays, while it holds anything
    }
    let _ = fs::remove_dir(root.join(BLOBS_DIR));
    Ok(removed)
}

fn io_err(path: &Path, what: &str, e: impl std::fmt::Display) -> RdyError {
    RdyError::new(
        E_IO,
        format!("cannot {what} {}: {e}", path.display()),
        "check the file and its permissions, then run rdy checkout again",
    )
}

/// `rel` as a path under the root: only plain names, never `..`, `/` or a prefix.
pub(crate) fn inside(rel: &str) -> Result<&Path, RdyError> {
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

/// Whether one of the directories between the root and `rel`'s file exists as something other
/// than a directory (a link, a file): what is under it is not the working tree's, so it is never
/// read, written or deleted through it.
pub(crate) fn behind_link(root: &Path, rel: &Path) -> bool {
    blocking_ancestor(root, rel).is_some()
}

/// The first directory between the root and `rel`'s file, nearest the root first, that exists
/// as something other than a directory (see `behind_link`), relative to the root.
pub(crate) fn blocking_ancestor(root: &Path, rel: &Path) -> Option<PathBuf> {
    ancestors(root, rel)
        .into_iter()
        .find(|d| fs::symlink_metadata(d).is_ok_and(|m| !m.file_type().is_dir()))
        .and_then(|d| d.strip_prefix(root).ok().map(Path::to_path_buf))
}

/// The file's identity on disk (device and inode), for a regular file not behind a link.
pub(crate) fn identity(root: &Path, rel: &str) -> Option<(u64, u64)> {
    let p = inside(rel).ok()?;
    if behind_link(root, p) {
        return None;
    }
    let m = fs::symlink_metadata(root.join(p)).ok()?;
    m.file_type().is_file().then(|| (m.dev(), m.ino()))
}

/// Whether writing `rel` would overwrite something checkout does not track: one of its
/// directories exists as something other than a directory, or the path exists, is not tracked,
/// and is not the same file as a tracked file the checkout drops (`dropped`: on a
/// case-insensitive filesystem `Notes.yaml` is the tracked `notes.yaml`).
fn occupied(
    root: &Path,
    rel: &str,
    tracked: &BTreeMap<String, BlockId>,
    dropped: &BTreeSet<(u64, u64)>,
) -> Result<bool, RdyError> {
    let p = inside(rel)?;
    if behind_link(root, p) {
        return Ok(true);
    }
    Ok(match fs::symlink_metadata(root.join(p)) {
        Err(_) => false,
        Ok(_) if tracked.contains_key(rel) => false,
        Ok(m) => !(m.file_type().is_file() && dropped.contains(&(m.dev(), m.ino()))),
    })
}

/// Gives the existing directory or file `dir` the spelling its path asks for. On a
/// case-insensitive filesystem `Guides` opens the directory stored as `guides`; it is renamed
/// through a temporary name, never deleted and recreated, so what else it holds stays in it.
fn fix_case(dir: &Path) -> Result<(), RdyError> {
    let (Some(parent), Some(want)) = (dir.parent(), dir.file_name()) else {
        return Ok(());
    };
    let Ok(meta) = fs::symlink_metadata(dir) else {
        return Ok(());
    };
    let Ok(listing) = fs::read_dir(parent) else {
        return Ok(());
    };
    let lower = want.to_string_lossy().to_lowercase();
    let mut stored = None;
    for e in listing.flatten() {
        let name = e.file_name();
        if name == want {
            return Ok(());
        }
        if name.to_string_lossy().to_lowercase() == lower
            && e.metadata()
                .is_ok_and(|m| m.dev() == meta.dev() && m.ino() == meta.ino())
        {
            stored = Some(name);
        }
    }
    let Some(stored) = stored else {
        return Ok(());
    };
    let from = parent.join(stored);
    let tmp = parent.join(format!(".rdy-case-{}", new_nonce()));
    fs::rename(&from, &tmp).map_err(|e| io_err(&from, "rename", e))?;
    if let Err(e) = fs::rename(&tmp, dir) {
        let _ = fs::rename(&tmp, &from);
        return Err(io_err(dir, "rename", e));
    }
    Ok(())
}

pub(crate) fn remove_file(path: &Path) -> Result<(), RdyError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(path, "remove", e)),
    }
}

pub(crate) fn remove_tree(path: &Path) -> Result<(), RdyError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(path, "remove", e)),
    }
}

/// Writes `text` at `rel` under `root`. What stands in the way and is not a directory (a file or
/// a link, checked by the caller or discarded with `--force`) is removed first, so a link is
/// replaced, never written through. With `cases`, each existing directory on the way is given
/// its path's spelling once (`fix_case`); `cases` holds the directories already done.
pub(crate) fn write_at(
    root: &Path,
    rel: &str,
    text: &str,
    cases: Option<&mut BTreeSet<PathBuf>>,
) -> Result<(), RdyError> {
    let p = inside(rel)?;
    make_dirs(root, p, cases)?;
    let path = root.join(p);
    if fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_dir()) {
        remove_file(&path)?;
    }
    fs::write(&path, text).map_err(|e| io_err(&path, "write", e))
}

/// As `write_at`, but the file is replaced in one step (a durable temporary file renamed over
/// it): an error or a crash leaves either the old text or the new, never a part. An existing file
/// first takes the spelling `rel` asks for (`fix_case`): on a case-insensitive filesystem a
/// rename over `notes.yaml` to `Notes.yaml` keeps the old spelling (a case-only rename).
pub(crate) fn replace_at(
    root: &Path,
    rel: &str,
    text: &str,
    cases: Option<&mut BTreeSet<PathBuf>>,
) -> Result<(), RdyError> {
    let p = inside(rel)?;
    make_dirs(root, p, cases)?;
    let path = root.join(p);
    match fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_file() => fix_case(&path)?,
        Ok(m) if !m.is_dir() => remove_file(&path)?, // a link is replaced, never written through
        _ => {}
    }
    rdy_store::fsutil::write_durable(&path, text.as_bytes()).map_err(|e| io_err(&path, "write", e))
}

/// A copy rdy keeps in the working copy's state (a base copy or a conflict copy): replaced in one
/// durable step, as the state files beside it are, so a crash never leaves a torn copy that the
/// state names.
pub(crate) fn write_state_copy(dir: &Path, rel: &str, text: &str) -> Result<(), RdyError> {
    write_state_copies(dir, &[(rel, text)])
}

/// `write_state_copy` for many copies, durable as one batch: a copy whose bytes already stand at
/// its path is left alone (no write, no flush); every other copy is written to its temp file and
/// fsynced, then each is renamed into place, then each touched directory is fsynced once. A
/// directory this creates is made durable too (its parent is fsynced), so the state never names a
/// copy whose directory entry a power loss can drop. After a crash a copy holds its old bytes or
/// its new bytes, never a part.
pub(crate) fn write_state_copies(dir: &Path, copies: &[(&str, &str)]) -> Result<(), RdyError> {
    let mut pending: Vec<(PathBuf, &[u8])> = vec![];
    let mut new_dirs_parents: Vec<PathBuf> = vec![];
    for (rel, text) in copies {
        let p = inside(rel)?;
        for d in ancestors(dir, p) {
            if !fs::symlink_metadata(&d).is_ok_and(|m| m.file_type().is_dir())
                && let Some(parent) = d.parent()
                && !new_dirs_parents.iter().any(|x| x == parent)
            {
                new_dirs_parents.push(parent.to_path_buf());
            }
        }
        make_dirs(dir, p, None)?;
        let path = dir.join(p);
        match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_file() => {
                if fs::read(&path).is_ok_and(|b| b == text.as_bytes()) {
                    continue;
                }
                fix_case(&path)?;
            }
            Ok(m) if !m.is_dir() => remove_file(&path)?, // a link is replaced, never written through
            _ => {}
        }
        pending.push((path, text.as_bytes()));
    }
    if pending.is_empty() {
        return Ok(());
    }
    rdy_store::fsutil::write_durable_batch(&pending, &new_dirs_parents).map_err(|e| {
        let at = pending.first().map_or(dir, |(p, _)| p.as_path());
        io_err(at, "write", e)
    })
}

/// Makes the copies under `dir` (a base copy directory) exactly `copies`: creates `dir` durably,
/// removes each copy whose file left the tree and each directory that leaves empty, then writes
/// the copies that differ (`write_state_copies`). A copy that already holds the right bytes costs
/// nothing, so an update that changes one file in a project of many flushes one file.
pub(crate) fn sync_state_copies(dir: &Path, copies: &[(&str, &str)]) -> Result<(), RdyError> {
    rdy_store::fsutil::create_dir_durable(dir).map_err(|e| io_err(dir, "create", e))?;
    let wanted: BTreeSet<&str> = copies.iter().map(|(p, _)| *p).collect();
    remove_stale(dir, "", &wanted)?;
    write_state_copies(dir, copies)
}

/// Removes what stands under `dir/rel` and is not in `wanted` (a path relative to `dir`, `/`
/// separated): files and links, then a directory that is left empty and holds no wanted path.
fn remove_stale(dir: &Path, rel: &str, wanted: &BTreeSet<&str>) -> Result<(), RdyError> {
    let here = if rel.is_empty() {
        dir.to_path_buf()
    } else {
        dir.join(rel)
    };
    let entries = match fs::read_dir(&here) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_err(&here, "read", e)),
    };
    for entry in entries {
        let entry = entry.map_err(|e| io_err(&here, "read", e))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let child = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        let path = entry.path();
        let is_dir = fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_dir());
        if !is_dir {
            if !wanted.contains(child.as_str()) {
                remove_file(&path)?;
            }
            continue;
        }
        remove_stale(dir, &child, wanted)?;
        let prefix = format!("{child}/");
        if !wanted.iter().any(|w| w.starts_with(&prefix)) {
            let _ = fs::remove_dir(&path); // fails, and stays, while it holds anything
        }
    }
    Ok(())
}

/// Creates the directories between the root and `p`'s file (what stands in the way and is not a
/// directory is removed first); with `cases`, see `write_at`. Callers run an occupied check first
/// (`occupied`, `behind_link`), so what is removed is never a file a person keeps.
fn make_dirs(
    root: &Path,
    p: &Path,
    mut cases: Option<&mut BTreeSet<PathBuf>>,
) -> Result<(), RdyError> {
    for d in ancestors(root, p) {
        match fs::symlink_metadata(&d) {
            Ok(m) if m.file_type().is_dir() => {
                if let Some(done) = cases.as_deref_mut()
                    && done.insert(d.clone())
                {
                    fix_case(&d)?;
                }
            }
            Ok(_) => {
                remove_file(&d)?;
                fs::create_dir(&d).map_err(|e| io_err(&d, "create", e))?;
            }
            Err(_) => fs::create_dir(&d).map_err(|e| io_err(&d, "create", e))?,
        }
    }
    Ok(())
}

/// Checks out `proj` into the working copy (wcco1, exact order): refuses with `E_WC_OCCUPIED`
/// before writing anything when a projected path holds a file checkout does not track (unless
/// `force`); writes the checkout marker (`CheckoutMark`) durably; deletes the tracked files the
/// projection lacks and the folder directories that leaves empty (never through a link: what is
/// behind one is kept); writes every projected file, giving existing directories the projected
/// spelling; replaces `base` and its file copies; clears the stage, the conflicts and the
/// briefings; binds to the projection's view and head; saves, which removes the marker last.
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
    let projected: BTreeSet<&str> = proj.files.iter().map(|f| f.path.as_str()).collect();
    let dropped: BTreeSet<(u64, u64)> = st
        .base
        .files
        .keys()
        .filter(|rel| !projected.contains(rel.as_str()))
        .filter_map(|rel| identity(root, rel))
        .collect();
    let mut taken = vec![];
    for f in &proj.files {
        if occupied(root, &f.path, &st.base.files, &dropped)? {
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
    st.mark_checkout(
        place,
        CheckoutMark {
            view: proj.view.clone(),
            head: proj.head.clone(),
            at_ms: now_ms(),
            update: false,
        },
    )?;
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
        if behind_link(root, p) {
            continue; // not the working tree's: kept
        }
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
    let mut cases = BTreeSet::new();
    for f in &proj.files {
        write_at(root, &f.path, &f.text, Some(&mut cases))?;
        summary.written.push(f.path.clone());
        match f.kind {
            FileKind::Doc => summary.docs += 1,
            FileKind::Folder => summary.folders += 1,
        }
    }
    let base_dir = place.state.join("base/files");
    let copies: Vec<(&str, &str)> = proj
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.text.as_str()))
        .collect();
    sync_state_copies(&base_dir, &copies)?;
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
    st.checkout = None;
    st.save(place)?;
    // an interrupted update's record goes with the marker it belonged to
    remove_file(&place.state.join(UPDATE_FILE))?;
    Ok(summary)
}

/// What `add_projected` did: the paths written, and those it left because the path holds
/// something the working copy does not track.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Added {
    pub written: Vec<String>,
    pub skipped: Vec<String>,
}

/// Adds the files of `proj` (the doc and folder files a write just created) to a bound working
/// copy: a path in `base.files` ignoring case (another doc's file, on disk or not) or one that
/// holds anything (a file, a directory, or a link or file on the way) is skipped, never
/// overwritten; any other is written, its base copy written, and its blocks and file added to
/// `base`. The state is not saved here.
pub fn add_projected(
    place: &Place,
    st: &mut WcState,
    proj: &Projection,
) -> Result<Added, RdyError> {
    let root = &place.root;
    let base_dir = place.state.join("base/files");
    let mut out = Added::default();
    for f in &proj.files {
        // tracked ignoring case, as the update planner and a file system may
        let lower = f.path.to_lowercase();
        if st.base.files.keys().any(|k| k.to_lowercase() == lower)
            || occupied(root, &f.path, &BTreeMap::new(), &BTreeSet::new())?
        {
            out.skipped.push(f.path.clone());
            continue;
        }
        write_at(root, &f.path, &f.text, None)?;
        write_state_copy(&base_dir, &f.path, &f.text)?;
        out.written.push(f.path.clone());
    }
    let base = base_from(proj);
    for (id, b) in base.blocks {
        if out.written.contains(&b.file) {
            st.base.blocks.insert(id, b);
        }
    }
    for (path, id) in base.files {
        if out.written.contains(&path) {
            st.base.files.insert(path, id);
        }
    }
    Ok(out)
}

/// What a write-back did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WriteBack {
    /// The paths written, in the projection's order.
    pub written: Vec<String>,
    /// The tracked paths deleted, sorted.
    pub removed: Vec<String>,
    /// The paths left as they were because they hold what the commit did not record (one
    /// `W_WC_KEPT` warning each, in the order met).
    pub kept: Vec<Diagnostic>,
}

/// The committed paths whose working file is no longer what `rdy add` staged (wccm1; R-P7 for a
/// commit), sorted: a staged copy whose file is missing or holds other bytes, and a staged
/// deletion whose path holds a file again (unless it is the same file as a staged copy: on a
/// case-insensitive filesystem a staged `Notes.yaml` is reached as `notes.yaml`).
pub fn changed_since_staged(
    place: &Place,
    st: &WcState,
    committed: &BTreeSet<String>,
) -> Vec<String> {
    let root = &place.root;
    let copies: BTreeSet<(u64, u64)> = committed
        .iter()
        .filter(|rel| !st.stage.deleted.contains(*rel))
        .filter_map(|rel| identity(root, rel))
        .collect();
    committed
        .iter()
        .filter(|rel| {
            let Ok(p) = inside(rel) else { return false };
            let reachable = !behind_link(root, p);
            if st.stage.deleted.contains(*rel) {
                reachable
                    && fs::symlink_metadata(root.join(p)).is_ok_and(|m| {
                        !(m.file_type().is_file() && copies.contains(&(m.dev(), m.ino())))
                    })
            } else {
                let disk = reachable.then(|| regular_bytes(&root.join(p))).flatten();
                disk.is_none() || disk != regular_bytes(&place.stage_file(rel))
            }
        })
        .cloned()
        .collect()
}

/// Puts back the base copy of each `committed` path a commit recorded nothing for (wccm1): a
/// staged copy (not a deletion) of a base path whose working file still holds the staged bytes
/// gets the base copy's canonical text, so the working copy is clean again. A path with later
/// edits, behind a link or not in the base is left as it is. Call it before the stage entries
/// go. Returns the paths written.
pub fn restore_unrecorded(
    place: &Place,
    st: &WcState,
    committed: &BTreeSet<String>,
) -> Result<Vec<String>, RdyError> {
    let dirty: BTreeSet<String> = changed_since_staged(place, st, committed)
        .into_iter()
        .collect();
    let mut out = vec![];
    for rel in committed {
        if st.stage.deleted.contains(rel) || dirty.contains(rel) || !st.base.files.contains_key(rel)
        {
            continue;
        }
        let Some(text) =
            regular_bytes(&place.base_file(rel)).and_then(|b| String::from_utf8(b).ok())
        else {
            continue;
        };
        if regular_bytes(&place.root.join(inside(rel)?)).is_some_and(|b| b == text.as_bytes()) {
            continue;
        }
        replace_at(&place.root, rel, &text, None)?;
        out.push(rel.clone());
    }
    Ok(out)
}

/// Whether the write-back may replace or delete what is at `rel`: nothing, or a regular file
/// (not behind a link) whose bytes are `text`, the staged copy or the base copy of `rel`, or the
/// staged or base copy of a tracked path that is the same file (a case-only rename). Anything
/// else holds what the commit did not record: a person's later edits, or a file rdy does not
/// track.
fn replaceable(place: &Place, st: &WcState, rel: &str, text: Option<&str>) -> bool {
    let root = &place.root;
    let Ok(p) = inside(rel) else { return false };
    if behind_link(root, p) {
        return false;
    }
    let path = root.join(p);
    let Ok(meta) = fs::symlink_metadata(&path) else {
        return true;
    };
    if !meta.file_type().is_file() {
        return false;
    }
    let Ok(bytes) = fs::read(&path) else {
        return false;
    };
    if text.is_some_and(|t| t.as_bytes() == bytes.as_slice()) {
        return true;
    }
    let known = |q: &str| {
        let same_bytes = |copy: PathBuf| regular_bytes(&copy).is_some_and(|b| b == bytes);
        same_bytes(place.stage_file(q))
            || (st.base.files.contains_key(q) && same_bytes(place.base_file(q)))
    };
    if known(rel) {
        return true;
    }
    // only when its own copies differ: the tracked paths that are this same file
    let same = (meta.dev(), meta.ino());
    st.base
        .files
        .keys()
        .filter(|q| q.as_str() != rel)
        .any(|q| identity(root, q) == Some(same) && known(q))
}

/// The warning for a path the write-back left alone.
fn kept(rel: &str, message: String, fix: String) -> Diagnostic {
    Diagnostic {
        file: rel.to_string(),
        line: 1,
        column: 1,
        code: W_WC_KEPT.into(),
        message,
        fix,
        severity: Severity::Warning,
    }
}

/// Deletes the tracked file at `rel` (never through a link) and its base copy, except a file
/// that is one of `keep` (the same file as a path just written: a case-only rename on a
/// case-insensitive filesystem). The working file is deleted only when `replaceable`; else it
/// stays, untracked, with a `W_WC_KEPT` warning. Returns whether `rel` was a plain path.
fn drop_tracked(
    place: &Place,
    st: &WcState,
    rel: &str,
    keep: &BTreeSet<(u64, u64)>,
    keep_base: &BTreeSet<(u64, u64)>,
    out: &mut WriteBack,
) -> Result<bool, RdyError> {
    let Ok(p) = inside(rel) else {
        return Ok(false);
    };
    let base_dir = place.state.join("base/files");
    if !identity(&place.root, rel).is_some_and(|i| keep.contains(&i)) {
        if !replaceable(place, st, rel, None) {
            out.kept.push(kept(
                rel,
                format!("{rel} is not deleted: it holds edits the commit did not record, and is no longer tracked"),
                format!("rdy add {rel} and commit it as a new doc, or delete it"),
            ));
        } else if !behind_link(&place.root, p) {
            let path = place.root.join(p);
            if fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_dir()) {
                remove_file(&path)?;
                out.removed.push(rel.to_string());
            }
        }
    }
    if !identity(&base_dir, rel).is_some_and(|i| keep_base.contains(&i)) {
        let base = place.base_file(rel);
        if fs::symlink_metadata(&base).is_ok_and(|m| !m.file_type().is_dir()) {
            remove_file(&base)?;
        }
    }
    Ok(true)
}

/// Removes the directories between `top` and `rel`'s file that are left empty, deepest first;
/// nothing when one of them is a link (what is behind it is not the working tree's).
pub(crate) fn prune_dirs(top: &Path, rel: &str) {
    let Ok(p) = inside(rel) else { return };
    if behind_link(top, p) {
        return;
    }
    let mut dirs = ancestors(top, p);
    dirs.reverse();
    for d in dirs {
        if fs::remove_dir(&d).is_err() {
            break; // it holds something (or is not a directory): it and those above stay
        }
    }
}

/// Writes a commit's projection back into the working copy (wccm1, exact): each projected file
/// at its path and in `base/files`, deleting its head's old path when it moved; the old file of
/// each `rewrite` id the projection lists as missing; each path of `removed_files` still tracked.
/// A path the projection writes is never deleted. Then `base.blocks` is rebuilt for the rewritten
/// files (an entry in a rewritten or deleted file is replaced by the projection's; an id no
/// longer in any file is dropped), `base.files` is updated, the folder directories left empty
/// are removed, and each `committed` path leaves the stage (its copy or its deletion). The state
/// is not saved here.
///
/// The write-back never destroys what the commit did not record. A projected path that holds
/// something other than its staged or base copy (a file rdy does not track, a person's later
/// edits, a directory or link on the way) is not written: that file's head keeps its old path,
/// base copy and base entries, a `W_WC_KEPT` warning names it, and a `ConflictKind::Kept` entry
/// refuses every later commit until `rdy update` turns it into a conflict (or a checkout replaces
/// the files): the old base would let the next commit create the commit's new blocks a second
/// time. A path to delete that holds
/// later edits stays on disk, untracked, with a warning. An old path that is the same file as a
/// written path (a case-only rename on a case-insensitive filesystem) is not deleted.
pub fn apply_writeback(
    place: &Place,
    st: &mut WcState,
    proj: &Projection,
    rewrite: &BTreeSet<BlockId>,
    removed_files: &BTreeSet<String>,
    committed: &BTreeSet<String>,
) -> Result<WriteBack, RdyError> {
    let root = &place.root;
    let base_dir = place.state.join("base/files");
    let projected: BTreeSet<&str> = proj.files.iter().map(|f| f.path.as_str()).collect();
    let mut out = WriteBack::default();
    let old_path = |files: &BTreeMap<String, BlockId>, id: &str| -> Vec<String> {
        files
            .iter()
            .filter(|(_, h)| h.as_str() == id)
            .map(|(p, _)| p.clone())
            .collect()
    };
    // which projected files may be written, decided before anything is written
    let mut blocked: BTreeSet<&str> = BTreeSet::new();
    let mut conflicts: Vec<ConflictEntry> = vec![];
    for f in &proj.files {
        if !replaceable(place, st, &f.path, Some(&f.text)) {
            blocked.insert(f.path.as_str());
            let olds: Vec<String> = old_path(&st.base.files, &f.id)
                .into_iter()
                .filter(|o| *o != f.path)
                .collect();
            let (message, fix) = if olds.is_empty() && st.base.files.contains_key(&f.path) {
                (
                    format!(
                        "{} is not written back: it holds edits the commit did not record",
                        f.path
                    ),
                    format!(
                        "run rdy update: it writes the store's version beside {} as a conflict; commit is refused until then",
                        f.path
                    ),
                )
            } else if olds.is_empty() {
                (
                    format!(
                        "{} is not written back: a file rdy does not track is in the way",
                        f.path
                    ),
                    format!(
                        "move {} aside, then run rdy update: it writes the store's version there with your file as a conflict; commit is refused until then",
                        f.path
                    ),
                )
            } else {
                (
                    format!(
                        "{} is not written back: a file rdy does not track, or edits the commit did not record, are in the way; the doc stays at {}",
                        f.path,
                        olds.join(", ")
                    ),
                    format!(
                        "move {} aside, then run rdy update: it writes the store's version there with your file as a conflict; commit is refused until then",
                        f.path
                    ),
                )
            };
            out.kept.push(kept(&f.path, message, fix));
            // the base stays old, so the file must not be committed again before a checkout
            let theirs: BTreeMap<BlockId, String> = proj
                .blocks
                .iter()
                .filter(|(_, b)| b.file == f.path)
                .filter_map(|(id, b)| Some((BlockId::parse(id).ok()?, b.version.clone())))
                .collect();
            conflicts.push(ConflictEntry {
                path: olds.first().cloned().unwrap_or_else(|| f.path.clone()),
                kind: ConflictKind::Kept,
                blocks: theirs.keys().cloned().collect(),
                theirs_versions: theirs,
                local_name: None,
                store_name: Some(f.path.clone()),
                store_parent: None,
            });
        }
    }
    for c in conflicts {
        st.conflicts.retain(|o| o.path != c.path);
        st.conflicts.push(c);
    }
    let blocked_heads: BTreeSet<&str> = proj
        .files
        .iter()
        .filter(|f| blocked.contains(f.path.as_str()))
        .map(|f| f.id.as_str())
        .collect();
    let mut keep: BTreeSet<(u64, u64)> = BTreeSet::new();
    let mut keep_base: BTreeSet<(u64, u64)> = BTreeSet::new();
    for f in proj
        .files
        .iter()
        .filter(|f| !blocked.contains(f.path.as_str()))
    {
        write_at(root, &f.path, &f.text, None)?;
        write_state_copy(&base_dir, &f.path, &f.text)?;
        keep.extend(identity(root, &f.path));
        keep_base.extend(identity(&base_dir, &f.path));
        out.written.push(f.path.clone());
    }
    let mut gone: BTreeSet<String> = BTreeSet::new();
    let mut drops: Vec<String> = vec![];
    for f in proj
        .files
        .iter()
        .filter(|f| !blocked.contains(f.path.as_str()))
    {
        drops.extend(
            old_path(&st.base.files, &f.id)
                .into_iter()
                .filter(|old| *old != f.path),
        );
    }
    for id in rewrite {
        if proj.missing.iter().any(|m| m == id.as_str()) {
            drops.extend(old_path(&st.base.files, id.as_str()));
        }
    }
    drops.extend(
        removed_files
            .iter()
            .filter(|rel| {
                st.base
                    .files
                    .get(*rel)
                    .is_some_and(|h| !blocked_heads.contains(h.as_str()))
            })
            .cloned(),
    );
    for old in drops {
        if !projected.contains(old.as_str())
            && !gone.contains(&old)
            && drop_tracked(place, st, &old, &keep, &keep_base, &mut out)?
        {
            gone.insert(old);
        }
    }
    // base.blocks: what the rewritten and deleted files held is replaced by the projection's
    let touched: BTreeSet<&str> = out
        .written
        .iter()
        .chain(gone.iter())
        .map(String::as_str)
        .collect();
    st.base
        .blocks
        .retain(|_, b| !touched.contains(b.file.as_str()));
    let mut fresh = base_from(proj);
    fresh
        .blocks
        .retain(|_, b| !blocked.contains(b.file.as_str()));
    fresh.files.retain(|p, _| !blocked.contains(p.as_str()));
    st.base.blocks.extend(fresh.blocks);
    // base.files: a deleted path and a projected head's old path go; the projection's come in
    let heads: BTreeSet<&BlockId> = fresh.files.values().collect();
    st.base
        .files
        .retain(|p, h| !gone.contains(p) && !heads.contains(h));
    st.base.files.extend(fresh.files);
    for rel in &gone {
        prune_dirs(root, rel);
        prune_dirs(&base_dir, rel);
    }
    for rel in committed {
        if inside(rel).is_ok() {
            crate::wc::state::stage_clear(place, rel)?;
        }
        st.stage.deleted.remove(rel);
    }
    out.removed.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mfb5_a_state_copy_is_synced_and_never_written_through_a_link() {
        let d = tempfile::tempdir().unwrap();
        let (r, synced) = rdy_store::fsutil::record_syncs(|| {
            write_state_copy(d.path(), "guides/intro.yaml", "- id: d-AAAAA\n")
        });
        r.unwrap();
        let at = d.path().join("guides/intro.yaml");
        assert!(synced.contains(&at), "{synced:?}");
        assert_eq!(std::fs::read_to_string(&at).unwrap(), "- id: d-AAAAA\n");
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            let target = outside.path().join("kept.txt");
            std::fs::write(&target, "kept").unwrap();
            std::fs::remove_file(&at).unwrap();
            std::os::unix::fs::symlink(&target, &at).unwrap();
            write_state_copy(d.path(), "guides/intro.yaml", "new\n").unwrap();
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "kept");
            assert!(std::fs::symlink_metadata(&at).unwrap().is_file());
        }
    }

    #[test]
    fn mfb5_a_new_nested_directory_is_made_durable_in_its_parent() {
        let d = tempfile::tempdir().unwrap();
        let (r, synced) = rdy_store::fsutil::record_syncs(|| {
            write_state_copy(d.path(), "guides/deep/intro.yaml", "x\n")
        });
        r.unwrap();
        // `guides` is new in the root, `deep` is new in `guides`: both entries need a flush
        assert!(synced.contains(&d.path().to_path_buf()), "{synced:?}");
        assert!(synced.contains(&d.path().join("guides")), "{synced:?}");
        assert!(synced.contains(&d.path().join("guides/deep")), "{synced:?}");
    }

    #[test]
    fn mfb5_an_update_that_changes_one_copy_writes_and_flushes_only_that_copy() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("base/files");
        let before = [
            ("a.yaml", "a\n"),
            ("guides/b.yaml", "b\n"),
            ("guides/c.yaml", "c\n"),
            ("old/gone.yaml", "gone\n"),
        ];
        sync_state_copies(&base, &before).unwrap();
        let after = [
            ("a.yaml", "a\n"),
            ("guides/b.yaml", "b changed\n"),
            ("guides/c.yaml", "c\n"),
        ];
        let (r, synced) = rdy_store::fsutil::record_syncs(|| sync_state_copies(&base, &after));
        r.unwrap();
        assert_eq!(
            synced,
            vec![base.join("guides/b.yaml"), base.join("guides")],
            "one copy flushed, its directory flushed once"
        );
        assert_eq!(
            std::fs::read_to_string(base.join("guides/b.yaml")).unwrap(),
            "b changed\n"
        );
        assert_eq!(
            std::fs::read_to_string(base.join("guides/c.yaml")).unwrap(),
            "c\n"
        );
        // a copy whose file left the tree goes, with the directory it leaves empty
        assert!(!base.join("old").exists());
        // nothing changed: no write and no flush at all
        let (r, synced) = rdy_store::fsutil::record_syncs(|| sync_state_copies(&base, &after));
        r.unwrap();
        assert!(synced.is_empty(), "{synced:?}");
    }

    #[test]
    fn mfb5_a_checkout_flushes_each_directory_once() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("base/files");
        let all = [
            ("a.yaml", "a\n"),
            ("b.yaml", "b\n"),
            ("guides/c.yaml", "c\n"),
            ("guides/d.yaml", "d\n"),
            ("guides/deep/e.yaml", "e\n"),
            ("notes/f.yaml", "f\n"),
        ];
        let (r, synced) = rdy_store::fsutil::record_syncs(|| sync_state_copies(&base, &all));
        r.unwrap();
        let (dirs, files): (Vec<_>, Vec<_>) = synced.iter().partition(|p| p.is_dir());
        assert_eq!(files.len(), 6, "{synced:?}");
        // the root, guides, guides/deep and notes hold copies; the new `base` and `base/files` entries are flushed in their parents
        let mut want = vec![
            d.path().to_path_buf(),
            d.path().join("base"),
            base.clone(),
            base.join("guides"),
            base.join("guides/deep"),
            base.join("notes"),
        ];
        let mut got: Vec<_> = dirs.into_iter().cloned().collect();
        want.sort();
        got.sort();
        assert_eq!(got, want, "each directory once: {synced:?}");
    }

    #[test]
    fn mfb5_a_stale_copy_in_the_way_of_a_directory_is_replaced() {
        let d = tempfile::tempdir().unwrap();
        let base = d.path().join("base/files");
        sync_state_copies(&base, &[("a", "file\n")]).unwrap();
        sync_state_copies(&base, &[("a/b.yaml", "inner\n")]).unwrap();
        assert_eq!(
            std::fs::read_to_string(base.join("a/b.yaml")).unwrap(),
            "inner\n"
        );
        sync_state_copies(&base, &[("a", "file\n")]).unwrap();
        assert_eq!(std::fs::read_to_string(base.join("a")).unwrap(), "file\n");
    }
}
