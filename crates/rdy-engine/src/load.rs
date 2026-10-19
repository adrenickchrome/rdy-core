//! `rdy load` (plan Dump and load; D-69AY3, D-WWAK7, D-D2NPN, D-FR0C4 F3 and F5): rebuilds a
//! project from a dump into a directory with no store. The new log is the source log byte for
//! byte, so every hash, landing, tag and open view is kept. The first open builds the index from
//! the log (the same replay as a reindex), then the load runs `fsck`.
//!
//! The steps (closed):
//! 1. Read the dump's prelude and header.
//! 2. Read its project files, and check the target before anything is written: not in a
//!    project's `.rdy` directory, no `.rdy/store`, no `.rdy/store.loading`, no live daemon that
//!    the target's socket record ties to its `.rdy` (final ruling 1: a daemon that still serves
//!    a store moved aside would keep taking writes), and no project file with other bytes than
//!    the dump. A file with the same bytes is kept.
//! 3. Create the project directory and its `.rdy` if missing, then `.rdy/store.loading`.
//! 4. Ingest every batch at its source place; the trailer must verify. The batches must end
//!    where the header says the source log ends, and there must be at least one (every store
//!    holds its format batch from its first open). When the dump's store format is older than
//!    this rdy's, append one batch that sets `meta/store_format` to this rdy's (FW-UP1, in its
//!    smallest form). Then check each dropped frame's authorization against the whole log (dump
//!    format 2, `rdy_store::ingest`): a refusal is damage at the end of the stream, where the
//!    check runs. Then write the store's `FORMAT`.
//!    The first batch also sets the store's mode. A holder (D-BPYN8) holds no project file: a
//!    holder dump that carries one is refused here, after the ingest has validated the mode and
//!    before any file is written, and step 8 takes the store back.
//! 5. Write each project file the target does not have.
//! 6. Take the new store's lock, then rename `.rdy/store.loading` to `.rdy/store`. The load
//!    holds the lock until it ends, so no other rdy process opens the project before step 7
//!    ends (D-R7N9J, D-A6ZZA).
//! 7. Open the project under that lock (the index build), run the caller's step if any (a
//!    clone's origin batch, `crate::clone`) once the store's meta refs pass their check (a
//!    failure is the load's fsck failure, ruling task-5), run `fsck`, and count landings, tags
//!    and views. A failure of `fsck` or the counts that names a target file the dump does not carry
//!    (`.rdy/local.yaml`, `.rdyignore`, a pack file outside the dump's set) names that file; any
//!    other such failure is the source's. The caller's step returns its own errors.
//!
//! A failure in steps 3 to 7 removes the store, every file step 5 wrote and every directory
//! step 3 or 5 created, so the target is as it was. The removal runs while the load holds the
//! store's lock; a load that finds it does not hold the lock removes nothing. The load never
//! deletes a file it did not write: a pack file at the target that the dump does not hold stays
//! where it is.
use crate::Rdy;
use crate::config_levels::{LOCAL_FILE, PROJECT_FILE};
use crate::error::{E_ALREADY_EXISTS, E_BAD_INPUT, E_DUMP, E_IO, E_LOCKED, RdyError};
use crate::fsck::FsckReport;
use crate::wc::ignore::IGNORE_FILE;
use rdy_store::dump::{DumpBatch, DumpHeader, DumpItem, DumpReader, DumpRefusal, Frame};
use rdy_store::format::FORMAT_FILE;
use rdy_store::fsutil::{create_dir_durable, sync_dir};
use rdy_store::holder::REF_STORE_MODE;
use rdy_store::ingest::Ingest;
use rdy_store::kinds::{self, RefUpdate};
use rdy_store::layout::STORE_DIR;
use rdy_store::segment::Location;
use rdy_store::{StoreError, StoreLock};
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, ErrorKind, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

/// The store directory a load writes before it renames it to `.rdy/store`.
const LOADING_DIR: &str = "store.loading";
/// The directory of the pack files, relative to the project root.
const PACKS_DIR: &str = ".rdy/packs";
/// What the person does after a load: a dump carries no working copy (O9).
const NEXT: &str = "run rdy checkout to bind a working copy; a dump carries none";
/// What the person does after a holder load: a holder has no working copy.
const HOLDER_NEXT: &str = "read the holder through the adapter at a tag";

