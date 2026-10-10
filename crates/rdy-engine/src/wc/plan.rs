//! The commit planner (wcdf1, FW-D14, R-P4, R-P11): the staged files, laid over the base, against
//! the base itself, as one write. Updates carry their checked-out bases; new entries are creates
//! with their client ids, placed by position; a block missing from every file is a delete only
//! when it is a doc asset; moves are a longest increasing subsequence away, so one moved paragraph
//! is one `move_to`; a link kind is named only when the file changed it, and then with the links
//! the file never shows.

use crate::error::{E_FILE_FORMAT, E_IO, E_PATH_CLASH, E_WC_INVALID, W_WC_UNSTAGED_MOVE};
use crate::query::field_json;
use crate::tree_io::{Diagnostic, FOLDER_FILE};
use crate::wc::ignore::Ignore;
use crate::wc::layout::{WcBlock, WcTree, read_tree};
use crate::wc::lint::lint_tree;
use crate::wc::state::{Place, WcState, stage_paths};
use rdy_model::{BlockId, FieldValue, tree};
use rdy_schema::{Pack, STORE_ONLY_FIELDS, STORE_ONLY_LINK_KINDS, Severity, TypeClass};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// One write that commits the staged files.
#[derive(Clone, Debug, Default)]
pub struct CommitPlan {
    /// `rdy_write` op objects, in the order to send.
    pub ops: Vec<Value>,
    /// Every id an op names.
    pub touched: BTreeSet<BlockId>,
    /// The ids the creates give their new blocks.
    pub created: BTreeSet<BlockId>,
    /// The doc and folder ids whose files the write-back rewrites.
    pub rewrite: BTreeSet<BlockId>,
    /// The base paths whose head is gone or now lives at another path.
    pub removed_files: BTreeSet<String>,
    /// The staged paths (copies and deletions) this plan consumes.
    pub committed: BTreeSet<String>,
    /// Figures to upload before the write: the op for `block` carries `"$upload:<n>"` (its
    /// index here) as its `blob` until the client has the hash.
    pub uploads: Vec<Upload>,
    /// Each block's file, line and column in the commit tree.
    pub positions: BTreeMap<BlockId, (String, usize, usize)>,
    pub warnings: Vec<Diagnostic>,
    /// The plan touches stable content: a block an op names, or the parent a create sits under,
    /// is stable (its own `lifecycle`, else its nearest ancestor's) in the base or in the commit
    /// tree.
    pub stable: bool,
}

/// A figure file to upload as its block's blob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upload {
    pub block: BlockId,
    pub file: PathBuf,
    pub media_type: Option<String>,
}

/// The staged files do not lint: every diagnostic, sorted by file, line and column.
#[derive(Clone, Debug)]
pub struct WcInvalid {
    pub diags: Vec<Diagnostic>,
}

/// The prefix of the placeholder an upload's op carries in place of the blob hash.
pub const UPLOAD_PREFIX: &str = "$upload:";

/// Whether a figure's `blob` is a blob hash (64 hex characters) rather than a path.
fn is_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The media type of an image or PDF file name, from its extension.
fn media_type_of(path: &str) -> Option<&'static str> {
    let ext = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        _ => return None,
    })
}

/// The path an entry names as its image: an entry of an `asset` type whose `blob` is a string that
/// is not a hash.
fn asset_path<'a>(b: &'a WcBlock, pack: &Pack) -> Option<&'a str> {
    if !pack.is_asset(&b.block.block_type) {
        return None;
    }
    b.block
        .fields
        .get("blob")
        .and_then(FieldValue::as_str)
        .filter(|s| !is_hash(s))
}

/// Why a figure's image was refused.
enum Refused {
    Outside,
    Missing,
}

/// The file a figure in `file` names by `rel` (R-P13): relative to the doc file's directory,
/// resolved lexically (`..` only while it stays inside `root`), then through the file system, so
/// a link out of the working tree does not escape. Returns the absolute path of a regular file.
fn figure_file(root: &Path, file: &str, rel: &str) -> Result<PathBuf, Refused> {
    let mut parts: Vec<&OsStr> = Path::new(file)
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(s) => parts.push(s),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop().ok_or(Refused::Outside)?;
            }
            Component::RootDir | Component::Prefix(_) => return Err(Refused::Outside),
        }
    }
    let joined = parts.iter().fold(root.to_path_buf(), |p, s| p.join(s));
    let real = fs::canonicalize(&joined).map_err(|_| Refused::Missing)?;
    let base = fs::canonicalize(root).map_err(|_| Refused::Missing)?;
    if !real.starts_with(&base) {
        return Err(Refused::Outside);
    }
    if !fs::metadata(&real).is_ok_and(|m| m.is_file()) {
        return Err(Refused::Missing);
    }
    Ok(real)
}

