//! The store upgrade (D-WWAK7): one logged batch that moves a store's feature manifest, with a
//! complete backup first. The backup is the only rollback.
//!
//! [`inspect`] plans an upgrade from the closed store's files: the `FORMAT` mirror, the sealed
//! log's `meta/store_features` and its last `meta/upgrade` receipt. It opens no store, takes or
//! creates no lock, and writes nothing. [`apply_locked`] runs the plan under the caller's lock:
//!
//! 1. it reads the store again and requires the plan it was given;
//! 2. an empty plan returns at once, with no backup and no batch;
//! 3. it copies the closed store to the sibling `.store-upgrade-<manifest hash>-<batch hash>`:
//!    the backup is private to this user before any byte is copied, its `store` directory holds
//!    the copy (every regular file but the lock and the open sentinel; a link or a special file
//!    is refused), every file and directory is fsynced, and `backup.json` is written last,
//!    beside the copy. Every step names an entry of an open directory and follows no link, and
//!    a backup or store path that was moved or replaced fails the upgrade;
//! 4. it opens the store with no upgrade policy and appends one marked batch: the new
//!    `meta/store_features` and the `meta/upgrade` receipt;
//! 5. it rebuilds the index and runs the project's or the holder's store check.
//!
//! `FORMAT` still mirrors the old manifest then: the engine runs its full fsck, then
//! [`publish_checked`] writes the new mirror. A later inspect of a store whose mirror lags the
//! receipt reports `pending_check`, and its plan is empty, so no second batch is written.
//! Nothing here restores a backup: a person does that, after stopping the daemon.
use crate::error::{StoreError, UpgradeFailure};
use crate::fileid::FileId;
use crate::format::{self, StoreFeature, StoreFeatures, UpgradeReceipt};
use crate::holder::StoreMode;
use crate::pio::{self, EntryKind};
use crate::segment::{self, HEADER, MARKER_MAX};
use crate::store::INDEX_OPEN_SENTINEL;
use crate::{Store, StoreLock, fsutil, kinds};
use rdy_model::Hash;
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// The most steps one registry, and so one plan, holds.
pub const MAX_STEPS: usize = 256;
/// The most entries (files and directories) a backup holds.
pub const MAX_BACKUP_ENTRIES: usize = 1_000_000;
/// The most bytes of one relative path in a backup.
pub const MAX_BACKUP_PATH_BYTES: usize = 1_048_576;
/// The file a complete backup writes last, at its top.
pub const BACKUP_MANIFEST: &str = "backup.json";
/// The directory at a backup's top that holds the copy of the store, beside [`BACKUP_MANIFEST`]:
/// so no store file can take the manifest's name or its temp file's.
pub const BACKUP_STORE: &str = "store";
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
    /// A dry run's report of a store whose sealed upgrade still waits for its completion (the
    /// full fsck, then `FORMAT`); false on every other report.
    pub pending: bool,
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

/// The refusal (E_STORE_FORMAT) of a sealed upgrade to `features` that this rdy cannot complete:
/// a newer rdy started it on a store with a read-only feature this rdy does not know. The text
/// is the read-only open's (`Store::open` refuses the same store with it).
pub fn unfinished_upgrade(features: &StoreFeatures) -> StoreError {
    crate::store::unfinished_upgrade(features)
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
            pending: false,
        });
    }
    // a reader of this process that outlived its handle still holds the index: refuse before the
    // backup copies a live index file (a copy the store would no longer match once it is
    // released, which would block the retry)
    crate::index::check_not_held(&dir.join("index.redb")).map_err(|e| match e {
        StoreError::HeldByReader(_) => StoreError::HeldByReader(dir.to_path_buf()),
        e => e,
    })?;
    let terminal = terminal.ok_or_else(|| moved(None))?;
    let name = format!("{BACKUP_PREFIX}{}-{terminal}", now.from.hash());
    let (parent_path, store_name) = split_store_path(dir)?;
    let backup = parent_path.join(&name);
    // every backup step names an entry of one of these handles, never a path
    let parent = pio::open_dir(parent_path)?;
    let source = Source {
        dir,
        handle: pio::open_dir_at(&parent, store_name)?,
    };
    if FileId::lstat(dir)? != FileId::of_file(&source.handle)? {
        return Err(moved(None));
    }
    let listing = list_tree(
        &source.handle,
        &[LOCK_FILE, INDEX_OPEN_SENTINEL],
        |_, _, _| Ok(()),
    )?;
    let target = Target {
        parent: &parent,
        parent_path,
        path: &backup,
        name: name.as_ref(),
    };
    make_backup(&source, &target, &listing, &now.from, terminal)?;
    drop(parent);
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
    // the store's own rollback undoes a failure before the marker's sync; from the sync on, the
    // batch stays (the store rebuilds its index from the log) and so does the backup
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
        pending: false,
    })
}

