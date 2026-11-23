//! Hub forks (D-99CWC, D-DZDB7, D-HX6BX): a new holder whose canon ends at a captured tag's
//! landing. The source is a holder's published state; the fork is a holder too.
//!
//! The steps (closed):
//! 1. The source is a holder (`this store is a project` otherwise). The tag is one the state
//!    captured (`no version <name>`). No removal ran since the state (`E_REDACTED_STATE`).
//! 2. The target is absent or an empty directory, outside the source's `.rdy`, with no store
//!    (`<dir> is not empty` otherwise). Every input check runs before any file is written.
//! 3. The fork's origins: the source's counted origins before its captured end, oldest first,
//!    then the fork's own (`fork: true`, the source repo, the tag and its landing).
//! 4. The export: the canon history at the tag's landing, with no base and no known key, up to
//!    the state's captured end. It goes to a batch file beside the target.
//! 5. A new holder in a staging directory beside the target, with a new repo id and the
//!    origins in its creating batch. It takes the export through the fresh fork's take
//!    (`rdy_store::Store::import_fork`), which runs every check of a pushed history.
//! 6. The seed: canon at the tag's landing, with no expected tip. Only the source's tags on that
//!    landing's `prev` chain get a ref (`rdy_store::fork::fork_tags`). Then `fsck`, and a
//!    durable close of the staging holder.
//! 7. The removal epoch of the source again, then one rename of the staging directory onto the
//!    target, and an open of the installed holder.
//!
//! A refusal or failure at any step removes the staging directory and the batch file, and
//! leaves the source and the target as they were. Nothing is retried as a full clone.
//!
//! A project fork ([`fork_project_at`], `rdy clone --fork --at`) takes the same steps, with
//! these differences:
//! - Step 1: the source is a project (a holder is a clone's holder refusal).
//! - Step 5: the staging directory gets the source's project files (`crate::dump::project_files`:
//!   `.rdy.yaml`, `.rdyignore` and packs, never `.rdy/local.yaml`), then a new project store with
//!   a new repo id (`rdy_store::Store::create_fork_project`). The project opens as any project
//!   does. Its take's batch writes the origins after the records, so a removal it carries reads
//!   as fetched.
//! - Step 6: the seed is one batch of refs, `canon` and the selected tags
//!   (`rdy_store::Store::seed_project_fork`). No source view comes, so the default view is fresh,
//!   as after `rdy init`: the first write makes it on the fork's canon.
use crate::clone::{check_new_dir, holder_source, install, io_at, staging_beside};
use crate::error::{E_INTERNAL, E_NOT_FOUND, FORK_DESTINATION_FIX, RdyError};
use crate::holder::is_a_project;
use crate::load::LoadSummary;
use crate::read_state::ReadState;
use crate::stream_take::{move_error, republish};
use crate::version::parse_version;
use crate::{Rdy, stream_export};
use rdy_model::{Hash, RepoId, Tag};
use rdy_store::holder::StoreMode;
use rdy_store::hub_canon::Moved;
use rdy_store::meta::Origin;
use rdy_store::sync_stream::StreamPurpose;
use rdy_store::take_batch::TakeError;
use rdy_store::{OpenPolicy, Store, StoreError};
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The prefix of a fork's staging directory and batch file beside the target.
const STAGING_PREFIX: &str = ".rdy-fork-";

