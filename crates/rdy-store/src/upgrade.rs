//! The store upgrade (D-WWAK7): one logged batch that moves a store's feature manifest, with a
//! complete backup first. The backup is the only rollback.
//!
//! [`inspect`] plans an upgrade from the closed store's files: the `FORMAT` mirror, the sealed
//! log's `meta/store_features` and its last `meta/upgrade` receipt. It opens no store, takes or
//! creates no lock, and writes nothing. [`apply_locked`] runs the plan under the caller's lock:
//!
//! 1. it reads the store again and requires the plan it was given;
//! 2. an empty plan returns at once, with no backup and no batch;
//! 3. it copies the closed store to the sibling `.store-upgrade-<manifest hash>-<batch hash>`
//!    (every regular file but the lock and the open sentinel; a link or a special file is
//!    refused), fsyncs every file and directory, and writes `backup.json` last;
//! 4. it opens the store with no upgrade policy and appends one marked batch: the new
//!    `meta/store_features` and the `meta/upgrade` receipt;
//! 5. it rebuilds the index and runs the project's or the holder's store check.
//!
//! `FORMAT` still mirrors the old manifest then: the engine runs its full fsck, then
//! [`publish_checked`] writes the new mirror. A later inspect of a store whose mirror lags the
//! receipt reports `pending_check`, and its plan is empty, so no second batch is written.
//! Nothing here restores a backup: a person does that, after stopping the daemon.
use crate::error::{StoreError, UpgradeFailure};
use crate::format::{self, StoreFeature, StoreFeatures, UpgradeReceipt};
use crate::holder::StoreMode;
use crate::segment::{self, HEADER, MARKER_MAX};
use crate::store::INDEX_OPEN_SENTINEL;
use crate::{Store, StoreLock, fsutil, kinds};
use rdy_model::Hash;
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// The most steps one registry, and so one plan, holds.
pub const MAX_STEPS: usize = 256;
/// The most entries (files and directories) a backup holds.
pub const MAX_BACKUP_ENTRIES: usize = 1_000_000;
/// The most bytes of one relative path in a backup.
pub const MAX_BACKUP_PATH_BYTES: usize = 1_048_576;
/// The file a complete backup writes last.
pub const BACKUP_MANIFEST: &str = "backup.json";
/// The name prefix of a backup, the store directory's sibling.
pub const BACKUP_PREFIX: &str = ".store-upgrade-";
/// The copy buffer of a backup.
const COPY_BUFFER: usize = 1 << 20;
/// The store's lock file, which no backup copies.
const LOCK_FILE: &str = "lock";

/// One registered upgrade step: it applies to a manifest that holds every feature of `from`
/// and not yet every feature of `to`, and replaces `from`'s features with `to`'s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpgradeStep {
    pub id: String,
    pub from: StoreFeatures,
    pub to: StoreFeatures,
}

/// What [`inspect`] found: the authoritative manifest (`from`), the manifest the planned steps
/// make (`to`), the step ids in execution order, the bytes a backup copies, and the last upgrade
/// receipt. `pending_check` is true when that receipt explains a sealed manifest whose `FORMAT`
/// mirror is still the old one: the engine's checks and the mirror are still to come.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpgradePlan {
    pub from: StoreFeatures,
    pub to: StoreFeatures,
    pub steps: Vec<String>,
    pub backup_bytes: u64,
    pub requires_upgrade: bool,
    pub pending_check: bool,
    pub receipt: Option<format::UpgradeReceipt>,
}

/// What an upgrade did. `backup` is the backup's path and `batch` the upgrade batch's marker.
/// `checked` stays false here: the engine's full fsck sets it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpgradeReport {
    pub dry_run: bool,
    pub changed: bool,
    pub from: StoreFeatures,
    pub to: StoreFeatures,
    pub steps: Vec<String>,
    pub backup: Option<PathBuf>,
    pub batch: Option<Hash>,
    pub reindexed: bool,
    pub checked: bool,
    pub receipt: Option<format::UpgradeReceipt>,
}

/// This rdy's upgrade steps. The first release registers none.
const REGISTRY: &[UpgradeStep] = &[];

/// Plans the upgrade of the store at `dir` with this rdy's steps (see the module docs). Reads
/// only: no store is opened, no lock is taken or created, nothing is written.
pub fn inspect(dir: &Path) -> Result<UpgradePlan, StoreError> {
    inspect_with(dir, REGISTRY).map(|(plan, _)| plan)
}