/// The parent directory of the store directory `dir` and the store directory's name there.
fn split_store_path(dir: &Path) -> io::Result<(&Path, &OsStr)> {
    let name = dir.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} does not end in a store directory name", dir.display()),
        )
    })?;
    let parent = dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok((parent, name))
}

/// The store a backup copies: its path, and its directory opened in its parent.
struct Source<'a> {
    dir: &'a Path,
    handle: File,
}

/// Where a backup goes: the store's parent directory (open, and its path), and the backup's
/// path and its name in that parent.
struct Target<'a> {
    parent: &'a File,
    parent_path: &'a Path,
    path: &'a Path,
    name: &'a OsStr,
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
fn relative(prefix: &str, name: &OsStr) -> Result<String, StoreError> {
    let name = name.to_str().ok_or_else(|| {
        StoreError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{name:?} is not a UTF-8 file name"),
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

/// The relative path `rel` split at its last component: its directory (`""` for the top) and
/// its name.
fn split_rel(rel: &str) -> (&str, &OsStr) {
    let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel));
    (dir, OsStr::new(name))
}

/// The refusal of a link or a special file where a directory or a regular file belongs, or
/// `e` itself.
fn file_type_error(e: io::Error) -> StoreError {
    if pio::is_link_refusal(&e) {
        failed(UpgradeFailure::BackupFileType, None)
    } else {
        e.into()
    }
}

/// The directory at the relative path `rel` under the open directory `root` (`""` is `root`),
/// opened component by component, never through a link.
fn open_rel_dir(root: &File, rel: &str) -> Result<File, StoreError> {
    let mut at = root.try_clone()?;
    if rel.is_empty() {
        return Ok(at);
    }
    for part in rel.split('/') {
        at = pio::open_dir_at(&at, OsStr::new(part)).map_err(file_type_error)?;
    }
    Ok(at)
}

/// The regular file `name` of the open directory `dir`, opened for reading, never through a
/// link; anything else is the file-type refusal.
fn open_file_at(dir: &File, name: &OsStr) -> Result<File, StoreError> {
    let f = pio::open_read_at(dir, name).map_err(file_type_error)?;
    if !f.metadata()?.is_file() {
        return Err(failed(UpgradeFailure::BackupFileType, None));
    }
    Ok(f)
}

/// Lists the store at `dir` for its backup ([`list_tree`], leaving out the lock file and the
/// open sentinel at its top). Reads only.
fn list_store(dir: &Path) -> Result<Listing, StoreError> {
    let root = pio::open_dir(dir)?;
    list_tree(&root, &[LOCK_FILE, INDEX_OPEN_SENTINEL], |_, _, _| Ok(()))
}

/// Lists the tree under the open directory `root`: every directory and regular file under it,
/// but the top-level names `skip`, each read through a handle and never through a link. A link
/// or any other kind of file is refused, and so is a listing past its entry or path bound.
/// `visit` is called with each directory's and file's relative path, its open handle (for
/// reading) and its metadata. Reads only.
fn list_tree(
    root: &File,
    skip: &[&str],
    mut visit: impl FnMut(&str, &File, &std::fs::Metadata) -> Result<(), StoreError>,
) -> Result<Listing, StoreError> {
    let max_entries = LIMITS.with(|l| l.get().0);
    let mut out = Listing::default();
    let mut entries = 0usize;
    let mut todo: Vec<String> = vec![String::new()];
    while let Some(prefix) = todo.pop() {
        let here = open_rel_dir(root, &prefix)?;
        let mut names: Vec<OsString> = Vec::new();
        pio::for_each_entry(&here, |name| {
            if prefix.is_empty() && skip.iter().any(|s| name == *s) {
                return Ok(());
            }
            entries += 1;
            if entries > max_entries {
                return Err(failed(UpgradeFailure::Limit, None));
            }
            names
                .try_reserve(1)
                .map_err(|_| failed(UpgradeFailure::Limit, None))?;
            names.push(name.to_os_string());
            Ok(())
        })?;
        for name in names {
            let rel = relative(&prefix, &name)?;
            match pio::entry_kind_at(&here, &name)? {
                EntryKind::Dir => {
                    let d = pio::open_dir_at(&here, &name).map_err(file_type_error)?;
                    visit(&rel, &d, &d.metadata()?)?;
                    out.dirs
                        .try_reserve(1)
                        .and_then(|()| todo.try_reserve(1))
                        .map_err(|_| failed(UpgradeFailure::Limit, None))?;
                    out.dirs.push(rel.clone());
                    todo.push(rel);
                }
                EntryKind::File => {
                    let f = open_file_at(&here, &name)?;
                    let m = f.metadata()?;
                    visit(&rel, &f, &m)?;
                    out.bytes = add_len(out.bytes, m.len())?;
                    out.files
                        .try_reserve(1)
                        .map_err(|_| failed(UpgradeFailure::Limit, None))?;
                    out.files.push((rel, m.len()));
                }
                EntryKind::Link | EntryKind::Other => {
                    return Err(failed(UpgradeFailure::BackupFileType, None));
                }
            }
        }
    }
    out.dirs.sort_unstable();
    out.files.sort_unstable();
    Ok(out)
}

/// `backup.json`: the store the backup is of (its manifest's hash and its last batch marker),
/// then its directories and files by relative path in the store, in path order, each file with
/// its length and BLAKE3 digest. The copies are in the backup's [`BACKUP_STORE`] directory.
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
fn stream(
    mut src: impl Read,
    mut dst: Option<&mut File>,
    buf: &mut [u8],
) -> io::Result<(u64, Hash)> {
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

/// `e`, naming `backup` when it is an upgrade failure that names none yet.
fn naming(e: StoreError, backup: &Path) -> StoreError {
    match e {
        StoreError::Upgrade {
            failure,
            backup: None,
        } => failed(failure, Some(backup)),
        e => e,
    }
}

/// Whether `meta` is owned by this user, with no permission bit for its group or others.
fn is_owned_private(meta: &std::fs::Metadata) -> bool {
    pio::owner(meta) == pio::own_uid() && pio::is_owner_only(meta)
}

/// The mode of a backup's copy of a file whose metadata is `source`: its owner may read it, and
/// write it only when the source's owner may; its group and others get nothing. So a copy is
/// never more open than its source.
fn copy_mode(source: &std::fs::Metadata) -> u32 {
    0o400 | (pio::mode_of(source) & 0o200)
}

/// Creates the private directory `name` in the open directory `dir` (mode 0o700 whatever the
/// umask) and opens it, never through a link. The open must find an empty directory of this
/// user's with that mode, or it is not the one made here.
fn make_private_dir_at(dir: &File, name: &OsStr) -> Result<File, StoreError> {
    pio::create_private_dir_at(dir, name)?;
    open_made_dir(dir, name)
}

/// Opens the directory `name` just created in the open directory `dir`
/// ([`make_private_dir_at`]), with its mode set to 0o700 first.
fn open_made_dir(dir: &File, name: &OsStr) -> Result<File, StoreError> {
    pio::set_mode_at(dir, name, 0o700)?;
    let made = pio::open_dir_at(dir, name).map_err(file_type_error)?;
    let m = made.metadata()?;
    if !(pio::is_private_dir(&m) && pio::owner(&m) == pio::own_uid() && pio::is_empty_dir(&made)?) {
        return Err(failed(UpgradeFailure::BackupMismatch, None));
    }
    Ok(made)
}

/// Whether the backup and the store are still at their paths: the backup's path names the
/// backup directory this upgrade wrote or read (`root`), that directory's [`BACKUP_STORE`]
/// entry is the copy directory it wrote or read (`copy`), a directory and never a link, and
/// the store's path names the store it copied. A path or an entry that was moved or replaced,
/// by a link or another directory, fails the upgrade before anything is appended.
fn in_place(
    source: &Source<'_>,
    target: &Target<'_>,
    root: &File,
    copy: &File,
) -> Result<(), StoreError> {
    let mismatch = || failed(UpgradeFailure::BackupMismatch, None);
    if FileId::lstat(target.path).ok() != Some(FileId::of_file(root)?) {
        return Err(mismatch());
    }
    // the copy is read through the backup's handle, so its entry there is the one checked
    let entry = match pio::open_dir_at(root, BACKUP_STORE.as_ref()) {
        Ok(entry) => entry,
        Err(e) if e.kind() == io::ErrorKind::NotFound || pio::is_link_refusal(&e) => {
            return Err(mismatch());
        }
        Err(e) => return Err(e.into()),
    };
    if FileId::of_file(&entry)? != FileId::of_file(copy)? {
        return Err(mismatch());
    }
    if FileId::lstat(source.dir).ok() != Some(FileId::of_file(&source.handle)?) {
        return Err(moved(None));
    }
    Ok(())
}

/// Makes the target the complete backup of the closed store `source` (`listing`). A new backup
/// is created with create-new semantics, private to this user before any byte is copied, with
/// the store's copy in its [`BACKUP_STORE`] directory: each directory and file is created new,
/// each file copied, fsynced and never more open than its source, each directory fsynced, and
/// `backup.json` written last beside the copy, then fsynced with the backup and its parent.
/// Every step names an entry of an open directory, never through a link. An existing backup is
/// used only when it passes [`reuse_backup`]; otherwise it is `E_UPGRADE_BACKUP`, and nothing in
/// it changes.
fn make_backup(
    source: &Source<'_>,
    target: &Target<'_>,
    listing: &Listing,
    from: &StoreFeatures,
    terminal: Hash,
) -> Result<(), StoreError> {
    match pio::create_private_dir_at(target.parent, target.name) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return reuse_backup(source, target, listing, from, terminal)
                .map_err(|e| naming(e, target.path));
        }
        Err(e) => return Err(e.into()),
    }
    write_backup(source, target, listing, from, terminal).map_err(|e| naming(e, target.path))
}

