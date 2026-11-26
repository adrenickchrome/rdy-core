//! The fetch of a project's incomplete history after a URL clone made it ready (D-GGTYA,
//! D-4PV25, D-KE83X, O16, O17).
//!
//! A clone from a hub that sends a canon tip's state (stream format 5) lists every canon
//! history key at the captured tip before its state take, writes them as the one checked key
//! file (`.rdy/sync/<remote>/history/keys`, [`write_key_file`]) and names their exact count in
//! the incomplete history ref. The project is ready then. This module brings the rest:
//!
//! 1. **Begin.** [`SyncService::begin_history`] resolves the remote as a pull does and takes
//!    its one-sync-per-remote guard. The [`HistoryFetch`] owns that guard for the whole fetch,
//!    including the yields between steps; dropping it (cancellation or completion) releases it.
//! 2. **Steps.** [`SyncService::resume_history_step`] does at most one of these, in this order,
//!    and rechecks the history ref first each time:
//!    - discovery and the state step, once per fetch (a changed hub repo id is
//!      `E_REPO_MISMATCH`; a hub that moved on does not change the frontier being completed);
//!    - once per fetch, the recheck of the staged files under `.rdy/sync/<remote>/history/`: a
//!      `.part` file is removed; a `.batch` file is checked whole again (its repo, purpose,
//!      root, bytes and limits, and each key listed in the key file once) and removed when it
//!      fails, so its keys are fetched again; a link is never followed;
//!    - one get of keys the key file lists that neither the store nor a staged file holds,
//!      written to the next numbered `.part` file, checked, synced and renamed to `.batch`
//!      (`00000000.batch` to `00001023.batch`, at most [`MAX_STAGED_BYTES`] raw bytes);
//!    - when every key is staged or held: the completion take, all staged files checked again
//!      off the writer and taken in one checked batch that moves no canon or view ref; the
//!      batch carries the fetched provenance of the sync ref, so the older records read as
//!      fetched;
//!    - when the store holds every key: the full check, captured on the writer (the published
//!      state, a reader and the frame check's bound, no record read) and run off it on that
//!      capture ([`crate::fsck::FullCheck`]);
//!    - after a passing full check: the second and last history ref write, `complete` true,
//!      only while the frontier and the redaction epoch are still the ones the check saw; when
//!      either changed the check runs once more first. Its publish reindexes the whole history
//!      and logs the drift moves the older landings cause, once, under cause `recheck`. Then
//!      the staged files are removed; a failed removal never undoes the completion.
//!
//!    A crash after the take and before the completion repeats the full check, not the take.
//!    Held progress is never stored: the steps return it.
//! 3. **Pull.** Every ordinary pull first resumes its remote's incomplete history under its own
//!    guard ([`resume_under_guard`]), then pulls as before.
use crate::engine::{Engine, with_writer_result};
use crate::error::{E_INTERNAL, E_IO, E_LIMIT, RdyError, upstream_protocol};
use crate::hub_sync::{GetEnd, GroupSizer, SyncService, Target, check_got};
use crate::hub_sync_wire::SyncState;
use crate::partial_history::{HistoryKeys, HistoryProgress, history_dir, key_file, read_key_file};
use crate::session::Session;
use crate::sync_state::RemoteGuard;
use crate::{Rdy, ReadState};
use rdy_model::{Hash, RepoId};
use rdy_schema::Issue;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_state::{MAX_HISTORY_RECORDS, SyncHistory};
use rdy_store::sync_stream::StreamPurpose;
use rdy_store::take_batch::FetchTarget;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// A test hook the parts of a history completion that run off the writer call at each named
/// point: `batches` (a staged file's check before the take), `frames`, `tags` and `state` (the
/// full check's frame walk, tag rules and state checks). Production installs none.
pub type CheckHook = Box<dyn FnMut(&'static str)>;

thread_local! {
    static CHECK_HOOK: std::cell::RefCell<Option<CheckHook>> = const { std::cell::RefCell::new(None) };
}

/// Runs `f` with `hook` installed for this thread's history completion (a test hook; see
/// [`CheckHook`]). The prior hook comes back when `f` returns or panics.
#[doc(hidden)]
pub fn with_check_hook_for_tests<T>(hook: CheckHook, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<CheckHook>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let prior = self.0.take();
            CHECK_HOOK.with(|c| *c.borrow_mut() = prior);
        }
    }
    let prior = CHECK_HOOK.with(|c| c.borrow_mut().replace(hook));
    let _restore = Restore(prior);
    f()
}