/// The figure rule (wcfg1), run before the entries are validated: every figure whose `blob` is a
/// path gets its `media_type` from the file extension when the entry gives none, and is refused
/// when the image is outside the working copy, missing, or of a type rdy cannot name.
pub fn resolve_figures(tree: &mut WcTree, root: &Path, pack: &Pack) -> Vec<Diagnostic> {
    let mut out = vec![];
    for b in &mut tree.blocks {
        let Some(path) = asset_path(b, pack).map(str::to_string) else {
            continue;
        };
        let id = b.block.id.clone();
        let ty = b.block.block_type.clone();
        let diag = |message: String, fix: &str| {
            Diagnostic::error(&b.file, b.at, E_FILE_FORMAT, message, fix)
        };
        let given = b.block.fields.contains_key("media_type");
        let inferred = media_type_of(&path);
        match figure_file(root, &b.file, &path) {
            Ok(_) => {}
            Err(Refused::Outside) => out.push(diag(
                format!("{ty} {id}: {path} is outside the working copy"),
                "keep the image inside the working copy, next to the doc or below it",
            )),
            Err(Refused::Missing) => out.push(diag(
                format!("{ty} {id}: no image at {path}"),
                "put the image at that path, relative to the doc file's directory",
            )),
        }
        if !given {
            match inferred {
                Some(m) => {
                    b.block
                        .fields
                        .insert("media_type".into(), FieldValue::Str(m.into()));
                }
                None => out.push(diag(
                    format!("{ty} {id}: set media_type for {path}"),
                    "add media_type, e.g. image/png, to the entry's fields",
                )),
            }
        }
    }
    out
}

/// Where the commit tree is built, under the state directory.
const TMP_DIR: &str = "tmp";
const COMMIT_DIR: &str = "commit";

/// Plans the commit of the staged files (wcdf1). The commit tree, every base file not staged plus
/// every staged copy less every staged deletion, is linted and diffed against the base tree.
pub fn plan_commit(place: &Place, st: &WcState, pack: &Pack) -> Result<CommitPlan, WcInvalid> {
    let staged = stage_paths(place);
    let tmp = place.state.join(TMP_DIR);
    let dir = tmp.join(COMMIT_DIR);
    let built = build_commit_tree(place, st, &staged, &dir);
    let commit = built.map(|()| read_tree(&dir, &Ignore::empty(), pack));
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir(&tmp); // only when nothing else is in it
    let commit = commit.map_err(|diags| WcInvalid { diags })?;
    let mut commit = commit;
    let mut diags = lint_tree(&mut commit, pack, None, Some(&place.root));
    diags.extend(unread(&commit, st, &staged));
    diags.sort_by(|a, b| (&a.file, a.line, a.column).cmp(&(&b.file, b.line, b.column)));
    if diags.iter().any(|d| d.severity == Severity::Error) {
        return Err(WcInvalid { diags });
    }
    let base = read_tree(&place.state.join("base/files"), &Ignore::empty(), pack);
    let committed: BTreeSet<String> = staged
        .iter()
        .chain(st.stage.deleted.iter())
        .cloned()
        .collect();
    let warnings = diags
        .into_iter()
        .filter(|d| committed.contains(&d.file))
        .collect();
    let mut plan = CommitPlan {
        committed,
        warnings,
        ..CommitPlan::default()
    };
    Diff {
        place,
        st,
        pack,
        base: Indexed::of(&base),
        commit: Indexed::of(&commit),
        base_tree: &base,
        commit_tree: &commit,
        staged: &staged,
    }
    .run(&mut plan);
    Ok(plan)
}

fn io_diag(file: &str, what: &str, e: impl std::fmt::Display) -> Diagnostic {
    Diagnostic::error(
        file,
        (1, 1),
        E_IO,
        format!("cannot {what}: {e}"),
        "check the working copy's state directory and its permissions",
    )
}

/// `rel` under `dir`, when it is a plain relative path.
fn under(dir: &Path, rel: &str) -> Result<PathBuf, Diagnostic> {
    let p = Path::new(rel);
    if rel.is_empty() || !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(io_diag(rel, "build the commit tree", "not a relative path"));
    }
    Ok(dir.join(p))
}