/// The new backup's steps ([`make_backup`]); its directory exists, empty.
fn write_backup(
    source: &Source<'_>,
    target: &Target<'_>,
    listing: &Listing,
    from: &StoreFeatures,
    terminal: Hash,
) -> Result<(), StoreError> {
    target.parent.sync_all()?;
    fsutil::record(target.parent_path);
    let root = open_made_dir(target.parent, target.name)?;
    let copy = make_private_dir_at(&root, BACKUP_STORE.as_ref())?;
    let copy_path = target.path.join(BACKUP_STORE);
    for d in &listing.dirs {
        let (at, name) = split_rel(d);
        make_private_dir_at(&open_rel_dir(&copy, at)?, name)?;
    }
    let mut buf = copy_buffer()?;
    let mut files = Vec::new();
    files
        .try_reserve_exact(listing.files.len())
        .map_err(|_| failed(UpgradeFailure::Limit, None))?;
    for (rel, len) in &listing.files {
        hit(Phase::Copy(rel))?;
        let (at, name) = split_rel(rel);
        let src = open_file_at(&open_rel_dir(&source.handle, at)?, name)?;
        let mode = copy_mode(&src.metadata()?);
        let mut dst = pio::create_private_at(&open_rel_dir(&copy, at)?, name)?;
        dst.set_permissions(pio::permissions(mode))?;
        let (n, digest) = stream(&src, Some(&mut dst), &mut buf)?;
        if n != *len {
            return Err(failed(UpgradeFailure::BackupMismatch, None));
        }
        hit(Phase::Sync(rel))?;
        dst.sync_all()?;
        fsutil::record(&copy_path.join(rel));
        files.push(BackupFile {
            path: rel.clone(),
            len: n,
            blake3: digest.to_string(),
        });
    }
    // the deepest directories first, then the copy and the backup itself
    for d in listing.dirs.iter().rev() {
        hit(Phase::Sync(d))?;
        open_rel_dir(&copy, d)?.sync_all()?;
        fsutil::record(&copy_path.join(d));
    }
    hit(Phase::Sync(""))?;
    copy.sync_all()?;
    fsutil::record(&copy_path);
    root.sync_all()?;
    fsutil::record(target.path);
    let manifest = BackupManifest {
        source_manifest: from.hash().to_string(),
        terminal_batch: terminal.to_string(),
        dirs: listing.dirs.clone(),
        files,
    };
    let json = serde_json::to_vec(&manifest).map_err(|e| StoreError::Decode(e.to_string()))?;
    // backup.json and its temp file stand beside the copy, so no store file can take their name
    fsutil::replace_in_dir(&root, target.path, BACKUP_MANIFEST.as_ref(), &json, None)?;
    in_place(source, target, &root, &copy)?;
    target.parent.sync_all()?;
    fsutil::record(target.parent_path);
    Ok(())
}

