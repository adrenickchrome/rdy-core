//! The working copy's local state: what is checked out, what is staged, what conflicts, and the
//! lock. It lives under `.rdy/wc/` (or a worktree's state directory) and is never a store object.

use crate::error::*;
use rdy_model::{BlockId, Hash};
use rdy_store::fsutil::write_durable;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The state directory of the main working copy, relative to the project.
pub const WC_DIR: &str = ".rdy/wc";
/// The directory of every worktree's state, relative to the project.
pub const WORKTREES_DIR: &str = ".rdy/worktrees";
/// The file in a worktree's root that names its project and its name.
pub const POINTER_FILE: &str = ".rdy-worktree";
/// How long a working-copy command waits for the lock.
pub const LOCK_WAIT: Duration = Duration::from_secs(10);
/// How often a waiting command tries the lock again.
pub const LOCK_POLL: Duration = Duration::from_millis(20);
/// The marker of a checkout in flight, in the state directory.
pub const CHECKOUT_FILE: &str = "checkout.yaml";
/// The record of an update in flight (its plan and the files' earlier text), in the state
/// directory: `rdy update` finishes it.
pub const UPDATE_FILE: &str = "update.json";
/// The most briefing records kept.
pub const MAX_BRIEFS: usize = 64;

/// Where a working copy lives: its files, its state and its project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// The directory that holds the files.
    pub root: PathBuf,
    /// The directory that holds the local state.
    pub state: PathBuf,
    /// The project (the directory with `.rdy`).
    pub project: PathBuf,
}

#[derive(Deserialize)]
struct Pointer {
    project: PathBuf,
    name: String,
}

impl Place {
    /// The project's own working copy.
    pub fn main(project: &Path) -> Place {
        Place {
            root: project.to_path_buf(),
            state: project.join(WC_DIR),
            project: project.to_path_buf(),
        }
    }

    /// Walks `start` and its ancestors. The first directory with a regular file `.rdy-worktree`
    /// gives a worktree place; else the first with a `.rdy` directory gives the main place. No
    /// path is canonicalized.
    pub fn locate(start: &Path) -> Option<Place> {
        for dir in start.ancestors() {
            if let Ok(m) = fs::symlink_metadata(dir.join(POINTER_FILE))
                && m.is_file()
                && let Some(p) = Place::from_pointer(dir)
            {
                return Some(p);
            }
            if fs::metadata(dir.join(".rdy")).is_ok_and(|m| m.is_dir()) {
                return Some(Place::main(dir));
            }
        }
        None
    }

    /// The worktree place `dir`'s pointer file names (`dir` holds the pointer itself; nothing is
    /// walked); `None` when the file is missing, does not parse or names an unsafe name.
    pub fn from_pointer(dir: &Path) -> Option<Place> {
        let text = fs::read_to_string(dir.join(POINTER_FILE)).ok()?;
        let ptr: Pointer = serde_yaml_ng::from_str(&text).ok()?;
        let mut parts = Path::new(&ptr.name).components();
        let plain = matches!(parts.next(), Some(Component::Normal(_))) && parts.next().is_none();
        if !plain {
            return None;
        }
        Some(Place {
            root: dir.to_path_buf(),
            state: ptr.project.join(WORKTREES_DIR).join(&ptr.name),
            project: ptr.project,
        })
    }

    /// The base copy of the file at `rel`.
    pub fn base_file(&self, rel: &str) -> PathBuf {
        self.state.join("base/files").join(rel)
    }

    /// The staged copy of the file at `rel`.
    pub fn stage_file(&self, rel: &str) -> PathBuf {
        self.state.join("stage/files").join(rel)
    }

    /// The store's side of the conflicted file at `rel`.
    pub fn conflict_file(&self, rel: &str) -> PathBuf {
        self.state.join("conflicts").join(rel)
    }
}

/// The project the pointer file directly in `dir` names: for a directory that holds a regular
/// file `.rdy-worktree` that parses, else `None`.
pub fn pointer_project(dir: &Path) -> Option<PathBuf> {
    let m = fs::symlink_metadata(dir.join(POINTER_FILE)).ok()?;
    if !m.is_file() {
        return None;
    }
    Place::from_pointer(dir).map(|p| p.project)
}