/// Finds a live daemon of the project at a path: its pid when the project's socket record ties a
/// live process to the project's `.rdy`, else `None`. The engine cannot see daemons (they are a
/// later crate), so the binary sets the probe once ([`set_daemon_probe`]).
pub type DaemonProbe = fn(&Path) -> Option<u32>;

static DAEMON_PROBE: OnceLock<DaemonProbe> = OnceLock::new();

/// Sets the probe step 2 asks for a live daemon on the target, once per process (the `rdy`
/// binary sets `rdy_daemon::live_daemon` in `main`). A later call keeps the first probe.
pub fn set_daemon_probe(probe: DaemonProbe) {
    let _ = DAEMON_PROBE.set(probe);
}

/// What `rdy load` did: the project, the dump's formats, what it held, the project files it
/// wrote and kept, the counts of landings, tags and views, and the `fsck` report.
#[derive(Clone, Debug, Serialize)]
pub struct LoadSummary {
    pub project: String,
    pub dump_format: u32,
    pub store_format: u32,
    pub batches: u64,
    pub frames: u64,
    pub files_written: Vec<String>,
    pub files_kept: Vec<String>,
    pub landings: usize,
    pub tags: usize,
    pub views: usize,
    pub fsck: FsckReport,
    pub next: String,
}

/// Loads the dump `file` into `project`, a directory with no store (F3). A refused or failed
/// load leaves the target as it was.
pub fn load(file: &Path, project: &Path) -> Result<LoadSummary, RdyError> {
    // step 1: the prelude and the header
    let f = open_dump(file)?;
    load_from(file, BufReader::new(f), project, None)
}

/// A step that runs after the open of step 7, under the load's lock and before `fsck`: a
/// clone's origin batch (`crate::clone`). Its error fails the load, which then undoes.
pub(crate) type AfterOpen<'a> = &'a mut dyn FnMut(&mut Rdy) -> Result<(), RdyError>;

/// Opens the dump `file` for a load; a missing file is `E_BAD_INPUT`.
pub(crate) fn open_dump(file: &Path) -> Result<File, RdyError> {
    File::open(file).map_err(|e| match e.kind() {
        ErrorKind::NotFound => RdyError::new(
            E_BAD_INPUT,
            format!("{} does not exist", file.display()),
            "check the path of the dump",
        ),
        _ => io_at(file, e),
    })
}

/// A dump stream whose prelude and header step 1 has read: the reader, positioned at the first
/// item, the dump format and the header.
pub(crate) struct Opened<R: Read> {
    reader: DumpReader<R>,
    pub(crate) dump_format: u32,
    pub(crate) header: DumpHeader,
}

/// Step 1 on the dump stream `r`: reads its prelude and header. `source` is the path the texts
/// name, as in [`load_from`].
pub(crate) fn open_stream<R: Read>(source: &Path, r: R) -> Result<Opened<R>, RdyError> {
    let (reader, dump_format, header) = DumpReader::open(r).map_err(|e| read_error(source, e))?;
    Ok(Opened {
        reader,
        dump_format,
        header,
    })
}

/// [`load`] of the dump stream `r` into `project`. `source` is the path every text names: the
/// dump file, or the source project's root for a clone of an open project (O24). `after_open`
/// runs in step 7 right after the open, before `fsck`.
pub(crate) fn load_from(
    source: &Path,
    r: impl Read,
    project: &Path,
    after_open: Option<AfterOpen>,
) -> Result<LoadSummary, RdyError> {
    let opened = open_stream(source, r)?;
    load_opened(source, opened, project, after_open)
}

