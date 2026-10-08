//! The update planner and three-way merge (wcup1, spec §12.4, D-WY79P): the bound view as the
//! store holds it now (theirs) against the base and the working files (local). Each doc and
//! folder is matched by its head id. Places (parent and name) resolve before contents, so a file
//! follows its folder; contents merge block by block; what both sides changed differently is a
//! conflict, written beside the file as `.theirs.yaml`. After update the base is theirs.

use crate::error::*;
use crate::io::{BlockYaml, KeyedLink, from_yaml_struct};
use crate::now_ms;
use crate::tree_io::FOLDER_FILE;
use crate::wc::files::{
    base_from, behind_link, changed_since_staged, identity, inside, prune_dirs, remove_file,
    remove_tree, write_at,
};
use crate::wc::layout::{FOLDER_THEIRS, FileKind, THEIRS_SUFFIX, WcBlock, WcTree};
use crate::wc::project::Projection;
use crate::wc::state::{
    Base, Bound, CheckoutMark, ConflictEntry, ConflictKind, Place, WcState, stage_clear,
    stage_paths, stage_put,
};
use rdy_model::{Block, BlockId};
use rdy_schema::{Pack, STORE_ONLY_FIELDS, Severity};
use serde::{Deserialize, Serialize};
use serde_yaml_ng::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;

/// What `rdy update` writes. Every path is relative to the root and `/`-separated.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UpdatePlan {
    /// The working files to write, sorted by path.
    pub writes: Vec<(String, String)>,
    /// The working files to delete, sorted; never a path also written.
    pub removes: Vec<String>,
    /// The `.theirs.yaml` (and `_folder.theirs.yaml`) files to write beside conflicted files.
    pub theirs_files: Vec<(String, String)>,
    /// The store's text of each conflicted file, kept under the state's `conflicts/`.
    pub conflict_copies: Vec<(String, String)>,
    pub conflicts: Vec<ConflictEntry>,
    /// Staged copies to write (a staged file that merged cleanly).
    pub stage_writes: Vec<(String, String)>,
    /// Stage entries (a copy and a deletion) to drop.
    pub stage_drops: Vec<String>,
    /// Paths to stage for deletion: a doc staged for deletion that the store moved.
    pub stage_deleted: Vec<String>,
    /// The new base: theirs.
    pub base: Base,
    /// The new `base/files` copies: theirs' texts.
    pub base_files: Vec<(String, String)>,
}

/// A folder renamed here and in the store: the store's place, as `_folder.theirs.yaml` holds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderTheirs {
    pub name: String,
    /// The store's parent directory, `""` at the top.
    pub parent: String,
}

/// The text of the `.theirs.yaml` of a doc or folder the store no longer has.
const DELETED_NOTE: &str = "# rdy: the store deleted it\n";

/// The file a conflict entry's store side is written to, relative to the root: `<stem>.theirs.yaml`
/// beside a doc file (or `_folder.yaml`), `<dir>/_folder.theirs.yaml` for a folder rename.
pub fn theirs_path(c: &ConflictEntry) -> String {
    match c.kind {
        ConflictKind::FolderRename => join(&c.path, FOLDER_THEIRS),
        _ => theirs_beside(&c.path),
    }
}