/// Test hook: [`inspect`] with the steps `registry`, through the same planner. No CLI, MCP,
/// config or environment path reaches it.
#[doc(hidden)]
pub fn inspect_with_registry_for_tests(
    dir: &Path,
    registry: &[UpgradeStep],
) -> Result<UpgradePlan, StoreError> {
    inspect_with(dir, registry).map(|(plan, _)| plan)
}

/// Runs `plan` on the store at `dir`, which `lock` (the caller's) guards (see the module docs).
/// A store that moved since the plan is `E_UPGRADE_MOVED`; an empty plan changes nothing. On
/// success the batch is durable, the index rebuilt and the store check passed; `FORMAT` is
/// published only by [`publish_checked`], after the engine's full fsck.
pub fn apply_locked(
    dir: &Path,
    lock: &StoreLock,
    plan: &UpgradePlan,
) -> Result<UpgradeReport, StoreError> {
    apply_with(dir, lock, plan, REGISTRY)
}

/// Test hook: [`apply_locked`] with the steps `registry`, through the same runner.
#[doc(hidden)]
pub fn apply_locked_with_registry_for_tests(
    dir: &Path,
    lock: &StoreLock,
    plan: &UpgradePlan,
    registry: &[UpgradeStep],
) -> Result<UpgradeReport, StoreError> {
    apply_with(dir, lock, plan, registry)
}

/// Publishes `FORMAT` for the upgrade `store` holds, once the engine's full fsck passed: the
/// log's manifest must be the last receipt's `to`, and `FORMAT` its `from` (or already the
/// manifest, which makes this a no-op). It writes `FORMAT` durably and nothing else: no object
/// or log frame changes.
pub fn publish_checked(store: &mut Store) -> Result<(), StoreError> {
    let log = store.store_features()?;
    if &log != store.features() {
        return Err(moved(None));
    }
    let dir = store.dir().to_path_buf();
    let mirror = format::read_format_file(&dir)?;
    if mirror.as_ref() == Some(&log) {
        return Ok(());
    }
    let receipt = match store.get_ref(format::REF_UPGRADE)? {
        Some(v) => Some(UpgradeReceipt::parse(&v).map_err(|e| {
            StoreError::Decode(format!("meta/upgrade is not a valid receipt: {e}"))
        })?),
        None => None,
    };
    let explained = receipt
        .is_some_and(|r| r.to == log.hash() && mirror.as_ref().is_some_and(|m| m.hash() == r.from));
    if !explained {
        return Err(moved(None));
    }
    format::write_format_file(&dir, &log)
}

/// Test hook: the upgrade's own open of the store at `dir` under `lock`, which accepts a
/// `FORMAT` its last receipt explains (the engine's completion opens the store so).
#[doc(hidden)]
pub fn open_pending_for_tests(dir: &Path, lock: &StoreLock) -> Result<Store, StoreError> {
    Store::open_unupgraded_locked(dir, lock)
}

/// A point of the upgrade where a test hook runs ([`with_hook_for_tests`]).
#[doc(hidden)]
pub enum Phase<'a> {
    /// Before the copy of the backup file at this relative path.
    Copy(&'a str),
    /// Before the fsync of the backup file or directory at this relative path (`""` is the
    /// backup itself).
    Sync(&'a str),
    /// The backup is complete and durable; the store is not open yet.
    BackupComplete,
    /// The store is open; nothing is appended yet.
    Opened(&'a mut Store),
    /// The upgrade batch is sealed and durable; the index is not rebuilt yet.
    Sealed(&'a mut Store),
    /// The index is rebuilt; the store check has not run.
    Reindexed(&'a mut Store),
    /// The store check passed; the store is not closed yet.
    Checked,
}

impl Phase<'_> {
    /// The phase's name, as a test prints it.
    pub fn name(&self) -> &'static str {
        match self {
            Phase::Copy(_) => "copy",
            Phase::Sync(_) => "sync",
            Phase::BackupComplete => "backup_complete",
            Phase::Opened(_) => "opened",
            Phase::Sealed(_) => "sealed",
            Phase::Reindexed(_) => "reindexed",
            Phase::Checked => "checked",
        }
    }
}

type Hook = Box<dyn for<'a> FnMut(Phase<'a>) -> io::Result<()>>;

thread_local! {
    /// This thread's test hook ([`with_hook_for_tests`]).
    static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    /// This thread's backup bounds ([`with_backup_limits_for_tests`]).
    static LIMITS: Cell<(usize, usize)> = const {
        Cell::new((MAX_BACKUP_ENTRIES, MAX_BACKUP_PATH_BYTES))
    };
}

/// Test hook: runs `f` with `hook` called at each [`Phase`] of an upgrade on this thread. An
/// error the hook returns fails the upgrade there, as an I/O error at that point would.
#[doc(hidden)]
pub fn with_hook_for_tests<T>(
    hook: impl for<'a> FnMut(Phase<'a>) -> io::Result<()> + 'static,
    f: impl FnOnce() -> T,
) -> T {
    struct Off;
    impl Drop for Off {
        fn drop(&mut self) {
            HOOK.with(|h| *h.borrow_mut() = None);
        }
    }
    HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
    let _off = Off;
    f()
}