/// Calls this thread's [`CheckHook`], if one is installed, at point `name`.
pub(crate) fn check_point(name: &'static str) {
    CHECK_HOOK.with(|c| {
        if let Some(h) = c.borrow_mut().as_mut() {
            h(name);
        }
    });
}

/// The most staged batch files of one history.
pub const MAX_STAGED_FILES: u32 = 1024;
/// The most raw bytes of one history's staged batch files together.
pub const MAX_STAGED_BYTES: u64 = 2_147_483_648;
/// The note of a URL clone from a hub that sends no canon state: the clone takes the whole
/// history before it is ready.
pub const OLD_HUB_NOTE: &str = "this hub sends history only; the clone waits for all of it";
/// The note of a ready URL clone whose history no process fetches now (MCP with no daemon).
pub const PAUSED_NOTE: &str = "history fetch paused; run rdy pull to resume it";

/// The ready text of a URL clone into `dir` whose history is still arriving.
pub fn ready_text(dir: &str, p: &HistoryProgress) -> String {
    format!(
        "ready: work in {dir} now; history arriving: {} of {} records",
        p.held, p.total
    )
}

/// What one step of a history fetch left: more to do, or the history complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryFetchStep {
    More(HistoryProgress),
    Complete(HistoryProgress),
}

impl HistoryFetchStep {
    /// The progress after the step.
    pub fn progress(&self) -> HistoryProgress {
        match self {
            HistoryFetchStep::More(p) | HistoryFetchStep::Complete(p) => *p,
        }
    }
}

/// One fetch of a project's incomplete history (see the module docs). It owns the remote's
/// guard for its whole life. Nothing in it is stored.
pub struct HistoryFetch {
    target: Target,
    _guard: Option<RemoteGuard>,
    warnings: Vec<Issue>,
    frontier: Hash,
    repo_id: RepoId,
    /// The hub's sync base and state, from this fetch's one discovery and state step.
    opened: Option<(String, SyncState)>,
    /// The checked key file's keys, read once per fetch.
    keys: Option<Vec<ObjectKey>>,
    /// The staged batch files that passed this fetch's recheck, by number, with their bytes and
    /// keys; None until the recheck ran.
    staged: Option<BTreeMap<u32, (u64, Vec<ObjectKey>)>>,
    sizer: Option<GroupSizer>,
    /// The halves of groups a get asked to split, got first, last pushed first.
    split: Vec<Vec<ObjectKey>>,
    /// The frontier and redaction epoch a passing full check saw.
    checked: Option<(Hash, u64)>,
    /// Whether the full check already ran again after a refused completion.
    rechecked: bool,
}

impl std::fmt::Debug for HistoryFetch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryFetch")
            .field("remote", &self.target.name)
            .field("frontier", &self.frontier)
            .finish_non_exhaustive()
    }
}

impl HistoryFetch {
    /// The fetch of remote `target`'s history in the project of `engine`. The project must hold
    /// a history ref of that remote.
    fn new(
        engine: &dyn Engine,
        target: Target,
        guard: Option<RemoteGuard>,
        warnings: Vec<Issue>,
    ) -> Result<HistoryFetch, RdyError> {
        let read = engine.read();
        let Some(h) = read.reader().sync_history(&target.name)? else {
            return Err(RdyError::new(
                crate::E_BAD_INPUT,
                format!("the project holds no history from remote {}", target.name),
                "run rdy pull",
            ));
        };
        Ok(HistoryFetch {
            target,
            _guard: guard,
            warnings,
            frontier: h.frontier,
            repo_id: h.repo_id,
            opened: None,
            keys: None,
            staged: None,
            sizer: None,
            split: Vec::new(),
            checked: None,
            rechecked: false,
        })
    }