/// A point of a fork that a test hook sees ([`fork_holder_at_with`]). No product path passes a
/// hook.
#[doc(hidden)]
pub enum ForkStep<'a> {
    /// The export is written: the batch file the take reads.
    Exported(&'a Path),
    /// The take is sealed and the seed has not run: the staging holder's store.
    Taken(&'a mut Store),
    /// The staging holder is closed and checked: the target the install renames onto.
    Closed(&'a Path),
}

/// A test hook: called at each [`ForkStep`]; an error stops the fork there.
#[doc(hidden)]
pub type ForkHook<'a> = &'a mut dyn FnMut(ForkStep<'_>) -> Result<(), RdyError>;

/// A checked fork in its staging directory, closed, waiting for its install.
struct Prepared {
    target: PathBuf,
    staging: tempfile::TempDir,
}

/// Forks the holder whose published state is `read` at its captured tag `tag` into `dir`, and
/// returns the new holder, open. See the module docs.
pub fn fork_holder_at(read: &ReadState, tag: &str, dir: &Path) -> Result<Rdy, RdyError> {
    fork_holder_at_with(read, tag, dir, &mut |_| Ok(()))
}

/// [`fork_holder_at`] with a test hook at each step.
#[doc(hidden)]
pub fn fork_holder_at_with(
    read: &ReadState,
    tag: &str,
    dir: &Path,
    hook: ForkHook<'_>,
) -> Result<Rdy, RdyError> {
    let prepared = prepare_fork(read, tag, dir, &mut *hook)?;
    install_fork(read, prepared, hook)
}

/// Steps 1 to 6: the checks, the export, and the closed staging holder.
fn prepare_fork(
    read: &ReadState,
    tag: &str,
    dir: &Path,
    hook: ForkHook<'_>,
) -> Result<Prepared, RdyError> {
    let reader = read.reader();
    if reader.mode()? != StoreMode::Holder {
        return Err(is_a_project());
    }
    let selected = captured_tag(read, tag)?;
    read.ensure_current_epoch()?;
    crate::load::inside_source_rdy(&read.root().join(".rdy"), dir)?;
    check_new_dir(dir, FORK_DESTINATION_FIX)?;
    let (source, origins) = lineage(read, &selected)?;
    let named: BTreeSet<Hash> = read.tags.iter().map(|(_, t)| t.id()).collect();
    let (target, staging) = staging_beside(dir, STAGING_PREFIX)?;
    let batch = export(read, &selected, staging.path())?;
    hook(ForkStep::Exported(batch.path()))?;
    let files = [File::open(batch.path()).map_err(|e| io_at(batch.path(), &e))?];
    let mut rdy = Rdy::create_holder_with_origins(staging.path(), RepoId::new(), &origins)?;
    let built = build(&mut rdy, &files, &selected, source, &named, &mut *hook);
    // the staging holder closes on every path, so its directory can go or move
    let closed = rdy.close();
    built?;
    closed?;
    drop(files);
    drop(batch);
    Ok(Prepared { target, staging })
}

/// Step 3: the source's repo id, and the fork's origins: the source's counted origins before
/// its captured end, oldest first, then the fork's own (D-HX6BX).
fn lineage(read: &ReadState, selected: &Tag) -> Result<(RepoId, Vec<Origin>), RdyError> {
    let reader = read.reader();
    let Some(source) = reader.repo_id()? else {
        return Err(RdyError::new(
            E_INTERNAL,
            "the store has no repo id",
            "report this as a bug in rdy",
        ));
    };
    let mut origins = reader.counted_fork_origins_before(read.seq())?;
    origins.push(Origin {
        fork: true,
        repo_id: source,
        tag: Some(selected.name.clone()),
        landing: Some(selected.landing),
    });
    Ok((source, origins))
}

/// Step 4: the canon history at the selected tag's landing, with no base and no known key, up
/// to the state's captured end, in a batch file beside the staging directory `staging`.
fn export(
    read: &ReadState,
    selected: &Tag,
    staging: &Path,
) -> Result<tempfile::NamedTempFile, RdyError> {
    let parent = staging.parent().unwrap_or(Path::new("."));
    let batch = tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .suffix(".rdysync")
        .tempfile_in(parent)
        .map_err(|e| io_at(parent, &e))?;
    {
        let mut out = std::io::BufWriter::new(batch.as_file());
        stream_export::write_history(
            read,
            StreamPurpose::CanonHistory,
            selected.landing,
            None,
            &BTreeSet::new(),
            &mut out,
        )?;
        out.flush().map_err(|e| io_at(batch.path(), &e))?;
    }
    Ok(batch)
}

/// Step 5's take and step 6's seed and check, in the staging holder `rdy`.
fn build(
    rdy: &mut Rdy,
    files: &[File],
    selected: &Tag,
    source: RepoId,
    named: &BTreeSet<Hash>,
    hook: ForkHook<'_>,
) -> Result<(), RdyError> {
    rdy.seal_open()?;
    let epoch = rdy.store().redaction_epoch();
    // the take seals its own batch, as the store's other self-sealing writes do
    rdy.store_mut().set_held(false)?;
    let taken = rdy
        .store_mut()
        .import_fork::<RdyError>(files, selected.landing, source, &[]);
    rdy.store_mut().set_held(true)?;
    taken.map_err(move_error)?;
    republish(rdy, epoch)?;
    hook(ForkStep::Taken(rdy.store_mut()))?;
    rdy.seal_open()?;
    let epoch = rdy.store().redaction_epoch();
    let moved = rdy
        .store_mut()
        .seed_fork(selected.landing, named, crate::now_ms())
        .map_err(seed_error)?;
    if moved
        != (Moved::Moved {
            tip: selected.landing,
        })
    {
        return Err(RdyError::new(
            E_INTERNAL,
            format!(
                "the fork's canon did not seed at landing {}",
                selected.landing
            ),
            "report this as a bug in rdy",
        ));
    }
    republish(rdy, epoch)?;
    rdy.fsck()?;
    Ok(())
}

/// Step 7: the source's removal epoch, the rename onto the target, and the open.
fn install_fork(read: &ReadState, prepared: Prepared, hook: ForkHook<'_>) -> Result<Rdy, RdyError> {
    hook(ForkStep::Closed(&prepared.target))?;
    read.ensure_current_epoch()?;
    install(prepared.staging, &prepared.target)?;
    Rdy::open_holder_with_policy(&prepared.target, OpenPolicy::RequireCurrent)
}

/// What a project fork made: its summary, the origin it records last, and its new repo id.
pub(crate) type ProjectFork = (LoadSummary, Origin, RepoId);

/// What a project fork's next step is: it carries no working copy, as a clone does.
const PROJECT_NEXT: &str = "run rdy checkout to bind a working copy; a clone carries none";

/// Forks the project whose published state is `read` at its captured tag `tag` into `dir`, a
/// new project (`rdy clone --fork --at`, D-99CWC). See the module docs. Returns the summary,
/// the fork's own origin and its new repo id.
pub(crate) fn fork_project_at(
    read: &ReadState,
    tag: &str,
    dir: &Path,
) -> Result<ProjectFork, RdyError> {
    let reader = read.reader();
    if reader.mode()? == StoreMode::Holder {
        return Err(holder_source());
    }
    let selected = captured_tag(read, tag)?;
    read.ensure_current_epoch()?;
    crate::load::inside_source_rdy(&read.root().join(".rdy"), dir)?;
    check_new_dir(dir, FORK_DESTINATION_FIX)?;
    let (source, origins) = lineage(read, &selected)?;
    let named: BTreeSet<Hash> = read.tags.iter().map(|(_, t)| t.id()).collect();
    let files = crate::dump::project_files(read.root())?;
    let (target, staging) = staging_beside(dir, STAGING_PREFIX)?;
    let batch = export(read, &selected, staging.path())?;
    // step 5: the project files, then the new store with its origins, then the open
    let written = write_files(staging.path(), &files)?;
    let repo = RepoId::new();
    let store = staging
        .path()
        .join(".rdy")
        .join(rdy_store::layout::STORE_DIR);
    Store::create_fork_project(&store, repo)?.close()?;
    let taken = [File::open(batch.path()).map_err(|e| io_at(batch.path(), &e))?];
    let mut rdy = Rdy::open(staging.path())?;
    let built = build_project(&mut rdy, &taken, &selected, source, &origins, &named);
    // the staging project closes on every path, so its directory can go or move
    let closed = rdy.close();
    let built = built?;
    closed?;
    drop(taken);
    drop(batch);
    // step 7
    read.ensure_current_epoch()?;
    install(staging, &target)?;
    let origin = origins
        .last()
        .cloned()
        .expect("the fork's own origin is last");
    let load = LoadSummary {
        project: target.display().to_string(),
        dump_format: rdy_store::dump::DUMP_FORMAT,
        store_format: rdy_store::format::DumpStore::Features(built.features),
        batches: built.batches,
        frames: built.fsck.frames as u64,
        files_written: written,
        files_kept: Vec::new(),
        landings: built.fsck.landings,
        tags: built.tags,
        views: built.views,
        fsck: built.fsck,
        next: PROJECT_NEXT.to_string(),
        chain: Vec::new(),
        stopped_at: None,
        warnings: Vec::new(),
    };
    Ok((load, origin, repo))
}

/// Writes the project files `files` (path and bytes) into the new project `root`, each durable;
/// returns their paths.
fn write_files(root: &Path, files: &[(String, Vec<u8>)]) -> Result<Vec<String>, RdyError> {
    let packs = root.join(".rdy").join("packs");
    rdy_store::fsutil::create_dir_durable(&packs).map_err(|e| io_at(&packs, &e))?;
    let mut out = Vec::with_capacity(files.len());
    for (path, bytes) in files {
        let file = root.join(path);
        rdy_store::fsutil::write_durable(&file, bytes).map_err(|e| io_at(&file, &e))?;
        out.push(path.clone());
    }
    Ok(out)
}

/// What a project fork's build found: the fork's features, its batches, `fsck`, and its tags
/// and views.
struct Built {
    features: rdy_store::format::StoreFeatures,
    batches: u64,
    fsck: crate::fsck::FsckReport,
    tags: usize,
    views: usize,
}

/// Step 5's take and step 6's seed and check, in the staging project `rdy`.
fn build_project(
    rdy: &mut Rdy,
    files: &[File],
    selected: &Tag,
    source: RepoId,
    origins: &[Origin],
    named: &BTreeSet<Hash>,
) -> Result<Built, RdyError> {
    rdy.seal_open()?;
    let epoch = rdy.store().redaction_epoch();
    // the take seals its own batch, as the store's other self-sealing writes do
    rdy.store_mut().set_held(false)?;
    let taken = rdy
        .store_mut()
        .import_fork::<RdyError>(files, selected.landing, source, origins);
    rdy.store_mut().set_held(true)?;
    taken.map_err(move_error)?;
    republish(rdy, epoch)?;
    rdy.seal_open()?;
    let epoch = rdy.store().redaction_epoch();
    rdy.store_mut().seed_project_fork(selected.landing, named)?;
    republish(rdy, epoch)?;
    rdy.seal_open()?;
    let fsck = rdy.fsck()?;
    let store = rdy.store();
    let mut batches = 0u64;
    // the walk that passes removed frames by their headers: the take holds the approved drops
    store
        .reader()
        .for_each_record_from(LOG_START, store.log_end(), |_, frame, _| {
            if let rdy_store::redaction::StoredFrame::Present { kind, .. } = frame
                && kind == rdy_store::kinds::BATCH_COMMIT
            {
                batches += 1;
            }
            Ok(())
        })?;
    Ok(Built {
        features: store.features().clone(),
        batches,
        fsck,
        tags: store.count_refs(rdy_store::holder::TAG_REF)?,
        views: store.count_refs(rdy_store::hub_canon::VIEW_REF)?,
    })
}

/// Where a log starts.
const LOG_START: rdy_store::segment::Location = rdy_store::segment::Location {
    segment: 0,
    offset: 0,
};

/// The tag of `read` that names `name`'s version, as `tag:<version>` reads it.
fn captured_tag(read: &ReadState, name: &str) -> Result<Tag, RdyError> {
    let want = parse_version(name)?;
    read.tags
        .iter()
        .find(|(v, _)| v.cmp_precedence(&want) == Ordering::Equal)
        .map(|(_, t)| t.clone())
        .ok_or_else(|| {
            RdyError::new(
                E_NOT_FOUND,
                format!("no version {name}"),
                "list versions with rdy tags",
            )
        })
}

/// A seed's error as the engine's: the store's errors as a move maps them.
fn seed_error(e: TakeError<StoreError>) -> RdyError {
    move_error(match e {
        TakeError::Validation(e) | TakeError::Store(e) => TakeError::Store(e),
        TakeError::Stream(e) => TakeError::Stream(e),
        TakeError::UncoveredChange(h) => TakeError::UncoveredChange(h),
        TakeError::ModeMismatch => TakeError::ModeMismatch,
        TakeError::RepoMismatch { source, target } => TakeError::RepoMismatch { source, target },
    })
}