/// Test hook: runs `f` with lower backup bounds on this thread: at most `entries` entries and
/// `path_bytes` bytes per relative path.
#[doc(hidden)]
pub fn with_backup_limits_for_tests<T>(
    entries: usize,
    path_bytes: usize,
    f: impl FnOnce() -> T,
) -> T {
    struct Restore((usize, usize));
    impl Drop for Restore {
        fn drop(&mut self) {
            LIMITS.with(|l| l.set(self.0));
        }
    }
    let _restore = Restore(LIMITS.with(|l| l.replace((entries, path_bytes))));
    f()
}

/// Calls this thread's hook, if any, at `phase`.
fn hit(phase: Phase<'_>) -> Result<(), StoreError> {
    let Some(mut hook) = HOOK.with(|h| h.borrow_mut().take()) else {
        return Ok(());
    };
    let r = hook(phase);
    HOOK.with(|h| *h.borrow_mut() = Some(hook));
    r.map_err(StoreError::from)
}

fn moved(backup: Option<PathBuf>) -> StoreError {
    StoreError::Upgrade {
        failure: UpgradeFailure::Moved,
        backup,
    }
}

fn failed(failure: UpgradeFailure, backup: Option<&Path>) -> StoreError {
    StoreError::Upgrade {
        failure,
        backup: backup.map(Path::to_path_buf),
    }
}

/// What the sealed log of a closed store says, read from its segment files with no lock and no
/// writer: the last sealed `meta/store_features` and `meta/upgrade` values and the last valid
/// batch marker's hash.
struct Sealed {
    features: Option<Vec<u8>>,
    receipt: Option<Vec<u8>>,
    terminal: Option<Hash>,
}

/// Reads [`Sealed`] from the segments in `dir`, by the rule `segment::sealed_ref_value` reads
/// one ref: frame by frame by its header, a ref's payload read and hash-checked, a batch counted
/// only at a marker that closes it, the walk ending at the first frame that does not fit its
/// file or the first marker that does not close its batch. Nothing is written.
fn sealed(dir: &Path) -> io::Result<Sealed> {
    let longest = format::REF_STORE_FEATURES
        .len()
        .max(format::REF_UPGRADE.len());
    let max_frame =
        (format::MAX_MANIFEST_BYTES.max(format::MAX_RECEIPT_BYTES) + longest + 64) as u64;
    let mut out = Sealed {
        features: None,
        receipt: None,
        terminal: None,
    };
    let mut seg = 0u32;
    'segments: loop {
        let f = match File::open(segment::seg_path(dir, seg)) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => break,
            Err(e) => return Err(e),
        };
        let file_len = f.metadata()?.len();
        let mut offset = 0u64;
        let mut since: Vec<(u64, Hash)> = vec![];
        let (mut features, mut receipt) = (None, None);
        while let Some((len, kind, hash)) = segment::header_at(&f, offset, file_len)? {
            let read = |n: u64| -> io::Result<Option<Vec<u8>>> {
                let mut p = vec![0u8; n as usize];
                crate::pio::read_exact_at(&f, &mut p, offset + HEADER)?;
                Ok((Hash::of(&p) == hash).then_some(p))
            };
            if kind == kinds::BATCH_COMMIT {
                let closes = u64::from(len) <= MARKER_MAX
                    && read(u64::from(len))?
                        .is_some_and(|p| segment::marker_matches(seg, &since, &p));
                if !closes {
                    break 'segments;
                }
                if let Some(v) = features.take() {
                    out.features = Some(v);
                }
                if let Some(v) = receipt.take() {
                    out.receipt = Some(v);
                }
                out.terminal = Some(hash);
                since.clear();
            } else {
                if kind == kinds::REF_UPDATE && u64::from(len) <= max_frame {
                    let Some(p) = read(u64::from(len))? else {
                        break 'segments;
                    };
                    if let Ok(u) = postcard::from_bytes::<kinds::RefUpdate>(&p) {
                        if u.name == format::REF_STORE_FEATURES {
                            features = Some(u.value);
                        } else if u.name == format::REF_UPGRADE {
                            receipt = Some(u.value);
                        }
                    }
                }
                since.push((offset, hash));
            }
            offset += HEADER + u64::from(len);
        }
        seg += 1;
    }
    Ok(out)
}