/// One human worktree, as `rdy worktree ls` shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub name: String,
    /// The worktree's directory (`dir.txt` in its state directory); empty when that is missing.
    pub dir: String,
    /// The view it is bound to; `None` while unbound.
    pub view: Option<String>,
    pub head: Option<String>,
}

/// The file in a worktree's state directory that holds the worktree's directory.
pub const WORKTREE_DIR_FILE: &str = "dir.txt";

/// Every state directory under the project's `.rdy/worktrees/`, sorted by name.
pub fn worktrees(project: &Path) -> Vec<WorktreeInfo> {
    let Ok(rd) = fs::read_dir(project.join(WORKTREES_DIR)) else {
        return vec![];
    };
    let mut out: Vec<WorktreeInfo> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            let state = e.path();
            let dir = fs::read_to_string(state.join(WORKTREE_DIR_FILE))
                .map(|t| t.trim_end_matches('\n').to_string())
                .unwrap_or_default();
            let bound: Option<Bound> = load_yaml(&state.join("bound.yaml")).ok().flatten();
            Some(WorktreeInfo {
                name,
                dir,
                view: bound.as_ref().map(|b| b.view.clone()),
                head: bound.map(|b| b.head),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// What the working copy is bound to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bound {
    pub view: String,
    pub head: String,
    pub at_ms: i64,
}

/// What the files were when they were last checked out or committed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Base {
    pub blocks: BTreeMap<BlockId, BaseBlock>,
    /// Path to the head id of that doc or folder file.
    pub files: BTreeMap<String, BlockId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseBlock {
    pub version: String,
    pub file: String,
    pub hidden: BTreeMap<String, Vec<HiddenLink>>,
}

/// A link the file never shows (it lives only in the store).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiddenLink {
    pub target: String,
    pub part: Option<String>,
}

/// What the person has staged for deletion. Staged files themselves are under `stage/files/`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stage {
    pub deleted: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    Blocks,
    DocRename,
    FolderRename,
    /// A commit was recorded, but its write-back left this doc's file alone (it held edits the
    /// commit did not record, or a file was in the way). Its base is still the old one, so an
    /// entry the commit created would be created again: every commit is refused until a checkout
    /// replaces the files. `path` is where the doc's file stays, `store_name` the path the store's
    /// version goes to, and `blocks` and `theirs_versions` the store's blocks of that file.
    Kept,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictEntry {
    pub path: String,
    pub kind: ConflictKind,
    pub blocks: Vec<BlockId>,
    pub theirs_versions: BTreeMap<BlockId, String>,
    pub local_name: Option<String>,
    pub store_name: Option<String>,
    pub store_parent: Option<String>,
}

/// The journal of a commit in flight: written before the commit, read to finish after a crash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub nonce: String,
    pub view: String,
    pub assigned: Vec<BlockId>,
    pub rewrite: Vec<BlockId>,
    pub files: Vec<String>,
    pub at_ms: i64,
}

/// A checkout or an update in flight: written durably before it changes any file, removed by the
/// save that finishes it. While it exists the files, the base copies and the state may disagree,
/// so every command that needs the binding refuses. `rdy checkout <view> --force` clears it; an
/// update's (`update`) is also cleared by `rdy update`, which finishes the update.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutMark {
    pub view: String,
    pub head: String,
    pub at_ms: i64,
    /// The rewrite in flight is `rdy update`'s, recorded in `UPDATE_FILE`: `rdy update` finishes
    /// it, keeping the local edits.
    #[serde(default)]
    pub update: bool,
}

/// A briefing recorded for a later commit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefEntry {
    pub token: String,
    pub ids: Vec<BlockId>,
    pub view: String,
    pub at_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WcState {
    pub bound: Option<Bound>,
    pub base: Base,
    pub stage: Stage,
    pub conflicts: Vec<ConflictEntry>,
    pub pending: Option<Pending>,
    pub briefs: Vec<BriefEntry>,
    /// An interrupted checkout (`checkout.yaml`); `None` when none is in flight.
    pub checkout: Option<CheckoutMark>,
}

fn io_err(path: &Path, what: &str, e: impl std::fmt::Display) -> RdyError {
    RdyError::new(
        E_IO,
        format!("cannot {what} {}: {e}", path.display()),
        "check the file and its permissions; if its contents are damaged, delete it and run rdy checkout again",
    )
}

