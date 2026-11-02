//! Seeding and reading a follower (D-AA4FD, D-BPYN8). [`seed`] is the load's own verified path
//! (`crate::load_chain`) with the follower's purpose: a single full dump of the dump format and
//! store this rdy writes, source bytes only, and the mark durable before the store takes its
//! place. [`FollowerCore`] reads the result as a project or a holder reads, through the read-only
//! publication. A log ahead of its mark opens for recovery only: its end reads, and nothing else
//! does, so no mixed state of source frames and older project files is published.
//!
//! A follower stopped inside an increment that holds a payload's batch and the later batch that
//! removes it cannot reopen: redaction bootstrap refuses its zeros (KNOWN-ISSUES 170). Its host
//! seeds a new follower. There is no raw-segment recovery and no bypass of bootstrap.
use crate::dump_error;
use crate::error::{E_ALREADY_EXISTS, E_REDACTION_CORRUPT, FOLLOWER_SEED};
use crate::fsck::FsckReport;
use crate::load::{check_target, open_dump, read_error};
use crate::load_chain::{LoadPurpose, chain_end, inspect, load_verified};
use crate::{Location, Rdy, RdyError, ReadState};
use rdy_store::dump::{DUMP_FORMAT, read_header_only};
use rdy_store::dump_chain::{ChainEnd, DumpKind};
use rdy_store::format::{self, DumpStore};
use rdy_store::upgrade::{self, UpgradeStep};
use rdy_store::{Store, StoreError, StoreLock, layout};
use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;

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

/// The follower at `project`, opened under the load's `lock` for its check.
pub(crate) fn open_locked(project: &Path, lock: &StoreLock) -> Result<Rdy, RdyError> {
    let store = Store::open_follower_locked(&project.join(".rdy").join(layout::STORE_DIR), lock)?;
    let mode = store.mode()?;
    let context = Rdy::follower_context(project, mode)?;
    Rdy::from_follower_store(project, store, context).map_err(|(e, _)| e)
}

/// The store of a follower: a bare store while its log is ahead of its mark, else a handle.
enum State {
    Recovering(Box<Store>),
    Ready(Box<Rdy>),
}

/// One open follower. It keeps the one store lock in both of its states.
pub struct FollowerCore {
    state: State,
}

impl FollowerCore {
    /// Opens the follower at `dir`. Every `E_REDACTION_CORRUPT` of the open keeps its code and
    /// message and takes the fix that a host can act on: seed a new follower.
    pub fn open(dir: &Path) -> Result<FollowerCore, RdyError> {
        let state = open_state(dir).map_err(|mut e| {
            if e.code == E_REDACTION_CORRUPT {
                e.fix = FOLLOWER_SEED.to_string();
            }
            e
        })?;
        Ok(FollowerCore { state })
    }

    /// The end of the follower's log.
    pub fn end(&self) -> Result<Location, RdyError> {
        Ok(match &self.state {
            State::Recovering(store) => store.follower_end()?,
            State::Ready(r) => r.store().follower_end()?,
        })
    }

    /// The follower's published state.
    pub fn read(&self) -> Result<Arc<ReadState>, RdyError> {
        Ok(self.ready()?.read())
    }

    /// The engine's own check of the follower, by mode.
    pub fn fsck(&self) -> Result<FsckReport, RdyError> {
        self.ready()?.fsck()
    }

    fn ready(&self) -> Result<&Rdy, RdyError> {
        match &self.state {
            State::Recovering(_) => Err(dump_error::follower_unfinished()),
            State::Ready(r) => Ok(r),
        }
    }
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