/// The store directory's `FORMAT` mirror, with the refusals of `Store::open` for a path that is
/// no store.
fn mirror_of(dir: &Path) -> Result<StoreFeatures, StoreError> {
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_dir() => {}
        Ok(_) => return Err(format::other_build(dir, "is not a directory")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(format::no_store(dir)),
        Err(e) => return Err(e.into()),
    }
    format::read_format_file(dir)?.ok_or_else(|| format::other_build(dir, "has no FORMAT file"))
}

/// The plan for the store at `dir` with `registry`, and the last valid batch marker's hash.
fn inspect_with(
    dir: &Path,
    registry: &[UpgradeStep],
) -> Result<(UpgradePlan, Option<Hash>), StoreError> {
    let mirror = mirror_of(dir)?;
    let log = sealed(&dir.join("segments"))?;
    let receipt = log
        .receipt
        .as_deref()
        .and_then(|v| UpgradeReceipt::parse(v).ok());
    // a store whose first batch never sealed has no manifest in its log yet: its open writes
    // the one FORMAT mirrors, and there is nothing to plan before that
    let Some(value) = log.features else {
        if log.terminal.is_some() {
            return Err(format::other_build(dir, "has a log with no store features"));
        }
        format::check_features(&mirror)?;
        return Ok((no_steps(mirror, false, receipt), log.terminal));
    };
    let from = format::decode_features(&value)?;
    let pending = if mirror == from || from.is_subset_of(&mirror) {
        // the mirror, or a first use whose batch never sealed (the open puts FORMAT back)
        false
    } else if receipt
        .as_ref()
        .is_some_and(|r| r.from == mirror.hash() && r.to == from.hash())
    {
        true
    } else {
        return Err(format::unmirrored(dir));
    };
    format::check_features(&from)?;
    if pending {
        // the sealed upgrade is completed first: no second batch is planned on top of it
        return Ok((no_steps(from, true, receipt), log.terminal));
    }
    let (steps, to) = plan_steps(&from, registry)?;
    if steps.is_empty() {
        return Ok((no_steps(from, false, receipt), log.terminal));
    }
    format::check_features(&to)?;
    let backup_bytes = list_store(dir)?.bytes;
    let plan = UpgradePlan {
        from,
        to,
        steps,
        backup_bytes,
        requires_upgrade: true,
        pending_check: false,
        receipt,
    };
    Ok((plan, log.terminal))
}

/// The plan with no steps from `from`.
fn no_steps(from: StoreFeatures, pending: bool, receipt: Option<UpgradeReceipt>) -> UpgradePlan {
    UpgradePlan {
        to: from.clone(),
        from,
        steps: vec![],
        backup_bytes: 0,
        requires_upgrade: false,
        pending_check: pending,
        receipt,
    }
}

/// The refusal of an invalid step registry.
fn bad_registry(why: String) -> StoreError {
    StoreError::Refused(format!("the upgrade step registry is invalid: {why}"))
}