/// [`load_from`] past step 1: loads the stream that [`open_stream`] opened. A caller that checks
/// the header first (a clone, O10) checks the header of the very stream this load consumes.
pub(crate) fn load_opened<R: Read>(
    source: &Path,
    opened: Opened<R>,
    project: &Path,
    after_open: Option<AfterOpen>,
) -> Result<LoadSummary, RdyError> {
    // the texts below name the stream by its source
    let file = source;
    let Opened {
        mut reader,
        dump_format,
        header,
    } = opened;
    // step 2: the project files come first; then check the target, writing nothing yet
    let mut files = Vec::new();
    let first = loop {
        let at = reader.offset();
        match reader.next().map_err(|e| read_error(file, e))? {
            Some(DumpItem::File { path, bytes }) => files.push((path, bytes)),
            Some(DumpItem::Batch(b)) => break Some((at, b)),
            None => break None,
        }
    };
    let absent = check_target(project, &files)?;
    let carried: Vec<String> = files.iter().map(|(path, _)| path.clone()).collect();
    let mut made = Made::default();
    let out = (|| {
        let (batches, frames, holder) = ingest(
            file,
            project,
            &mut reader,
            first,
            header.log_end,
            header.store_format,
            &mut made,
        )?;
        // the ingest validated the mode: a holder carries no project file (D-BPYN8)
        if holder && !files.is_empty() {
            return Err(holder_files());
        }
        let mut written = Vec::new();
        let mut kept = Vec::new();
        for (path, bytes) in &files {
            if absent.contains(path) {
                write_new(project, path, bytes, &mut made)?;
                written.push(path.clone());
            } else {
                kept.push(path.clone());
            }
        }
        // step 6: the store takes its place, locked by this load until the load ends
        let dot = project.join(".rdy");
        let loading = dot.join(LOADING_DIR);
        let store = dot.join(STORE_DIR);
        if std::fs::symlink_metadata(&store).is_ok() {
            return Err(has_store(project));
        }
        let lock = StoreLock::take(&loading).map_err(|e| target_error(&loading, e))?;
        made.lock = Some(lock.clone());
        std::fs::rename(&loading, &store).map_err(|e| io_at(&loading, e))?;
        made.store = Some(store);
        sync_dir(&dot).map_err(|e| io_at(&dot, e))?;
        // step 7: the index build, fsck and the counts
        let (fsck, tags, views) = check(project, &carried, &lock, &mut made, after_open)?;
        Ok(LoadSummary {
            project: project.display().to_string(),
            dump_format,
            store_format: header.store_format,
            batches,
            frames,
            files_written: written,
            files_kept: kept,
            landings: fsck.landings,
            tags,
            views,
            fsck,
            next: if holder { HOLDER_NEXT } else { NEXT }.to_string(),
        })
    })();
    // step 8: a failure takes back everything this load wrote
    if out.is_err() {
        made.undo();
    }
    out
}

/// Step 2: refuses a target in a project's `.rdy`, a target with a store, with a load's store
/// directory, with a live daemon, or with a project file of other bytes than the dump. Returns
/// the dumped paths the target does not have.
fn check_target(project: &Path, files: &[(String, Vec<u8>)]) -> Result<Vec<String>, RdyError> {
    inside_rdy(project)?;
    let dot = project.join(".rdy");
    if std::fs::symlink_metadata(dot.join(STORE_DIR)).is_ok() {
        return Err(has_store(project));
    }
    let loading = dot.join(LOADING_DIR);
    if std::fs::symlink_metadata(&loading).is_ok() {
        return Err(loading_exists(&loading));
    }
    if let Some(pid) = DAEMON_PROBE.get().and_then(|probe| probe(project)) {
        let dir = project.display();
        return Err(RdyError::new(
            E_LOCKED,
            format!("a daemon still runs for {dir} (pid {pid})"),
            format!("run rdy serve --stop in {dir}, then load or clone again"),
        ));
    }
    let mut absent = Vec::new();
    let mut differ = Vec::new();
    for (path, bytes) in files {
        let at = project.join(path);
        match std::fs::symlink_metadata(&at) {
            Err(e) if e.kind() == ErrorKind::NotFound => {
                absent.push(path.clone());
                continue;
            }
            Err(e) => return Err(io_at(&at, e)),
            Ok(m) if !m.is_file() && !m.file_type().is_symlink() => {
                differ.push(path.as_str());
                continue;
            }
            Ok(_) => {}
        }
        match std::fs::read(&at) {
            Ok(have) if have == *bytes => {}
            Ok(_) => differ.push(path.as_str()),
            // a link that names nothing, or a directory behind a link
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::IsADirectory) => {
                differ.push(path.as_str())
            }
            Err(e) => return Err(io_at(&at, e)),
        }
    }
    if !differ.is_empty() {
        return Err(RdyError::new(
            E_ALREADY_EXISTS,
            format!(
                "{} has {} with other bytes than the dump",
                project.display(),
                differ.join(", ")
            ),
            "move those files aside, or load into a new directory",
        ));
    }
    Ok(absent)
}