/// Links `from` at a new `to`; where the file system refuses a link, copies it into a new file.
/// Never writes through a file already at `to` (it may be a link to a base copy): that is
/// `AlreadyExists`.
fn link_in(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::hard_link(from, to) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => {
            let mut src = fs::File::open(from)?;
            let mut dst = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(to)?;
            std::io::copy(&mut src, &mut dst).map(|_| ())
        }
        r => r,
    }
}

/// The error of a staged path that names the same file as a base path, the file system ignoring
/// case or Unicode form; `shared` is the part of the two paths that clashes.
fn clash(staged: &str, base: &str, shared: Option<(&str, &str)>) -> Diagnostic {
    let message = match shared {
        Some((a, b)) if a != staged => format!(
            "{staged} and {base} are under directories with the same name ignoring case ({a} and {b})"
        ),
        _ => format!("{staged} and {base} have the same name ignoring case"),
    };
    Diagnostic::error(
        staged,
        (1, 1),
        E_PATH_CLASH,
        message,
        format!(
            "stage the deletion of {base} as well (a rename), or give {staged} the spelling {base} has"
        ),
    )
}

/// The staged paths that clash, ignoring case, with a base path that stays in the commit tree:
/// on a file system that ignores case both name one file, so one would land on the other.
fn clashes(staged: &[String], kept: &[&String]) -> Vec<Diagnostic> {
    let prefixes = |p: &'_ str| -> Vec<String> {
        let mut out = vec![];
        for (i, c) in p.char_indices() {
            if c == '/' {
                out.push(p[..i].to_string());
            }
        }
        out.push(p.to_string());
        out
    };
    let mut seen: BTreeMap<String, (String, &str)> = BTreeMap::new();
    for rel in kept {
        for pre in prefixes(rel) {
            seen.entry(pre.to_lowercase())
                .or_insert((pre, rel.as_str()));
        }
    }
    let mut out = vec![];
    for rel in staged {
        for pre in prefixes(rel) {
            if let Some((theirs, base)) = seen.get(&pre.to_lowercase())
                && *theirs != pre
            {
                out.push(clash(rel, base, Some((&pre, theirs))));
                break;
            }
        }
    }
    out
}

/// Rebuilds `dir`: every base file that is not staged, linked from its base copy; every staged
/// copy, linked from the stage; a staged deletion left out. A staged path that clashes with a
/// base file is refused before anything is linked.
fn build_commit_tree(
    place: &Place,
    st: &WcState,
    staged: &[String],
    dir: &Path,
) -> Result<(), Vec<Diagnostic>> {
    let shown = dir.display().to_string();
    match fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(vec![io_diag(&shown, "clear the commit tree", e)]),
    }
    fs::create_dir_all(dir).map_err(|e| vec![io_diag(&shown, "create the commit tree", e)])?;
    let is_staged: BTreeSet<&str> = staged.iter().map(String::as_str).collect();
    let kept: Vec<&String> = st
        .base
        .files
        .keys()
        .filter(|rel| !is_staged.contains(rel.as_str()) && !st.stage.deleted.contains(*rel))
        .collect();
    let found = clashes(staged, &kept);
    if !found.is_empty() {
        return Err(found);
    }
    let one = |e: Diagnostic| vec![e];
    for rel in &kept {
        link_in(&place.base_file(rel), &under(dir, rel).map_err(one)?)
            .map_err(|e| one(io_diag(rel, "link the base copy", e)))?;
    }
    for rel in staged {
        let to = under(dir, rel).map_err(one)?;
        match link_in(&place.stage_file(rel), &to) {
            Ok(()) => {}
            // the file system names it as a kept base file (Unicode form or case folding
            // beyond what `clashes` sees)
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let base = kept
                    .iter()
                    .find(|b| under(dir, b).is_ok_and(|p| same_file(&p, &to)))
                    .map_or("a base file", |b| b.as_str());
                return Err(one(clash(rel, base, None)));
            }
            Err(e) => return Err(one(io_diag(rel, "link the staged copy", e))),
        }
    }
    Ok(())
}

/// Whether `a` and `b` are one file.
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::symlink_metadata(a), fs::symlink_metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