/// Uses the existing backup ([`make_backup`]) when it is complete, private and independent of
/// the store: it holds only `backup.json` and the copy, every directory and file in it is a
/// directory or a regular file of this user's with no bit for others, `backup.json` is the text
/// this store's files give now, the copy holds exactly the files it lists with their recorded
/// lengths and digests, and no copied file is one of the store's own files (a hard link would
/// change with the active log). Nothing in the backup changes.
fn reuse_backup(
    source: &Source<'_>,
    target: &Target<'_>,
    listing: &Listing,
    from: &StoreFeatures,
    terminal: Hash,
) -> Result<(), StoreError> {
    let mismatch = || failed(UpgradeFailure::BackupMismatch, None);
    let opened = |e: io::Error| {
        if pio::is_link_refusal(&e) {
            mismatch()
        } else {
            e.into()
        }
    };
    let root = pio::open_dir_at(target.parent, target.name).map_err(opened)?;
    if !is_owned_private(&root.metadata()?) {
        return Err(mismatch());
    }
    // what backup.json must say: this store's files as they are now, and which files they are
    let mut buf = copy_buffer()?;
    let mut ids = BTreeSet::new();
    let mut files = Vec::new();
    files
        .try_reserve_exact(listing.files.len())
        .map_err(|_| failed(UpgradeFailure::Limit, None))?;
    for (rel, _) in &listing.files {
        let (at, name) = split_rel(rel);
        let src = open_file_at(&open_rel_dir(&source.handle, at)?, name)?;
        ids.insert(FileId::of_file(&src)?);
        let (len, digest) = stream(&src, None, &mut buf)?;
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
    // the backup's top holds backup.json and the copy, and nothing else
    let mut top: Vec<OsString> = Vec::new();
    pio::for_each_entry(&root, |name| {
        if top.len() == 2 {
            return Err(mismatch());
        }
        top.push(name.to_os_string());
        Ok(())
    })?;
    top.sort();
    if top != [BACKUP_MANIFEST, BACKUP_STORE] {
        return Err(mismatch());
    }
    // backup.json: a private regular file, read through no link, and at most one byte more
    // than the expected text
    let manifest = match open_file_at(&root, BACKUP_MANIFEST.as_ref()) {
        Ok(f) => f,
        Err(StoreError::Upgrade { .. }) => return Err(mismatch()),
        Err(e) => return Err(e),
    };
    if !is_owned_private(&manifest.metadata()?) {
        return Err(mismatch());
    }
    let mut got = Vec::new();
    got.try_reserve_exact(want.len() + 1)
        .map_err(|_| failed(UpgradeFailure::Limit, None))?;
    (&manifest)
        .take(want.len() as u64 + 1)
        .read_to_end(&mut got)?;
    if got != want {
        return Err(mismatch());
    }
    // the copy holds exactly what backup.json lists, each file private, independent of the
    // store's files and with its recorded digest
    let copy = pio::open_dir_at(&root, BACKUP_STORE.as_ref()).map_err(opened)?;
    if !is_owned_private(&copy.metadata()?) {
        return Err(mismatch());
    }
    let mut digests: Vec<BackupFile> = Vec::new();
    digests
        .try_reserve_exact(expected.files.len())
        .map_err(|_| failed(UpgradeFailure::Limit, None))?;
    let held = list_tree(&copy, &[], |rel, f, m| {
        if !is_owned_private(m) || ids.contains(&FileId::of(m)) {
            return Err(mismatch());
        }
        if m.is_file() {
            if digests.len() == expected.files.len() {
                return Err(mismatch());
            }
            let (len, digest) = stream(f, None, &mut buf)?;
            digests.push(BackupFile {
                path: rel.to_string(),
                len,
                blake3: digest.to_string(),
            });
        }
        Ok(())
    });
    let held = match held {
        Ok(held) => held,
        Err(StoreError::Upgrade { .. }) => return Err(mismatch()),
        Err(e) => return Err(e),
    };
    digests.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    if held.dirs != expected.dirs || digests != expected.files {
        return Err(mismatch());
    }
    in_place(source, target, &root, &copy)
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
        assert_eq!(relative("", "FORMAT".as_ref()).unwrap(), "FORMAT");
        assert_eq!(relative("a", "b".as_ref()).unwrap(), "a/b");
        // the path bound counts the joined path's bytes
        with_backup_limits_for_tests(10, 3, || {
            assert_eq!(relative("a", "b".as_ref()).unwrap(), "a/b");
            assert!(matches!(
                relative("a", "bc".as_ref()),
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