    /// The remote this fetch completes.
    pub fn remote(&self) -> &str {
        &self.target.name
    }

    /// The frontier this fetch completes.
    pub fn frontier(&self) -> Hash {
        self.frontier
    }
}

/// The checked key file of remote `remote` at `url` in the project at `root`: `keys`, the canon
/// history's keys at `frontier` of hub repo `repo_id` in the hub's order, written durably, then
/// read back through the key file's own check (closed value, bounds, portable distinct keys,
/// exact count). Returns the incomplete history ref that names their count.
pub(crate) fn write_key_file(
    root: &Path,
    remote: &str,
    url: &str,
    repo_id: RepoId,
    frontier: Hash,
    keys: Vec<ObjectKey>,
) -> Result<SyncHistory, RdyError> {
    if keys.len() as u64 > MAX_HISTORY_RECORDS {
        return Err(RdyError::new(
            E_LIMIT,
            format!(
                "the hub's history at {frontier} lists more than {MAX_HISTORY_RECORDS} records"
            ),
            "ask the hub's operator to split the repo's history",
        ));
    }
    let history = SyncHistory {
        repo_id,
        url: url.to_string(),
        frontier,
        complete: false,
        records: keys.len() as u64,
    };
    let dir = history_dir(root, remote);
    rdy_store::fsutil::create_dir_durable(&dir).map_err(|e| io_at(&dir, &e))?;
    let path = key_file(root, remote);
    let value = HistoryKeys {
        repo_id,
        frontier,
        keys,
    };
    let bytes = serde_json::to_vec(&value).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("the history key file does not serialize: {e}"),
            "report this as a bug",
        )
    })?;
    drop(value);
    rdy_store::fsutil::write_durable(&path, &bytes).map_err(|e| io_at(&path, &e))?;
    drop(bytes);
    read_key_file(&path, &history)?;
    Ok(history)
}

fn io_at(path: &Path, e: &std::io::Error) -> RdyError {
    RdyError::new(
        E_IO,
        format!("{}: {e}", path.display()),
        "check the path, its permissions and disk space",
    )
}

/// The name of staged batch file `n`.
fn batch_name(n: u32) -> String {
    format!("{n:08}.batch")
}

/// The number of a staged file name `name` with extension `ext`, when it is one.
fn staged_number(name: &str, ext: &str) -> Option<u32> {
    let stem = name.strip_suffix(ext)?.strip_suffix('.')?;
    (stem.len() == 8 && stem.bytes().all(|b| b.is_ascii_digit()))
        .then(|| stem.parse().ok())
        .flatten()
        .filter(|n| *n < MAX_STAGED_FILES)
}

/// Removes every staged batch and part file of `dir`, best effort.
fn remove_staged(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".batch") || name.ends_with(".part") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let _ = rdy_store::fsutil::sync_dir(dir);
}

/// Checks the staged batch file at `path` whole: a regular file (no link followed) holding one
/// canon history batch of repo `repo` rooted at `frontier`, each key listed in `listed` and not
/// in `seen`. Returns its bytes and keys, or None when it fails any rule.
fn check_staged(
    path: &Path,
    repo: RepoId,
    frontier: Hash,
    listed: &HashSet<ObjectKey>,
    seen: &HashSet<ObjectKey>,
) -> Option<(u64, Vec<ObjectKey>)> {
    let f = rdy_store::pio::open_no_follow(std::fs::OpenOptions::new().read(true), path).ok()?;
    let meta = f.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_STAGED_BYTES {
        return None;
    }
    let checked = crate::stream_check::check_batch_file(f, Some(repo), |_, _, _| {}).ok()?;
    let report = checked.report();
    let root = ObjectKey {
        kind: kinds::LANDING,
        hash: frontier,
    };
    if report.header.purpose != StreamPurpose::CanonHistory || report.header.root() != root {
        return None;
    }
    let keys = report.keys.clone();
    if keys.iter().any(|k| !listed.contains(k) || seen.contains(k)) {
        return None;
    }
    Some((meta.len(), keys))
}

