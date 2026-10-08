//! The working-copy status (spec §12.2, wcss1): what changed in the files since the base, what is
//! staged, what the scan left untracked and what conflicts. Files are compared by head id, so a
//! renamed doc is one `renamed` entry, not a deletion and an addition.

use crate::tree_io::FOLDER_FILE;
use crate::wc::layout::WcTree;
use crate::wc::state::{ConflictEntry, ConflictKind, Place, WcState, stage_paths};
use rdy_model::{BlockId, Hash};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// How a file differs from the base.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    Modified,
    Added,
    Deleted,
    /// The same doc or folder under another name in the same directory.
    Renamed,
    /// The same doc or folder in another directory.
    Moved,
}

impl ChangeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeKind::Modified => "modified",
            ChangeKind::Added => "added",
            ChangeKind::Deleted => "deleted",
            ChangeKind::Renamed => "renamed",
            ChangeKind::Moved => "moved",
        }
    }
}

/// One changed file. A folder file is named by its directory with a trailing `/`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StatusEntry {
    pub path: String,
    pub change: ChangeKind,
    /// For `renamed` and `moved`: the base path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// A path the scan met but did not read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UntrackedEntry {
    pub path: String,
    pub reason: String,
    /// The command that lints it.
    pub hint: String,
}

/// One open conflict.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConflictLine {
    pub path: String,
    pub kind: ConflictKind,
    pub message: String,
}

/// The working copy against its base. Every list is sorted by path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WcStatus {
    pub view: String,
    pub head: String,
    pub staged: Vec<StatusEntry>,
    pub unstaged: Vec<StatusEntry>,
    pub untracked: Vec<UntrackedEntry>,
    pub conflicts: Vec<ConflictLine>,
}

/// A member file as the comparison sees it: its head (`None` when the file gave no id) and the
/// hash of its bytes.
struct Item {
    head: Option<BlockId>,
    hash: Hash,
}

/// The listed name of a file: a folder file is its directory with a trailing `/`.
fn shown(path: &str) -> String {
    match path.strip_suffix(FOLDER_FILE) {
        Some(dir) if dir.ends_with('/') => dir.to_string(),
        _ => path.to_string(),
    }
}

/// The directory that holds the doc or folder a file describes: a folder's `_folder.yaml` lives
/// inside it, so the folder's own directory is its parent.
fn holder(path: &str) -> &Path {
    let p = Path::new(path);
    let p = if p.file_name().is_some_and(|n| n == FOLDER_FILE) {
        p.parent().unwrap_or(p)
    } else {
        p
    };
    p.parent().unwrap_or(Path::new(""))
}

/// The id of the first entry of a file's bytes (the doc or the folder), when it has one.
fn head_of(bytes: &[u8]) -> Option<BlockId> {
    let v: serde_yaml_ng::Value = serde_yaml_ng::from_slice(bytes).ok()?;
    let first = match &v {
        serde_yaml_ng::Value::Sequence(s) => s.first()?,
        other => other,
    };
    BlockId::parse(first.get("id")?.as_str()?).ok()
}

/// The change of the file at `path` against the base, or `None` when it is the base's. `vacated`
/// says whether a base path no longer holds its head (so a head found at another path moved
/// there).
fn classify(
    place: &Place,
    st: &WcState,
    path: &str,
    item: &Item,
    vacated: &dyn Fn(&str, &BlockId) -> bool,
) -> Option<(ChangeKind, Option<String>)> {
    let Some(head) = &item.head else {
        return Some((ChangeKind::Added, None));
    };
    if st.base.files.get(path) == Some(head) {
        let base = fs::read(place.base_file(path)).ok().map(|b| Hash::of(&b));
        return (base.as_ref() != Some(&item.hash)).then_some((ChangeKind::Modified, None));
    }
    let from = st
        .base
        .files
        .iter()
        .find(|(p, h)| *h == head && p.as_str() != path && vacated(p, h));
    Some(match from {
        Some((p, _)) => {
            let kind = if holder(p) == holder(path) {
                ChangeKind::Renamed
            } else {
                ChangeKind::Moved
            };
            (kind, Some(shown(p)))
        }
        None => (ChangeKind::Added, None),
    })
}