/// The errors of a commit tree that does not read every file it holds: a file the reader leaves
/// untracked (it does not parse, its first entry is not the doc, it is too large), and a file
/// the reader never reaches (its directory has no `_folder.yaml`, or one that does not load).
/// Each staged path and each base path not staged for deletion must be read, so that no block
/// of a file that is not read turns into a delete.
fn unread(commit: &WcTree, st: &WcState, staged: &[String]) -> Vec<Diagnostic> {
    let untracked: BTreeMap<&str, &str> = commit
        .untracked
        .iter()
        .map(|u| (u.path.as_str(), u.reason.as_str()))
        .collect();
    let mut out: Vec<Diagnostic> = commit
        .untracked
        .iter()
        .map(|u| {
            Diagnostic::error(
                &u.path,
                u.at.unwrap_or((1, 1)),
                E_WC_INVALID,
                format!("{} is not read: {}", u.path, u.reason),
                u.fix.clone(),
            )
        })
        .collect();
    let expected: BTreeSet<&String> = staged
        .iter()
        .chain(
            st.base
                .files
                .keys()
                .filter(|rel| !st.stage.deleted.contains(*rel)),
        )
        .collect();
    let blobs: BTreeSet<&str> = commit.blobs.iter().map(String::as_str).collect();
    for rel in expected {
        if commit.files.contains_key(rel.as_str())
            || untracked.contains_key(rel.as_str())
            || blobs.contains(rel.as_str())
        {
            continue;
        }
        let dirs = rel
            .char_indices()
            .filter(|&(_, c)| c == '/')
            .map(|(i, _)| &rel[..i]);
        let mut why = None;
        for d in dirs {
            let ff = format!("{d}/{FOLDER_FILE}");
            if *rel == ff {
                break;
            }
            if let Some(reason) = untracked.get(d) {
                why = Some((
                    format!("{rel} is in {d}, which is not read: {reason}"),
                    format!("rdy check {d}"),
                ));
            } else if commit.files.contains_key(&ff) {
                continue;
            } else if untracked.contains_key(ff.as_str()) {
                why = Some((
                    format!("{rel} is in {d}, whose {ff} does not load"),
                    format!("fix {ff} (rdy check {ff}) and stage it"),
                ));
            } else {
                why = Some((
                    format!("{rel} is in {d}, which has no {ff}"),
                    format!("restore {ff} and stage it, or move {rel} into a folder"),
                ));
            }
            break;
        }
        let (message, fix) = why.unwrap_or_else(|| {
            (
                format!("{rel} is not read as a member of the commit"),
                format!("rdy check {rel}"),
            )
        });
        out.push(Diagnostic::error(rel, (1, 1), E_WC_INVALID, message, fix));
    }
    out
}

/// A tree's blocks by id (the first entry of an id) and by parent, in tree order.
struct Indexed<'a> {
    by_id: BTreeMap<&'a BlockId, &'a WcBlock>,
    kids: BTreeMap<Option<&'a BlockId>, Vec<&'a WcBlock>>,
    /// Each block's place in the tree's block list.
    seq: BTreeMap<&'a BlockId, usize>,
}