fn theirs_beside(path: &str) -> String {
    format!(
        "{}{THEIRS_SUFFIX}",
        path.strip_suffix(".yaml").unwrap_or(path)
    )
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// `(the part before the last '/', the last part)`; the first is `""` when there is no '/'.
fn split_last(s: &str) -> (&str, &str) {
    match s.rfind('/') {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => ("", s),
    }
}

fn kind_of(path: &str) -> FileKind {
    if path == FOLDER_FILE || path.ends_with(&format!("/{FOLDER_FILE}")) {
        FileKind::Folder
    } else {
        FileKind::Doc
    }
}

/// The directory a folder file names, or the path of a doc without `.yaml`.
fn stem_of(path: &str, kind: FileKind) -> &str {
    match kind {
        FileKind::Folder => path
            .strip_suffix(FOLDER_FILE)
            .map_or(path, |d| d.trim_end_matches('/')),
        FileKind::Doc => path.strip_suffix(".yaml").unwrap_or(path),
    }
}

/// A doc's or folder's place: its parent folder (`None` at the top) and its name.
type Placed = (Option<BlockId>, String);

struct SideFile {
    path: String,
    text: String,
    kind: FileKind,
}

/// One side of the merge: each head's file, and each path's head.
#[derive(Default)]
struct Side {
    by_head: BTreeMap<BlockId, SideFile>,
    by_path: BTreeMap<String, BlockId>,
}

impl Side {
    /// Adds a file; a second file with a head already met is left out.
    fn add(&mut self, head: BlockId, path: String, text: String, kind: FileKind) {
        if self.by_head.contains_key(&head) {
            return;
        }
        self.by_path.insert(path.clone(), head.clone());
        self.by_head.insert(head, SideFile { path, text, kind });
    }

    fn place(&self, head: &BlockId) -> Option<Placed> {
        let f = self.by_head.get(head)?;
        let (pdir, name) = split_last(stem_of(&f.path, f.kind));
        let parent = if pdir.is_empty() {
            None
        } else {
            self.by_path.get(&join(pdir, FOLDER_FILE)).cloned()
        };
        Some((parent, name.to_string()))
    }

    fn text(&self, head: &BlockId) -> Option<&str> {
        self.by_head.get(head).map(|f| f.text.as_str())
    }
}

/// Where every doc and folder ends up: the resolved places, and each folder's directory.
struct Places {
    placed: BTreeMap<BlockId, Placed>,
    dirs: BTreeMap<BlockId, String>,
}

impl Places {
    /// The directory of folder `id`, from the resolved places of it and its parents.
    fn dir(&mut self, id: &BlockId, stack: &mut Vec<BlockId>) -> Result<String, RdyError> {
        if let Some(d) = self.dirs.get(id) {
            return Ok(d.clone());
        }
        if stack.contains(id) {
            return Err(RdyError::new(
                E_WC_CONFLICT,
                format!(
                    "folder {id} would end up inside itself: folders were moved here and in the store into each other"
                ),
                "move the folders back here, then run rdy update again",
            ));
        }
        let Some((parent, name)) = self.placed.get(id).cloned() else {
            return Err(RdyError::new(
                E_WC_CONFLICT,
                format!("folder {id} has no place here or in the store"),
                "run rdy checkout --force (it discards uncommitted edits)",
            ));
        };
        stack.push(id.clone());
        let pdir = match &parent {
            None => String::new(),
            Some(p) => self.dir(p, stack)?,
        };
        stack.pop();
        let d = join(&pdir, &name);
        self.dirs.insert(id.clone(), d.clone());
        Ok(d)
    }

    /// The path of the file of `head`, at its resolved place.
    fn path(&mut self, head: &BlockId, kind: FileKind) -> Result<String, RdyError> {
        match kind {
            FileKind::Folder => Ok(join(&self.dir(head, &mut vec![])?, FOLDER_FILE)),
            FileKind::Doc => {
                let (parent, name) = self.placed.get(head).cloned().unwrap_or_default();
                let dir = match &parent {
                    None => String::new(),
                    Some(p) => self.dir(p, &mut vec![])?,
                };
                Ok(join(&dir, &format!("{name}.yaml")))
            }
        }
    }
}

/// An entry as the merge compares it.
#[derive(Clone, PartialEq)]
struct Content {
    block: Block,
    keyed: Vec<(String, usize, String, Option<String>)>,
}

/// One entry of a file: its id (`None` for a new local entry), its YAML as written, its parent
/// and what the merge compares.
#[derive(Clone)]
struct Entry {
    id: Option<BlockId>,
    raw: Value,
    parent: Option<BlockId>,
    content: Option<Content>,
}

/// The parsed block, less what is not content: `order` (position decides), `parent` (compared as
/// position), pins, rdy's own fields, and a head's name (its file or directory name).
fn content(block: &Block, keyed: &[KeyedLink], head: bool) -> Content {
    let mut b = block.clone();
    b.order.clear();
    b.parent = None;
    b.pins.clear();
    if head {
        b.fields.remove("name");
    }
    for f in STORE_ONLY_FIELDS {
        b.fields.remove(*f);
    }
    let mut keyed: Vec<_> = keyed
        .iter()
        .map(|k| {
            (
                k.kind().to_string(),
                k.at(),
                k.text().to_string(),
                k.part().map(String::from),
            )
        })
        .collect();
    keyed.sort();
    Content { block: b, keyed }
}

fn raw_entries(text: &str, kind: FileKind) -> Result<Vec<Value>, String> {
    let v: Value = serde_yaml_ng::from_str(text).map_err(|e| e.to_string())?;
    match (kind, v) {
        (FileKind::Folder, v) => Ok(vec![v]),
        (FileKind::Doc, Value::Sequence(s)) => Ok(s),
        (FileKind::Doc, _) => Err("a doc file is a list of entries".into()),
    }
}

/// The entries of a text rdy wrote: a base copy or the store's.
fn written_entries(path: &str, text: &str, kind: FileKind) -> Result<Vec<Entry>, RdyError> {
    let bad = |e: String| {
        RdyError::new(
            E_IO,
            format!("cannot read rdy's copy of {path}: {e}"),
            "run rdy checkout --force (it discards uncommitted edits)",
        )
    };
    raw_entries(text, kind)
        .map_err(bad)?
        .into_iter()
        .enumerate()
        .map(|(i, raw)| {
            let y: BlockYaml =
                serde_yaml_ng::from_value(raw.clone()).map_err(|e| bad(e.to_string()))?;
            let (block, keyed) = from_yaml_struct(y).map_err(|e| bad(e.message))?;
            Ok(Entry {
                id: Some(block.id.clone()),
                parent: block.parent.clone(),
                content: Some(content(&block, &keyed, i == 0)),
                raw,
            })
        })
        .collect()
}

/// The entries of a working file, with the blocks the reader made of them.
fn local_entries(
    path: &str,
    text: &str,
    kind: FileKind,
    blocks: &[&WcBlock],
) -> Result<Vec<Entry>, RdyError> {
    let raws = raw_entries(text, kind).map_err(|e| {
        RdyError::new(
            E_WC_INVALID,
            format!("{path} does not read: {e}"),
            format!("rdy check {path}"),
        )
    })?;
    let by_seq: BTreeMap<usize, &WcBlock> = blocks.iter().map(|b| (b.seq, *b)).collect();
    Ok(raws
        .into_iter()
        .enumerate()
        .map(|(i, raw)| match by_seq.get(&i) {
            Some(b) => Entry {
                id: (!b.new).then(|| b.block.id.clone()),
                parent: b.block.parent.clone(),
                content: Some(content(&b.block, &b.keyed, i == 0)),
                raw,
            },
            None => Entry {
                id: None,
                parent: None,
                content: None,
                raw,
            },
        })
        .collect())
}

/// A block's position in a file: its parent, and the nearest earlier sibling every side holds.
type Pos = (Option<BlockId>, Option<BlockId>);

fn positions(list: &[Entry], head: &BlockId, common: &BTreeSet<BlockId>) -> BTreeMap<BlockId, Pos> {
    let mut last: BTreeMap<Option<BlockId>, BlockId> = BTreeMap::new();
    let mut out = BTreeMap::new();
    for e in list {
        let Some(id) = &e.id else { continue };
        if id == head {
            continue;
        }
        out.insert(id.clone(), (e.parent.clone(), last.get(&e.parent).cloned()));
        if common.contains(id) {
            last.insert(e.parent.clone(), id.clone());
        }
    }
    out
}

fn set_key(raw: &mut Value, key: &str, v: Option<Value>) {
    if let Value::Mapping(m) = raw {
        match v {
            Some(v) => {
                m.insert(key.into(), v);
            }
            None => {
                m.remove(key);
            }
        }
    }
}

/// The index just past the entry at `a` and the entries after it that sit inside it.
fn end_of(res: &[Entry], a: usize) -> usize {
    let Some(top) = res[a].id.clone() else {
        return a + 1;
    };
    let parents: BTreeMap<&BlockId, Option<&BlockId>> = res
        .iter()
        .filter_map(|e| Some((e.id.as_ref()?, e.parent.as_ref())))
        .collect();
    let mut j = a + 1;
    while j < res.len() {
        let mut cur = res[j].parent.as_ref();
        let mut inside = false;
        for _ in 0..=res.len() {
            match cur {
                Some(c) if *c == top => {
                    inside = true;
                    break;
                }
                Some(c) => cur = parents.get(c).copied().flatten(),
                None => break,
            }
        }
        if !inside {
            break;
        }
        j += 1;
    }
    j
}

fn position_of(res: &[Entry], id: &BlockId) -> Option<usize> {
    res.iter().position(|e| e.id.as_ref() == Some(id))
}

/// Rule 3: merges the entries of one file by block id. A block changed on both sides to
/// different entries (its content, or its position), or deleted on one side and changed on the
/// other, is a conflict: `Err` lists them. Else local's entries, with each block changed only in
/// theirs replaced, each block new in theirs inserted after its previous sibling in theirs (or
/// first under its parent), each block theirs moved moved likewise, and each block theirs
/// deleted that local did not change dropped.
fn merge_entries(
    head: &BlockId,
    base: &[Entry],
    local: &[Entry],
    theirs: &[Entry],
) -> Result<Vec<Value>, Vec<BlockId>> {
    let index = |l: &[Entry]| -> BTreeMap<BlockId, usize> {
        l.iter()
            .enumerate()
            .filter_map(|(i, e)| Some((e.id.clone()?, i)))
            .collect()
    };
    let (bi, li, ti) = (index(base), index(local), index(theirs));
    let common: BTreeSet<BlockId> = bi
        .keys()
        .filter(|id| li.contains_key(*id) && ti.contains_key(*id))
        .cloned()
        .collect();
    let (bpos, lpos, tpos) = (
        positions(base, head, &common),
        positions(local, head, &common),
        positions(theirs, head, &common),
    );
    let mut conflicts: BTreeSet<BlockId> = BTreeSet::new();
    let mut drop: BTreeSet<BlockId> = BTreeSet::new();
    let mut take_content: BTreeSet<BlockId> = BTreeSet::new();
    let mut take_pos: BTreeSet<BlockId> = BTreeSet::new();
    let mut insert: BTreeSet<BlockId> = BTreeSet::new();
    let ids: BTreeSet<&BlockId> = bi.keys().chain(li.keys()).chain(ti.keys()).collect();
    for id in ids {
        let b = bi.get(id).map(|&i| &base[i]);
        let l = li.get(id).map(|&i| &local[i]);
        let t = ti.get(id).map(|&i| &theirs[i]);
        let (bp, lp, tp) = (bpos.get(id), lpos.get(id), tpos.get(id));
        match (b, l, t) {
            (None, Some(l), Some(t)) => {
                if l.content != t.content || lp != tp {
                    conflicts.insert(id.clone());
                }
            }
            (None, None, Some(_)) => {
                insert.insert(id.clone());
            }
            (Some(b), None, Some(t)) => {
                if t.content != b.content || tp != bp {
                    conflicts.insert(id.clone());
                }
            }
            (Some(b), Some(l), None) => {
                if l.content != b.content || lp != bp {
                    conflicts.insert(id.clone());
                } else {
                    drop.insert(id.clone());
                }
            }
            (Some(b), Some(l), Some(t)) => {
                let (lc, tc) = (l.content != b.content, t.content != b.content);
                let (lm, tm) = (lp != bp, tp != bp);
                if (lc && tc && l.content != t.content) || (lm && tm && lp != tp) {
                    conflicts.insert(id.clone());
                    continue;
                }
                if tc && !lc {
                    take_content.insert(id.clone());
                }
                if tm && !lm {
                    take_pos.insert(id.clone());
                }
            }
            _ => {}
        }
    }
    if !conflicts.is_empty() {
        return Err(conflicts.into_iter().collect());
    }
    let mut res: Vec<Entry> = local
        .iter()
        .filter(|e| e.id.as_ref().is_none_or(|id| !drop.contains(id)))
        .cloned()
        .collect();
    for e in &mut res {
        let Some(id) = e.id.clone() else { continue };
        let Some(t) = ti.get(&id).map(|&i| &theirs[i]) else {
            continue;
        };
        if take_content.contains(&id) {
            let mut raw = t.raw.clone();
            if id != *head && !take_pos.contains(&id) {
                set_key(&mut raw, "parent", e.raw.get("parent").cloned());
            }
            e.raw = raw;
            e.content = t.content.clone();
        }
        if take_pos.contains(&id) {
            set_key(&mut e.raw, "parent", t.raw.get("parent").cloned());
            e.parent = t.parent.clone();
        }
    }
    let mut lost: Vec<BlockId> = vec![];
    for (ti_at, t) in theirs.iter().enumerate() {
        let Some(id) = &t.id else { continue };
        let moved = take_pos.contains(id);
        if !moved && !insert.contains(id) {
            continue;
        }
        let group: Vec<Entry> = if moved {
            let Some(i) = position_of(&res, id) else {
                continue;
            };
            let j = end_of(&res, i);
            res.drain(i..j).collect()
        } else {
            vec![t.clone()]
        };
        let anchor = theirs[..ti_at]
            .iter()
            .rev()
            .filter(|o| o.parent == t.parent)
            .filter_map(|o| o.id.as_ref())
            .find_map(|sid| position_of(&res, sid));
        let at = match anchor {
            Some(a) => end_of(&res, a),
            None => match t.parent.as_ref().and_then(|p| position_of(&res, p)) {
                Some(p) => p + 1,
                None => {
                    // its parent is gone here: deleted on one side, given a child on the other
                    lost.push(id.clone());
                    continue;
                }
            },
        };
        res.splice(at..at, group);
    }
    if !lost.is_empty() {
        lost.sort();
        return Err(lost);
    }
    if res.first().is_some_and(|e| e.id.as_ref() != Some(head)) {
        // the head is gone while entries stay: deleted on one side, added to on the other
        return Err(vec![head.clone()]);
    }
    Ok(res.into_iter().map(|e| e.raw).collect())
}

/// The canonical text of a merged file; `None` when no entry is left.
fn render(kind: FileKind, raws: Vec<Value>) -> Option<String> {
    if raws.is_empty() {
        return None;
    }
    let text = match kind {
        FileKind::Folder => serde_yaml_ng::to_string(&raws[0]),
        FileKind::Doc => serde_yaml_ng::to_string(&Value::Sequence(raws)),
    };
    Some(text.expect("yaml of parsed entries"))
}

/// What becomes of one doc's or folder's content.
enum Outcome {
    /// Its text, or `None` when the file goes.
    Text(Option<String>),
    /// The blocks changed on both sides.
    Conflict(Vec<BlockId>),
}

/// The store's version of every block of theirs' file at `path`.
fn versions(theirs: &Projection, path: &str) -> BTreeMap<BlockId, String> {
    theirs
        .blocks
        .iter()
        .filter(|(_, b)| b.file == path)
        .filter_map(|(id, b)| Some((BlockId::parse(id).ok()?, b.version.clone())))
        .collect()
}

/// `text` as YAML comment lines.
fn commented(text: &str) -> String {
    text.lines().map(|l| format!("# {l}\n")).collect()
}

/// Plans `rdy update` (wcup1, exact): refuses with `E_WC_DIRTY` when a staged file changed after
/// `rdy add`, with `E_WC_INVALID` when a tracked file does not read, and with `E_WC_OCCUPIED`
/// when a path to write holds a file rdy does not track; resolves every doc's and folder's place
/// (a folder or doc moved on both sides to different places keeps the local one and records a
/// rename conflict), then merges each one's content three ways (rules 1 to 3). Reads only.
pub fn plan_update(
    place: &Place,
    st: &WcState,
    tree: &WcTree,
    theirs: &Projection,
    _pack: &Pack,
) -> Result<UpdatePlan, RdyError> {
    let root = &place.root;
    let staged: BTreeSet<String> = stage_paths(place).into_iter().collect();
    let committed: BTreeSet<String> = staged.iter().chain(&st.stage.deleted).cloned().collect();
    let dirty = changed_since_staged(place, st, &committed);
    if let Some(first) = dirty.first() {
        return Err(RdyError::new(
            E_WC_DIRTY,
            format!("{} changed after rdy add", dirty.join(", ")),
            format!("stage it again (rdy add {first}), or restore the staged copy"),
        ));
    }

    let mut base = Side::default();
    for (path, head) in &st.base.files {
        let text = fs::read_to_string(place.base_file(path)).map_err(|e| {
            RdyError::new(
                E_IO,
                format!("cannot read the base copy of {path}: {e}"),
                "run rdy checkout --force (it discards uncommitted edits)",
            )
        })?;
        base.add(head.clone(), path.clone(), text, kind_of(path));
    }
    let mut their = Side::default();
    for f in &theirs.files {
        if let Ok(h) = BlockId::parse(&f.id) {
            their.add(h, f.path.clone(), f.text.clone(), f.kind);
        }
    }

    // a tracked file the reader cannot read would merge as deleted
    let known = |h: &BlockId| base.by_head.contains_key(h) || their.by_head.contains_key(h);
    let mut broken: BTreeSet<String> = tree
        .diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .filter(|d| tree.files.get(&d.file).is_some_and(|f| known(&f.head)))
        .map(|d| d.file.clone())
        .collect();
    for p in st.base.files.keys() {
        let present = inside(p).is_ok_and(|q| !behind_link(root, q))
            && fs::symlink_metadata(root.join(p)).is_ok();
        if present && !tree.files.contains_key(p) {
            broken.insert(p.clone());
        }
    }
    if !broken.is_empty() {
        let list: Vec<String> = broken.into_iter().collect();
        return Err(RdyError::new(
            E_WC_INVALID,
            format!(
                "update cannot read {} tracked file(s): {}",
                list.len(),
                list.join(", ")
            ),
            format!(
                "fix them (rdy check {}) or restore them, then update again",
                list[0]
            ),
        ));
    }

    let mut local = Side::default();
    for (path, f) in &tree.files {
        let text = fs::read_to_string(root.join(path)).map_err(|e| {
            RdyError::new(
                E_IO,
                format!("cannot read {path}: {e}"),
                "check the file, then update again",
            )
        })?;
        local.add(f.head.clone(), path.clone(), text, f.kind);
    }
    let mut blocks_of: BTreeMap<&str, Vec<&WcBlock>> = BTreeMap::new();
    for b in &tree.blocks {
        blocks_of.entry(b.file.as_str()).or_default().push(b);
    }

    // rule 0: places first
    let heads: BTreeSet<BlockId> = base
        .by_head
        .keys()
        .chain(local.by_head.keys())
        .chain(their.by_head.keys())
        .cloned()
        .collect();
    let mut places = Places {
        placed: BTreeMap::new(),
        dirs: BTreeMap::new(),
    };
    let mut renames: BTreeMap<BlockId, (Placed, Placed)> = BTreeMap::new();
    for h in &heads {
        let resolved = match (base.place(h), local.place(h), their.place(h)) {
            (Some(b), Some(l), Some(t)) => {
                if l == b {
                    t
                } else if t == b || t == l {
                    l
                } else {
                    renames.insert(h.clone(), (l.clone(), t));
                    l
                }
            }
            (_, Some(l), _) => l,
            (_, None, Some(t)) => t,
            (_, None, None) => continue,
        };
        places.placed.insert(h.clone(), resolved);
    }

    let mut out = UpdatePlan::default();
    let mut write_heads: BTreeMap<String, BlockId> = BTreeMap::new();
    let mut writes: BTreeMap<String, String> = BTreeMap::new();
    let mut removes: BTreeSet<String> = BTreeSet::new();
    let mut theirs_files: BTreeMap<String, String> = BTreeMap::new();
    let mut copies: BTreeMap<String, String> = BTreeMap::new();
    let mut drops: BTreeSet<String> = BTreeSet::new();
    let mut conflicted: BTreeSet<BlockId> = BTreeSet::new();
    let mut stays_deleted: BTreeSet<BlockId> = BTreeSet::new();
    for h in &heads {
        let (b, l, t) = (
            base.by_head.get(h),
            local.by_head.get(h),
            their.by_head.get(h),
        );
        let Some(kind) = l.or(t).map(|f| f.kind) else {
            continue; // gone on both sides
        };
        let target = places.path(h, kind)?;
        let (bt, lt, tt) = (base.text(h), local.text(h), their.text(h));
        // only here: kept, following its folder; rule 2: theirs equals the base; or both agree
        let keep_local =
            (b.is_none() && t.is_none()) || (b.is_some() && tt.is_some() && tt == bt) || lt == tt;
        // new in the store; rule 1: local equals the base
        let take_theirs = (b.is_none() && l.is_none()) || (b.is_some() && lt.is_some() && lt == bt);
        let outcome = if b.is_none() && t.is_none() {
            Outcome::Text(lt.map(String::from))
        } else if take_theirs {
            Outcome::Text(tt.map(String::from))
        } else if keep_local {
            Outcome::Text(lt.map(String::from))
        } else {
            let entries = |f: Option<&SideFile>| -> Result<Vec<Entry>, RdyError> {
                f.map_or(Ok(vec![]), |f| written_entries(&f.path, &f.text, f.kind))
            };
            let le = match l {
                Some(f) => local_entries(
                    &f.path,
                    &f.text,
                    f.kind,
                    blocks_of.get(f.path.as_str()).map_or(&[], Vec::as_slice),
                )?,
                None => vec![],
            };
            match merge_entries(h, &entries(b)?, &le, &entries(t)?) {
                Ok(raws) => Outcome::Text(render(kind, raws)),
                Err(ids) => Outcome::Conflict(ids),
            }
        };
        let lp = l.map(|f| f.path.as_str());
        let is_staged = |p: Option<&str>| p.is_some_and(|p| staged.contains(p));
        match outcome {
            Outcome::Text(None) => {
                if let Some(lp) = lp {
                    removes.insert(lp.to_string());
                }
                if is_staged(lp) {
                    drops.insert(lp.unwrap_or_default().to_string());
                }
                if t.is_some() {
                    stays_deleted.insert(h.clone());
                }
            }
            Outcome::Text(Some(text)) => {
                if lp != Some(target.as_str()) || lt != Some(text.as_str()) {
                    writes.insert(target.clone(), text.clone());
                    write_heads.insert(target.clone(), h.clone());
                    if let Some(lp) = lp
                        && lp != target
                    {
                        removes.insert(lp.to_string());
                    }
                }
                // rule 7: a staged file that merged cleanly is staged with the merged text
                if let Some(lp) = lp
                    && staged.contains(lp)
                {
                    let as_base = t.is_some_and(|t| t.path == target && t.text == text);
                    if lp != target || as_base {
                        drops.insert(lp.to_string());
                    }
                    if !as_base {
                        out.stage_writes.push((target.clone(), text));
                    }
                }
            }
            Outcome::Conflict(ids) => {
                conflicted.insert(h.clone());
                if let Some(l) = l
                    && l.path != target
                {
                    writes.insert(target.clone(), l.text.clone());
                    write_heads.insert(target.clone(), h.clone());
                    removes.insert(l.path.clone());
                }
                theirs_files.insert(
                    theirs_beside(&target),
                    tt.map_or_else(|| DELETED_NOTE.to_string(), String::from),
                );
                copies.insert(target.clone(), tt.unwrap_or_default().to_string());
                if is_staged(lp) {
                    drops.insert(lp.unwrap_or_default().to_string());
                }
                out.conflicts.push(ConflictEntry {
                    path: target.clone(),
                    kind: ConflictKind::Blocks,
                    blocks: ids,
                    theirs_versions: t.map(|t| versions(theirs, &t.path)).unwrap_or_default(),
                    local_name: None,
                    store_name: None,
                    store_parent: None,
                });
            }
        }
        // rule 4: moved on both sides to different places
        if let (Some((lpl, tpl)), Some(t)) = (renames.get(h), t) {
            conflicted.insert(h.clone());
            if is_staged(lp) {
                drops.insert(lp.unwrap_or_default().to_string());
            }
            let store_parent = split_last(stem_of(&t.path, kind)).0.to_string();
            let (path, kind_c) = match kind {
                FileKind::Folder => {
                    let dir = places.dir(h, &mut vec![])?;
                    let file = join(&dir, FOLDER_THEIRS);
                    let mut text = serde_yaml_ng::to_string(&FolderTheirs {
                        name: tpl.1.clone(),
                        parent: store_parent.clone(),
                    })
                    .expect("folder theirs yaml");
                    if let Some(prev) = theirs_files.get(&file) {
                        // the folder's content conflicts too: the store's _folder.yaml follows
                        text.push_str("# rdy: the store's _folder.yaml:\n");
                        text.push_str(&commented(prev));
                    }
                    theirs_files.insert(file, text);
                    (dir, ConflictKind::FolderRename)
                }
                FileKind::Doc => {
                    theirs_files.insert(
                        theirs_beside(&target),
                        format!("# rdy: in the store this doc is at {}\n{}", t.path, t.text),
                    );
                    copies
                        .entry(target.clone())
                        .or_insert_with(|| t.text.clone());
                    (target.clone(), ConflictKind::DocRename)
                }
            };
            out.conflicts.push(ConflictEntry {
                path,
                kind: kind_c,
                blocks: vec![],
                theirs_versions: BTreeMap::new(),
                local_name: Some(lpl.1.clone()),
                store_name: Some(tpl.1.clone()),
                store_parent: Some(store_parent),
            });
        }
    }
    for p in writes.keys() {
        removes.remove(p);
    }

    // rule 5: nothing rdy does not track is overwritten
    let leaving: BTreeSet<(u64, u64)> = removes.iter().filter_map(|p| identity(root, p)).collect();
    let mut taken: Vec<String> = vec![];
    for (path, h) in &write_heads {
        let p = inside(path)?;
        if behind_link(root, p) {
            taken.push(path.clone());
            continue;
        }
        match fs::symlink_metadata(root.join(p)) {
            Err(_) => {}
            Ok(m) if !m.file_type().is_file() => taken.push(path.clone()),
            Ok(_) => {
                let here = identity(root, path);
                let own = local.by_head.get(h).and_then(|f| identity(root, &f.path));
                let fine = here.is_some_and(|i| Some(i) == own || leaving.contains(&i));
                if !fine {
                    taken.push(path.clone());
                }
            }
        }
    }
    for path in theirs_files.keys() {
        let p = inside(path)?;
        if behind_link(root, p)
            || fs::symlink_metadata(root.join(p)).is_ok_and(|m| !m.file_type().is_file())
        {
            taken.push(path.clone());
        }
    }
    if !taken.is_empty() {
        return Err(RdyError::new(
            E_WC_OCCUPIED,
            format!(
                "update would overwrite {} untracked file(s): {}",
                taken.len(),
                taken
                    .iter()
                    .take(10)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "move them, then update again",
        ));
    }

    // rule 7 for staged deletions: they follow the store's path, or go with the base path
    let new_base: BTreeSet<&str> = theirs.files.iter().map(|f| f.path.as_str()).collect();
    for p in &st.stage.deleted {
        match st.base.files.get(p) {
            Some(h) if conflicted.contains(h) => {
                drops.insert(p.clone());
            }
            Some(h) if stays_deleted.contains(h) => {
                if let Some(t) = their.by_head.get(h)
                    && t.path != *p
                {
                    drops.insert(p.clone());
                    out.stage_deleted.push(t.path.clone());
                }
            }
            _ if !new_base.contains(p.as_str()) => {
                drops.insert(p.clone());
            }
            _ => {}
        }
    }

    out.writes = writes.into_iter().collect();
    out.removes = removes.into_iter().collect();
    out.theirs_files = theirs_files.into_iter().collect();
    out.conflict_copies = copies.into_iter().collect();
    out.stage_drops = drops.into_iter().collect();
    out.stage_writes.sort();
    out.stage_deleted.sort();
    // rule 6: the base is theirs
    out.base = base_from(theirs);
    out.base_files = theirs
        .files
        .iter()
        .map(|f| (f.path.clone(), f.text.clone()))
        .collect();
    Ok(out)
}

/// Applies an update plan (wcup1, exact order): marks the working copy as being rewritten (the
/// checkout marker, so an interrupted update is finished by `rdy checkout --force`); writes the
/// files; deletes the files to remove (never one just written, never through a link) and the
/// directories that leaves empty; writes the `.theirs.yaml` files and the state's `conflicts/`
/// copies; replaces the base and its file copies with theirs; updates the stage; replaces the
/// conflicts on the same paths; binds to theirs' head (rule 8); saves, which clears the marker.
pub fn apply_update(
    place: &Place,
    st: &mut WcState,
    plan: &UpdatePlan,
    theirs: &Projection,
) -> Result<(), RdyError> {
    let root = &place.root;
    st.mark_checkout(
        place,
        CheckoutMark {
            view: theirs.view.clone(),
            head: theirs.head.clone(),
            at_ms: now_ms(),
        },
    )?;
    // an existing directory takes the spelling its new path asks for (a case-only rename)
    let mut cases = BTreeSet::new();
    for (p, text) in &plan.writes {
        write_at(root, p, text, Some(&mut cases))?;
    }
    let written: BTreeSet<(u64, u64)> = plan
        .writes
        .iter()
        .filter_map(|(p, _)| identity(root, p))
        .collect();
    for p in &plan.removes {
        let Ok(rel) = inside(p) else { continue };
        if behind_link(root, rel) || identity(root, p).is_some_and(|i| written.contains(&i)) {
            continue;
        }
        let path = root.join(rel);
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
            remove_file(&path)?;
        }
        prune_dirs(root, p);
    }
    for (p, text) in &plan.theirs_files {
        write_at(root, p, text, None)?;
    }
    let mkdir = |d: &std::path::Path| {
        fs::create_dir_all(d).map_err(|e| {
            RdyError::new(
                E_IO,
                format!("cannot create {}: {e}", d.display()),
                "check the working copy's state directory and its permissions",
            )
        })
    };
    let conflicts_dir = place.state.join("conflicts");
    if !plan.conflict_copies.is_empty() {
        mkdir(&conflicts_dir)?;
    }
    for (p, text) in &plan.conflict_copies {
        write_at(&conflicts_dir, p, text, None)?;
    }
    let base_dir = place.state.join("base/files");
    remove_tree(&base_dir)?;
    mkdir(&base_dir)?;
    for (p, text) in &plan.base_files {
        write_at(&base_dir, p, text, None)?;
    }
    st.base = plan.base.clone();
    for p in &plan.stage_drops {
        stage_clear(place, p)?;
        st.stage.deleted.remove(p);
    }
    for (p, text) in &plan.stage_writes {
        stage_put(place, p, text.as_bytes())?;
    }
    st.stage.deleted.extend(plan.stage_deleted.iter().cloned());
    let paths: BTreeSet<&str> = plan.conflicts.iter().map(|c| c.path.as_str()).collect();
    st.conflicts.retain(|c| !paths.contains(c.path.as_str()));
    st.conflicts.extend(plan.conflicts.iter().cloned());
    st.bound = Some(Bound {
        view: theirs.view.clone(),
        head: theirs.head.clone(),
        at_ms: now_ms(),
    });
    st.checkout = None;
    st.save(place)
}