/// Refuses a target in a project's `.rdy` directory or below it (final ruling 3): a load there
/// puts a `.rdy.yaml` or a store among that project's files, and the project no longer opens.
/// Compares resolved paths, as `rdy dump` does, so a symlink or `..` does not pass. A project's
/// `.rdy` is known by its name as the target is written, then by its resolved place, and by its
/// name or its store along the resolved target: a symlinked `.rdy` resolves to a directory of any
/// name (final review C1).
fn inside_rdy(project: &Path) -> Result<(), RdyError> {
    let (abs, resolved) = resolve_target(project)?;
    // each project .rdy the target names as written, at its resolved place
    let guarded: Vec<PathBuf> = abs
        .ancestors()
        .filter(|dir| is_project_dot_rdy(dir))
        .filter_map(|dir| std::fs::canonicalize(dir).ok())
        .collect();
    let inside = guarded.iter().any(|g| resolved.starts_with(g))
        || resolved
            .ancestors()
            .any(|dir| is_project_dot_rdy(dir) || holds_store(dir));
    if inside {
        return Err(inside_error(project));
    }
    Ok(())
}

/// Refuses a target in `dot`, the `.rdy` directory of a clone's source, or below it, comparing
/// resolved paths (final review C1). The clone runs it before anything is read or written.
pub(crate) fn inside_source_rdy(dot: &Path, project: &Path) -> Result<(), RdyError> {
    let dot = std::fs::canonicalize(dot).map_err(|e| io_at(dot, e))?;
    let (_, resolved) = resolve_target(project)?;
    if resolved.starts_with(&dot) {
        return Err(inside_error(project));
    }
    Ok(())
}

/// The target `project` made absolute as written, and resolved. The part of the target that does
/// not exist yet is taken as written, its `..` steps applied to the resolved part: the load
/// creates it as plain directories.
fn resolve_target(project: &Path) -> Result<(PathBuf, PathBuf), RdyError> {
    let abs = std::path::absolute(project).map_err(|e| io_at(project, e))?;
    let mut base = abs.as_path();
    let mut rest = Vec::new();
    let mut resolved = loop {
        match std::fs::canonicalize(base) {
            Ok(c) => break c,
            Err(e) if e.kind() == ErrorKind::NotFound => match (base.parent(), base.file_name()) {
                (Some(parent), Some(name)) => {
                    rest.push(name.to_os_string());
                    base = parent;
                }
                // `..` as the last step: resolve the parent
                (Some(parent), None) => {
                    rest.push("..".into());
                    base = parent;
                }
                _ => return Err(io_at(project, e)),
            },
            Err(e) => return Err(io_at(base, e)),
        }
    };
    for part in rest.iter().rev() {
        match Path::new(part).components().next() {
            Some(Component::ParentDir) => {
                resolved.pop();
            }
            Some(Component::Normal(_)) => resolved.push(part),
            _ => {}
        }
    }
    Ok((abs, resolved))
}

/// The refusal of a target in a project's `.rdy`.
fn inside_error(project: &Path) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("{} is inside a project's .rdy directory", project.display()),
        "load into a new directory outside any .rdy, for example a sibling of the project",
    )
}

/// Whether `dir` is a project's `.rdy` directory: named `.rdy`, with the project's `.rdy.yaml`
/// beside it or a store in it.
fn is_project_dot_rdy(dir: &Path) -> bool {
    dir.file_name() == Some(".rdy".as_ref())
        && (dir.with_file_name(PROJECT_FILE).is_file()
            || std::fs::symlink_metadata(dir.join(STORE_DIR)).is_ok())
}

/// Whether `dir`, under any name, holds a project's store: `store/FORMAT`. A symlinked `.rdy`
/// resolves to such a directory.
fn holds_store(dir: &Path) -> bool {
    dir.join(STORE_DIR).join(FORMAT_FILE).is_file()
}