impl<'a> Indexed<'a> {
    fn of(t: &'a WcTree) -> Indexed<'a> {
        let mut by_id = BTreeMap::new();
        let mut kids: BTreeMap<Option<&BlockId>, Vec<&WcBlock>> = BTreeMap::new();
        let mut seq = BTreeMap::new();
        for (i, b) in t.blocks.iter().enumerate() {
            if by_id.contains_key(&b.block.id) {
                continue;
            }
            by_id.insert(&b.block.id, b);
            seq.insert(&b.block.id, i);
            kids.entry(b.block.parent.as_ref()).or_default().push(b);
        }
        Indexed { by_id, kids, seq }
    }

    /// The content (non-tree) children of `parent`, in file order.
    fn content(&self, parent: Option<&BlockId>) -> Vec<&'a WcBlock> {
        self.kids
            .get(&parent)
            .map(|k| {
                k.iter()
                    .copied()
                    .filter(|b| !tree::is_tree_type(&b.block.block_type))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every block, a parent before its children, siblings in tree order.
    fn pre_order(&self, t: &'a WcTree) -> Vec<&'a WcBlock> {
        let mut out = vec![];
        let mut seen: BTreeSet<&BlockId> = BTreeSet::new();
        let roots: Vec<&WcBlock> = t
            .blocks
            .iter()
            .filter(|b| {
                self.by_id
                    .get(&b.block.id)
                    .is_some_and(|f| std::ptr::eq(*f, *b))
                    && b.block
                        .parent
                        .as_ref()
                        .is_none_or(|p| !self.by_id.contains_key(p))
            })
            .collect();
        let mut stack: Vec<&WcBlock> = roots.into_iter().rev().collect();
        while let Some(b) = stack.pop() {
            if !seen.insert(&b.block.id) {
                continue;
            }
            out.push(b);
            if let Some(k) = self.kids.get(&Some(&b.block.id)) {
                stack.extend(k.iter().rev().copied());
            }
        }
        // a parent cycle (the lint refuses one) still lists every block once
        for b in self.by_id.values() {
            if !seen.contains(&b.block.id) {
                out.push(b);
            }
        }
        out
    }

    /// How many ancestors `id` has in the tree.
    /// `id`'s own `lifecycle`, else its nearest ancestor's, is `stable` (a null value counts as
    /// none), as the engine's briefing check reads it.
    fn stable(&self, id: &BlockId) -> bool {
        let mut cur = self.by_id.get(id);
        let mut steps = 0;
        while let Some(b) = cur {
            match b.block.fields.get("lifecycle") {
                Some(FieldValue::Null) | None => {}
                Some(v) => return v.as_str() == Some("stable"),
            }
            steps += 1;
            if steps > self.by_id.len() {
                break;
            }
            cur = b.block.parent.as_ref().and_then(|p| self.by_id.get(p));
        }
        false
    }

    fn depth(&self, id: &BlockId) -> usize {
        let mut n = 0;
        let mut cur = self.by_id.get(id).and_then(|b| b.block.parent.as_ref());
        while let Some(p) = cur {
            n += 1;
            if n > self.by_id.len() {
                break;
            }
            cur = self.by_id.get(p).and_then(|b| b.block.parent.as_ref());
        }
        n
    }
}

/// The ids of the longest increasing subsequence of `seq` by its numbers: patience sorting with
/// predecessor links, scanning left to right.
fn lis<'a>(seq: &[(&'a BlockId, usize)]) -> BTreeSet<&'a BlockId> {
    let mut tails: Vec<usize> = vec![];
    let mut prev: Vec<Option<usize>> = vec![None; seq.len()];
    for (i, &(_, v)) in seq.iter().enumerate() {
        let pos = tails.partition_point(|&t| seq[t].1 < v);
        if pos > 0 {
            prev[i] = Some(tails[pos - 1]);
        }
        if pos == tails.len() {
            tails.push(i);
        } else {
            tails[pos] = i;
        }
    }
    let mut out = BTreeSet::new();
    let mut cur = tails.last().copied();
    while let Some(i) = cur {
        out.insert(seq[i].0);
        cur = prev[i];
    }
    out
}

/// One link as a file shows it: its target (an id, or a key when `keyed`) and its part.
#[derive(Clone, PartialEq, Eq)]
struct Shown {
    target: String,
    part: Option<String>,
    keyed: bool,
}

/// The links an entry shows, by kind, keyed targets at their places (llst1); a kind with no
/// target is absent.
fn shown_links(b: &WcBlock) -> BTreeMap<&str, Vec<Shown>> {
    let mut out: BTreeMap<&str, Vec<Shown>> = BTreeMap::new();
    for (kind, list) in &b.block.links {
        out.insert(
            kind,
            list.iter()
                .map(|l| Shown {
                    target: l.target.to_string(),
                    part: l.part.clone(),
                    keyed: false,
                })
                .collect(),
        );
    }
    let mut keyed: Vec<_> = b.keyed.iter().collect();
    keyed.sort_by(|x, y| (x.kind(), x.at()).cmp(&(y.kind(), y.at())));
    for k in keyed {
        let list = out.entry(k.kind()).or_default();
        let at = k.at().min(list.len());
        list.insert(
            at,
            Shown {
                target: k.text().to_string(),
                part: k.part().map(str::to_string),
                keyed: true,
            },
        );
    }
    out.retain(|_, v| !v.is_empty());
    out
}

fn link_json(target: &str, part: Option<&str>) -> Value {
    let mut m = Map::new();
    m.insert("target".into(), target.into());
    if let Some(p) = part {
        m.insert("part".into(), p.into());
    }
    Value::Object(m)
}

fn shown_json(list: &[Shown]) -> Vec<Value> {
    list.iter()
        .map(|l| link_json(&l.target, l.part.as_deref()))
        .collect()
}

fn store_only_field(f: &str) -> bool {
    STORE_ONLY_FIELDS.contains(&f)
}