/// The recheck of the staged files of `dir` (module docs): part files go, failing batch files
/// go, links go (never followed), and the batch files that pass are returned by number.
fn recheck_staged(
    dir: &Path,
    repo: RepoId,
    frontier: Hash,
    keys: &[ObjectKey],
) -> Result<BTreeMap<u32, (u64, Vec<ObjectKey>)>, RdyError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(io_at(dir, &e)),
    };
    let listed: HashSet<ObjectKey> = keys.iter().copied().collect();
    let mut batches = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| io_at(dir, &e))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry.file_type().map_err(|e| io_at(&path, &e))?;
        if name.ends_with(".part") {
            std::fs::remove_file(&path).map_err(|e| io_at(&path, &e))?;
            continue;
        }
        if !name.ends_with(".batch") {
            continue;
        }
        match staged_number(&name, "batch") {
            Some(n) if kind.is_file() => batches.push((n, path)),
            // a link, a directory or a name out of the numbered range is never read
            _ => std::fs::remove_file(&path).map_err(|e| io_at(&path, &e))?,
        }
    }
    batches.sort();
    let mut out = BTreeMap::new();
    let mut seen: HashSet<ObjectKey> = HashSet::new();
    let mut bytes = 0u64;
    for (n, path) in batches {
        match check_staged(&path, repo, frontier, &listed, &seen) {
            Some((len, ks)) if bytes.saturating_add(len) <= MAX_STAGED_BYTES => {
                bytes += len;
                seen.extend(ks.iter().copied());
                out.insert(n, (len, ks));
            }
            _ => std::fs::remove_file(&path).map_err(|e| io_at(&path, &e))?,
        }
    }
    rdy_store::fsutil::sync_dir(dir).map_err(|e| io_at(dir, &e))?;
    Ok(out)
}

/// A serialized read on `engine` with a typed result (as [`with_writer_result`] is for a
/// write): it writes nothing and seals no batch.
fn with_reader_result<T: Send + 'static>(
    engine: &dyn Engine,
    f: impl FnOnce(&Rdy) -> Result<T, RdyError> + Send + 'static,
) -> Result<T, RdyError> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<T>(1);
    engine.read_serialized(Box::new(move |r| {
        let v = f(r)?;
        let _ = tx.send(v);
        Ok(serde_json::Value::Null)
    }))?;
    rx.try_recv().map_err(|_| {
        RdyError::new(
            E_INTERNAL,
            "a read returned without its result",
            "report this as a bug in rdy",
        )
    })
}

/// Resumes the incomplete history of `t`'s remote in the project of `engine`, if it has one,
/// to its end, under the guard the caller (a pull) already holds.
pub(crate) fn resume_under_guard(
    service: &SyncService,
    engine: &dyn Engine,
    session: &Session,
    t: &Target,
) -> Result<(), RdyError> {
    match engine.read().partial_history()? {
        Some((remote, _)) if remote == t.name => {}
        _ => return Ok(()),
    }
    let mut fetch = HistoryFetch::new(engine, t.clone(), None, Vec::new())?;
    while let HistoryFetchStep::More(_) = service.step(engine, session, &mut fetch)? {}
    Ok(())
}