fn deleted(path: &str) -> StatusEntry {
    StatusEntry {
        path: shown(path),
        change: ChangeKind::Deleted,
        from: None,
    }
}

fn sorted(mut v: Vec<StatusEntry>) -> Vec<StatusEntry> {
    v.sort_by(|a, b| a.path.cmp(&b.path).then(a.change.cmp(&b.change)));
    v
}

/// Compares the working tree, the stage and the base (wcss1).
///
/// - **Working changes** are the member files against the base by head id; a base path whose head
///   is in no member file is `deleted`.
/// - **Staged** is the same comparison over the stage: the staged copies, and the staged
///   deletions whose head no staged copy carries.
/// - **Unstaged** is every working change that is not staged with the same bytes, a staged file
///   whose working bytes differ from its copy, and a staged addition whose file is gone.
pub fn compute(place: &Place, st: &WcState, tree: &WcTree) -> WcStatus {
    let (view, head) = st
        .bound
        .as_ref()
        .map(|b| (b.view.clone(), b.head.clone()))
        .unwrap_or_default();
    let working: BTreeMap<&str, Item> = tree
        .files
        .iter()
        .map(|(p, f)| {
            (
                p.as_str(),
                Item {
                    head: (!f.new_head).then(|| f.head.clone()),
                    hash: f.hash,
                },
            )
        })
        .collect();
    let mut copies: BTreeMap<String, Item> = BTreeMap::new();
    for p in stage_paths(place) {
        if let Ok(bytes) = fs::read(place.stage_file(&p)) {
            copies.insert(
                p,
                Item {
                    head: head_of(&bytes),
                    hash: Hash::of(&bytes),
                },
            );
        }
    }
    let working_heads: BTreeSet<&BlockId> =
        working.values().filter_map(|i| i.head.as_ref()).collect();
    let staged_heads: BTreeSet<&BlockId> =
        copies.values().filter_map(|i| i.head.as_ref()).collect();

    let work_vacated =
        |p: &str, h: &BlockId| working.get(p).and_then(|i| i.head.as_ref()) != Some(h);
    // A base path is vacated in the stage only by what the stage holds: a staged copy at the path
    // with another head, or a staged deletion. The working tree does not count (a half-staged
    // rename is not a staged rename).
    let stage_vacated = |p: &str, h: &BlockId| match copies.get(p) {
        Some(c) => c.head.as_ref() != Some(h),
        None => st.stage.deleted.contains(p),
    };

    let mut staged = vec![];
    for (p, item) in &copies {
        if let Some((change, from)) = classify(place, st, p, item, &stage_vacated) {
            staged.push(StatusEntry {
                path: shown(p),
                change,
                from,
            });
        }
    }
    for p in &st.stage.deleted {
        if let Some(h) = st.base.files.get(p)
            && !staged_heads.contains(h)
            && !copies.contains_key(p)
        {
            staged.push(deleted(p));
        }
    }

    let mut unstaged = vec![];
    // base paths that an unstaged `renamed` or `moved` entry already reports as vacated
    let mut reported_from: BTreeSet<String> = BTreeSet::new();
    for (p, item) in &working {
        let change = classify(place, st, p, item, &work_vacated);
        match (copies.get(*p), change) {
            (Some(c), _) if c.hash == item.hash => {}
            (Some(_), None) => unstaged.push(StatusEntry {
                path: shown(p),
                change: ChangeKind::Modified,
                from: None,
            }),
            (_, Some((change, from))) => {
                reported_from.extend(from.clone());
                unstaged.push(StatusEntry {
                    path: shown(p),
                    change,
                    from,
                });
            }
            (None, None) => {}
        }
    }
    // A staged deletion whose file is in the tree again: the stage and the tree disagree.
    for p in &st.stage.deleted {
        if working.contains_key(p.as_str()) && !unstaged.iter().any(|e| e.path == shown(p)) {
            unstaged.push(StatusEntry {
                path: shown(p),
                change: ChangeKind::Added,
                from: None,
            });
        }
    }
    for (p, h) in &st.base.files {
        if st.stage.deleted.contains(p) || reported_from.contains(&shown(p)) {
            continue;
        }
        // Gone from the tree and its head nowhere in it, or moved on in the tree while the stage
        // still keeps the head here (only the new copy is staged).
        let gone = !working_heads.contains(h);
        let half_staged = work_vacated(p, h) && staged_heads.contains(h) && !stage_vacated(p, h);
        if gone || half_staged {
            unstaged.push(deleted(p));
        }
    }
    for p in copies.keys() {
        if !working.contains_key(p.as_str()) && !st.base.files.contains_key(p) {
            unstaged.push(deleted(p));
        }
    }

    let untracked = tree
        .untracked
        .iter()
        .map(|u| UntrackedEntry {
            path: u.path.clone(),
            reason: u.reason.clone(),
            hint: format!("rdy check {}", u.path),
        })
        .collect();

    let mut conflicts: Vec<ConflictLine> = st
        .conflicts
        .iter()
        .map(|c| ConflictLine {
            path: c.path.clone(),
            kind: c.kind,
            message: conflict_message(c),
        })
        .collect();
    conflicts.sort_by(|a, b| a.path.cmp(&b.path));

    WcStatus {
        view,
        head,
        staged: sorted(staged),
        unstaged: sorted(unstaged),
        untracked,
        conflicts,
    }
}