fn read_opt(path: &Path) -> Result<Option<String>, RdyError> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(path, "read", e)),
    }
}

fn load_yaml<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, RdyError> {
    match read_opt(path)? {
        None => Ok(None),
        Some(s) => serde_yaml_ng::from_str(&s)
            .map(Some)
            .map_err(|e| io_err(path, "parse", e)),
    }
}

fn put(path: &Path, bytes: &[u8]) -> Result<(), RdyError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err(parent, "create", e))?;
    }
    write_durable(path, bytes).map_err(|e| io_err(path, "write", e))
}

fn put_yaml<T: Serialize>(path: &Path, v: &T) -> Result<(), RdyError> {
    let text = serde_yaml_ng::to_string(v).map_err(|e| io_err(path, "encode", e))?;
    put(path, text.as_bytes())
}

fn remove(path: &Path) -> Result<(), RdyError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(path, "remove", e)),
    }
}

impl WcState {
    /// Reads the state; a missing file reads as empty.
    pub fn load(place: &Place) -> Result<WcState, RdyError> {
        let s = &place.state;
        let base = match read_opt(&s.join("base/blocks.json"))? {
            None => Base::default(),
            Some(t) => serde_json::from_str(&t)
                .map_err(|e| io_err(&s.join("base/blocks.json"), "parse", e))?,
        };
        Ok(WcState {
            bound: WcState::load_bound(place)?,
            base,
            stage: load_yaml(&s.join("stage/deleted.yaml"))?.unwrap_or_default(),
            conflicts: load_yaml(&s.join("conflicts.yaml"))?.unwrap_or_default(),
            pending: load_yaml(&s.join("pending.yaml"))?,
            briefs: load_yaml(&s.join("briefs.yaml"))?.unwrap_or_default(),
            checkout: load_yaml(&s.join(CHECKOUT_FILE))?,
        })
    }

    /// Reads only `bound.yaml` (what the working copy is bound to); a missing file is `None`.
    pub fn load_bound(place: &Place) -> Result<Option<Bound>, RdyError> {
        load_yaml(&place.state.join("bound.yaml"))
    }

    /// Writes every file with `write_durable`. An empty `pending`, `bound` or `checkout` deletes
    /// its file. The checkout marker is written or removed last, so a save that fails part way
    /// leaves the marker of the checkout it was finishing.
    pub fn save(&self, place: &Place) -> Result<(), RdyError> {
        let s = &place.state;
        match &self.bound {
            Some(b) => put_yaml(&s.join("bound.yaml"), b)?,
            None => remove(&s.join("bound.yaml"))?,
        }
        let blocks = serde_json::to_vec(&self.base)
            .map_err(|e| io_err(&s.join("base/blocks.json"), "encode", e))?;
        put(&s.join("base/blocks.json"), &blocks)?;
        put_yaml(&s.join("stage/deleted.yaml"), &self.stage)?;
        put_yaml(&s.join("conflicts.yaml"), &self.conflicts)?;
        match &self.pending {
            Some(p) => put_yaml(&s.join("pending.yaml"), p)?,
            None => remove(&s.join("pending.yaml"))?,
        }
        put_yaml(&s.join("briefs.yaml"), &self.briefs)?;
        self.save_checkout_mark(place)
    }

    /// Sets the checkout marker and writes only its file, durably.
    pub fn mark_checkout(&mut self, place: &Place, mark: CheckoutMark) -> Result<(), RdyError> {
        self.checkout = Some(mark);
        self.save_checkout_mark(place)
    }

    /// Clears the checkout marker and removes only its file.
    pub fn unmark_checkout(&mut self, place: &Place) -> Result<(), RdyError> {
        self.checkout = None;
        self.save_checkout_mark(place)
    }

    fn save_checkout_mark(&self, place: &Place) -> Result<(), RdyError> {
        let path = place.state.join(CHECKOUT_FILE);
        match &self.checkout {
            Some(m) => put_yaml(&path, m),
            None => remove(&path),
        }
    }