/// The steps of `registry` that apply from `from`, in dependency order, then by step id, and the
/// manifest they make. A step applies to a manifest that holds every feature of its `from` and
/// not yet every feature of its `to`; it replaces its `from`'s features with its `to`'s. Each
/// step runs at most once.
fn plan_steps(
    from: &StoreFeatures,
    registry: &[UpgradeStep],
) -> Result<(Vec<String>, StoreFeatures), StoreError> {
    if registry.len() > MAX_STEPS {
        return Err(bad_registry(format!("more than {MAX_STEPS} steps")));
    }
    let mut ids = BTreeSet::new();
    for s in registry {
        if !format::valid_feature_name(&s.id) || !ids.insert(s.id.as_str()) {
            return Err(bad_registry(format!("step {:?} is not a valid id", s.id)));
        }
        for m in [&s.from, &s.to] {
            m.validate()
                .map_err(|e| bad_registry(format!("step {}: {e}", s.id)))?;
        }
    }
    let mut current = from.clone();
    let mut done = vec![false; registry.len()];
    let mut steps = vec![];
    loop {
        let next = registry
            .iter()
            .enumerate()
            .filter(|(i, s)| {
                !done[*i] && s.from.is_subset_of(&current) && !s.to.is_subset_of(&current)
            })
            .min_by(|(_, a), (_, b)| a.id.cmp(&b.id));
        let Some((i, s)) = next else {
            break;
        };
        done[i] = true;
        let mut features: Vec<StoreFeature> = current
            .features
            .iter()
            .filter(|f| !s.from.features.contains(f))
            .cloned()
            .collect();
        for t in &s.to.features {
            features.retain(|f| f.name != t.name);
            features.push(t.clone());
        }
        current = StoreFeatures {
            manifest_version: current.manifest_version,
            ..StoreFeatures::of(features)
        };
        current
            .validate_written()
            .map_err(|e| bad_registry(format!("step {} makes an invalid manifest: {e}", s.id)))?;
        steps.push(s.id.clone());
    }
    Ok((steps, current))
}

fn apply_with(
    dir: &Path,
    lock: &StoreLock,
    plan: &UpgradePlan,
    registry: &[UpgradeStep],
) -> Result<UpgradeReport, StoreError> {
    if !lock.guards(dir) {
        return Err(StoreError::Locked(dir.to_path_buf()));
    }
    let (now, terminal) = inspect_with(dir, registry)?;
    if now.from != plan.from
        || now.to != plan.to
        || now.steps != plan.steps
        || now.pending_check != plan.pending_check
    {
        return Err(moved(None));
    }
    if now.steps.is_empty() {
        return Ok(UpgradeReport {
            dry_run: false,
            changed: false,
            to: now.from.clone(),
            from: now.from,
            steps: vec![],
            backup: None,
            batch: None,
            reindexed: false,
            checked: false,
            receipt: now.receipt,
        });
    }
    let terminal = terminal.ok_or_else(|| moved(None))?;
    let name = format!("{BACKUP_PREFIX}{}-{terminal}", now.from.hash());
    let parent = dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let backup = parent.join(&name);
    let listing = list_store(dir)?;
    make_backup(dir, &backup, &listing, &now.from, terminal)?;
    hit(Phase::BackupComplete)?;

    let mut store = Store::open_unupgraded_locked(dir, lock)?;
    hit(Phase::Opened(&mut store))?;
    if store.features() != &now.from {
        return Err(moved(Some(backup)));
    }
    let receipt = UpgradeReceipt {
        from: now.from.hash(),
        to: now.to.hash(),
        steps: now.steps.clone(),
        source_end: store.log_end(),
        backup: name,
    };
    // the store's own rollback undoes a failure before the seal; the backup stays
    let batch = store.commit_upgrade_refs(&now.to, &receipt.to_json())?;
    hit(Phase::Sealed(&mut store))?;
    store.reindex()?;
    hit(Phase::Reindexed(&mut store))?;
    let checked = store.mode().and_then(|mode| match mode {
        StoreMode::Holder => store.check_holder(),
        StoreMode::Project => store.check_objects(),
    });
    if checked.is_err() {
        drop(store);
        return Err(failed(UpgradeFailure::CheckFailed, Some(&backup)));
    }
    hit(Phase::Checked)?;
    store.close()?;
    Ok(UpgradeReport {
        dry_run: false,
        changed: true,
        from: now.from,
        to: now.to,
        steps: now.steps,
        backup: Some(backup),
        batch: Some(batch),
        reindexed: true,
        checked: false,
        receipt: Some(receipt),
    })
}

/// The entries a backup of a store copies: its directories and its regular files (with their
/// lengths) by relative path, in path order, and the sum of the files' lengths.
#[derive(Debug, Default)]
struct Listing {
    dirs: Vec<String>,
    files: Vec<(String, u64)>,
    bytes: u64,
}

/// Adds `len` to `total`, or the limit refusal when the sum does not fit a u64.
fn add_len(total: u64, len: u64) -> Result<u64, StoreError> {
    total
        .checked_add(len)
        .ok_or_else(|| failed(UpgradeFailure::Limit, None))
}