struct Diff<'a> {
    place: &'a Place,
    st: &'a WcState,
    pack: &'a Pack,
    base: Indexed<'a>,
    commit: Indexed<'a>,
    base_tree: &'a WcTree,
    commit_tree: &'a WcTree,
    staged: &'a [String],
}

impl<'a> Diff<'a> {
    fn run(&self, plan: &mut CommitPlan) {
        let placed = self.placements();
        for b in self.commit.pre_order(self.commit_tree) {
            let id = &b.block.id;
            plan.positions
                .insert(id.clone(), (b.file.clone(), b.at.0, b.at.1));
            let op = match self.base.by_id.get(id) {
                Some(old) => self.update(old, b, placed.get(id)),
                None => {
                    plan.created.insert(id.clone());
                    Some(self.create(b, placed.get(id)))
                }
            };
            let op = op.map(|op| self.upload(b, op, plan));
            if let Some(op) = op {
                plan.touched.insert(id.clone());
                plan.ops.push(op);
            }
        }
        let gone = self.deletes(plan);
        self.files(plan, &gone);
        let parents = plan
            .ops
            .iter()
            .filter(|o| o["op"] == "create")
            .filter_map(|o| {
                BlockId::parse(o["parent"].as_str()?)
                    .ok()
                    .filter(|p| !plan.created.contains(p))
            });
        let subjects: Vec<BlockId> = plan
            .touched
            .iter()
            .filter(|id| !plan.created.contains(*id))
            .cloned()
            .chain(parents)
            .collect();
        plan.stable = subjects
            .iter()
            .any(|id| self.base.stable(id) || self.commit.stable(id));
    }

    /// A figure whose `blob` is a path: its image becomes an upload, and the op carries the
    /// placeholder for the hash in its `blob` field.
    fn upload(&self, b: &WcBlock, mut op: Value, plan: &mut CommitPlan) -> Value {
        let Some(path) = asset_path(b, self.pack) else {
            return op;
        };
        // the lint refused every path this cannot resolve
        let Ok(file) = figure_file(&self.place.root, &b.file, path) else {
            return op;
        };
        let media_type = b
            .block
            .fields
            .get("media_type")
            .and_then(FieldValue::as_str)
            .map(str::to_string);
        let placeholder = format!("{UPLOAD_PREFIX}{}", plan.uploads.len());
        if let Some(m) = op.as_object_mut() {
            let fields = m.entry("fields").or_insert_with(|| json!({}));
            if let Some(f) = fields.as_object_mut() {
                f.insert("blob".into(), placeholder.into());
            }
        }
        plan.uploads.push(Upload {
            block: b.block.id.clone(),
            file,
            media_type,
        });
        op
    }