    /// `E_WC_DIRTY` while an interrupted checkout's marker exists.
    pub fn require_settled(&self) -> Result<(), RdyError> {
        match &self.checkout {
            None => Ok(()),
            Some(m) if m.update => Err(RdyError::new(
                E_WC_DIRTY,
                format!(
                    "an update of {} was interrupted: the files and the working copy's state may disagree",
                    m.view
                ),
                "rdy update (it finishes the update and keeps your edits)",
            )),
            Some(m) => Err(RdyError::new(
                E_WC_DIRTY,
                format!(
                    "a checkout of {} was interrupted: the files and the working copy's state may disagree",
                    m.view
                ),
                format!("rdy checkout {} --force", m.view),
            )),
        }
    }

    /// The binding; `E_WC_DIRTY` while a checkout is interrupted (`require_settled`), else
    /// `E_WC_NOT_BOUND` when nothing is checked out.
    pub fn require_bound(&self) -> Result<&Bound, RdyError> {
        self.require_settled()?;
        self.bound.as_ref().ok_or_else(|| {
            RdyError::new(
                E_WC_NOT_BOUND,
                "this directory is not a working copy",
                "rdy checkout <view>",
            )
        })
    }

    /// Records a briefing, keeping the newest `MAX_BRIEFS`.
    pub fn record_brief(&mut self, e: BriefEntry) {
        self.briefs.push(e);
        let extra = self.briefs.len().saturating_sub(MAX_BRIEFS);
        self.briefs.drain(..extra);
    }
}

/// A path under `base`, refused when it is absolute or climbs out.
fn checked(rel: &str) -> Result<&Path, RdyError> {
    let p = Path::new(rel);
    if rel.is_empty() || !p.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            format!("{rel:?} is not a path inside the working copy"),
            "use a path relative to the working copy's root, without .. or a leading /",
        ));
    }
    Ok(p)
}

/// Stages `bytes` as the copy of the file at `rel`.
pub fn stage_put(place: &Place, rel: &str, bytes: &[u8]) -> Result<(), RdyError> {
    checked(rel)?;
    put(&place.stage_file(rel), bytes)
}

/// Drops the staged copy of `rel`, and the directories it leaves empty.
pub fn stage_clear(place: &Place, rel: &str) -> Result<(), RdyError> {
    checked(rel)?;
    let path = place.stage_file(rel);
    remove(&path)?;
    let top = place.state.join("stage/files");
    let mut dir = path.parent();
    while let Some(d) = dir {
        if d == top || fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
    Ok(())
}

/// The staged paths, sorted, `/`-separated, relative to the root.
pub fn stage_paths(place: &Place) -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for entry in rd.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Ok(ft) = entry.file_type() else { continue };
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if ft.is_dir() {
                walk(&entry.path(), &rel, out);
            } else if ft.is_file() && !name.starts_with('.') {
                out.push(rel);
            }
        }
    }
    let mut out = vec![];
    walk(&place.state.join("stage/files"), "", &mut out);
    out.sort();
    out
}

/// The exclusive lock every working-copy command takes.
#[derive(Debug)]
pub struct WcLock(File);

impl WcLock {
    /// Creates `state`, opens `state/lock` and takes the lock. While another holder has it, tries
    /// again every `LOCK_POLL` until `wait` has passed, then `E_WC_LOCKED`.
    pub fn take(state: &Path, wait: Duration) -> Result<WcLock, RdyError> {
        fs::create_dir_all(state).map_err(|e| io_err(state, "create", e))?;
        let path = state.join("lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| io_err(&path, "open", e))?;
        let deadline = Instant::now() + wait;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(WcLock(file)),
                Err(TryLockError::Error(e)) => return Err(io_err(&path, "lock", e)),
                Err(TryLockError::WouldBlock) => {}
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(RdyError::new(
                    E_WC_LOCKED,
                    "another rdy command is using this working copy",
                    "wait for it to finish, then retry",
                ));
            }
            std::thread::sleep(LOCK_POLL.min(deadline - now));
        }
    }
}

impl Drop for WcLock {
    /// Unlocks explicitly: the lock belongs to the open file description, which a child process
    /// shares until it execs.
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

static NONCE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh nonce: 32 lowercase hex characters, the first 16 bytes of BLAKE3 over the process id,
/// the time in nanoseconds, a process-wide counter and the thread id.
pub fn new_nonce() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = NONCE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let seed = format!(
        "{}:{nanos}:{n}:{:?}",
        std::process::id(),
        std::thread::current().id()
    );
    Hash::of(seed.as_bytes()).to_string()[..32].to_string()
}