/// Steps 3 and 4: creates the project and its `.rdy` if missing, then streams every batch into
/// `.rdy/store.loading` and verifies the trailer. The batches must end at `log_end`, the end of
/// the source log that the header names, and there must be at least one. Then a dump of a store
/// format older than this rdy's (`store_format`) gets its one upgrade batch, and the store gets
/// its `FORMAT`. Returns the dump's batches and frames written, and whether the first batch set
/// the holder mode (the ingest has refused any other mode value).
fn ingest<R: Read>(
    file: &Path,
    project: &Path,
    reader: &mut DumpReader<R>,
    first: Option<(u64, DumpBatch)>,
    log_end: Location,
    store_format: u32,
    made: &mut Made,
) -> Result<(u64, u64, bool), RdyError> {
    let holder = first.as_ref().is_some_and(|(_, b)| sets_holder_mode(b));
    let dot = project.join(".rdy");
    made.create_dirs(&dot)?;
    let loading = dot.join(LOADING_DIR);
    let mut ingest = match Ingest::create(&loading) {
        Ok(i) => i,
        Err(StoreError::Io(e)) if e.kind() == ErrorKind::AlreadyExists => {
            // another load made it after the check: never ours to remove
            return Err(loading_exists(&loading));
        }
        Err(e) => {
            // the directory may stand half made: it did not exist at the check, so it is ours
            made.store = Some(loading.clone());
            return Err(target_error(&loading, e));
        }
    };
    made.store = Some(loading.clone());
    let mut next = first;
    let mut n = 0u64;
    while let Some((at, b)) = next {
        n += 1;
        ingest.batch(n, &b).map_err(|e| match e {
            StoreError::Dump { what, .. } => damaged(file, at, &what),
            e => target_error(&loading, e),
        })?;
        let at = reader.offset();
        next = match reader.next().map_err(|e| read_error(file, e))? {
            Some(DumpItem::Batch(b)) => Some((at, b)),
            // the reader refuses a file after a batch
            Some(DumpItem::File { .. }) => {
                return Err(damaged(file, at, "a record of kind 241 cannot stand here"));
            }
            None => None,
        };
    }
    let end = ingest.end();
    if end != log_end {
        return Err(incomplete(format!(
            "its header says the log ends at segment {} offset {}, but its batches end at segment {} offset {}",
            log_end.segment, log_end.offset, end.segment, end.offset
        )));
    }
    if n == 0 {
        return Err(incomplete("it holds no batches".to_string()));
    }
    // a dump of an older store format gets one logged batch that names this rdy's format
    ingest
        .upgrade(store_format)
        .map_err(|e| target_error(&loading, e))?;
    // the dropped frames' authorizations are checked against the whole stream, at its end
    let at = reader.offset();
    let report = ingest.finish().map_err(|e| match e {
        StoreError::Dump { what, .. } => damaged(file, at, &what),
        e => target_error(&loading, e),
    })?;
    Ok((report.batches, report.frames, holder))
}

/// Whether batch `b`, the first of a log, sets the mode ref. `Ingest::batch` judges the value
/// (only `holder`) and refuses the batch otherwise, so a load that goes on has a holder.
fn sets_holder_mode(b: &DumpBatch) -> bool {
    b.frames.iter().any(|f| match f {
        Frame::Present {
            kind: kinds::REF_UPDATE,
            payload,
            ..
        } => {
            rdy_store::bounded::decode::<RefUpdate>(payload).is_ok_and(|u| u.name == REF_STORE_MODE)
        }
        _ => false,
    })
}

/// A holder dump that carries a project file.
fn holder_files() -> RdyError {
    RdyError::new(
        E_DUMP,
        "a holder dump must not contain project files",
        "write a new dump from the holder",
    )
}

/// Step 5: writes one project file the target does not have, durably, never over a file.
fn write_new(project: &Path, path: &str, bytes: &[u8], made: &mut Made) -> Result<(), RdyError> {
    if path.starts_with(&format!("{PACKS_DIR}/")) {
        made.create_dirs(&project.join(PACKS_DIR))?;
    }
    let at = project.join(path);
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&at)
        .map_err(|e| io_at(&at, e))?;
    made.files.push(at.clone());
    f.write_all(bytes)
        .and_then(|()| f.sync_all())
        .map_err(|e| io_at(&at, e))?;
    let parent = at.parent().unwrap_or(project);
    sync_dir(parent).map_err(|e| io_at(parent, e))
}

