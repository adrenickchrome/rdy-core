//! Seeding and reading a follower (D-AA4FD, D-BPYN8). [`seed`] is the load's own verified path
//! (`crate::load_chain`) with the follower's purpose: a single full dump of the dump format and
//! store this rdy writes, source bytes only, and the mark durable before the store takes its
//! place. [`FollowerCore`] reads the result as a project or a holder reads, through the read-only
//! publication. A log ahead of its mark opens for recovery only: its end reads, and nothing else
//! does, so no mixed state of source frames and older project files is published.
//!
//! [`FollowerCore::follow`] takes one checked increment (`rdy_store::follower`), completes the
//! source's project files, publishes a new state, runs the seed's fsck on it and only then
//! replaces the mark. No state is published between the source's append, its removals and its
//! files: until the mark is durable every read of the core is refused.
//!
//! A follower stopped inside an increment that holds a payload's batch and the later batch that
//! removes it cannot reopen: redaction bootstrap refuses its zeros (KNOWN-ISSUES 170). Its host
//! seeds a new follower. There is no raw-segment recovery and no bypass of bootstrap.
use crate::dump_error;
use crate::error::{E_ALREADY_EXISTS, E_REDACTION_CORRUPT, FOLLOWER_SEED};
use crate::fsck::FsckReport;
use crate::handle::{check_start, enter};
use crate::load::{check_target, holder_files, open_dump, read_error};
use crate::load_chain::{LoadPurpose, chain_end, inspect, load_verified, owner_files, scan_error};
use crate::read_state::Delta;
use crate::{Location, Rdy, RdyError, ReadState, ReloadOutcome};
use rdy_store::dump::{DUMP_FORMAT, read_header_only};
use rdy_store::dump_chain::{ChainEnd, DumpKind};
use rdy_store::dump_scan::{ChainLink, ScanError};
use rdy_store::follower::{
    FollowError, FollowLimit, FollowStep, FollowerFailure, FollowerFile, FollowerMark,
    follow_limit, follow_step, prepare_follow,
};
use rdy_store::format::{self, DumpStore};
use rdy_store::fsutil::{sync_dir, write_private_durable};
use rdy_store::holder::StoreMode;
use rdy_store::start_lock::StartGuard;
use rdy_store::upgrade::{self, UpgradeStep};
use rdy_store::{Store, StoreError, StoreLock, layout};
use serde::Serialize;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What a follow did: the chain end it reached, the source batches and data frames it appended
/// in this call, and whether it appended none.
#[derive(Clone, Debug, Serialize)]
pub struct FollowReport {
    pub end: ChainEnd,
    pub batches: u64,
    pub frames: u64,
    pub repeated: bool,
}

/// Seeds a follower of the source of the full dump `dump` in `dir`, which does not exist or is
/// empty, and returns its chain end. A refused or failed seed leaves `dir` as it was.
pub fn seed(dump: &Path, dir: &Path) -> Result<ChainEnd, RdyError> {
    seed_with(dump, dir, None)
}

/// Test hook: [`seed`] with the upgrade steps `registry` in place of this rdy's.
#[doc(hidden)]
pub fn seed_with_registry_for_tests(
    dump: &Path,
    dir: &Path,
    registry: &[UpgradeStep],
) -> Result<ChainEnd, RdyError> {
    seed_with(dump, dir, Some(registry))
}

fn seed_with(
    dump: &Path,
    dir: &Path,
    registry: Option<&[UpgradeStep]>,
) -> Result<ChainEnd, RdyError> {
    check_seed_dump(dump, registry)?;
    let plan = inspect(&[dump.to_path_buf()], None)?;
    let Some(scan) = plan.scan() else {
        return Err(dump_error::seed_store_format());
    };
    check_seed_target(dir)?;
    load_verified(&plan, dir, LoadPurpose::Follower)?;
    Ok(chain_end(scan))
}

/// The seed's refusals from `dump`'s prelude and header, before anything else reads it: an
/// increment first, then a dump of another format, or of a store this rdy cannot write or that
/// needs an upgrade. A stream that does not open keeps its own diagnostic.
fn check_seed_dump(dump: &Path, registry: Option<&[UpgradeStep]>) -> Result<(), RdyError> {
    let mut f = open_dump(dump)?;
    let (dump_format, header) = read_header_only(&mut f).map_err(|e| read_error(dump, e))?;
    if header.chain.is_some_and(|c| c.kind == DumpKind::Increment) {
        return Err(dump_error::seed_increment());
    }
    let DumpStore::Features(manifest) = &header.store else {
        return Err(dump_error::seed_store_format());
    };
    if dump_format != DUMP_FORMAT || format::check_writable(manifest).is_err() {
        return Err(dump_error::seed_store_format());
    }
    let needs_upgrade = match registry {
        None => upgrade::manifest_needs_upgrade(manifest),
        Some(r) => upgrade::manifest_needs_upgrade_with_registry_for_tests(manifest, r),
    };
    if needs_upgrade.unwrap_or(true) {
        return Err(dump_error::seed_store_format());
    }
    Ok(())
}