impl SyncService<'_> {
    /// Begins the fetch of remote `remote`'s incomplete history in the project of `engine`:
    /// the remote's target as a pull resolves it, and its guard, which the fetch owns until it
    /// is dropped. A second sync of that remote meanwhile is `E_LOCKED`.
    pub fn begin_history(
        &self,
        engine: &dyn Engine,
        session: &Session,
        remote: &str,
    ) -> Result<HistoryFetch, RdyError> {
        let (target, guard, warnings) = self.resolve_target(session, Some(remote), true)?;
        HistoryFetch::new(engine, target, guard, warnings)
    }

    /// One step of `fetch` (module docs): at most one get, the completion take, the full
    /// check, or the completion ref batch. `Complete` once the history ref is complete.
    pub fn resume_history_step(
        &self,
        engine: &dyn Engine,
        session: &Session,
        fetch: &mut HistoryFetch,
    ) -> Result<HistoryFetchStep, RdyError> {
        self.step(engine, session, fetch).map_err(|e| {
            let t = fetch.target.clone();
            let warnings = fetch.warnings.clone();
            self.fail(Some(&t), &warnings, e)
        })
    }

    fn step(
        &self,
        engine: &dyn Engine,
        _session: &Session,
        fetch: &mut HistoryFetch,
    ) -> Result<HistoryFetchStep, RdyError> {
        let read = engine.read();
        let remote = fetch.target.name.clone();
        let root = read.root().to_path_buf();
        let dir = history_dir(&root, &remote);
        // the ref, again at each step
        let Some(h) = read.reader().sync_history(&remote)? else {
            return Err(RdyError::new(
                crate::E_BAD_INPUT,
                format!("the project holds no history from remote {remote}"),
                "run rdy pull",
            ));
        };
        if h.frontier != fetch.frontier || h.repo_id != fetch.repo_id {
            return Err(RdyError::new(
                crate::E_BAD_INPUT,
                format!("the history from remote {remote} changed during its fetch"),
                "run rdy pull to resume it",
            ));
        }
        if h.complete {
            drop(read);
            remove_staged(&dir);
            return Ok(HistoryFetchStep::Complete(HistoryProgress {
                frontier: h.frontier,
                complete: true,
                held: h.records,
                total: h.records,
            }));
        }
        if fetch.keys.is_none() {
            fetch.keys = Some(read_key_file(&key_file(&root, &remote), &h)?.keys);
        }
        if fetch.staged.is_none() {
            let keys = fetch.keys.as_deref().unwrap_or_default();
            fetch.staged = Some(recheck_staged(&dir, h.repo_id, h.frontier, keys)?);
        }
        let reader = read.reader();
        let keys = fetch.keys.as_deref().unwrap_or_default();
        let mut missing = Vec::new();
        for k in keys {
            if !reader.has_kind(k.kind, &k.hash)? {
                missing.push(*k);
            }
        }
        if !missing.is_empty() {
            let staged: HashSet<ObjectKey> = fetch
                .staged
                .iter()
                .flat_map(|s| s.values())
                .flat_map(|(_, ks)| ks.iter().copied())
                .collect();
            let want: Vec<ObjectKey> = missing
                .iter()
                .filter(|k| !staged.contains(k))
                .copied()
                .collect();
            if !want.is_empty() {
                self.fetch_one(&read, fetch, &dir, &want)?;
            } else {
                drop(read);
                self.take_staged(engine, fetch, &dir)?;
            }
            let read = engine.read();
            return Ok(HistoryFetchStep::More(progress(&read, fetch, &h)?));
        }
        // the store holds every listed key: the full check, then the completion
        let Some((frontier, epoch)) = fetch.checked else {
            drop(read);
            let capture = with_reader_result(engine, Rdy::capture_full_check)?;
            let frontier = capture.frontier.unwrap_or(h.frontier);
            let epoch = capture.epoch;
            capture.finish()?;
            fetch.checked = Some((frontier, epoch));
            let read = engine.read();
            return Ok(HistoryFetchStep::More(progress(&read, fetch, &h)?));
        };
        drop(read);
        let r = remote.clone();
        let done = with_writer_result(engine, move |rdy| {
            // N1: the frontier and the epoch the check saw, compared in the final batch's call
            let current = rdy.store.reader().sync_history(&r)?;
            if current.is_none_or(|c| c.complete || c.frontier != frontier)
                || rdy.store.redaction_epoch() != epoch
            {
                return Ok(false);
            }
            crate::sync_fetch::complete_history(rdy, &r, frontier, epoch)?;
            Ok(true)
        })?;
        if !done {
            if fetch.rechecked {
                return Err(RdyError::new(
                    crate::E_BAD_INPUT,
                    format!("the history from remote {remote} changed again during its check"),
                    "run rdy pull to resume it",
                ));
            }
            fetch.rechecked = true;
            fetch.checked = None;
            let read = engine.read();
            return Ok(HistoryFetchStep::More(progress(&read, fetch, &h)?));
        }
        // the completion is durable: the staged files go, and a failure there undoes nothing
        remove_staged(&dir);
        Ok(HistoryFetchStep::Complete(HistoryProgress {
            frontier,
            complete: true,
            held: h.records,
            total: h.records,
        }))
    }

    /// One get of the next group of `want` (or of a group a split left), into the next numbered
    /// `.part` file of `dir`, checked, synced and renamed to `.batch`.
    fn fetch_one(
        &self,
        read: &ReadState,
        fetch: &mut HistoryFetch,
        dir: &Path,
        want: &[ObjectKey],
    ) -> Result<(), RdyError> {
        if fetch.opened.is_none() {
            // a changed hub repo id is E_REPO_MISMATCH here (`SyncService::open`)
            fetch.opened = Some(self.open(&fetch.target, read, &[])?);
        }
        let (base_url, state) = fetch.opened.as_ref().expect("opened above");
        let sizer = fetch
            .sizer
            .get_or_insert_with(|| GroupSizer::for_state(state));
        let keys: Vec<ObjectKey> = match fetch.split.pop() {
            Some(k) => k,
            None => want[..sizer.next(want)].to_vec(),
        };
        let staged = fetch.staged.get_or_insert_with(BTreeMap::new);
        let n = staged.keys().next_back().map_or(0, |n| n + 1);
        if n >= MAX_STAGED_FILES {
            return Err(RdyError::new(
                E_LIMIT,
                format!("the history fetch staged {MAX_STAGED_FILES} batch files"),
                "run rdy pull to resume it; the staged files are taken first",
            ));
        }
        rdy_store::fsutil::create_dir_durable(dir).map_err(|e| io_at(dir, &e))?;
        let part = dir.join(format!("{n:08}.part"));
        let mut file = rdy_store::pio::open_no_follow(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true),
            &part,
        )
        .map_err(|e| io_at(&part, &e))?;
        let root = ObjectKey {
            kind: kinds::LANDING,
            hash: fetch.frontier,
        };
        let got = self.get_one(
            read,
            &fetch.target,
            base_url,
            state,
            StreamPurpose::CanonHistory,
            root,
            &keys,
            &mut file,
        );
        let written = match got {
            Ok(GetEnd::Got(w)) => w,
            Ok(GetEnd::Split) => {
                drop(file);
                let _ = std::fs::remove_file(&part);
                let (front, back) = keys.split_at(keys.len() / 2);
                fetch.split.push(back.to_vec());
                fetch.split.push(front.to_vec());
                return Ok(());
            }
            Err(e) => {
                drop(file);
                let _ = std::fs::remove_file(&part);
                return Err(e);
            }
        };
        let total: u64 = staged.values().map(|(b, _)| *b).sum();
        if total.saturating_add(written) > MAX_STAGED_BYTES {
            drop(file);
            let _ = std::fs::remove_file(&part);
            return Err(RdyError::new(
                E_LIMIT,
                format!("the history fetch's staged files exceed {MAX_STAGED_BYTES} bytes"),
                "run rdy pull to resume it; the staged files are taken first",
            ));
        }
        file.sync_all().map_err(|e| io_at(&part, &e))?;
        let checked = check_got(
            file,
            fetch.repo_id,
            StreamPurpose::CanonHistory,
            root,
            sizer,
            written,
        );
        let checked = match checked {
            Ok(c) => c,
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                return Err(e);
            }
        };
        let got_keys = checked.report().keys.clone();
        drop(checked);
        if got_keys.iter().any(|k| !keys.contains(k)) {
            let _ = std::fs::remove_file(&part);
            return Err(upstream_protocol());
        }
        let batch = dir.join(batch_name(n));
        std::fs::rename(&part, &batch).map_err(|e| io_at(&batch, &e))?;
        rdy_store::fsutil::sync_dir(dir).map_err(|e| io_at(dir, &e))?;
        staged.insert(n, (written, got_keys));
        Ok(())
    }

    /// The completion take: every staged file of `dir` checked whole again (a file that fails is
    /// removed, and its keys are fetched again by a later step), then all of them in one checked
    /// batch at the frontier.
    fn take_staged(
        &self,
        engine: &dyn Engine,
        fetch: &mut HistoryFetch,
        dir: &Path,
    ) -> Result<(), RdyError> {
        let staged = fetch.staged.take().unwrap_or_default();
        let mut files = Vec::with_capacity(staged.len());
        let mut kept = BTreeMap::new();
        let mut failed = false;
        for (n, (len, keys)) in staged {
            let path = dir.join(batch_name(n));
            let opened =
                rdy_store::pio::open_no_follow(std::fs::OpenOptions::new().read(true), &path);
            check_point("batches");
            let checked = opened.ok().and_then(|f| {
                crate::stream_check::check_batch_file(f, Some(fetch.repo_id), |_, _, _| {}).ok()
            });
            match checked {
                Some(c) if c.report().keys == keys => {
                    files.push(c);
                    kept.insert(n, (len, keys));
                }
                _ => {
                    let _ = std::fs::remove_file(&path);
                    failed = true;
                }
            }
        }
        if failed {
            // a changed or corrupt file: its keys are fetched again
            fetch.staged = Some(kept);
            return Ok(());
        }
        fetch.staged = Some(kept);
        if files.is_empty() {
            return Err(upstream_protocol());
        }
        let target = FetchTarget {
            remote: fetch.target.name.clone(),
            url: fetch.target.url.clone(),
            purpose: StreamPurpose::CanonHistory,
            root: fetch.frontier,
            view: None,
        };
        with_writer_result(engine, move |rdy| {
            crate::sync_fetch::take_history_checked(rdy, &files, &target)
        })?;
        Ok(())
    }
}

/// The progress of `fetch` over `read`: the listed keys the store or a staged file holds, each
/// once.
fn progress(
    read: &ReadState,
    fetch: &HistoryFetch,
    h: &SyncHistory,
) -> Result<HistoryProgress, RdyError> {
    let staged: HashSet<ObjectKey> = fetch
        .staged
        .iter()
        .flat_map(|s| s.values())
        .flat_map(|(_, ks)| ks.iter().copied())
        .collect();
    let reader = read.reader();
    let mut held = 0u64;
    for k in fetch.keys.as_deref().unwrap_or_default() {
        if staged.contains(k) || reader.has_kind(k.kind, &k.hash)? {
            held += 1;
        }
    }
    Ok(HistoryProgress {
        frontier: h.frontier,
        complete: false,
        held: held.min(h.records),
        total: h.records,
    })
}

/// The paths of remote `remote`'s staged batch files in the project at `root`, in name order.
#[doc(hidden)]
pub fn staged_batches_for_tests(root: &Path, remote: &str) -> Vec<PathBuf> {
    crate::partial_history::staged_files(&history_dir(root, remote)).unwrap_or_default()
}