/// A relative path joined from `prefix` and `name`, within the path bound; `name` must be one
/// normal component. A name that is not UTF-8 is an I/O error.
fn relative(prefix: &str, name: &std::ffi::OsStr, path: &Path) -> Result<String, StoreError> {
    let name = name.to_str().ok_or_else(|| {
        StoreError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a UTF-8 file name", path.display()),
        ))
    })?;
    if !is_component(name) {
        return Err(failed(UpgradeFailure::BackupFileType, None));
    }
    let len = prefix.len() + usize::from(!prefix.is_empty()) + name.len();
    if len > LIMITS.with(|l| l.get().1) {
        return Err(failed(UpgradeFailure::Limit, None));
    }
    let mut rel = String::new();
    rel.try_reserve_exact(len)
        .map_err(|_| failed(UpgradeFailure::Limit, None))?;
    if !prefix.is_empty() {
        rel.push_str(prefix);
        rel.push('/');
    }
    rel.push_str(name);
    Ok(rel)
}

/// Whether `name` is one normal path component: not empty, `.` or `..`, with no separator.
fn is_component(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0'])
}

/// Lists the store at `dir` for its backup: every directory and regular file under it, but the
/// lock file and the open sentinel at its top. A link or any other kind of file is refused, and
/// so is a listing past its entry or path bound. Reads only.
fn list_store(dir: &Path) -> Result<Listing, StoreError> {
    list_tree(dir, &[LOCK_FILE, INDEX_OPEN_SENTINEL])
}

/// Lists the tree at `root` ([`list_store`]), leaving out the top-level names `skip`.
fn list_tree(root: &Path, skip: &[&str]) -> Result<Listing, StoreError> {
    let max_entries = LIMITS.with(|l| l.get().0);
    let mut out = Listing::default();
    let mut entries = 0usize;
    let mut todo: Vec<String> = vec![String::new()];
    while let Some(prefix) = todo.pop() {
        let here = if prefix.is_empty() {
            root.to_path_buf()
        } else {
            root.join(&prefix)
        };
        for e in std::fs::read_dir(&here)? {
            let e = e?;
            let path = e.path();
            if prefix.is_empty() && skip.iter().any(|s| e.file_name() == *s) {
                continue;
            }
            entries += 1;
            if entries > max_entries {
                return Err(failed(UpgradeFailure::Limit, None));
            }
            let rel = relative(&prefix, &e.file_name(), &path)?;
            let m = std::fs::symlink_metadata(&path)?;
            let t = m.file_type();
            if t.is_dir() {
                out.dirs
                    .try_reserve(1)
                    .and_then(|()| todo.try_reserve(1))
                    .map_err(|_| failed(UpgradeFailure::Limit, None))?;
                out.dirs.push(rel.clone());
                todo.push(rel);
            } else if t.is_file() {
                out.bytes = add_len(out.bytes, m.len())?;
                out.files
                    .try_reserve(1)
                    .map_err(|_| failed(UpgradeFailure::Limit, None))?;
                out.files.push((rel, m.len()));
            } else {
                return Err(failed(UpgradeFailure::BackupFileType, None));
            }
        }
    }
    out.dirs.sort_unstable();
    out.files.sort_unstable();
    Ok(out)
}

/// `backup.json`: the store the backup is of (its manifest's hash and its last batch marker),
/// then its directories and files by relative path, in path order, each file with its length
/// and BLAKE3 digest.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BackupManifest {
    source_manifest: String,
    terminal_batch: String,
    dirs: Vec<String>,
    files: Vec<BackupFile>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BackupFile {
    path: String,
    len: u64,
    blake3: String,
}

/// A zeroed copy buffer of [`COPY_BUFFER`] bytes, allocated fallibly.
fn copy_buffer() -> Result<Vec<u8>, StoreError> {
    let mut buf = Vec::new();
    buf.try_reserve_exact(COPY_BUFFER)
        .map_err(|_| failed(UpgradeFailure::Limit, None))?;
    buf.resize(COPY_BUFFER, 0);
    Ok(buf)
}