/// A seed's target does not exist or is an empty directory; the load's own target checks follow.
fn check_seed_target(dir: &Path) -> Result<(), RdyError> {
    let empty = match std::fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_none(),
        Err(e) if e.kind() == ErrorKind::NotFound => std::fs::symlink_metadata(dir).is_err(),
        Err(_) => false,
    };
    if !empty {
        return Err(RdyError::new(
            E_ALREADY_EXISTS,
            format!("{} is not empty", dir.display()),
            "choose an empty directory for the follower",
        ));
    }
    check_target(dir, &[]).map(drop)
}

/// The follower at `project`, opened under the load's `lock` for its check. The load holds
/// `start`, the project's start lock, and this open borrows it.
pub(crate) fn open_locked(
    project: &Path,
    lock: &StoreLock,
    start: &StartGuard,
) -> Result<Rdy, RdyError> {
    check_start(project, start)?;
    let store = Store::open_follower_locked(&project.join(".rdy").join(layout::STORE_DIR), lock)?;
    let mode = store.mode()?;
    let context = Rdy::follower_context(project, mode)?;
    Rdy::from_follower_store(project, store, context).map_err(|(e, _)| e)
}

/// The store of a follower: a bare store while its log is ahead of its mark, else a handle.
enum State {
    Recovering(Box<Store>),
    Ready(Box<Rdy>),
    /// Only inside a change of state.
    Moving,
}

/// One open follower. It keeps the one store lock in each of its states.
pub struct FollowerCore {
    dir: PathBuf,
    state: State,
    /// A follow has begun and has not finished: every read is refused.
    recovering: bool,
}

impl FollowerCore {
    /// Opens the follower at `dir`. Every `E_REDACTION_CORRUPT` of the open keeps its code and
    /// message and takes the fix that a host can act on: seed a new follower.
    pub fn open(dir: &Path) -> Result<FollowerCore, RdyError> {
        FollowerCore::open_with(dir, None)
    }

    /// Opens the follower at `dir` as [`FollowerCore::open`] does, under `start`, the project's
    /// start lock, which the caller holds and keeps and the open borrows. A guard of another
    /// project is `E_LOCKED`.
    pub fn open_under_start(dir: &Path, start: &StartGuard) -> Result<FollowerCore, RdyError> {
        check_start(dir, start)?;
        FollowerCore::open_with(dir, Some(start))
    }

    fn open_with(dir: &Path, start: Option<&StartGuard>) -> Result<FollowerCore, RdyError> {
        // held across the open: start lock before store lock
        let _entry = match start {
            Some(_) => None,
            None => Some(enter(dir)?),
        };
        let state = open_state(dir).map_err(|mut e| {
            if e.code == E_REDACTION_CORRUPT {
                e.fix = FOLLOWER_SEED.to_string();
            }
            e
        })?;
        Ok(FollowerCore {
            dir: dir.to_path_buf(),
            state,
            recovering: false,
        })
    }

    /// The end of the follower's log.
    pub fn end(&self) -> Result<Location, RdyError> {
        Ok(self.store()?.follower_end()?)
    }

    /// The follower's published state.
    pub fn read(&self) -> Result<Arc<ReadState>, RdyError> {
        Ok(self.ready()?.read())
    }

    /// The engine's own check of the follower, by mode.
    pub fn fsck(&self) -> Result<FsckReport, RdyError> {
        self.ready()?.fsck()
    }