/// Step 7: opens the loaded project under the load's `lock` (the open builds its index from the
/// log), runs `after_open` once the store's meta refs pass their check, then `fsck`, counts the
/// tags and views, and closes it. An open that finds the load does not hold the store keeps
/// every file (`made.keep`): it is not the load's to remove. A failure of the meta check, `fsck`
/// or the counts that names a target file outside `carried` (the dump's project files) names
/// it; `after_open` returns its own errors.
fn check(
    project: &Path,
    carried: &[String],
    lock: &StoreLock,
    made: &mut Made,
    after_open: Option<AfterOpen>,
) -> Result<(FsckReport, usize, usize), RdyError> {
    let fails = |e: RdyError| fails_fsck(project, carried, e);
    let mut r = match Rdy::open_locked(project, lock) {
        Ok(r) => r,
        Err(e) if e.code == E_LOCKED => {
            made.keep = true;
            let dir = project.display();
            return Err(RdyError::new(
                E_DUMP,
                format!(
                    "the dump loaded into {dir}, but another rdy process opened the project before the check finished"
                ),
                format!(
                    "close the other sessions on {dir}, then run rdy fsck there; if it fails, delete {dir} and load again"
                ),
            ));
        }
        Err(e) => return Err(fails(e)),
    };
    if let Some(step) = after_open {
        // a step writes meta refs, never over bad ones (ruling task-5): first the loaded store's
        // meta refs pass the store's own check, with the source's repo id, and a failure is the
        // load's, in the words fsck gives: fsck's first failure, which is load's for this dump
        // (it can be a fault that fsck finds before the meta refs)
        if let Err(meta) = r.store().check_meta() {
            let e = r.fsck().err().unwrap_or_else(|| meta.into());
            let _ = r.close();
            return Err(fails(e));
        }
        // the step's own errors are its own; fsck's and the counts' are the load's
        if let Err(e) = step(&mut r) {
            let _ = r.close();
            return Err(e);
        }
    }
    let counted = (|| {
        let fsck = r.fsck()?;
        let tags = r.store().count_refs("tag/")?;
        let views = r.store().count_refs("view/")?;
        Ok((fsck, tags, views))
    })();
    r.close().map_err(fails)?;
    counted.map_err(fails)
}

/// A failure of step 7 other than the lock. When it names a target file that the dump does not
/// carry (final ruling 2), in its message or its fix, as a full path or relative to the project
/// (a config file's parse error names it only in its fix), that file stops the project from
/// opening; otherwise the loaded project does not pass, and the source needs the repair.
fn fails_fsck(project: &Path, carried: &[String], e: RdyError) -> RdyError {
    if let Some(at) = target_only(project, carried)
        .into_iter()
        .find(|(rel, at)| {
            let full = at.display().to_string();
            [&e.message, &e.fix]
                .iter()
                .any(|t| t.contains(&full) || t.contains(rel.as_str()))
        })
        .map(|(_, at)| at)
    {
        let at = at.display();
        return RdyError::new(
            E_DUMP,
            format!(
                "the dump loaded, but {at}, which the dump does not carry, stops the project from opening: {}",
                e.message
            ),
            format!("fix or move {at}, then load again"),
        );
    }
    RdyError::new(
        E_DUMP,
        format!("the dump loads, but the project fails fsck: {}", e.message),
        "run rdy fsck on the source project, repair it, then dump again",
    )
}

/// The files at the target that the project reads and the dump does not carry (`carried`):
/// `.rdy/local.yaml` (F5 never carries it), `.rdyignore`, and each entry of `.rdy/packs`; each
/// relative to the project and in full.
fn target_only(project: &Path, carried: &[String]) -> Vec<(String, PathBuf)> {
    let mut rel = vec![LOCAL_FILE.to_string(), IGNORE_FILE.to_string()];
    if let Ok(entries) = std::fs::read_dir(project.join(PACKS_DIR)) {
        let mut packs: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().to_str().map(|n| format!("{PACKS_DIR}/{n}")))
            .collect();
        packs.sort();
        rel.extend(packs);
    }
    rel.into_iter()
        .filter(|r| !carried.contains(r))
        .map(|r| {
            let at = project.join(&r);
            (r, at)
        })
        .filter(|(_, at)| std::fs::symlink_metadata(at).is_ok())
        .collect()
}

/// What a load wrote at the target, to take it back on a failure (step 8).
#[derive(Default)]
struct Made {
    /// The directories this load created, outermost first.
    dirs: Vec<PathBuf>,
    /// The project files this load created.
    files: Vec<PathBuf>,
    /// The store directory this load created, under its current name.
    store: Option<PathBuf>,
    /// The new store's lock, held from before the rename until the load ends: the undo runs
    /// under it.
    lock: Option<StoreLock>,
    /// Set when the load finds it does not hold the store: the undo then removes nothing.
    keep: bool,
}