/// Streams `src` through `buf`, into `dst` when there is one; returns the bytes read and their
/// BLAKE3 digest.
fn stream(src: &mut File, mut dst: Option<&mut File>, buf: &mut [u8]) -> io::Result<(u64, Hash)> {
    let mut hasher = blake3::Hasher::new();
    let mut n = 0u64;
    loop {
        let got = match src.read(buf) {
            Ok(0) => break,
            Ok(got) => got,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        hasher.update(&buf[..got]);
        if let Some(d) = dst.as_deref_mut() {
            d.write_all(&buf[..got])?;
        }
        n += got as u64;
    }
    Ok((n, Hash(*hasher.finalize().as_bytes())))
}

/// Makes `backup` the complete backup of the closed store at `dir` (`listing`). A new backup is
/// created with create-new semantics, each file copied and fsynced, each directory fsynced, and
/// `backup.json` written last and fsynced with its parent. An existing backup is used only when
/// its `backup.json` is this store's and every file it lists has its recorded length and digest;
/// otherwise it is `E_UPGRADE_BACKUP`, and nothing in it changes.
fn make_backup(
    dir: &Path,
    backup: &Path,
    listing: &Listing,
    from: &StoreFeatures,
    terminal: Hash,
) -> Result<(), StoreError> {
    let parent = backup.parent().unwrap_or(Path::new("."));
    match std::fs::create_dir(backup) {
        Ok(()) => fsutil::sync_dir(parent)?,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return reuse_backup(dir, backup, listing, from, terminal);
        }
        Err(e) => return Err(e.into()),
    }
    let mut buf = copy_buffer()?;
    for d in &listing.dirs {
        std::fs::create_dir(backup.join(d))?;
    }
    let mut files = Vec::new();
    files
        .try_reserve_exact(listing.files.len())
        .map_err(|_| failed(UpgradeFailure::Limit, Some(backup)))?;
    for (rel, len) in &listing.files {
        hit(Phase::Copy(rel))?;
        let mut src = File::open(dir.join(rel))?;
        let to = backup.join(rel);
        let mut dst = OpenOptions::new().write(true).create_new(true).open(&to)?;
        let (n, digest) = stream(&mut src, Some(&mut dst), &mut buf)?;
        if n != *len {
            return Err(failed(UpgradeFailure::BackupMismatch, Some(backup)));
        }
        hit(Phase::Sync(rel))?;
        dst.sync_all()?;
        fsutil::record(&to);
        files.push(BackupFile {
            path: rel.clone(),
            len: n,
            blake3: digest.to_string(),
        });
    }
    // the deepest directories first, then the backup itself
    for d in listing.dirs.iter().rev() {
        hit(Phase::Sync(d))?;
        fsutil::sync_dir(&backup.join(d))?;
    }
    hit(Phase::Sync(""))?;
    fsutil::sync_dir(backup)?;
    let manifest = BackupManifest {
        source_manifest: from.hash().to_string(),
        terminal_batch: terminal.to_string(),
        dirs: listing.dirs.clone(),
        files,
    };
    let json = serde_json::to_vec(&manifest).map_err(|e| StoreError::Decode(e.to_string()))?;
    fsutil::write_durable(&backup.join(BACKUP_MANIFEST), &json)?;
    fsutil::sync_dir(parent)?;
    Ok(())
}