    /// Follows the one increment at `increment` (D-AA4FD). See the module docs. A refusal before
    /// the append changes nothing, so a follower that was readable stays readable. A failure
    /// after a complete source batch leaves reads refused until the same increment is retried.
    pub fn follow(&mut self, increment: &Path) -> Result<FollowReport, RdyError> {
        let prior = self.recovering;
        let store = self.store()?;
        let dir = store.dir().to_path_buf();
        let mark = rdy_store::follower::read_mark(&dir)?;
        let Some(mark) = mark else {
            return Err(dump_error::not_a_follower());
        };
        let path = increment.to_path_buf();
        let verified =
            prepare_follow(store, &mark, increment).map_err(|e| follow_error(&path, e))?;
        if verified.mode() == StoreMode::Holder && !verified.files().is_empty() {
            return Err(holder_files());
        }
        let files = owner_files(increment, verified.files())?;
        let before = verified.actual();
        let marked = verified.is_marked();
        if !marked && before == mark.chain.end {
            check_local_files(&self.dir, &mark.files)?;
        }
        let next = verified.chain().clone();
        let finished = marked && before == mark.chain.end && !self.unfinished();
        if finished {
            return Ok(FollowReport {
                end: next,
                batches: 0,
                frames: 0,
                repeated: true,
            });
        }
        // set before the first append: a kept batch must never read as a finished state
        self.recovering = true;
        let store = self.store_mut()?;
        let report = match store.follow_verified(verified) {
            Ok(r) => r,
            Err(e) => {
                // only an append that kept nothing, and that a rollback undid, restores reads
                if store.follower_end().is_ok_and(|end| end == before) && !store.is_poisoned() {
                    self.recovering = prior;
                }
                return Err(follow_error(&path, e));
            }
        };
        let rows = replace_source_files(&self.dir, &mark.files, &files)?;
        self.fresh_state()?;
        self.ready_unchecked()?.fsck()?;
        let new = FollowerMark {
            chain: next.clone(),
            files: rows,
        };
        self.store_mut()?.write_follow_mark(&new)?;
        self.recovering = false;
        Ok(FollowReport {
            end: next,
            batches: report.batches,
            frames: report.frames,
            repeated: report.batches == 0,
        })
    }

    /// Whether a follow of this open has not finished, or the log was ahead when it opened.
    fn unfinished(&self) -> bool {
        self.recovering || matches!(self.state, State::Recovering(_))
    }

    /// The prepared state of the follow's end. A handle renews the removal caches before its
    /// reload or publish can capture them, for a retry that appended no batch too; a bare store
    /// becomes a handle with fresh caches.
    fn fresh_state(&mut self) -> Result<(), RdyError> {
        match std::mem::replace(&mut self.state, State::Moving) {
            State::Moving => Err(moving()),
            State::Recovering(store) => {
                let store = *store;
                let context = match store
                    .mode()
                    .map_err(RdyError::from)
                    .and_then(|mode| Rdy::follower_context(&self.dir, mode))
                {
                    Ok(c) => c,
                    Err(e) => {
                        self.state = State::Recovering(Box::new(store));
                        return Err(e);
                    }
                };
                match Rdy::from_follower_store(&self.dir, store, context) {
                    Ok(r) => {
                        self.state = State::Ready(Box::new(r));
                        Ok(())
                    }
                    Err((e, store)) => {
                        self.state = State::Recovering(Box::new(store));
                        Err(e)
                    }
                }
            }
            State::Ready(mut r) => {
                r.renew_epoch_caches();
                let out = match r.reload_pack() {
                    Ok(ReloadOutcome::Invalid { error, .. }) => Err(*error),
                    Ok(_) => r.publish(Delta::Full),
                    Err(e) => Err(e),
                }
                .and_then(|()| match r.read().error.clone() {
                    Some(e) => Err(e),
                    None => Ok(()),
                });
                self.state = State::Ready(r);
                out
            }
        }
    }

    fn store(&self) -> Result<&Store, RdyError> {
        match &self.state {
            State::Recovering(store) => Ok(store),
            State::Ready(r) => Ok(r.store()),
            State::Moving => Err(moving()),
        }
    }

    fn store_mut(&mut self) -> Result<&mut Store, RdyError> {
        match &mut self.state {
            State::Recovering(store) => Ok(store),
            State::Ready(r) => Ok(r.store_mut()),
            State::Moving => Err(moving()),
        }
    }

    fn ready(&self) -> Result<&Rdy, RdyError> {
        if self.recovering {
            return Err(dump_error::follower_unfinished());
        }
        self.ready_unchecked()
    }

    fn ready_unchecked(&self) -> Result<&Rdy, RdyError> {
        match &self.state {
            State::Recovering(_) | State::Moving => Err(dump_error::follower_unfinished()),
            State::Ready(r) => Ok(r),
        }
    }
}

/// A follower whose change of state panicked.
fn moving() -> RdyError {
    dump_error::follower_unfinished()
}

