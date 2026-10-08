//! Checkout, dirty paths and the write-back: the working copy's files against its base.

use crate::error::*;
use crate::now_ms;
use crate::tree_io::Diagnostic;
use crate::wc::layout::{FileKind, WcTree};
use crate::wc::project::Projection;
use crate::wc::state::{
    Base, BaseBlock, Bound, CheckoutMark, ConflictEntry, ConflictKind, Place, Stage, WcState,
    new_nonce, stage_paths,
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

/// Whether one of the directories between the root and `rel`'s file exists as something other
/// than a directory (a link, a file): what is under it is not the working tree's, so it is never
/// read, written or deleted through it.
fn behind_link(root: &Path, rel: &Path) -> bool {
    ancestors(root, rel)
        .iter()
        .any(|d| fs::symlink_metadata(d).is_ok_and(|m| !m.file_type().is_dir()))
}

/// The file's identity on disk (device and inode), for a regular file not behind a link.
fn identity(root: &Path, rel: &str) -> Option<(u64, u64)> {
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

/// Gives the existing directory `dir` the spelling its path asks for. On a case-insensitive
/// filesystem `Guides` opens the directory stored as `guides`; it is renamed through a temporary
/// name, never deleted and recreated, so what else it holds stays in it.
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
/// replaced, never written through. With `cases`, each existing directory on the way is given
/// its path's spelling once (`fix_case`); `cases` holds the directories already done.
fn write_at(
    root: &Path,
    rel: &str,
    text: &str,
    mut cases: Option<&mut BTreeSet<PathBuf>>,
) -> Result<(), RdyError> {
    let p = inside(rel)?;
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
    let path = root.join(p);
    if fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_dir()) {
        remove_file(&path)?;
    }
    fs::write(&path, text).map_err(|e| io_err(&path, "write", e))
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
    remove_tree(&base_dir)?;
    fs::create_dir_all(&base_dir).map_err(|e| io_err(&base_dir, "create", e))?;
    for f in &proj.files {
        write_at(&base_dir, &f.path, &f.text, None)?;
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
    st.checkout = None;
    st.save(place)?;
    Ok(summary)
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
fn prune_dirs(top: &Path, rel: &str) {
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
/// refuses every later commit until a checkout replaces the files (the old base would let the
/// next commit create the commit's new blocks a second time). A path to delete that holds
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
                        "copy {} aside, then run rdy checkout --force (it discards uncommitted edits); commit is refused until then",
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
                        "move {} aside, then run rdy checkout --force (it discards uncommitted edits); commit is refused until then",
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
                        "move {} aside, then run rdy checkout --force (it discards uncommitted edits); commit is refused until then",
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
        write_at(&base_dir, &f.path, &f.text, None)?;
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