/// Uses the existing `backup` when it is complete and matches the store ([`make_backup`]).
fn reuse_backup(
    dir: &Path,
    backup: &Path,
    listing: &Listing,
    from: &StoreFeatures,
    terminal: Hash,
) -> Result<(), StoreError> {
    let mismatch = || failed(UpgradeFailure::BackupMismatch, Some(backup));
    if !std::fs::symlink_metadata(backup)?.file_type().is_dir() {
        return Err(mismatch());
    }
    // what backup.json must say: this store's files as they are now
    let mut buf = copy_buffer()?;
    let mut files = Vec::new();
    files
        .try_reserve_exact(listing.files.len())
        .map_err(|_| failed(UpgradeFailure::Limit, Some(backup)))?;
    for (rel, _) in &listing.files {
        let (len, digest) = stream(&mut File::open(dir.join(rel))?, None, &mut buf)?;
        files.push(BackupFile {
            path: rel.clone(),
            len,
            blake3: digest.to_string(),
        });
    }
    let expected = BackupManifest {
        source_manifest: from.hash().to_string(),
        terminal_batch: terminal.to_string(),
        dirs: listing.dirs.clone(),
        files,
    };
    let want = serde_json::to_vec(&expected).map_err(|e| StoreError::Decode(e.to_string()))?;
    // read at most one byte more than the expected text
    let mut got = Vec::new();
    match File::open(backup.join(BACKUP_MANIFEST)) {
        Ok(f) => {
            got.try_reserve_exact(want.len() + 1)
                .map_err(|_| failed(UpgradeFailure::Limit, Some(backup)))?;
            f.take(want.len() as u64 + 1).read_to_end(&mut got)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(mismatch()),
        Err(e) => return Err(e.into()),
    }
    if got != want {
        return Err(mismatch());
    }
    // the backup holds exactly what backup.json lists, each file with its recorded digest
    let held = match list_tree(backup, &[BACKUP_MANIFEST]) {
        Ok(held) => held,
        Err(StoreError::Upgrade { .. }) => return Err(mismatch()),
        Err(e) => return Err(e),
    };
    let listed: Vec<(String, u64)> = expected
        .files
        .iter()
        .map(|f| (f.path.clone(), f.len))
        .collect();
    if held.dirs != expected.dirs || held.files != listed {
        return Err(mismatch());
    }
    for f in &expected.files {
        let (len, digest) = stream(&mut File::open(backup.join(&f.path))?, None, &mut buf)?;
        if len != f.len || digest.to_string() != f.blake3 {
            return Err(mismatch());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok5_backup_byte_sums_refuse_overflow() {
        assert_eq!(add_len(1, 2).unwrap(), 3);
        assert_eq!(add_len(u64::MAX - 1, 1).unwrap(), u64::MAX);
        match add_len(u64::MAX, 1) {
            Err(StoreError::Upgrade {
                failure: UpgradeFailure::Limit,
                backup: None,
            }) => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ok5_relative_paths_have_only_normal_components() {
        for bad in ["", ".", "..", "a/b", "a\\b", "a\0b"] {
            assert!(!is_component(bad), "{bad:?}");
        }
        let p = Path::new("x");
        assert_eq!(relative("", "FORMAT".as_ref(), p).unwrap(), "FORMAT");
        assert_eq!(relative("a", "b".as_ref(), p).unwrap(), "a/b");
        // the path bound counts the joined path's bytes
        with_backup_limits_for_tests(10, 3, || {
            assert_eq!(relative("a", "b".as_ref(), p).unwrap(), "a/b");
            assert!(matches!(
                relative("a", "bc".as_ref(), p),
                Err(StoreError::Upgrade {
                    failure: UpgradeFailure::Limit,
                    ..
                })
            ));
        });
    }

    #[test]
    fn ok5_steps_follow_dependencies_then_ids() {
        let f = |name: &str| StoreFeature {
            name: name.into(),
            class: format::FeatureClass::Compatible,
            required_rdy: "99.0.0".into(),
        };
        let m = |names: &[&str]| StoreFeatures::of(names.iter().map(|n| f(n)).collect());
        let step = |id: &str, from: &[&str], to: &[&str]| UpgradeStep {
            id: id.into(),
            from: m(from),
            to: m(to),
        };
        let registry = [
            step("alpha", &["zz_b"], &["zz_b", "zz_c"]),
            step("zeta", &[], &["zz_b"]),
            step("beta", &[], &["zz_a"]),
            // never applies: it needs a feature no step makes
            step("gamma", &["zz_x"], &["zz_y"]),
        ];
        let (steps, to) = plan_steps(&m(&[]), &registry).unwrap();
        assert_eq!(steps, ["beta", "zeta", "alpha"]);
        assert_eq!(to, m(&["zz_a", "zz_b", "zz_c"]));
        // nothing applies to the manifest they make
        let (again, same) = plan_steps(&to, &registry).unwrap();
        assert!(again.is_empty());
        assert_eq!(same, to);
        // the registry's bounds
        let many: Vec<UpgradeStep> = (0..=MAX_STEPS)
            .map(|i| step(&format!("s{i}"), &[], &[]))
            .collect();
        assert!(matches!(
            plan_steps(&m(&[]), &many),
            Err(StoreError::Refused(_))
        ));
        for id in ["", "Bad", "1a", "a-b"] {
            assert!(
                matches!(
                    plan_steps(&m(&[]), &[step(id, &[], &["zz_a"])]),
                    Err(StoreError::Refused(_))
                ),
                "{id:?}"
            );
        }
        let twice = [step("a", &[], &["zz_a"]), step("a", &[], &["zz_b"])];
        assert!(matches!(
            plan_steps(&m(&[]), &twice),
            Err(StoreError::Refused(_))
        ));
    }
}