/// The engine's error of a follow's refusal (TASK-1002 C-6, C-7): the chain loader's five
/// mismatches keep their code and message and take the follow's fix.
fn follow_error(path: &Path, e: FollowError) -> RdyError {
    let files = [path.to_path_buf()];
    match e {
        FollowError::Store(e) => e.into(),
        FollowError::Scan(ScanError::Link { link, .. }) => {
            let mismatch = matches!(
                link,
                ChainLink::Repo { .. }
                    | ChainLink::StoreFormat { .. }
                    | ChainLink::Position { .. }
                    | ChainLink::Marker
                    | ChainLink::Digest { .. }
            );
            let error = scan_error(&files, ScanError::Link { file: 0, link });
            if mismatch {
                dump_error::for_follow(error)
            } else {
                error
            }
        }
        FollowError::Scan(e) => scan_error(&files, e),
        FollowError::Follower(f) => match f {
            FollowerFailure::FullDump => dump_error::follow_full_dump(),
            FollowerFailure::SavedChain => dump_error::follow_saved_chain(),
            FollowerFailure::WrittenPrefix => dump_error::follow_written_prefix(),
            FollowerFailure::LocalFiles => dump_error::follow_local_files(),
            FollowerFailure::ChangedInput => dump_error::follow_changed_input(),
            FollowerFailure::NotFollower => dump_error::not_a_follower(),
        },
        FollowError::Limit { unit } => {
            let name = match unit {
                FollowLimit::Batches => "batches",
                FollowLimit::Frames => "frames",
                FollowLimit::Segments => "segments",
                FollowLimit::PrefixFrames => "prefix frames",
            };
            dump_error::follow_limit(follow_limit(unit), name)
        }
    }
}

/// Refuses a follower whose listed source files differ from their saved hashes: it is no editable
/// working copy.
fn check_local_files(dir: &Path, listed: &[FollowerFile]) -> Result<(), RdyError> {
    for f in listed {
        let at = dir.join(&f.path);
        let same = std::fs::symlink_metadata(&at).is_ok_and(|m| m.is_file())
            && std::fs::read(&at).is_ok_and(|b| rdy_model::Hash::of(&b) == f.hash);
        if !same {
            return Err(dump_error::follow_local_files());
        }
    }
    Ok(())
}

/// Makes the follower's source files the increment's `files`: each one that differs is replaced,
/// each path `old` lists and `files` does not is removed, and every directory touched is synced.
/// It touches no other path. Returns the new mark's rows, sorted by the path's bytes.
fn replace_source_files(
    dir: &Path,
    old: &[FollowerFile],
    files: &[(String, Vec<u8>)],
) -> Result<Vec<FollowerFile>, RdyError> {
    let io = |at: &Path, e: std::io::Error| crate::load::io_at(at, e);
    let mut rows = Vec::new();
    rows.try_reserve_exact(files.len()).map_err(|_| {
        RdyError::new(
            crate::error::E_LIMIT,
            "record allocation failed",
            "Free memory, then run the command again.",
        )
    })?;
    for (path, bytes) in files {
        let at = dir.join(path);
        let hash = rdy_model::Hash::of(bytes);
        rows.push(FollowerFile {
            path: path.clone(),
            hash,
        });
        let regular = std::fs::symlink_metadata(&at).is_ok_and(|m| m.is_file());
        if regular && std::fs::read(&at).is_ok_and(|have| have == *bytes) {
            continue;
        }
        if let Some(parent) = at.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
            sync_dir(parent).map_err(|e| io(parent, e))?;
        }
        write_private_durable(&at, bytes).map_err(|e| io(&at, e))?;
        follow_step(FollowStep::FileReplaced);
    }
    for gone in old {
        if files.iter().any(|(path, _)| *path == gone.path) {
            continue;
        }
        let at = dir.join(&gone.path);
        match std::fs::remove_file(&at) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(io(&at, e)),
        }
        if let Some(parent) = at.parent() {
            sync_dir(parent).map_err(|e| io(parent, e))?;
        }
        follow_step(FollowStep::FileReplaced);
    }
    follow_step(FollowStep::FilesSynced);
    rows.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    Ok(rows)
}

/// The open's state. The saved end is judged before a project file is read into a context.
fn open_state(dir: &Path) -> Result<State, RdyError> {
    let store = layout::open_follower_store(&dir.join(".rdy"))?;
    let mark = rdy_store::follower::read_mark(store.dir())?.ok_or(StoreError::FollowerMark(
        rdy_store::error::FollowerMarkFailure::Missing,
    ))?;
    if store.follower_end()? != mark.chain.end {
        return Ok(State::Recovering(Box::new(store)));
    }
    let mode = store.mode()?;
    let context = Rdy::follower_context(dir, mode)?;
    let r = Rdy::from_follower_store(dir, store, context).map_err(|(e, _)| e)?;
    Ok(State::Ready(Box::new(r)))
}