    /// Where each placed block goes (R-P4): per parent, the content children the commit tree
    /// holds in file order; a child outside the longest run kept in base order, or one whose
    /// parent changed, or a new one, sits after its previous sibling. When first, it sits before
    /// the first later sibling in the run (the only siblings under the parent when its op runs),
    /// or is appended when there is none. A doc or folder whose parent changed moves under its
    /// new parent.
    fn placements(&self) -> BTreeMap<&'a BlockId, Map<String, Value>> {
        let mut out = BTreeMap::new();
        let base_pos: BTreeMap<&BlockId, usize> = self
            .base
            .kids
            .keys()
            .flat_map(|p| {
                self.base
                    .content(*p)
                    .into_iter()
                    .enumerate()
                    .map(|(i, b)| (&b.block.id, i))
            })
            .collect();
        for (parent, kids) in &self.commit.kids {
            let parent_of =
                |id: &BlockId| self.base.by_id.get(id).map(|old| old.block.parent.as_ref());
            let content = self.commit.content(*parent);
            let kept: Vec<(&BlockId, usize)> = content
                .iter()
                .filter(|b| parent_of(&b.block.id) == Some(*parent))
                .filter_map(|b| Some((&b.block.id, *base_pos.get(&b.block.id)?)))
                .collect();
            let run = lis(&kept);
            for (i, b) in content.iter().enumerate() {
                let id = &b.block.id;
                if parent_of(id) == Some(*parent) && run.contains(id) {
                    continue;
                }
                let mut m = Map::new();
                if let Some(p) = parent {
                    m.insert("parent".into(), p.to_string().into());
                }
                if i > 0 {
                    m.insert("after".into(), content[i - 1].block.id.to_string().into());
                } else if let Some(next) = content[1..].iter().find(|n| run.contains(&n.block.id)) {
                    // the ops follow F, so only a block of the run is under P when this one is
                    // placed; with none, it is appended and the rest of F follows it
                    m.insert("before".into(), next.block.id.to_string().into());
                }
                out.insert(id, m);
            }
            for b in kids
                .iter()
                .filter(|b| tree::is_tree_type(&b.block.block_type))
            {
                let id = &b.block.id;
                if parent_of(id).is_some_and(|old| old != *parent) {
                    let mut m = Map::new();
                    if let Some(p) = parent {
                        m.insert("parent".into(), p.to_string().into());
                    }
                    out.insert(id, m);
                }
            }
        }
        out
    }

    fn create(&self, b: &WcBlock, placed: Option<&Map<String, Value>>) -> Value {
        let mut op = Map::new();
        op.insert("op".into(), "create".into());
        op.insert("type".into(), b.block.block_type.clone().into());
        op.insert("id".into(), b.block.id.to_string().into());
        match placed {
            Some(m) => op.extend(m.clone()),
            None => {
                if let Some(p) = &b.block.parent {
                    op.insert("parent".into(), p.to_string().into());
                }
            }
        }
        let fields: Map<String, Value> = b
            .block
            .fields
            .iter()
            .filter(|(k, _)| !store_only_field(k))
            .map(|(k, v)| (k.clone(), field_json(v)))
            .collect();
        op.insert("fields".into(), Value::Object(fields));
        op.insert("body".into(), b.block.body.clone().into());
        let links: Map<String, Value> = shown_links(b)
            .into_iter()
            .filter(|(k, _)| !STORE_ONLY_LINK_KINDS.contains(k))
            .map(|(k, v)| (k.to_string(), Value::Array(shown_json(&v))))
            .collect();
        op.insert("links".into(), Value::Object(links));
        Value::Object(op)
    }

    /// The update of a block both trees hold, naming only what changed; `None` when nothing did.
    fn update(
        &self,
        old: &WcBlock,
        new: &WcBlock,
        placed: Option<&Map<String, Value>>,
    ) -> Option<Value> {
        let (b, c) = (&old.block, &new.block);
        let mut op = Map::new();
        if b.block_type != c.block_type {
            op.insert("type".into(), c.block_type.clone().into());
        }
        let fields: Map<String, Value> = c
            .fields
            .iter()
            .filter(|(k, v)| !store_only_field(k) && b.fields.get(*k) != Some(*v))
            .map(|(k, v)| (k.clone(), field_json(v)))
            .collect();
        if !fields.is_empty() {
            op.insert("fields".into(), Value::Object(fields));
        }
        let unset: Vec<Value> = b
            .fields
            .keys()
            .filter(|k| !store_only_field(k) && !c.fields.contains_key(*k))
            .map(|k| k.clone().into())
            .collect();
        if !unset.is_empty() {
            op.insert("unset".into(), Value::Array(unset));
        }
        if b.body != c.body {
            op.insert("body".into(), c.body.clone().into());
        }
        let links = self.links(old, new);
        if !links.is_empty() {
            op.insert("links".into(), Value::Object(links));
        }
        if let Some(m) = placed {
            op.insert("move_to".into(), Value::Object(m.clone()));
        }
        if op.is_empty() {
            return None;
        }
        op.insert("op".into(), "update".into());
        op.insert("id".into(), c.id.to_string().into());
        if let Some(bb) = self.st.base.blocks.get(&c.id) {
            op.insert("base".into(), bb.version.clone().into());
        }
        Some(Value::Object(op))
    }

    /// The per-kind link rule (FW-D14): a kind the file left as it was is not named; a changed
    /// kind, or one with a keyed target (the client cannot resolve keys), is named with the
    /// file's links and then the block's hidden links of that kind. A store-only kind never is.
    fn links(&self, old: &WcBlock, new: &WcBlock) -> Map<String, Value> {
        let (b, c) = (shown_links(old), shown_links(new));
        let hidden = self.st.base.blocks.get(&new.block.id).map(|bb| &bb.hidden);
        let kinds: BTreeSet<&str> = b.keys().chain(c.keys()).copied().collect();
        let mut out = Map::new();
        for kind in kinds {
            if STORE_ONLY_LINK_KINDS.contains(&kind) {
                continue;
            }
            let now = c.get(kind).map(Vec::as_slice).unwrap_or_default();
            let was = b.get(kind).map(Vec::as_slice).unwrap_or_default();
            if now == was && !now.iter().any(|l| l.keyed) {
                continue;
            }
            let mut list = shown_json(now);
            if let Some(h) = hidden.and_then(|h| h.get(kind)) {
                list.extend(h.iter().map(|l| link_json(&l.target, l.part.as_deref())));
            }
            out.insert(kind.to_string(), Value::Array(list));
        }
        out
    }

    /// The deletes (rule 5), deepest first: a doc asset the base holds and no file of the commit
    /// tree does, unless a working file that is not staged holds it (R-P11: a warning instead).
    /// Returns every base id the commit tree lacks.
    fn deletes(&self, plan: &mut CommitPlan) -> Vec<&'a BlockId> {
        let gone: Vec<&WcBlock> = self
            .base
            .pre_order(self.base_tree)
            .into_iter()
            .filter(|b| !self.commit.by_id.contains_key(&b.block.id))
            .collect();
        if gone.is_empty() {
            return vec![];
        }
        let working = self.unstaged_holders(plan);
        let mut dels: Vec<&WcBlock> = vec![];
        for b in &gone {
            let id = &b.block.id;
            if self.pack.class_of(&b.block.block_type) != TypeClass::DocAsset {
                continue;
            }
            if let Some((file, at)) = working.get(id) {
                plan.warnings.push(Diagnostic {
                    file: file.clone(),
                    line: at.0,
                    column: at.1,
                    code: W_WC_UNSTAGED_MOVE.into(),
                    message: format!("{id} was moved to {file}, which is not staged"),
                    fix: format!("rdy add {file}"),
                    severity: Severity::Warning,
                });
                continue;
            }
            dels.push(b);
        }
        dels.sort_by_key(|b| {
            (
                std::cmp::Reverse(self.base.depth(&b.block.id)),
                self.base.seq.get(&b.block.id).copied(),
            )
        });
        for b in dels {
            let id = &b.block.id;
            let mut op = json!({"op": "delete", "id": id.to_string()});
            if let Some(bb) = self.st.base.blocks.get(id) {
                op["base"] = bb.version.clone().into();
            }
            plan.touched.insert(id.clone());
            plan.ops.push(op);
        }
        gone.into_iter().map(|b| &b.block.id).collect()
    }

    /// The blocks the working files that are not staged (and not staged for deletion) hold, with
    /// where each is.
    fn unstaged_holders(&self, plan: &CommitPlan) -> BTreeMap<BlockId, (String, (usize, usize))> {
        let ignore = Ignore::load(&self.place.root).unwrap_or_else(|_| Ignore::empty());
        let tree = read_tree(&self.place.root, &ignore, self.pack);
        let mut out = BTreeMap::new();
        for b in tree.blocks {
            if !plan.committed.contains(&b.file) && !b.new {
                out.entry(b.block.id).or_insert((b.file, b.at));
            }
        }
        out
    }

    /// `rewrite` and `removed_files`.
    fn files(&self, plan: &mut CommitPlan, gone: &[&BlockId]) {
        let head_of = |files: &BTreeMap<String, crate::wc::layout::WcFile>, path: &str| {
            files.get(path).map(|f| f.head.clone())
        };
        let mut rewrite: BTreeSet<BlockId> = BTreeSet::new();
        for rel in self.staged {
            rewrite.extend(head_of(&self.commit_tree.files, rel));
        }
        for (id, c) in &self.commit.by_id {
            match self.base.by_id.get(id) {
                Some(b) if b.file != c.file => {
                    rewrite.extend(head_of(&self.base_tree.files, &b.file));
                    rewrite.extend(head_of(&self.commit_tree.files, &c.file));
                }
                None => rewrite.extend(head_of(&self.commit_tree.files, &c.file)),
                _ => {}
            }
        }
        for id in gone {
            if let Some(b) = self.base.by_id.get(id) {
                rewrite.extend(head_of(&self.base_tree.files, &b.file));
            }
        }
        rewrite.retain(|id| self.commit.by_id.contains_key(id));
        plan.rewrite = rewrite;
        let now: BTreeMap<&BlockId, &str> = self
            .commit_tree
            .files
            .iter()
            .map(|(path, f)| (&f.head, path.as_str()))
            .collect();
        plan.removed_files = self
            .st
            .base
            .files
            .iter()
            .filter(|(path, head)| now.get(head) != Some(&path.as_str()))
            .map(|(path, _)| path.clone())
            .collect();
    }
}
