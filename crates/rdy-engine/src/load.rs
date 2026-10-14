//! `rdy load` (plan Dump and load; D-69AY3, D-WWAK7, D-D2NPN, D-FR0C4 F3 and F5): rebuilds a
//! project from a dump into a directory with no store. The new log is the source log byte for
//! byte, so every hash, landing, tag and open view is kept. The first open builds the index from
//! the log (the same replay as a reindex), then the load runs `fsck`.
//!
//! The steps (closed):
//! 1. Read the dump's prelude and header.
//! 2. Read its project files, and check the target before anything is written: no
//!    `.rdy/store`, no `.rdy/store.loading`, and no project file with other bytes than the dump.
//!    A file with the same bytes is kept.
//! 3. Create the project directory and its `.rdy` if missing, then `.rdy/store.loading`.
//! 4. Ingest every batch at its source place; the trailer must verify. The batches must end
//!    where the header says the source log ends, and there must be at least one (every store
//!    holds its format batch from its first open).
//! 5. Write each project file the target does not have.
//! 6. Take the new store's lock, then rename `.rdy/store.loading` to `.rdy/store`. The load
//!    holds the lock until it ends, so no other rdy process opens the project before step 7
//!    ends (D-R7N9J, D-A6ZZA).
//! 7. Open the project under that lock (the index build), run `fsck`, and count landings, tags
//!    and views.
//!
//! A failure in steps 3 to 7 removes the store, every file step 5 wrote and every directory
//! step 3 or 5 created, so the target is as it was. The removal runs while the load holds the
//! store's lock; a load that finds it does not hold the lock removes nothing. The load never
//! deletes a file it did not write: a pack file at the target that the dump does not hold stays
//! where it is.
use crate::Rdy;
use crate::error::{E_ALREADY_EXISTS, E_BAD_INPUT, E_DUMP, E_IO, E_LOCKED, RdyError};
use crate::fsck::FsckReport;
use rdy_store::dump::{DumpBatch, DumpItem, DumpReader};
use rdy_store::fsutil::{create_dir_durable, sync_dir};
use rdy_store::ingest::Ingest;
use rdy_store::layout::STORE_DIR;
use rdy_store::segment::Location;
use rdy_store::{StoreError, StoreLock};
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

/// The store directory a load writes before it renames it to `.rdy/store`.
const LOADING_DIR: &str = "store.loading";
/// The directory of the pack files, relative to the project root.
const PACKS_DIR: &str = ".rdy/packs";
/// What the person does after a load: a dump carries no working copy (O9).
const NEXT: &str = "run rdy checkout to bind a working copy; a dump carries none";

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
    let f = File::open(file).map_err(|e| match e.kind() {
        ErrorKind::NotFound => RdyError::new(
            E_BAD_INPUT,
            format!("{} does not exist", file.display()),
            "check the path of the dump",
        ),
        _ => io_at(file, e),
    })?;
    let (mut reader, dump_format, header) =
        DumpReader::open(BufReader::new(f)).map_err(|e| read_error(file, e))?;
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
    let mut made = Made::default();
    let out = (|| {
        let (batches, frames) =
            ingest(file, project, &mut reader, first, header.log_end, &mut made)?;
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
        let (fsck, tags, views) = check(project, &lock, &mut made)?;
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
            next: NEXT.to_string(),
        })
    })();
    // step 8: a failure takes back everything this load wrote
    if out.is_err() {
        made.undo();
    }
    out
}

/// Step 2: refuses a target with a store, with a load's store directory, or with a project file
/// of other bytes than the dump. Returns the dumped paths the target does not have.
fn check_target(project: &Path, files: &[(String, Vec<u8>)]) -> Result<Vec<String>, RdyError> {
    let dot = project.join(".rdy");
    if std::fs::symlink_metadata(dot.join(STORE_DIR)).is_ok() {
        return Err(has_store(project));
    }
    let loading = dot.join(LOADING_DIR);
    if std::fs::symlink_metadata(&loading).is_ok() {
        return Err(loading_exists(&loading));
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

/// Steps 3 and 4: creates the project and its `.rdy` if missing, then streams every batch into
/// `.rdy/store.loading` and verifies the trailer. The batches must end at `log_end`, the end of
/// the source log that the header names, and there must be at least one. Returns the batches
/// and frames written.
fn ingest<R: Read>(
    file: &Path,
    project: &Path,
    reader: &mut DumpReader<R>,
    first: Option<(u64, DumpBatch)>,
    log_end: Location,
    made: &mut Made,
) -> Result<(u64, u64), RdyError> {
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
    let report = ingest.finish().map_err(|e| target_error(&loading, e))?;
    if report.end != log_end {
        return Err(incomplete(format!(
            "its header says the log ends at segment {} offset {}, but its batches end at segment {} offset {}",
            log_end.segment, log_end.offset, report.end.segment, report.end.offset
        )));
    }
    if report.batches == 0 {
        return Err(incomplete("it holds no batches".to_string()));
    }
    Ok((report.batches, report.frames))
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
/// log), runs `fsck`, counts the tags and views, and closes it. An open that finds the load
/// does not hold the store keeps every file (`made.keep`): it is not the load's to remove.
fn check(
    project: &Path,
    lock: &StoreLock,
    made: &mut Made,
) -> Result<(FsckReport, usize, usize), RdyError> {
    let r = match Rdy::open_locked(project, lock) {
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
        Err(e) => return Err(fails_fsck(e)),
    };
    let counted = (|| {
        let fsck = r.fsck()?;
        let tags = r.store().count_refs("tag/")?;
        let views = r.store().count_refs("view/")?;
        Ok((fsck, tags, views))
    })();
    r.close().map_err(fails_fsck)?;
    counted.map_err(fails_fsck)
}

/// A failure of step 7 other than the lock: the loaded project does not pass.
fn fails_fsck(e: RdyError) -> RdyError {
    RdyError::new(
        E_DUMP,
        format!("the dump loads, but the project fails fsck: {}", e.message),
        "run rdy fsck on the source project, repair it, then dump again",
    )
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
        StoreError::Dump { what, .. } if what == "it does not start with RDYDUMP" => RdyError::new(
            E_DUMP,
            format!("{}: {what}", file.display()),
            "pass a file that rdy dump wrote",
        ),
        StoreError::Dump { what, .. }
            if (what.starts_with("dump format ") || what.starts_with("store format "))
                && what.ends_with(" is not one this rdy reads") =>
        {
            RdyError::new(
                E_DUMP,
                format!("{}: {what}", file.display()),
                "load it with the rdy that wrote it, or a later one",
            )
        }
        StoreError::Dump { at, what } => damaged(file, at, &what),
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

/// `file` is damaged at byte `at`.
fn damaged(file: &Path, at: u64, what: &str) -> RdyError {
    RdyError::new(
        E_DUMP,
        format!("{} is damaged at byte {at}: {what}", file.display()),
        "dump the project again; rdy never repairs a dump",
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