impl Made {
    /// Creates `dir` and its missing ancestors durably, and remembers each one it creates.
    fn create_dirs(&mut self, dir: &Path) -> Result<(), RdyError> {
        let mut missing = Vec::new();
        let mut cur = Some(dir);
        while let Some(d) = cur {
            if d.as_os_str().is_empty() || std::fs::symlink_metadata(d).is_ok() {
                break;
            }
            missing.push(d.to_path_buf());
            cur = d.parent();
        }
        missing.reverse();
        // remembered first: a creation that fails half way is taken back too
        self.dirs.extend(missing);
        create_dir_durable(dir).map_err(|e| io_at(dir, e))?;
        Ok(())
    }

    /// Removes the store, every file and every directory this load created, while the load
    /// still holds the store's lock; then releases the lock. A directory that holds anything
    /// else stays. A load that does not hold the store (`keep`) removes nothing.
    fn undo(self) {
        if self.keep {
            return;
        }
        if let Some(store) = &self.store {
            // `.rdy/store` goes back to its loading name first, in one step: an open that races
            // the removal then finds no store, and never adds a file to the one being removed
            let gone = match store.parent() {
                Some(dot) if store.file_name() == Some(STORE_DIR.as_ref()) => {
                    let back = dot.join(LOADING_DIR);
                    match std::fs::rename(store, &back) {
                        Ok(()) => back,
                        Err(_) => store.clone(),
                    }
                }
                _ => store.clone(),
            };
            let _ = std::fs::remove_dir_all(gone);
        }
        for f in self.files.iter().rev() {
            let _ = std::fs::remove_file(f);
        }
        for d in self.dirs.iter().rev() {
            let _ = std::fs::remove_dir(d);
        }
        // only now may another process take the store, which no longer exists
        drop(self.lock);
    }
}

/// An error of the dump reader at `file`: the prelude and format cases name the file, every
/// other case is damage at a byte.
fn read_error(file: &Path, e: StoreError) -> RdyError {
    match e {
        StoreError::Dump { at, what } => match DumpRefusal::of(&what) {
            DumpRefusal::NotADump => RdyError::new(
                E_DUMP,
                format!("{}: {what}", file.display()),
                "pass a file that rdy dump wrote",
            ),
            DumpRefusal::FormatNotRead => RdyError::new(
                E_DUMP,
                format!("{}: {what}", file.display()),
                "load it with the rdy that wrote it, or a later one",
            ),
            DumpRefusal::Damaged | DumpRefusal::Claim => damaged(file, at, &what),
        },
        StoreError::Io(e) => io_at(file, e),
        e => e.into(),
    }
}

/// The dump's batches are not the whole source log (`what` says how).
fn incomplete(what: String) -> RdyError {
    RdyError::new(
        E_DUMP,
        format!("the dump is incomplete: {what}"),
        "make a new dump from the source with rdy dump",
    )
}

/// `file` is damaged at byte `at`. Dropped frames that claim more bytes than rdy writes for
/// them (`DumpRefusal::Claim`, ruling task-9) may not be rdy's dump at all, and their fix says so.
fn damaged(file: &Path, at: u64, what: &str) -> RdyError {
    let fix = match DumpRefusal::of(what) {
        DumpRefusal::Claim => {
            "the dump is damaged or was not written by rdy; dump the source again"
        }
        _ => "dump the project again; rdy never repairs a dump",
    };
    RdyError::new(
        E_DUMP,
        format!("{} is damaged at byte {at}: {what}", file.display()),
        fix,
    )
}

/// An error of the new store at `dir`: its i/o errors are the target's.
fn target_error(dir: &Path, e: StoreError) -> RdyError {
    match e {
        StoreError::Io(e) => io_at(dir, e),
        e => e.into(),
    }
}

fn has_store(project: &Path) -> RdyError {
    RdyError::new(
        E_ALREADY_EXISTS,
        format!("{} already has a store", project.display()),
        "load into a new directory, or move .rdy/store aside first",
    )
}

fn loading_exists(loading: &Path) -> RdyError {
    RdyError::new(
        E_LOCKED,
        format!(
            "{} exists: another load runs, or one was stopped",
            loading.display()
        ),
        "wait for the other load; if none runs, remove .rdy/store.loading",
    )
}

/// An i/o error at `path`, named.
fn io_at(path: &Path, e: std::io::Error) -> RdyError {
    RdyError::new(
        E_IO,
        format!("{}: {e}", path.display()),
        "check file permissions and disk space",
    )
}