/// The status line of one open conflict.
pub fn conflict_message(c: &ConflictEntry) -> String {
    match c.kind {
        ConflictKind::Blocks => format!(
            "{}: changed here and in the store (see {}.theirs.yaml)",
            c.path,
            c.path.strip_suffix(".yaml").unwrap_or(&c.path),
        ),
        ConflictKind::Kept => match c.store_name.as_deref() {
            Some(to) if to != c.path => format!(
                "{}: committed, but not written back to {to} (a file is in the way); move {to} aside, then run rdy checkout --force",
                c.path
            ),
            _ => format!(
                "{}: committed, but not written back (it holds edits the commit did not record); copy it aside, then run rdy checkout --force",
                c.path
            ),
        },
        ConflictKind::DocRename | ConflictKind::FolderRename => {
            let what = if c.kind == ConflictKind::DocRename {
                "doc"
            } else {
                "folder"
            };
            format!(
                "{what} renamed here to {}, in the store to {}",
                c.local_name.as_deref().unwrap_or("?"),
                c.store_name.as_deref().unwrap_or("?")
            )
        }
    }
}

fn section(out: &mut String, title: &str, lines: impl Iterator<Item = String>) {
    let lines: Vec<String> = lines.collect();
    if lines.is_empty() {
        return;
    }
    out.push_str(title);
    out.push('\n');
    for l in lines {
        out.push_str("  ");
        out.push_str(&l);
        out.push('\n');
    }
}

fn entry_line(e: &StatusEntry) -> String {
    match &e.from {
        Some(from) => format!("{:<9} {from} -> {}", e.change.as_str(), e.path),
        None => format!("{:<9} {}", e.change.as_str(), e.path),
    }
}

/// The text of a status, without a trailing newline (the CLI appends it to the store status after
/// one blank line).
pub fn status_text(s: &WcStatus) -> String {
    let clean = s.staged.is_empty()
        && s.unstaged.is_empty()
        && s.untracked.is_empty()
        && s.conflicts.is_empty();
    let mut out = format!(
        "working copy bound to {}{}\n",
        s.view,
        if clean { ", clean" } else { "" }
    );
    section(&mut out, "staged:", s.staged.iter().map(entry_line));
    section(&mut out, "not staged:", s.unstaged.iter().map(entry_line));
    section(
        &mut out,
        "untracked (check with rdy check <path>):",
        s.untracked
            .iter()
            .map(|u| format!("{}: {}", u.path, u.reason)),
    );
    section(
        &mut out,
        "conflicts:",
        s.conflicts.iter().map(|c| c.message.clone()),
    );
    out.truncate(out.trim_end().len());
    out
}
